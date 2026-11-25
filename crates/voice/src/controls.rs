//! The engine's shared knobs: the interface turns them, the audio threads read
//! them. All atomics — the audio callback never waits on a lock for a setting.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::denoise::Strength;

/// Default speech detector threshold (RNNoise probability, 0..1).
pub const DEFAULT_VAD_THRESHOLD: f32 = 0.55;

#[derive(Debug)]
pub struct Controls {
    /// The microphone is off (our voice does not go out).
    pub muted: AtomicBool,
    /// Sound is off: we hear no one and the microphone is silent too.
    pub deafened: AtomicBool,
    pub denoise: AtomicBool,
    denoise_strength: AtomicU8,
    pub agc: AtomicBool,
    /// The gate is open — talking right now.
    pub speaking: AtomicBool,
    /// Frames sent / held back by the gate (diagnostics).
    pub sent: AtomicU64,
    pub gated: AtomicU64,
    /// Frames taken from the microphone and given to the output: stuck at zero
    /// means the device opened but no sound flows through it.
    pub captured: AtomicU64,
    pub played: AtomicU64,
    mic_gain: AtomicU32,
    out_gain: AtomicU32,
    vad_threshold: AtomicU32,
    mic_level: AtomicU32,
    mic_peak: AtomicU32,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            muted: AtomicBool::new(false),
            deafened: AtomicBool::new(false),
            denoise: AtomicBool::new(true),
            denoise_strength: AtomicU8::new(Strength::Normal as u8),
            agc: AtomicBool::new(true),
            speaking: AtomicBool::new(false),
            sent: AtomicU64::new(0),
            gated: AtomicU64::new(0),
            captured: AtomicU64::new(0),
            played: AtomicU64::new(0),
            mic_gain: AtomicU32::new(1.0f32.to_bits()),
            out_gain: AtomicU32::new(1.0f32.to_bits()),
            vad_threshold: AtomicU32::new(DEFAULT_VAD_THRESHOLD.to_bits()),
            mic_level: AtomicU32::new(0),
            mic_peak: AtomicU32::new(0),
        }
    }
}

fn load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

fn store(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

impl Controls {
    /// Manual microphone gain (a factor, 1.0 = unchanged).
    pub fn mic_gain(&self) -> f32 {
        load(&self.mic_gain)
    }

    pub fn set_mic_gain(&self, gain: f32) {
        store(&self.mic_gain, gain.clamp(0.0, 4.0));
    }

    /// How hard noise suppression works.
    pub fn denoise_strength(&self) -> Strength {
        Strength::from_u8(self.denoise_strength.load(Ordering::Relaxed))
    }

    pub fn set_denoise_strength(&self, strength: Strength) {
        self.denoise_strength.store(strength as u8, Ordering::Relaxed);
    }

    /// Overall volume of others (a factor).
    pub fn out_gain(&self) -> f32 {
        load(&self.out_gain)
    }

    pub fn set_out_gain(&self, gain: f32) {
        store(&self.out_gain, gain.clamp(0.0, 3.0));
    }

    pub fn vad_threshold(&self) -> f32 {
        load(&self.vad_threshold)
    }

    pub fn set_vad_threshold(&self, threshold: f32) {
        store(&self.vad_threshold, threshold);
    }

    /// Smoothed microphone level 0..1 — for the meter.
    pub fn mic_level(&self) -> f32 {
        load(&self.mic_level)
    }

    pub(crate) fn set_mic_level(&self, level: f32) {
        store(&self.mic_level, level);
    }

    /// Peak level since the last reset: 0 = the microphone is dead.
    pub fn mic_peak(&self) -> f32 {
        load(&self.mic_peak)
    }

    pub(crate) fn note_peak(&self, level: f32) {
        if level > self.mic_peak() {
            store(&self.mic_peak, level);
        }
    }

    pub(crate) fn reset_stats(&self) {
        self.sent.store(0, Ordering::Relaxed);
        self.gated.store(0, Ordering::Relaxed);
        self.captured.store(0, Ordering::Relaxed);
        self.played.store(0, Ordering::Relaxed);
        self.speaking.store(false, Ordering::Relaxed);
        store(&self.mic_level, 0.0);
        store(&self.mic_peak, 0.0);
    }
}
