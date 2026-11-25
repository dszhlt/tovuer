//! The tovuer voice engine.
//!
//! The chain: microphone → high-pass filter → RNNoise noise suppression →
//! automatic gain → speech detector → Opus (48 kHz, 20 ms) → network →
//! jitter buffer → decoder → mixer → speakers.
//!
//! The logic ([`capture`], [`mixer`], [`codec`], [`dsp`]) knows nothing about
//! devices and is tested on synthetic signals; only [`device`] touches hardware.

pub mod capture;
pub mod codec;
pub mod controls;
pub mod denoise;
pub mod device;
pub mod dsp;
pub mod engine;
pub mod mixer;
pub mod resample;
pub mod sound;

/// Sample rate of the whole voice chain.
pub const SAMPLE_RATE: u32 = 48_000;
/// A frame is 20 ms.
pub const FRAME: usize = 960;

pub use capture::Packet;
pub use controls::{Controls, DEFAULT_VAD_THRESHOLD};
pub use denoise::Strength;
pub use device::{AudioError, DeviceInfo, StreamInfo, list_devices};
pub use engine::Engine;
pub use mixer::PeerStats;
pub use sound::Sound;
