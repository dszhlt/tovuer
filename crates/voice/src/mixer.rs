//! Receiving voice: each peer gets its own packet queue with an adaptive
//! delay (a jitter buffer) and its own decoder; every 20 ms everything is
//! mixed into one frame.
//!
//! Voice arrives as QUIC datagrams straight from the speaker: a packet may be
//! lost (the decoder conceals the gap), arrive late or out of order (it takes
//! its place if that is not played yet), or come in a burst after a stall.
//! The buffer keeps just as much reserve as the current delay spread needs:
//! 40–60 ms on a good link, growing on its own to 0.5 s over a poor relay.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::FRAME;
use crate::codec::Decoder;
use crate::controls::Controls;
use crate::dsp::{rms, soft_limit};

const FRAME_MS: f64 = 20.0;
const MIN_TARGET: usize = 2;
const START_TARGET: usize = 3;
const MAX_TARGET: usize = 25;
/// Hard cap on the queue (2 s): anything older is pointless to play.
const MAX_QUEUE: usize = 100;
/// How many frames in a row are concealed on an empty queue before rebuffering.
const MAX_CONCEAL: u32 = 5;
/// A pause longer than this between packets is a new phrase, not jitter.
const NEW_SPURT_GAP_MS: f64 = 1000.0;
/// Window length (in packets) for finding the delay floor: 3 s of speech.
const FLOOR_WINDOW: u32 = 150;
/// The interface sound queue is at most 3 s.
const MAX_SOUND_SAMPLES: usize = 48_000 * 3;

struct Slot {
    seq: u16,
    end: bool,
    /// `None` — a gap in the numbering (the packet is lost or late): the decoder conceals it.
    data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The peer is quiet.
    Idle,
    /// A phrase started; building up reserve before playback.
    Buffering,
    Playing,
}

struct Peer {
    decoder: Decoder,
    queue: VecDeque<Slot>,
    phase: Phase,
    waited: usize,
    conceal_run: u32,
    last_seq: Option<u16>,
    /// When and with which number the current phrase started — the reference
    /// for measuring how late packets are.
    spurt: Option<(Instant, u16)>,
    /// The smallest lateness in the current and the previous window: the floor
    /// the spread is measured from. Windows let the floor follow the network
    /// (a shift of the whole delay is not jitter) and clock drift.
    min_cur: f64,
    min_prev: f64,
    window: u32,
    /// Estimated delay spread, ms (a decaying maximum).
    spread: f64,
    late_ticks: u32,
    level: f32,
}

impl Peer {
    fn new() -> Option<Self> {
        Some(Self {
            decoder: Decoder::new().ok()?,
            queue: VecDeque::new(),
            phase: Phase::Idle,
            waited: 0,
            conceal_run: 0,
            last_seq: None,
            spurt: None,
            min_cur: 0.0,
            min_prev: f64::INFINITY,
            window: 0,
            spread: (START_TARGET - 1) as f64 * FRAME_MS,
            late_ticks: 0,
            level: 0.0,
        })
    }

    /// Target reserve in frames: the delay spread plus one frame on top.
    fn target(&self) -> usize {
        ((self.spread / FRAME_MS).ceil() as usize + 1).clamp(MIN_TARGET, MAX_TARGET)
    }

    fn restart_window(&mut self) {
        self.min_cur = 0.0;
        self.min_prev = f64::INFINITY;
        self.window = 0;
    }

    fn observe_arrival(&mut self, now: Instant, seq: u16) {
        if self.spurt.is_none() {
            self.spurt = Some((now, seq));
            self.restart_window();
        }
        let (start, start_seq) = self.spurt.unwrap_or((now, seq));
        let expected = seq.wrapping_sub(start_seq) as f64 * FRAME_MS;
        let mut offset = now.saturating_duration_since(start).as_secs_f64() * 1000.0 - expected;
        if offset - self.min_cur.min(self.min_prev) > NEW_SPURT_GAP_MS {
            // missed the end of the previous phrase — start measuring anew
            self.spurt = Some((now, seq));
            self.restart_window();
            offset = 0.0;
        }
        self.min_cur = self.min_cur.min(offset);
        self.window += 1;
        if self.window >= FLOOR_WINDOW {
            self.min_prev = self.min_cur;
            self.min_cur = offset;
            self.window = 0;
        }
        let deviation = offset - self.min_cur.min(self.min_prev);
        self.spread = deviation.max(self.spread * 0.9985).min(MAX_TARGET as f64 * FRAME_MS);
    }

