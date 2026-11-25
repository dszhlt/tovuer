//! The microphone chain: filter → noise suppression (RNNoise, see
//! [`crate::denoise`]) → gain →
//! speech detector → Opus. In: mono 48 kHz in chunks of any length;
//! out: finished 20 ms packets.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::codec::{CodecError, DEFAULT_BITRATE, Encoder, MAX_PACKET};
use crate::controls::Controls;
use crate::denoise::Suppressor;
use crate::dsp::{Agc, Biquad, rms, soft_limit};
use crate::{FRAME, SAMPLE_RATE};

/// Frames still sent after speech ends (240 ms): keeps word endings and the
/// pauses inside a phrase.
const HANGOVER_FRAMES: u32 = 12;

/// Encoded frames kept in reserve so the start of a word goes out too when
/// the gate opens (40 ms).
const PREROLL_FRAMES: usize = 2;

/// Below this level a frame is silence whatever the detector thinks.
const SILENCE_FLOOR: f32 = 0.002;

/// A finished packet for the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub seq: u16,
    /// The last frame of a phrase — silence follows.
    pub end: bool,
    pub data: Vec<u8>,
}

pub struct CaptureProcessor {
    ctl: Arc<Controls>,
    pending: Vec<f32>,
    high_pass: Biquad,
    denoise: Suppressor,
    agc: Agc,
    encoder: Encoder,
    seq: u16,
    open: bool,
    hangover: u32,
    preroll: VecDeque<Vec<u8>>,
    meter: f32,
}

impl CaptureProcessor {
    pub fn new(ctl: Arc<Controls>) -> Result<Self, CodecError> {
        let mut denoise = Suppressor::new();
        // the network takes half a second to load: start now if it is the choice
        if ctl.denoise_strength() == crate::denoise::Strength::Neural {
            denoise.warm_up();
        }
        Ok(Self {
            ctl,
            pending: Vec::with_capacity(FRAME * 4),
            high_pass: Biquad::high_pass(SAMPLE_RATE as f32, 90.0, 0.707),
            denoise,
            agc: Agc::new(),
            encoder: Encoder::new(DEFAULT_BITRATE)?,
            seq: 0,
            open: false,
            hangover: 0,
            preroll: VecDeque::with_capacity(PREROLL_FRAMES + 1),
            meter: 0.0,
        })
    }

    /// Feed microphone samples; `emit` is called for every packet to send.
    pub fn feed(&mut self, samples: &[f32], emit: &mut dyn FnMut(Packet)) {
        self.pending.extend_from_slice(samples);
        let mut offset = 0;
        let mut frame = [0.0f32; FRAME];
        while self.pending.len() - offset >= FRAME {
            frame.copy_from_slice(&self.pending[offset..offset + FRAME]);
            offset += FRAME;
            self.process_frame(&mut frame, emit);
        }
        self.pending.drain(..offset);
    }

    fn process_frame(&mut self, frame: &mut [f32; FRAME], emit: &mut dyn FnMut(Packet)) {
        self.ctl.captured.fetch_add(1, Ordering::Relaxed);
        self.high_pass.process_block(frame);

        // RNNoise also gives a speech probability — the gate opens on that
        // (keyboard clatter does not pass), whether suppression is on or not
        let denoise_on = self.ctl.denoise.load(Ordering::Relaxed);
        let vad = self.denoise.process(frame, denoise_on, self.ctl.denoise_strength());
        self.ctl.set_neural_state(self.denoise.neural_state());
        let (answered, missed) = self.denoise.neural_counts();
        self.ctl.neural_answered.store(answered, Ordering::Relaxed);
        self.ctl.neural_missed.store(missed, Ordering::Relaxed);

        let raw_level = rms(frame);
        let voiced = vad >= self.ctl.vad_threshold() && raw_level > SILENCE_FLOOR;

        let manual = self.ctl.mic_gain();
        if manual != 1.0 {
            for s in frame.iter_mut() {
                *s *= manual;
            }
        }
        if self.ctl.agc.load(Ordering::Relaxed) {
            self.agc.process(frame, voiced);
        }
        for s in frame.iter_mut() {
            *s = soft_limit(*s);
        }

        // the meter works with the microphone off too — you can see it hears at all
        let level = rms(frame);
        self.meter += (level - self.meter) * 0.35;
        self.ctl.set_mic_level(self.meter);
        self.ctl.note_peak(level);

        let muted = self.ctl.muted.load(Ordering::Relaxed) || self.ctl.deafened.load(Ordering::Relaxed);
        if muted {
            if self.open {
                // cut off mid-word — close the phrase properly with silence
                self.open = false;
                self.hangover = 0;
                let silence = [0.0f32; FRAME];
                if let Some(data) = self.encode(&silence) {
                    self.send(data, true, emit);
                }
            }
            self.preroll.clear();
            self.ctl.speaking.store(false, Ordering::Relaxed);
            return;
        }

        // Encode every frame even when not sending: the codec state stays
        // continuous, and the last couple of frames become the phrase's pre-roll.
        let Some(data) = self.encode(frame) else {
            return;
        };

        if voiced {
            if !self.open {
                self.open = true;
                while let Some(early) = self.preroll.pop_front() {
                    self.send(early, false, emit);
                }
            }
            self.hangover = HANGOVER_FRAMES;
            self.send(data, false, emit);
        } else if self.open {
            self.hangover = self.hangover.saturating_sub(1);
            let end = self.hangover == 0;
            self.send(data, end, emit);
            if end {
                self.open = false;
            }
        } else {
            self.ctl.gated.fetch_add(1, Ordering::Relaxed);
            if self.preroll.len() == PREROLL_FRAMES {
                self.preroll.pop_front();
            }
            self.preroll.push_back(data);
        }
        self.ctl.speaking.store(self.open, Ordering::Relaxed);
    }

