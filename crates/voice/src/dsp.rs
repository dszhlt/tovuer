//! Small signal processing: filters, a limiter, automatic gain.

use std::f32::consts::PI;

/// A biquad filter (RBJ cookbook), transposed direct form II.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// High-pass: cuts hum and boom below `f0`.
    pub fn high_pass(fs: f32, f0: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * f0 / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Low-pass: protection from aliasing before downsampling.
    pub fn low_pass(fs: f32, f0: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * f0 / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn process_block(&mut self, block: &mut [f32]) {
        for s in block {
            *s = self.process(*s);
        }
    }
}

/// A soft limiter: transparent up to 0.85, above it eases towards ±1
/// (instead of the rasp of hard clipping).
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    const T: f32 = 0.85;
    if x > T {
        T + (1.0 - T) * ((x - T) / (1.0 - T)).tanh()
    } else if x < -T {
        -T - (1.0 - T) * ((-x - T) / (1.0 - T)).tanh()
    } else {
        x
    }
}

/// RMS level of a block, 0..1.
pub fn rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let sum: f32 = block.iter().map(|s| s * s).sum();
    (sum / block.len() as f32).sqrt()
}

pub fn peak(block: &[f32]) -> f32 {
    block.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

/// Automatic gain: brings speech to one level, so a quiet microphone needs no
/// "say again" and a loud one does not hurt the ears. The gain rises slowly
/// (≈1.5 s), falls fast (≈100 ms) and changes only on frames with voice — the
/// background between phrases is not pumped up.
#[derive(Clone, Debug)]
pub struct Agc {
    gain: f32,
    applied: f32,
}

impl Agc {
    /// Target speech level (RMS), about −20 dBFS.
    pub const TARGET: f32 = 0.1;
    pub const MIN_GAIN: f32 = 0.5;
    pub const MAX_GAIN: f32 = 8.0;

    pub fn new() -> Self {
        Self { gain: 1.0, applied: 1.0 }
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn process(&mut self, frame: &mut [f32], voiced: bool) {
        if frame.is_empty() {
            return;
        }
        let level = rms(frame);
        if voiced && level > 1e-4 {
            let want = (Self::TARGET / level).clamp(Self::MIN_GAIN, Self::MAX_GAIN);
            let k = if want > self.gain { 0.02 } else { 0.25 };
            self.gain += (want - self.gain) * k;
        }
        // keep peaks off the ceiling: the limiter is a safety net, not the normal mode
        let p = peak(frame);
        if p * self.gain > 0.95 {
            self.gain = (0.95 / p).max(Self::MIN_GAIN);
        }
        // a smooth ramp from the old gain to the new one — no steps
        let step = (self.gain - self.applied) / frame.len() as f32;
        for s in frame.iter_mut() {
            self.applied += step;
            *s *= self.applied;
        }
        self.applied = self.gain;
    }
}

impl Default for Agc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, fs: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * freq * i as f32 / fs).sin()).collect()
    }

    #[test]
    fn high_pass_kills_rumble_keeps_voice() {
        let mut hp = Biquad::high_pass(48_000.0, 90.0, 0.707);
        let mut low = sine(20.0, 48_000.0, 48_000, 0.5);
        hp.process_block(&mut low);
        assert!(rms(&low[24_000..]) < 0.05, "a 20 Hz hum must be suppressed");

        let mut hp = Biquad::high_pass(48_000.0, 90.0, 0.707);
        let mut voice = sine(1_000.0, 48_000.0, 48_000, 0.5);
        hp.process_block(&mut voice);
        assert!((rms(&voice[24_000..]) - 0.5 / 2f32.sqrt()).abs() < 0.01, "1 kHz passes untouched");
    }

    #[test]
    fn low_pass_kills_highs() {
        let mut lp = Biquad::low_pass(48_000.0, 4_000.0, 0.707);
        let mut hi = sine(16_000.0, 48_000.0, 9_600, 0.5);
        lp.process_block(&mut hi);
        assert!(rms(&hi[4_800..]) < 0.05);
    }

    #[test]
    fn soft_limit_is_transparent_then_bounded() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.84), -0.84);
        for x in [0.9f32, 1.5, 10.0, 1e6] {
            let y = soft_limit(x);
            assert!(y > 0.85 && y <= 1.0, "{x} -> {y}");
            assert_eq!(soft_limit(-x), -y);
        }
        assert!(soft_limit(0.95) < soft_limit(1.2), "monotonic");
    }

    #[test]
    fn agc_lifts_quiet_speech_and_tames_loud() {
        let mut agc = Agc::new();
        let mut last = 0.0;
        for _ in 0..400 {
            let mut f = sine(300.0, 48_000.0, 960, 0.02);
            agc.process(&mut f, true);
            last = rms(&f);
        }
        assert!(last > 0.06, "a quiet voice is raised: {last}");
        assert!(agc.gain() <= Agc::MAX_GAIN);

        let mut agc = Agc::new();
        for _ in 0..50 {
            let mut f = sine(300.0, 48_000.0, 960, 0.9);
            agc.process(&mut f, true);
            assert!(peak(&f) <= 0.96, "no clipping");
        }
        assert!(agc.gain() < 1.0);
    }

    #[test]
    fn agc_ignores_background_between_phrases() {
        let mut agc = Agc::new();
        for _ in 0..500 {
            let mut f = sine(300.0, 48_000.0, 960, 0.001);
            agc.process(&mut f, false);
        }
        assert!((agc.gain() - 1.0).abs() < 1e-6, "silence does not pump up the gain");
    }
}
