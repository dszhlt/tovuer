//! Noise suppression: RNNoise at the strength the person picked.
//!
//! - **soft** lets a share of the original through: the noise is only
//!   turned down (about 10 dB), the voice stays natural;
//! - **normal** is RNNoise as it is;
//! - **strong** runs RNNoise twice and then lowers whatever stays near the
//!   background's level — hiss RNNoise lets through, the rest between words
//!   — the quietest background, at the price of a thinner voice;
//! - **ai** is a neural network, DeepFilterNet3 (see [`crate::neural`]):
//!   keyboard clatter, the street and voices around go too, as with Krisp.
//!
//! RNNoise runs whatever the strength: its speech probability opens the
//! microphone's gate.

use nnnoiseless::DenoiseState;

use crate::FRAME;
use crate::neural::{self, Neural};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Strength {
    Soft = 0,
    #[default]
    Normal = 1,
    Strong = 2,
    Neural = 3,
}

impl Strength {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Strength::Soft,
            2 => Strength::Strong,
            3 => Strength::Neural,
            _ => Strength::Normal,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strength::Soft => "soft",
            Strength::Normal => "normal",
            Strength::Strong => "strong",
            Strength::Neural => "ai",
        }
    }
}

/// How much of the original the soft mode keeps.
const SOFT_KEEP: f32 = 0.3;
/// The strong mode's floor between words (about −26 dB).
const STRONG_FLOOR: f32 = 0.05;
/// The strong mode's gate: shut up to this far above the background (in
/// dB), open from the next one. RNNoise's own speech probability cannot
/// tell hiss from a voice, the level can.
const GATE_SHUT_DB: f32 = 3.0;
const GATE_OPEN_DB: f32 = 9.0;
/// How fast the gate follows, per 10 ms: opens at once, closes over ~100 ms.
const GATE_ATTACK: f32 = 0.7;
const GATE_RELEASE: f32 = 0.12;
/// The background is the quietest moment of the last 1.28 s (in 10 ms steps).
const FLOOR_WINDOW: usize = 128;
/// Quieter than this (about −90 dBFS) is digital silence, not a background.
const FLOOR_MIN: f32 = 1.0;

const HALF: usize = DenoiseState::FRAME_SIZE;
const _: () = assert!(HALF == neural::HOP, "RNNoise and the network both step 10 ms");

pub struct Suppressor {
    first: Box<DenoiseState<'static>>,
    second: Box<DenoiseState<'static>>,
    gain: f32,
    /// Recent power after cleaning, smoothed (mean square, int16 scale):
    /// the quietest of it is the background.
    recent: std::collections::VecDeque<f32>,
    level: f32,
    /// The previous 10 ms of input: RNNoise gives its output that much
    /// later, and the soft mode mixes the original in line with it.
    late: [f32; HALF],
    /// The neural network, started the first time it is wanted.
    neural: Option<Neural>,
    /// RNNoise's last outputs, oldest first: when the network has no answer,
    /// the one as far behind as the network is stands in — no jump in time.
    rn_past: [[f32; HALF]; neural::DELAY_HOPS - 1],
}

impl Default for Suppressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Suppressor {
    pub fn new() -> Self {
        Self {
            first: DenoiseState::new(),
            second: DenoiseState::new(),
            gain: 1.0,
            recent: std::collections::VecDeque::with_capacity(FLOOR_WINDOW),
            level: 0.0,
            late: [0.0; HALF],
            neural: None,
            rn_past: [[0.0; HALF]; neural::DELAY_HOPS - 1],
        }
    }

    /// Start loading the neural network now, before it is needed (it takes
    /// about half a second, on a thread of its own).
    pub fn warm_up(&mut self) {
        self.neural.get_or_insert_with(Neural::start);
    }

    /// Where the neural network is, if it was ever wanted.
    pub fn neural_state(&self) -> Option<neural::State> {
        self.neural.as_ref().map(Neural::state)
    }

    /// Pieces the network cleaned in time, and those RNNoise took instead.
    pub fn neural_counts(&self) -> (u64, u64) {
        self.neural.as_ref().map_or((0, 0), |n| (n.answered, n.missed))
    }

    /// Why the neural network would not load.
    pub fn neural_error(&self) -> Option<String> {
        self.neural.as_ref().and_then(Neural::error)
    }

