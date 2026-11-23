//! Short interface sounds: joining/leaving a voice channel, mute, a mention.
//! Synthesized on the fly — soft decaying chimes, no files.

use std::f32::consts::PI;

use crate::SAMPLE_RATE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Someone (or you) joined the voice channel.
    Join,
    Leave,
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    /// You were mentioned in the chat.
    Mention,
}

/// One note: the fundamental plus a quiet octave, a fast attack, exponential decay.
fn note(freq: f32, ms: f32, amp: f32) -> Vec<f32> {
    let n = (SAMPLE_RATE as f32 * ms / 1000.0) as usize;
    let attack = (SAMPLE_RATE as f32 * 0.004) as usize;
    let release = (SAMPLE_RATE as f32 * 0.012) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            let mut env = (-t * 1000.0 / (ms * 0.45)).exp();
            if i < attack {
                env *= i as f32 / attack as f32;
            }
            if n - i < release {
                env *= (n - i) as f32 / release as f32;
            }
            let wave = (2.0 * PI * freq * t).sin() + 0.25 * (2.0 * PI * freq * 2.0 * t).sin();
            amp * env * wave
        })
        .collect()
}

fn sequence(notes: &[(f32, f32)], amp: f32) -> Vec<f32> {
    notes.iter().flat_map(|&(freq, ms)| note(freq, ms, amp)).collect()
}

/// Samples of a sound (mono, 48 kHz).
pub fn render(sound: Sound) -> Vec<f32> {
    const D5: f32 = 587.33;
    const A5: f32 = 880.0;
    const E5: f32 = 659.25;
    const A4: f32 = 440.0;
    const C6: f32 = 1046.5;
    const E6: f32 = 1318.5;
    match sound {
        Sound::Join => sequence(&[(D5, 110.0), (A5, 190.0)], 0.07),
        Sound::Leave => sequence(&[(A5, 110.0), (D5, 190.0)], 0.07),
        Sound::Unmute => sequence(&[(E5, 90.0)], 0.06),
        Sound::Mute => sequence(&[(A4, 90.0)], 0.06),
        Sound::Undeafen => sequence(&[(A4, 70.0), (E5, 110.0)], 0.06),
        Sound::Deafen => sequence(&[(E5, 70.0), (A4, 110.0)], 0.06),
        Sound::Mention => sequence(&[(C6, 90.0), (E6, 170.0)], 0.05),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::peak;

    #[test]
    fn sounds_are_short_quiet_and_click_free() {
        for sound in [
            Sound::Join,
            Sound::Leave,
            Sound::Mute,
            Sound::Unmute,
            Sound::Deafen,
            Sound::Undeafen,
            Sound::Mention,
        ] {
            let s = render(sound);
            let secs = s.len() as f32 / SAMPLE_RATE as f32;
            assert!(secs > 0.05 && secs < 0.5, "{sound:?}: {secs} c");
            let p = peak(&s);
            assert!(p > 0.01 && p < 0.12, "{sound:?}: peak {p} — audible but gentle");
            assert!(s[0].abs() < 1e-3 && s[s.len() - 1].abs() < 1e-2, "{sound:?}: no clicks at the edges");
        }
    }

    #[test]
    fn join_and_leave_differ() {
        assert_ne!(render(Sound::Join), render(Sound::Leave));
    }
}