    fn encode(&mut self, frame: &[f32; FRAME]) -> Option<Vec<u8>> {
        let mut packet = [0u8; MAX_PACKET];
        match self.encoder.encode(frame, &mut packet) {
            Ok(len) if len > 0 => Some(packet[..len].to_vec()),
            _ => None,
        }
    }

    fn send(&mut self, data: Vec<u8>, end: bool, emit: &mut dyn FnMut(Packet)) {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        self.ctl.sent.fetch_add(1, Ordering::Relaxed);
        emit(Packet { seq, end, data });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// A synthetic "voice": a vowel from harmonics with pitch jitter — speech-like
    /// enough for the RNNoise detector to fire reliably.
    pub(crate) fn voice(samples: usize, start: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| {
                let t = (start + i) as f32 / SAMPLE_RATE as f32;
                let f0 = 140.0 + 25.0 * (2.0 * PI * 3.0 * t).sin();
                let phase = 2.0 * PI * f0 * t;
                let formants = [(1.0, 1.0), (2.0, 0.7), (3.0, 0.5), (5.0, 0.45), (7.0, 0.3), (11.0, 0.2), (17.0, 0.1)];
                let s: f32 = formants.iter().map(|(h, a)| a * (phase * h).sin()).sum();
                let syllable = 0.55 + 0.45 * (2.0 * PI * 4.0 * t).sin();
                0.12 * s * syllable
            })
            .collect()
    }

    fn collect(proc: &mut CaptureProcessor, samples: &[f32]) -> Vec<Packet> {
        let mut out = Vec::new();
        for chunk in samples.chunks(441) {
            proc.feed(chunk, &mut |p| out.push(p));
        }
        out
    }

    #[test]
    fn silence_sends_nothing() {
        let ctl = Arc::new(Controls::default());
        let mut proc = CaptureProcessor::new(ctl.clone()).unwrap();
        let packets = collect(&mut proc, &vec![0.0; SAMPLE_RATE as usize]);
        assert!(packets.is_empty());
        assert!(!ctl.speaking.load(Ordering::Relaxed));
        assert!(ctl.gated.load(Ordering::Relaxed) >= 49);
    }

    #[test]
    fn speech_opens_gate_then_closes_with_end_flag() {
        let ctl = Arc::new(Controls::default());
        let mut proc = CaptureProcessor::new(ctl.clone()).unwrap();
        let mut input = vec![0.0; SAMPLE_RATE as usize / 2];
        input.extend(voice(SAMPLE_RATE as usize * 2, 0));
        let speech = collect(&mut proc, &input);
        assert!(speech.len() > 60, "two seconds of speech give frames: {}", speech.len());
        assert!(ctl.speaking.load(Ordering::Relaxed));
        assert!(speech.iter().all(|p| !p.end), "a phrase does not close mid-speech");

        let tail = collect(&mut proc, &vec![0.0; SAMPLE_RATE as usize]);
        assert!(!tail.is_empty() && tail.len() <= HANGOVER_FRAMES as usize + 2, "tail: {}", tail.len());
        assert!(tail.last().unwrap().end, "the last frame is marked END");
        assert_eq!(tail.iter().filter(|p| p.end).count(), 1);
        assert!(!ctl.speaking.load(Ordering::Relaxed));

        // numbers run with no gaps — the listener will not take a pause for a loss
        let all: Vec<u16> = speech.iter().chain(&tail).map(|p| p.seq).collect();
        for pair in all.windows(2) {
            assert_eq!(pair[1], pair[0].wrapping_add(1));
        }
    }

    #[test]
    fn mute_cuts_the_phrase_cleanly() {
        let ctl = Arc::new(Controls::default());
        let mut proc = CaptureProcessor::new(ctl.clone()).unwrap();
        let speech = collect(&mut proc, &voice(SAMPLE_RATE as usize, 0));
        assert!(!speech.is_empty());

        ctl.muted.store(true, Ordering::Relaxed);
        let after = collect(&mut proc, &voice(SAMPLE_RATE as usize, SAMPLE_RATE as usize));
        assert_eq!(after.len(), 1, "after muting only the closing frame goes out");
        assert!(after[0].end);
        assert!(ctl.mic_level() > 0.01, "the level meter works while muted");

        ctl.muted.store(false, Ordering::Relaxed);
        let again = collect(&mut proc, &voice(SAMPLE_RATE as usize, 2 * SAMPLE_RATE as usize));
        assert!(!again.is_empty(), "after unmuting it is heard again");
    }

    #[test]
    fn strict_threshold_blocks_everything() {
        let ctl = Arc::new(Controls::default());
        ctl.set_vad_threshold(1.1);
        let mut proc = CaptureProcessor::new(ctl).unwrap();
        assert!(collect(&mut proc, &voice(SAMPLE_RATE as usize, 0)).is_empty());
    }
}