    /// Clean a 20 ms frame in place (when `on`). Returns RNNoise's speech
    /// probability, 0..1 — it is worked out with suppression off too.
    pub fn process(&mut self, frame: &mut [f32; FRAME], on: bool, strength: Strength) -> f32 {
        let mut vad = 0.0f32;
        let mut scaled = [0.0f32; HALF];
        let mut cleaned = [0.0f32; HALF];
        let mut again = [0.0f32; HALF];
        // RNNoise works on 10 ms at int16 scale
        for half in frame.as_chunks_mut::<HALF>().0 {
            for (dst, src) in scaled.iter_mut().zip(half.iter()) {
                *dst = src * 32768.0;
            }
            let p = self.first.process_frame(&mut cleaned, &scaled);
            vad = vad.max(p);
            let late = std::mem::replace(&mut self.late, scaled);
            // RNNoise's output, kept a little for the network's stand-in
            let mut rn_now = [0.0f32; HALF];
            for (dst, c) in rn_now.iter_mut().zip(cleaned.iter()) {
                *dst = c / 32768.0;
            }
            let rn_behind = self.rn_past[0];
            self.rn_past.rotate_left(1);
            self.rn_past[neural::DELAY_HOPS - 2] = rn_now;
            if !on {
                continue;
            }
            match strength {
                Strength::Neural => {
                    // the original goes to the network; if it has no answer
                    // in time (or is still loading), RNNoise's from as far
                    // back stands in
                    let piece: &[f32; HALF] = half;
                    *half = self.neural.get_or_insert_with(Neural::start).process(piece).unwrap_or(rn_behind);
                }
                Strength::Soft => {
                    for ((s, c), o) in half.iter_mut().zip(cleaned.iter()).zip(late.iter()) {
                        *s = (c + SOFT_KEEP * (o - c)) / 32768.0;
                    }
                }
                Strength::Normal => {
                    for (s, c) in half.iter_mut().zip(cleaned.iter()) {
                        *s = c / 32768.0;
                    }
                }
                Strength::Strong => {
                    self.second.process_frame(&mut again, &cleaned);
                    let power = (again.iter().map(|x| x * x).sum::<f32>() / HALF as f32).max(FLOOR_MIN);
                    // a little smoothing: RNNoise leaves hiss flickering from one 10 ms to the next
                    self.level = if self.level == 0.0 { power } else { self.level + (power - self.level) * 0.5 };
                    if self.recent.len() == FLOOR_WINDOW {
                        self.recent.pop_front();
                    }
                    self.recent.push_back(self.level);
                    let floor = self.recent.iter().copied().fold(f32::MAX, f32::min);
                    let above = 10.0 * (self.level / floor).log10();
                    let open = ((above - GATE_SHUT_DB) / (GATE_OPEN_DB - GATE_SHUT_DB)).clamp(0.0, 1.0);
                    let target = STRONG_FLOOR + (1.0 - STRONG_FLOOR) * open;
                    let speed = if target > self.gain { GATE_ATTACK } else { GATE_RELEASE };
                    let from = self.gain;
                    self.gain += (target - self.gain) * speed;
                    // a ramp across the 10 ms: no clicks when the gain moves
                    for (i, (s, c)) in half.iter_mut().zip(again.iter()).enumerate() {
                        let g = from + (self.gain - from) * (i + 1) as f32 / HALF as f32;
                        *s = c * g / 32768.0;
                    }
                }
            }
        }
        vad
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::rms;

    /// A fixed pseudo-random sequence, -0.5..0.5.
    fn random(n: usize) -> Vec<f32> {
        let mut seed = 0x1234_5678u32;
        (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as f32 / u32::MAX as f32 - 0.5
            })
            .collect()
    }

    fn frames(samples: &[f32]) -> Vec<[f32; FRAME]> {
        samples.as_chunks::<FRAME>().0.to_vec()
    }

    /// A fan or an air conditioner: a deep, steady rumble.
    fn fan(frames_n: usize) -> Vec<[f32; FRAME]> {
        let mut low = 0.0f32;
        frames(&random(frames_n * FRAME).into_iter().map(|w| {
            low += (w - low) * 0.05;
            low * 0.4
        }).collect::<Vec<_>>())
    }

    /// The hiss of a cheap microphone: flat, high.
    fn hiss(frames_n: usize) -> Vec<[f32; FRAME]> {
        frames(&random(frames_n * FRAME).into_iter().map(|w| w * 0.1).collect::<Vec<_>>())
    }

    /// The level left of `input` after the suppressor settles.
    fn left(input: &[[f32; FRAME]], on: bool, strength: Strength) -> f32 {
        let mut s = Suppressor::new();
        let mut levels = Vec::new();
        for (i, frame) in input.iter().enumerate() {
            let mut f = *frame;
            s.process(&mut f, on, strength);
            if i >= input.len() / 2 {
                levels.push(rms(&f));
            }
        }
        levels.iter().sum::<f32>() / levels.len() as f32
    }

    #[test]
    fn a_fan_is_turned_down_softly_or_taken_away() {
        let noise = fan(150);
        let raw = left(&noise, false, Strength::Normal);
        let soft = left(&noise, true, Strength::Soft);
        let normal = left(&noise, true, Strength::Normal);
        let strong = left(&noise, true, Strength::Strong);
        assert!(raw > 0.01, "the rumble is there: {raw}");
        assert!((raw * 0.2..raw * 0.4).contains(&soft), "soft keeps about {SOFT_KEEP} of it: {soft} of {raw}");
        assert!(normal < raw * 0.05, "normal takes it away: {normal}");
        assert!(strong <= normal * 1.1, "strong too: {strong}");
    }

    #[test]
    fn strong_quiets_the_hiss_normal_leaves() {
        let noise = hiss(150);
        let normal = left(&noise, true, Strength::Normal);
        let strong = left(&noise, true, Strength::Strong);
        assert!(strong < normal * 0.5, "strong: {strong}, normal: {normal}");
    }

