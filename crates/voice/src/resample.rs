//! A streaming resampler: cubic (Catmull-Rom) interpolation with an
//! anti-aliasing filter when downsampling. Needed when a device cannot do
//! 48 kHz natively (WASAPI at 44.1 kHz, headsets at 16 kHz and so on).

use crate::dsp::Biquad;

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Position of the next output sample within `buf`.
    pos: f64,
    buf: Vec<f32>,
    low_pass: Option<[Biquad; 2]>,
    passthrough: bool,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let passthrough = from_rate == to_rate || from_rate == 0 || to_rate == 0;
        let low_pass = (to_rate < from_rate && !passthrough).then(|| {
            // a 4th-order Butterworth from two sections, cut just below the new Nyquist
            let f0 = 0.45 * to_rate as f32;
            [
                Biquad::low_pass(from_rate as f32, f0, 0.541_196_1),
                Biquad::low_pass(from_rate as f32, f0, 1.306_563),
            ]
        });
        Self {
            step: if passthrough { 1.0 } else { from_rate as f64 / to_rate as f64 },
            pos: 1.0,
            // one sample of history so the interpolation has something to lean on
            buf: vec![0.0],
            low_pass,
            passthrough,
        }
    }

    /// Add input samples, append the finished output samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        match &mut self.low_pass {
            Some(stages) => self.buf.extend(input.iter().map(|&s| {
                let s = stages[0].process(s);
                stages[1].process(s)
            })),
            None => self.buf.extend_from_slice(input),
        }
        loop {
            let i = self.pos.floor() as usize;
            if i + 2 >= self.buf.len() {
                break;
            }
            let t = (self.pos - i as f64) as f32;
            let (p0, p1, p2, p3) = (self.buf[i - 1], self.buf[i], self.buf[i + 1], self.buf[i + 2]);
            // Catmull-Rom
            let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
            let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
            let c = -0.5 * p0 + 0.5 * p2;
            out.push(((a * t + b) * t + c) * t + p1);
            self.pos += self.step;
        }
        // drop what is used up, keeping one sample left of the position
        let keep_from = (self.pos.floor() as usize).saturating_sub(1).min(self.buf.len());
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::rms;
    use std::f32::consts::PI;

    fn sine(freq: f32, fs: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| 0.5 * (2.0 * PI * freq * i as f32 / fs).sin()).collect()
    }

    fn run(from: u32, to: u32, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut r = Resampler::new(from, to);
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.process(c, &mut out);
        }
        out
    }

    #[test]
    fn same_rate_is_passthrough() {
        let x = sine(440.0, 48_000.0, 1000);
        assert_eq!(run(48_000, 48_000, &x, 97), x);
    }

    #[test]
    fn output_length_tracks_ratio() {
        for (from, to) in [(44_100u32, 48_000u32), (48_000, 44_100), (16_000, 48_000), (96_000, 48_000), (48_000, 8_000)] {
            let x = sine(440.0, from as f32, from as usize);
            let y = run(from, to, &x, 441);
            let want = to as i64;
            // a couple of input samples always stay in reserve for interpolation
            assert!((y.len() as i64 - want).abs() <= 8, "{from}->{to}: {} instead of ~{want}", y.len());
        }
    }

    #[test]
    fn tone_survives_resampling() {
        for (from, to) in [(44_100u32, 48_000u32), (48_000, 44_100), (16_000, 48_000), (96_000, 48_000)] {
            let x = sine(1_000.0, from as f32, from as usize);
            let y = run(from, to, &x, 512);
            let level = rms(&y[y.len() / 4..]);
            assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.02, "{from}->{to}: rms {level}");
        }
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let x = sine(700.0, 44_100.0, 8_000);
        let a = run(44_100, 48_000, &x, 8_000);
        let b = run(44_100, 48_000, &x, 37);
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().zip(&b) {
            assert!((p - q).abs() < 1e-6);
        }
    }

    #[test]
    fn downsampling_filters_out_of_band_content() {
        // 12 kHz downsampled to 16 kHz (Nyquist 8 kHz) must vanish, not fold back
        let x = sine(12_000.0, 48_000.0, 48_000);
        let y = run(48_000, 16_000, &x, 480);
        assert!(rms(&y[y.len() / 2..]) < 0.03);
    }
}
