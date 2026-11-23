//! The client's link to the room: one background task owns the local
//! [`tovuer_node::Node`], starts it when the person logs in, restarts it with
//! new details and passes its events to the interface. There is no server —
//! the node itself talks to the other members.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tovuer_node::{Config, Event, Network, Node, SecretKey};
use tovuer_proto::{ClientMsg, ErrorCode, ServerMsg};
use tovuer_voice::Engine;

/// Which room to join and as whom.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NetConfig {
    /// Room name, normalized.
    pub room: String,
    pub password: Option<String>,
    pub name: String,
}

pub enum NetCmd {
    Send(ClientMsg),
    /// Join a room (or rejoin with new details).
    Connect { session: u64, config: NetConfig },
    /// Leave the room and wait for a new `Connect`.
    Stop,
    Shutdown,
}

#[derive(Debug)]
pub enum NetEvent {
    /// Joining `server` (the room name).
    Connecting { attempt: u32, server: String },
    Msg(Box<ServerMsg>),
    /// Could not open the room; the next try is in `retry_in`.
    Lost { reason: String, retry_in: Duration },
    /// The room will not have us (nick taken, removed by the owner).
    Rejected { code: ErrorCode, msg: String },
    /// Median round trip to the other members; `None` — nobody else is here.
    Latency(Option<Duration>),
}

/// What the node needs from the client: identity and where to keep things.
#[derive(Clone)]
pub struct NodeSetup {
    pub version: String,
    pub secret: SecretKey,
    pub data_dir: PathBuf,
    pub files_dir: PathBuf,
    pub release_dir: Option<PathBuf>,
    pub network: Network,
}

/// Handle to the network task.
#[derive(Clone)]
pub struct NetHandle {
    cmd: mpsc::UnboundedSender<NetCmd>,
    session: Arc<AtomicU64>,
    voice: mpsc::Sender<Bytes>,
    node: Arc<Mutex<Option<Node>>>,
}

impl NetHandle {
    pub fn send(&self, msg: ClientMsg) {
        let _ = self.cmd.send(NetCmd::Send(msg));
    }

    pub fn connect(&self, cfg: NetConfig) {
        let session = self.session.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.cmd.send(NetCmd::Connect { session, config: cfg });
    }

    pub fn stop(&self) {
        self.session.fetch_add(1, Ordering::Relaxed);
        let _ = self.cmd.send(NetCmd::Stop);
    }

    pub fn is_current(&self, session: u64) -> bool {
        self.session.load(Ordering::Relaxed) == session
    }

    pub fn shutdown(&self) {
        let _ = self.cmd.send(NetCmd::Shutdown);
    }

    /// A voice frame from the audio thread: never blocks; dropped if the queue is full.
    pub fn voice(&self, frame: Vec<u8>) {
        let _ = self.voice.try_send(Bytes::from(frame));
    }

    /// The running node — for file transfers and updates.
    pub fn node(&self) -> Option<Node> {
        self.node.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A handle to nowhere — for tests and screenshots.
    pub fn detached() -> Self {
        let (cmd, _) = mpsc::unbounded_channel();
        let (voice, _) = mpsc::channel(1);
        Self { cmd, voice, node: Arc::default(), session: Arc::default() }
    }

    /// A handle whose commands can be read back — for tests.
    pub fn recorded() -> (Self, mpsc::UnboundedReceiver<NetCmd>) {
        let (cmd, rx) = mpsc::unbounded_channel();
        let (voice, _) = mpsc::channel(1);
        (Self { cmd, voice, node: Arc::default(), session: Arc::default() }, rx)
    }
}

/// Start the network task. Voice from peers goes straight into `engine`.
pub fn spawn(events: mpsc::UnboundedSender<(u64, NetEvent)>, engine: Arc<Engine>, setup: NodeSetup) -> NetHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (voice_tx, voice_rx) = mpsc::channel::<Bytes>(64);
    let node = Arc::new(Mutex::new(None));
    tokio::spawn(run(cmd_rx, voice_rx, events, engine, setup, node.clone()));
    NetHandle { cmd: cmd_tx, voice: voice_tx, node, session: Arc::default() }
}

/// Pause before the next attempt: 1, 2, 4, 8, 15, 15… seconds.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.saturating_sub(1).min(4)).min(15))
}

/// Normalize a room name as typed (case-insensitive, spaces become dashes).
pub fn normalize_room(input: &str) -> Option<String> {
    tovuer_node::normalize_room(input)
}

