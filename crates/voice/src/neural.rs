//! The "ai" strength: DeepFilterNet3, a neural network for noise in the
//! class of Krisp — keyboard clatter, the street, voices around — where
//! RNNoise is weaker. It costs about a tenth of a CPU core and 30 ms.
//!
//! The model's state cannot leave the thread that made it, so it lives on a
//! thread of its own: the microphone hands it 10 ms at a time and waits a
//! few milliseconds for the answer. While the model loads (about half a
//! second), or when it is late, that piece goes through RNNoise instead —
//! the sound never stops.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use df::tract::{DfParams, DfTract, RuntimeParams};

/// 10 ms at 48 kHz: the model's step.
pub const HOP: usize = 480;
/// The network's output is this many steps behind its input (its STFT and
/// lookahead: 30 ms); RNNoise's is one step behind.
pub const DELAY_HOPS: usize = 3;
/// How long the microphone waits for a piece (the model takes about 1 ms;
/// the wait covers the thread being scheduled late now and then).
const WAIT: Duration = Duration::from_millis(10);

/// Where the model is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Loading = 1,
    Ready = 2,
    /// It would not load: the other strengths still work.
    Failed = 3,
}

impl State {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(State::Loading),
            2 => Some(State::Ready),
            3 => Some(State::Failed),
            _ => None,
        }
    }
}

type Piece = (u64, [f32; HOP]);

pub struct Neural {
    to: SyncSender<Piece>,
    from: Receiver<Piece>,
    state: Arc<AtomicU8>,
    /// Why it would not load.
    error: Arc<Mutex<Option<String>>>,
    seq: u64,
    /// Pieces the network cleaned in time, and those RNNoise had to take.
    pub answered: u64,
    pub missed: u64,
}

impl Neural {
    /// Start loading the model on its own thread.
    pub fn start() -> Self {
        let (to, inbox) = sync_channel::<Piece>(4);
        let (outbox, from) = sync_channel::<Piece>(8);
        let state = Arc::new(AtomicU8::new(State::Loading as u8));
        let error = Arc::new(Mutex::new(None));
        let (shared, why) = (state.clone(), error.clone());
        let started = std::thread::Builder::new().name("noise-ai".into()).spawn(move || {
            // the thresholds of DeepFilterNet's own tool: below them they
            // pass frames as clean or as noise, and the library's defaults
            // leave much of the hiss and clatter
            let params = RuntimeParams::default_with_ch(1).with_thresholds(-15.0, 35.0, 35.0);
            let mut model = match DfTract::new(DfParams::default(), &params) {
                Ok(model) => model,
                Err(e) => {
                    let first = format!("{e:#}").lines().next().unwrap_or_default().chars().take(300).collect();
                    *why.lock().unwrap_or_else(|e| e.into_inner()) = Some(first);
                    shared.store(State::Failed as u8, Ordering::Relaxed);
                    return;
                }
            };
            shared.store(State::Ready as u8, Ordering::Relaxed);
            let mut out = ndarray::Array2::<f32>::zeros((1, HOP));
            while let Ok((seq, input)) = inbox.recv() {
                let Ok(noisy) = ndarray::ArrayView2::from_shape((1, HOP), &input[..]) else { continue };
                if model.process(noisy, out.view_mut()).is_err() {
                    continue;
                }
                let mut piece = [0.0f32; HOP];
                for (dst, src) in piece.iter_mut().zip(out.iter()) {
                    *dst = *src;
                }
                if outbox.send((seq, piece)).is_err() {
                    return; // the microphone is closed
                }
            }
        });
        if started.is_err() {
            state.store(State::Failed as u8, Ordering::Relaxed);
        }
        Self { to, from, state, error, seq: 0, answered: 0, missed: 0 }
    }

    /// Why the model would not load, if it would not.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed)).unwrap_or(State::Failed)
    }

    /// Clean 10 ms of sound (-1..1). `None` — not ready, or too late this
    /// time: clean it some other way.
    pub fn process(&mut self, piece: &[f32; HOP]) -> Option<[f32; HOP]> {
        if self.state() != State::Ready {
            return None;
        }
        self.seq += 1;
        // the model is behind: skip a piece rather than queue up delay
        if self.to.try_send((self.seq, *piece)).is_err() {
            self.missed += 1;
            return None;
        }
        loop {
            match self.from.recv_timeout(WAIT) {
                Ok((seq, out)) if seq == self.seq => {
                    self.answered += 1;
                    return Some(out);
                }
                // the answer to a piece that came too late
                Ok(_) => continue,
                Err(_) => {
                    self.missed += 1;
                    return None;
                }
            }
        }
    }
}