    fn push(&mut self, now: Instant, seq: u16, end: bool, data: &[u8]) {
        if let Some(last) = self.last_seq {
            let ahead = seq.wrapping_sub(last);
            if ahead == 0 {
                return; // a duplicate
            }
            if ahead >= 0x8000 {
                // late: it takes its place if that is still waiting in the queue
                if let Some(slot) = self.queue.iter_mut().find(|s| s.seq == seq && s.data.is_none()) {
                    slot.data = Some(data.to_vec());
                    slot.end = end;
                }
                return;
            }
            if (2..=4).contains(&ahead) {
                for gap in 1..ahead {
                    self.queue.push_back(Slot { seq: last.wrapping_add(gap), end: false, data: None });
                }
            }
        }
        self.last_seq = Some(seq);
        self.observe_arrival(now, seq);
        self.queue.push_back(Slot { seq, end, data: Some(data.to_vec()) });
        if end {
            self.spurt = None;
        }
        if self.queue.len() > MAX_QUEUE {
            let keep = self.target();
            self.queue.drain(..self.queue.len() - keep);
        }
        if self.phase == Phase::Idle {
            self.phase = Phase::Buffering;
            self.waited = 0;
        }
    }

    /// Produce one frame. `false` — this peer is quiet right now.
    fn tick(&mut self, pcm: &mut [f32; FRAME]) -> bool {
        let target = self.target();
        match self.phase {
            Phase::Idle => return false,
            Phase::Buffering => {
                self.waited += 1;
                let short_phrase = self.queue.iter().any(|s| s.end);
                let ready = self.queue.len() >= target || short_phrase || self.waited > target + 2;
                if !ready {
                    return false;
                }
                if self.queue.is_empty() {
                    self.phase = Phase::Idle;
                    return false;
                }
                self.phase = Phase::Playing;
                self.conceal_run = 0;
                self.late_ticks = 0;
            }
            Phase::Playing => {}
        }

        // Catch up if too much piled up (after a network stall packets arrive in
        // a burst): a big pile is cut at once, a small one by a frame every 2 s.
        let excess = self.queue.len().saturating_sub(target);
        if excess > 15 {
            self.queue.drain(..excess);
            self.late_ticks = 0;
        } else if excess > 3 {
            self.late_ticks += 1;
            if self.late_ticks > 100 {
                self.queue.pop_front();
                self.late_ticks = 0;
            }
        } else {
            self.late_ticks = 0;
        }

        match self.queue.pop_front() {
            Some(slot) => {
                self.conceal_run = 0;
                let ok = match &slot.data {
                    Some(data) => self.decoder.decode(data, pcm).is_ok(),
                    None => self.decoder.conceal(pcm).is_ok(),
                };
                if slot.end && self.queue.is_empty() {
                    self.phase = Phase::Idle;
                }
                ok
            }
            None => {
                // the queue ran dry mid-phrase: the reserve was not enough
                self.conceal_run += 1;
                if self.conceal_run == 1 {
                    self.spread = (self.spread + FRAME_MS).min(MAX_TARGET as f64 * FRAME_MS);
                }
                if self.conceal_run > MAX_CONCEAL {
                    self.phase = Phase::Buffering;
                    self.waited = 0;
                    return false;
                }
                self.decoder.conceal(pcm).is_ok()
            }
        }
    }
}

/// Diagnostics for one peer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeerStats {
    pub id: u32,
    /// Current playback delay, ms.
    pub delay_ms: u32,
    pub queued: usize,
    pub level: f32,
}

pub struct Mixer {
    ctl: Arc<Controls>,
    peers: HashMap<u32, Peer>,
    volumes: HashMap<u32, f32>,
    muted: HashSet<u32>,
    sounds: VecDeque<f32>,
}

impl Mixer {
    pub fn new(ctl: Arc<Controls>) -> Self {
        Self {
            ctl,
            peers: HashMap::new(),
            volumes: HashMap::new(),
            muted: HashSet::new(),
            sounds: VecDeque::new(),
        }
    }