    #[test]
    fn soft_mixes_the_original_in_step() {
        // in step, a clean voice comes out the same with suppression on or
        // off; out of step the mix would comb it and lose level
        let voice = frames(&crate::capture::tests::voice(FRAME * 100, 0));
        let raw = left(&voice, false, Strength::Normal);
        let soft = left(&voice, true, Strength::Soft);
        assert!((raw * 0.85..raw * 1.15).contains(&soft), "soft {soft}, raw {raw}");
    }

    #[test]
    fn the_voice_gets_through_every_strength() {
        let voice = frames(&crate::capture::tests::voice(FRAME * 100, 0));
        let raw = left(&voice, false, Strength::Normal);
        for strength in [Strength::Soft, Strength::Normal, Strength::Strong] {
            let kept = left(&voice, true, strength);
            assert!(kept > raw * 0.3, "{strength:?} keeps the voice: {kept} of {raw}");
        }
        let mut s = Suppressor::new();
        let mut f = voice[60];
        for frame in &voice[..60] {
            s.process(&mut frame.clone(), true, Strength::Strong);
        }
        assert!(s.process(&mut f, true, Strength::Strong) > 0.5, "speech is still recognized");
    }

    /// A suppressor with the neural network loaded (it loads on its own thread).
    fn with_network() -> Suppressor {
        let mut s = Suppressor::new();
        s.warm_up();
        let started = std::time::Instant::now();
        while s.neural_state() == Some(neural::State::Loading) {
            assert!(started.elapsed() < std::time::Duration::from_secs(60), "the model never loaded");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(s.neural_state(), Some(neural::State::Ready), "{:?}", s.neural_error());
        s
    }

    /// The level left after `s` settles on `input`, in dB against the input.
    fn left_db(s: &mut Suppressor, input: &[[f32; FRAME]], strength: Strength) -> f32 {
        let (mut before, mut after) = (0.0f32, 0.0f32);
        for (i, frame) in input.iter().enumerate() {
            let mut f = *frame;
            s.process(&mut f, true, strength);
            if i >= input.len() / 2 {
                before += rms(frame).powi(2);
                after += rms(&f).powi(2);
            }
        }
        10.0 * (after / before).log10()
    }

    #[test]
    fn the_network_loads_aside_and_nothing_waits_for_it() {
        let mut s = Suppressor::new();
        let noise = hiss(5);
        let started = std::time::Instant::now();
        for frame in &noise {
            let mut f = *frame;
            s.process(&mut f, true, Strength::Neural);
        }
        // five frames while it loads: RNNoise stands in, at once
        assert!(started.elapsed() < std::time::Duration::from_millis(200), "{:?}", started.elapsed());
        assert!(s.neural_state().is_some(), "asking for it starts it");
    }

    #[test]
    fn the_stand_in_is_as_late_as_the_network() {
        // while the network loads, RNNoise stands in — two steps later than
        // it would come by itself, as late as the network's own answers
        let input = fan(5);
        let (mut ai, mut plain) = (Suppressor::new(), Suppressor::new());
        let (mut a, mut p) = (Vec::new(), Vec::new());
        for frame in &input {
            let (mut x, mut y) = (*frame, *frame);
            ai.process(&mut x, true, Strength::Neural);
            plain.process(&mut y, true, Strength::Normal);
            a.extend_from_slice(&x);
            p.extend_from_slice(&y);
        }
        assert_eq!(ai.neural_state(), Some(neural::State::Loading), "still loading: these were stand-ins");
        let shift = (neural::DELAY_HOPS - 1) * HALF;
        assert!(a[..shift].iter().all(|v| *v == 0.0));
        assert_eq!(&a[shift..], &p[..p.len() - shift]);
    }

    #[test]
    fn the_network_takes_hiss_and_clatter_rnnoise_leaves() {
        let mut s = with_network();
        let hiss_left = left_db(&mut s, &hiss(150), Strength::Neural);
        let rnnoise_left = left_db(&mut Suppressor::new(), &hiss(150), Strength::Normal);
        assert!(hiss_left < rnnoise_left - 10.0, "hiss: the network leaves {hiss_left:.1} dB, RNNoise {rnnoise_left:.1} dB");

        // keyboard: sharp clicks every 150 ms
        let clicks: Vec<f32> = random(150 * FRAME)
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                let p = i % 7200;
                if p < 240 { w * 1.6 * (1.0 - p as f32 / 240.0) } else { 0.0 }
            })
            .collect();
        let mut s = with_network();
        let clicks_left = left_db(&mut s, &frames(&clicks), Strength::Neural);
        assert!(clicks_left < -15.0, "clicks: {clicks_left:.1} dB left");
    }

    // That the network keeps a voice was measured on real speech —
    // DeepFilterNet's own recordings, alone and under 0 dB of street noise:
    // the level stays within 1 dB, SI-SDR 10–20 dB. Those recordings may not
    // be shipped here (CC BY-NC), and a synthetic voice is no test: the
    // network rightly takes it for noise.
}
