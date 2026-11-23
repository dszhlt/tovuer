//! The whole engine: open the devices and voice flows both ways.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::capture::{CaptureProcessor, Packet};
use crate::controls::Controls;
use crate::device::{self, AudioError, StreamInfo, Streams};
use crate::mixer::{Mixer, PeerStats};
use crate::sound::{self, Sound};

/// The id under which our own voice enters the mixer during the mic test.
const MONITOR_PEER: u32 = u32::MAX;

/// The voice engine. Created once for the client's lifetime; [`Engine::start`]
/// opens the devices on joining a voice channel, [`Engine::stop`] closes them
/// on leaving.
pub struct Engine {
    ctl: Arc<Controls>,
    mixer: Arc<Mutex<Mixer>>,
    streams: Mutex<Option<Streams>>,
    errors: Arc<Mutex<Option<String>>>,
    /// The mic test: hear yourself through the whole chain.
    monitor: Arc<AtomicBool>,
    on_packet: Arc<dyn Fn(Packet) + Send + Sync>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Engine {
    /// `on_packet` is called from the audio thread for every frame ready to be
    /// sent. It must not block.
    pub fn new(on_packet: impl Fn(Packet) + Send + Sync + 'static) -> Self {
        let ctl = Arc::new(Controls::default());
        let mixer = Arc::new(Mutex::new(Mixer::new(ctl.clone())));
        let monitor = Arc::new(AtomicBool::new(false));
        let on_packet: Arc<dyn Fn(Packet) + Send + Sync> = {
            let mixer = mixer.clone();
            let monitor = monitor.clone();
            Arc::new(move |packet: Packet| {
                if monitor.load(Ordering::Relaxed) {
                    lock(&mixer).push(Instant::now(), MONITOR_PEER, packet.seq, packet.end, &packet.data);
                }
                on_packet(packet);
            })
        };
        Self {
            ctl,
            mixer,
            streams: Mutex::new(None),
            errors: Arc::new(Mutex::new(None)),
            monitor,
            on_packet,
        }
    }

    /// Settings and meters (mute, gain, microphone level…).
    pub fn controls(&self) -> &Arc<Controls> {
        &self.ctl
    }

    /// Open the microphone and output. `mic`/`spk` — part of a device name, empty — system default.
    /// Calling it again while running does nothing.
    pub fn start(&self, mic: &str, spk: &str) -> Result<StreamInfo, AudioError> {
        let mut streams = lock(&self.streams);
        if let Some(active) = streams.as_ref() {
            return Ok(active.info.clone());
        }
        self.ctl.reset_stats();
        lock(&self.mixer).clear();
        *lock(&self.errors) = None;
        let ctl = self.ctl.clone();
        let make_processor = move || CaptureProcessor::new(ctl.clone()).map_err(|e| AudioError(e.to_string()));
        let opened = device::open(mic, spk, &make_processor, self.on_packet.clone(), self.mixer.clone(), &self.errors)?;
        let info = opened.info.clone();
        *streams = Some(opened);
        Ok(info)
    }

    /// Close the devices.
    pub fn stop(&self) {
        let closed = lock(&self.streams).take();
        drop(closed);
        lock(&self.mixer).clear();
        self.ctl.reset_stats();
    }

    pub fn running(&self) -> bool {
        lock(&self.streams).is_some()
    }

    pub fn info(&self) -> Option<StreamInfo> {
        lock(&self.streams).as_ref().map(|s| s.info.clone())
    }

    /// A device error that happened while running (headset unplugged and so on).
    pub fn take_error(&self) -> Option<String> {
        lock(&self.errors).take()
    }

    /// A voice packet from the network.
    pub fn push(&self, sender: u32, seq: u16, end: bool, data: &[u8]) {
        if sender == MONITOR_PEER {
            return;
        }
        lock(&self.mixer).push(Instant::now(), sender, seq, end, data);
    }

    /// Play an interface sound (if the engine is running).
    pub fn play(&self, sound: Sound) {
        if self.running() {
            lock(&self.mixer).play(&sound::render(sound));
        }
    }

    pub fn set_monitor(&self, on: bool) {
        self.monitor.store(on, Ordering::Relaxed);
        if !on {
            lock(&self.mixer).retain_except(MONITOR_PEER);
        }
    }

    pub fn monitor(&self) -> bool {
        self.monitor.load(Ordering::Relaxed)
    }

    pub fn set_peer_volume(&self, id: u32, volume: f32) {
        lock(&self.mixer).set_volume(id, volume);
    }

    pub fn set_peer_muted(&self, id: u32, muted: bool) {
        lock(&self.mixer).set_muted(id, muted);
    }

    /// Keep only those who are in the voice channel now.
    pub fn retain_peers(&self, active: &[u32]) {
        let mut keep = active.to_vec();
        if self.monitor() {
            keep.push(MONITOR_PEER);
        }
        lock(&self.mixer).retain(&keep);
    }

    /// A peer's voice level, 0..1.
    pub fn peer_level(&self, id: u32) -> f32 {
        lock(&self.mixer).level(id)
    }

    pub fn peer_stats(&self) -> Vec<PeerStats> {
        lock(&self.mixer).stats().into_iter().filter(|s| s.id != MONITOR_PEER).collect()
    }
}