    /// A packet from the network. `now` — when it arrived (jitter is measured by it).
    pub fn push(&mut self, now: Instant, sender: u32, seq: u16, end: bool, data: &[u8]) {
        if data.is_empty() || self.muted.contains(&sender) {
            return;
        }
        let peer = match self.peers.entry(sender) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                let Some(peer) = Peer::new() else { return };
                slot.insert(peer)
            }
        };
        peer.push(now, seq, end, data);
    }

    /// Mix one frame (20 ms, mono 48 kHz).
    pub fn mix(&mut self, out: &mut [f32; FRAME]) {
        self.ctl.played.fetch_add(1, Ordering::Relaxed);
        out.fill(0.0);
        let deafened = self.ctl.deafened.load(Ordering::Relaxed);
        let mut pcm = [0.0f32; FRAME];
        for (id, peer) in self.peers.iter_mut() {
            if peer.tick(&mut pcm) {
                peer.level += (rms(&pcm) - peer.level) * 0.5;
                let volume = self.volumes.get(id).copied().unwrap_or(1.0);
                if !deafened && volume > 0.0 {
                    for (o, s) in out.iter_mut().zip(pcm.iter()) {
                        *o += s * volume;
                    }
                }
            } else {
                peer.level *= 0.5;
            }
        }
        let gain = self.ctl.out_gain();
        if gain != 1.0 {
            for s in out.iter_mut() {
                *s *= gain;
            }
        }
        // interface sounds bypass the overall volume and are heard even when deafened
        for s in out.iter_mut() {
            match self.sounds.pop_front() {
                Some(x) => *s += x,
                None => break,
            }
        }
        for s in out.iter_mut() {
            *s = soft_limit(*s);
        }
    }

    /// Queue a short interface sound.
    pub fn play(&mut self, samples: &[f32]) {
        if self.sounds.len() + samples.len() <= MAX_SOUND_SAMPLES {
            self.sounds.extend(samples.iter().copied());
        }
    }

    /// A particular peer's volume (1.0 = unchanged).
    pub fn set_volume(&mut self, id: u32, volume: f32) {
        self.volumes.insert(id, volume.clamp(0.0, 3.0));
    }

    /// Local mute: this peer is silent only for us.
    pub fn set_muted(&mut self, id: u32, muted: bool) {
        if muted {
            self.muted.insert(id);
            self.peers.remove(&id);
        } else {
            self.muted.remove(&id);
        }
    }

    pub fn is_muted(&self, id: u32) -> bool {
        self.muted.contains(&id)
    }

    /// Forget everyone not in `active` (they left the voice channel).
    pub fn retain(&mut self, active: &[u32]) {
        self.peers.retain(|id, _| active.contains(id));
        self.volumes.retain(|id, _| active.contains(id));
        self.muted.retain(|id| active.contains(id));
    }

    /// Remove one peer, leave the others.
    pub fn retain_except(&mut self, id: u32) {
        self.peers.remove(&id);
    }

    pub fn clear(&mut self) {
        self.peers.clear();
        self.sounds.clear();
    }

    /// A peer's sound level 0..1 (for the "talking" indicator).
    pub fn level(&self, id: u32) -> f32 {
        self.peers.get(&id).map(|p| p.level).unwrap_or(0.0)
    }

    pub fn stats(&self) -> Vec<PeerStats> {
        let mut out: Vec<PeerStats> = self
            .peers
            .iter()
            .map(|(id, p)| PeerStats {
                id: *id,
                delay_ms: (p.target() as f64 * FRAME_MS) as u32,
                queued: p.queue.len(),
                level: p.level,
            })
            .collect();
        out.sort_by_key(|s| s.id);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SAMPLE_RATE;
    use crate::codec::{DEFAULT_BITRATE, Encoder, MAX_PACKET};
    use std::f32::consts::PI;
    use std::time::Duration;

    /// A source of encoded, numbered "speech" frames.
    struct Talker {
        enc: Encoder,
        seq: u16,
        n: usize,
    }

    impl Talker {
        fn new() -> Self {
            Self { enc: Encoder::new(DEFAULT_BITRATE).unwrap(), seq: 0, n: 0 }
        }

        fn frame(&mut self) -> (u16, Vec<u8>) {
            let mut pcm = [0.0f32; FRAME];
            for (i, s) in pcm.iter_mut().enumerate() {
                let t = (self.n * FRAME + i) as f32 / SAMPLE_RATE as f32;
                *s = 0.3 * (2.0 * PI * 220.0 * t).sin() + 0.1 * (2.0 * PI * 660.0 * t).sin();
            }
            self.n += 1;
            let mut packet = [0u8; MAX_PACKET];
            let len = self.enc.encode(&pcm, &mut packet).unwrap();
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            (seq, packet[..len].to_vec())
        }
    }

    fn mixer() -> (Mixer, Arc<Controls>) {
        let ctl = Arc::new(Controls::default());
        (Mixer::new(ctl.clone()), ctl)
    }

    fn tick(m: &mut Mixer) -> f32 {
        let mut out = [0.0; FRAME];
        m.mix(&mut out);
        rms(&out)
    }

    const TICK: Duration = Duration::from_millis(20);

    #[test]
    fn steady_stream_plays_after_a_short_delay() {
        let (mut m, _) = mixer();
        let mut talker = Talker::new();
        let t0 = Instant::now();
        let mut first_sound = None;
        let mut loud = 0;
        for i in 0..100u32 {
            let (seq, data) = talker.frame();
            m.push(t0 + TICK * i, 7, seq, false, &data);
            if tick(&mut m) > 0.05 {
                first_sound.get_or_insert(i);
                loud += 1;
            }
        }
        let first = first_sound.expect("sound must appear");
        assert!(first <= 6, "start delay {first} frames");
        assert!(loud >= 90, "the stream plays without dropouts: {loud}/100");
        assert!(m.level(7) > 0.05);
        assert!(m.stats()[0].delay_ms <= 80, "a steady network needs little reserve: {:?}", m.stats());
    }

    #[test]
    fn phrase_end_goes_quiet_without_concealment_tail() {
        let (mut m, _) = mixer();
        let mut talker = Talker::new();
        let t0 = Instant::now();
        for i in 0..30u32 {
            let (seq, data) = talker.frame();
            m.push(t0 + TICK * i, 1, seq, i == 29, &data);
            tick(&mut m);
        }
        // play out the reserve
        for _ in 0..10 {
            tick(&mut m);
        }
        for _ in 0..20 {
            assert!(tick(&mut m) < 1e-4, "after END — clean silence, not PLC");
        }
        assert_eq!(m.stats()[0].queued, 0);
    }

    /// After a 0.1–3 s pause the start of the next phrase must not be thrown
    /// away as "late".
    #[test]
    fn speech_after_a_pause_is_heard_immediately() {
        let (mut m, _) = mixer();
        let mut talker = Talker::new();
        let t0 = Instant::now();
        for i in 0..25u32 {
            let (seq, data) = talker.frame();
            m.push(t0 + TICK * i, 1, seq, i == 24, &data);
            tick(&mut m);
        }
        // a second of silence: the sender is quiet, frame numbers stand still
        for _ in 0..50 {
            tick(&mut m);
        }
        let t1 = t0 + TICK * 75;
        let mut heard_at = None;
        for i in 0..25u32 {
            let (seq, data) = talker.frame();
            m.push(t1 + TICK * i, 1, seq, false, &data);
            if tick(&mut m) > 0.05 {
                heard_at.get_or_insert(i);
            }
        }
        assert!(heard_at.expect("the second phrase must be heard") <= 6);
    }

    #[test]
    fn burst_after_network_stall_does_not_add_latency_forever() {
        let (mut m, _) = mixer();
        let mut talker = Talker::new();
        let t0 = Instant::now();
        for i in 0..20u32 {
            let (seq, data) = talker.frame();
            m.push(t0 + TICK * i, 1, seq, false, &data);
            tick(&mut m);
        }
        // the network stalled for 1.2 s…
        for _ in 0..60 {
            tick(&mut m);
        }
        // …and dumped everything at once
        let burst_at = t0 + TICK * 80;
        for _ in 0..60 {
            let (seq, data) = talker.frame();
            m.push(burst_at, 1, seq, false, &data);
        }
        for i in 0..100u32 {
            let (seq, data) = talker.frame();
            m.push(burst_at + TICK * (i + 1), 1, seq, false, &data);
            tick(&mut m);
        }
        let s = m.stats()[0];
        assert!(s.queued <= MAX_TARGET + 4, "no tail piles up: {s:?}");
    }

    #[test]
    fn jitter_grows_the_buffer_and_calm_shrinks_it() {
        let (mut m, _) = mixer();
        let mut talker = Talker::new();
        let t0 = Instant::now();
        let mut i = 0u32;
        // every 5 frames the network holds a burst back by 100 ms
        for _ in 0..40 {
            let batch_at = t0 + TICK * (i + 5);
            for _ in 0..5 {
                let (seq, data) = talker.frame();
                m.push(batch_at, 1, seq, false, &data);
                i += 1;
            }
            for _ in 0..5 {
                tick(&mut m);
            }
        }
        let rough = m.stats()[0].delay_ms;
        assert!(rough >= 100, "the reserve grew for the jitter: {rough} ms");

        // the network calmed down
        let base = t0 + TICK * (i + 5);
        for k in 0..3000u32 {
            let (seq, data) = talker.frame();
            m.push(base + TICK * k, 1, seq, false, &data);
            tick(&mut m);
        }
        let calm = m.stats()[0].delay_ms;
        assert!(calm < rough, "on a steady network the reserve shrank: {rough} → {calm} ms");
    }

    #[test]
    fn small_sequence_gaps_are_concealed_and_old_packets_dropped() {
        let (mut m, _) = mixer();
        let mut talker = Talker::new();
        let t0 = Instant::now();
        let frames: Vec<_> = (0..10).map(|_| talker.frame()).collect();
        for (i, (seq, data)) in frames.iter().enumerate() {
            if i == 4 || i == 5 {
                continue; // two packets were lost
            }
            m.push(t0 + TICK * i as u32, 1, *seq, false, data);
        }
        assert_eq!(m.stats()[0].queued, 10, "the gap is filled with placeholders for PLC");
        // a repeat of an accepted packet does not grow the queue
        m.push(t0, 1, frames[3].0, false, &frames[3].1);
        assert_eq!(m.stats()[0].queued, 10);
        // a late packet takes its waiting place instead of being concealed
        m.push(t0, 1, frames[4].0, false, &frames[4].1);
        assert_eq!(m.stats()[0].queued, 10);
        let queue = &m.peers[&1].queue;
        assert!(queue.iter().find(|s| s.seq == frames[4].0).is_some_and(|s| s.data.is_some()), "packet 4 is in place");
        assert!(queue.iter().find(|s| s.seq == frames[5].0).is_some_and(|s| s.data.is_none()), "packet 5 is still missing");
    }

    #[test]
    fn local_mute_volume_and_deafen() {
        let (mut m, ctl) = mixer();
        let mut a = Talker::new();
        let t0 = Instant::now();
        let mut run = |m: &mut Mixer, from: u32, n: u32| -> f32 {
            let mut sum = 0.0;
            for i in 0..n {
                let (seq, data) = a.frame();
                m.push(t0 + TICK * (from + i), 1, seq, false, &data);
                sum += tick(m);
            }
            sum / n as f32
        };
        let normal = run(&mut m, 0, 50);
        assert!(normal > 0.1);

        m.set_volume(1, 0.25);
        let quiet = run(&mut m, 50, 50);
        assert!(quiet < normal * 0.4, "the peer's volume is turned down: {quiet} vs {normal}");

        m.set_volume(1, 1.0);
        ctl.deafened.store(true, Ordering::Relaxed);
        assert!(run(&mut m, 100, 50) < 1e-4, "everyone is deafened");
        assert!(m.stats()[0].queued <= MAX_TARGET, "and the queue does not pile up");
        ctl.deafened.store(false, Ordering::Relaxed);

        m.set_muted(1, true);
        assert!(m.is_muted(1));
        assert!(run(&mut m, 150, 50) < 1e-4, "local mute");
        m.set_muted(1, false);
        assert!(run(&mut m, 200, 50) > 0.1, "heard again");
    }

    #[test]
    fn interface_sounds_play_even_when_deafened() {
        let (mut m, ctl) = mixer();
        ctl.deafened.store(true, Ordering::Relaxed);
        m.play(&vec![0.2; FRAME * 2]);
        assert!(tick(&mut m) > 0.1);
        assert!(tick(&mut m) > 0.1);
        assert!(tick(&mut m) < 1e-6);
    }

    #[test]
    fn retain_forgets_those_who_left() {
        let (mut m, _) = mixer();
        let mut t = Talker::new();
        let now = Instant::now();
        for id in [1, 2, 3] {
            let (seq, data) = t.frame();
            m.push(now, id, seq, false, &data);
        }
        m.retain(&[2]);
        assert_eq!(m.stats().iter().map(|s| s.id).collect::<Vec<_>>(), vec![2]);
    }
}