struct Running {
    node: Node,
    events: mpsc::UnboundedReceiver<Event>,
}

async fn open(cfg: &NetConfig, setup: &NodeSetup, engine: &Arc<Engine>) -> anyhow::Result<Running> {
    let (tx, events) = mpsc::unbounded_channel();
    let engine = engine.clone();
    let sink = Arc::new(move |sender: u32, seq: u16, end: bool, data: &[u8]| engine.push(sender, seq, end, data));
    let node = Node::start(
        Config {
            room: cfg.room.clone(),
            password: cfg.password.clone().unwrap_or_default(),
            nick: cfg.name.clone(),
            version: setup.version.clone(),
            secret: setup.secret.clone(),
            data_dir: setup.data_dir.clone(),
            files_dir: setup.files_dir.clone(),
            release_dir: setup.release_dir.clone(),
            network: setup.network.clone(),
        },
        tx,
        sink,
    )
    .await?;
    Ok(Running { node, events })
}

async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<NetCmd>,
    mut voice_rx: mpsc::Receiver<Bytes>,
    events: mpsc::UnboundedSender<(u64, NetEvent)>,
    engine: Arc<Engine>,
    setup: NodeSetup,
    shared: Arc<Mutex<Option<Node>>>,
) {
    let set_shared = |node: Option<Node>| *shared.lock().unwrap_or_else(|e| e.into_inner()) = node;
    let mut running: Option<Running> = None;
    let mut session = 0;
    // a room that could not be opened is tried again after a pause
    let mut retry: Option<(NetConfig, u32, tokio::time::Instant)> = None;

    loop {
        let node_event = async {
            match running.as_mut() {
                Some(r) => r.events.recv().await,
                None => std::future::pending().await,
            }
        };
        let retry_at = retry.as_ref().map(|(_, _, at)| *at);
        let retry_due = async {
            match retry_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };

        let connect: Option<(NetConfig, u32)> = tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(NetCmd::Connect { session: next, config }) => {
                    session = next;
                    Some((config, 1))
                }
                Some(NetCmd::Send(msg)) => {
                    if let Some(r) = &running {
                        r.node.send(msg);
                    }
                    None
                }
                Some(NetCmd::Stop) => {
                    retry = None;
                    if let Some(r) = running.take() {
                        set_shared(None);
                        r.node.shutdown().await;
                    }
                    None
                }
                Some(NetCmd::Shutdown) | None => {
                    if let Some(r) = running.take() {
                        set_shared(None);
                        r.node.shutdown().await;
                    }
                    return;
                }
            },
            Some(frame) = voice_rx.recv() => {
                if let Some(r) = &running {
                    r.node.voice(frame);
                }
                None
            }
            event = node_event => {
                match event {
                    Some(Event::Msg(msg)) => {
                        let _ = events.send((session, NetEvent::Msg(msg)));
                    }
                    Some(Event::Latency(rtt)) => {
                        let _ = events.send((session, NetEvent::Latency(rtt)));
                    }
                    Some(Event::Rejected { code, msg }) => {
                        set_shared(None);
                        running = None;
                        let _ = events.send((session, NetEvent::Rejected { code, msg }));
                    }
                    None => {
                        set_shared(None);
                        running = None;
                    }
                }
                None
            }
            _ = retry_due => retry.take().map(|(cfg, attempt, _)| (cfg, attempt)),
        };

        if let Some((cfg, attempt)) = connect {
            retry = None;
            if let Some(r) = running.take() {
                set_shared(None);
                r.node.shutdown().await;
            }
            let _ = events.send((session, NetEvent::Connecting { attempt, server: cfg.room.clone() }));
            match open(&cfg, &setup, &engine).await {
                Ok(r) => {
                    set_shared(Some(r.node.clone()));
                    running = Some(r);
                }
                Err(e) => {
                    let retry_in = backoff(attempt);
                    let _ = events.send((session, NetEvent::Lost { reason: format!("{e:#}"), retry_in }));
                    retry = Some((cfg, attempt + 1, tokio::time::Instant::now() + retry_in));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let secs: Vec<u64> = (1..=8).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 15, 15, 15, 15]);
        assert_eq!(backoff(0).as_secs(), 1);
    }

    #[test]
    fn room_names() {
        assert_eq!(normalize_room(" Our Place ").as_deref(), Some("our-place"));
        assert_eq!(normalize_room("!!!"), None);
    }
}
