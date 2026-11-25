//! The peer. Every running client is one: it keeps a copy of the room,
//! connects directly to every other member it can find, exchanges missing
//! operations with them, sends its own voice and video straight to the
//! people in the same voice channel and serves files it has.
//!
//! For the local client it looks like the old server did: it takes
//! [`ClientMsg`]s and answers with [`ServerMsg`]s.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use bytes::Bytes;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::timeout;
use tovuer_proto::{self as proto, ChannelKind, ClientMsg, ErrorCode, FileMeta, ReplyRef, ServerMsg, User, VideoSource};

use crate::discovery::{Discovery, LocalNet, RoomDht};
use crate::log::{OpLog, Offer};
use crate::ops::{Body, Key, Op};
use crate::room::{RoomKeys, normalize_room};
use crate::state::{Effect, MAX_CHANNELS, Room, clean_emoji};
use crate::util::{hex, random, unhex};
use crate::wire::{ALPN, Frame, OPS_BATCH, PeerState, TAG_CONTROL, TAG_FILE, TAG_VIDEO, read_frame, write_frame};

/// Biggest file a member can share.
pub const MAX_FILE: u64 = 50 << 20;
/// Our own user id in events for the local client; peers get 2, 3, …
const ME: u32 = 1;
/// How long joining waits for someone to answer before the room opens anyway.
const JOIN_WAIT: Duration = Duration::from_secs(12);
/// After the room lookup finished, how long to wait for slow connections.
const JOIN_GRACE: Duration = Duration::from_secs(4);
/// After the first peer caught us up, how long to wait for the others it told us about.
const SYNC_GRACE: Duration = Duration::from_millis(1500);
/// Two connections to the same peer this close together are a simultaneous
/// dial; an older one is a leftover from before the peer restarted.
const DUPLICATE_WINDOW: Duration = Duration::from_secs(3);
const DIAL_TIMEOUT: Duration = Duration::from_secs(20);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const PUBLISH_EVERY: Duration = Duration::from_secs(5 * 60);
const LOOKUP_ALONE_EVERY: Duration = Duration::from_secs(45);
const LOOKUP_EVERY: Duration = Duration::from_secs(5 * 60);
const LATENCY_EVERY: Duration = Duration::from_secs(3);
const RESYNC_EVERY: Duration = Duration::from_secs(60);
const MAX_KNOWN_PEERS: usize = 64;
/// Remote clocks further ahead than this do not drag ours along.
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;
/// Video frames on their way to one peer at once: more wait, and only the
/// newest of them goes — a slow link drops frames instead of lagging. Four
/// keep 45 frames a second going over a link with a 90 ms round trip.
const VIDEO_IN_FLIGHT: usize = 4;
/// A frame not delivered by then is given up.
const VIDEO_SEND_TIMEOUT: Duration = Duration::from_secs(3);
const VIDEO_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How the node reaches other peers.
#[derive(Clone, Debug)]
pub enum Network {
    /// Public internet: n0 relays for hole punching, the BitTorrent DHT to find the room.
    Internet,
    /// Loopback only, rooms found through an in-process list (tests).
    Local(LocalNet),
}

pub struct Config {
    /// Room name as typed.
    pub room: String,
    pub password: String,
    pub nick: String,
    pub version: String,
    /// Identity of this install; peers know us by its public key.
    pub secret: SecretKey,
    /// Rooms are stored in subfolders here.
    pub data_dir: PathBuf,
    /// Shared files, named by content hash.
    pub files_dir: PathBuf,
    pub network: Network,
}

#[derive(Debug)]
pub enum Event {
    Msg(Box<ServerMsg>),
    /// Cannot stay in the room (nick taken, removed by the owner): the node has stopped.
    Rejected { code: ErrorCode, msg: String },
    /// Median round trip to the peers; `None` — nobody else is here.
    Latency(Option<Duration>),
}

/// Where incoming voice goes: `(sender id, sequence number, last frame of a phrase, Opus packet)`.
pub type VoiceSink = Arc<dyn Fn(u32, u16, bool, &[u8]) + Send + Sync>;
/// Where incoming video goes: `(sender id, a checked [`proto::video`] frame)`.
pub type VideoSink = Arc<dyn Fn(u32, Bytes) + Send + Sync>;

#[derive(Clone)]
pub struct Node {
    inner: Arc<Inner>,
}

struct Inner {
    keys: Arc<RoomKeys>,
    secret: SecretKey,
    me: Key,
    endpoint: Endpoint,
    discovery: Discovery,
    internet: bool,
    room_dir: PathBuf,
    files_dir: PathBuf,
    events: mpsc::UnboundedSender<Event>,
    voice_sink: VoiceSink,
    video_sink: VideoSink,
    core: Mutex<Core>,
    stop: watch::Sender<bool>,
}

struct Peer {
    id: u32,
    conn: Connection,
    tx: mpsc::UnboundedSender<Frame>,
    /// The newest of our video frames for this peer.
    video: watch::Sender<Option<Bytes>>,
    state: PeerState,
    addr: EndpointAddr,
    since: Instant,
}

struct Core {
    room: Room,
    log: OpLog,
    clock: u64,
    peers: HashMap<Key, Peer>,
    dialing: HashSet<Key>,
    /// A fresher address that turned up while a dial was already running.
    retry: HashMap<Key, EndpointAddr>,
    /// Address book: everyone we have ever talked to in this room.
    known: HashMap<Key, (EndpointAddr, u64)>,
    ids: HashMap<Key, u32>,
    next_id: u32,
    me: PeerState,
    welcomed: bool,
    synced: Option<Instant>,
    lookup_done: Option<Instant>,
    created_room: bool,
    stopped: bool,
}

impl Core {
    fn tick(&mut self) -> u64 {
        self.clock = proto::now_ms().max(self.clock + 1);
        self.clock
    }

    fn observe(&mut self, ts: u64) {
        if ts <= proto::now_ms() + MAX_CLOCK_SKEW_MS {
            self.clock = self.clock.max(ts);
        }
    }

    fn local_id(&mut self, key: Key) -> u32 {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.ids.insert(key, id);
        id
    }

    fn user(&self, key: &Key, me: &Key) -> Option<User> {
        let admin = self.room.owner() == Some(*key);
        if key == me {
            return Some(User {
                id: ME,
                name: self.me.nick.clone(),
                voice: self.me.voice.clone(),
                muted: self.me.muted || self.me.deafened,
                deafened: self.me.deafened,
                admin,
                video: self.me.video.filter(|_| self.me.voice.is_some()),
            });
        }
        let peer = self.peers.get(key)?;
        Some(User {
            id: peer.id,
            name: peer.state.nick.clone(),
            voice: peer.state.voice.clone(),
            muted: peer.state.muted || peer.state.deafened,
            deafened: peer.state.deafened,
            admin,
            video: peer.state.video.filter(|_| peer.state.voice.is_some()),
        })
    }

    fn users(&self, me: &Key) -> Vec<User> {
        std::iter::once(me).chain(self.peers.keys()).filter_map(|k| self.user(k, me)).collect()
    }

    fn broadcast(&self, frame: &Frame, except: Option<&Key>) {
        for (key, peer) in &self.peers {
            if Some(key) != except {
                let _ = peer.tx.send(frame.clone());
            }
        }
    }

    fn reply_ref(&self, id: u64) -> Option<ReplyRef> {
        let (m, _) = self.room.message(id)?;
        let quote = match (&m.file, m.text.is_empty()) {
            (Some(f), true) => f.name.clone(),
            _ => m.text.clone(),
        };
        Some(ReplyRef { id: m.id, from: m.from, text: proto::one_line(&quote, proto::REPLY_PREVIEW_CHARS) })
    }
}

/// Diagnostics on stderr when `TOVUER_DEBUG` is set.
macro_rules! debug {
    ($($arg:tt)*) => {
        if std::env::var_os("TOVUER_DEBUG").is_some() {
            eprintln!("[tovuer {}] {}", chrono_now(), format!($($arg)*));
        }
    };
}

fn chrono_now() -> String {
    let ms = proto::now_ms();
    format!("{:02}:{:02}.{:03}", (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

fn short(key: &Key) -> String {
    hex(&key[..3])
}

fn key_of(id: &EndpointId) -> Key {
    *id.as_bytes()
}

fn clean_state(mut state: PeerState) -> PeerState {
    state.nick = proto::clean_name(&state.nick).unwrap_or_else(|| "anon".into());
    state.voice = state.voice.and_then(|v| proto::clean_channel(&v));
    if state.voice.is_none() {
        state.video = None;
        state.watching = false;
    }
    state.stream = state.stream.and_then(|n| proto::clean_name(&n));
    state.version = proto::one_line(&state.version, 40);
    state
}

fn valid_file_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn load_known(dir: &Path) -> HashMap<Key, (EndpointAddr, u64)> {
    std::fs::read(dir.join("peers.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<(EndpointAddr, u64)>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(addr, seen)| (key_of(&addr.id), (addr, seen)))
        .collect()
}

fn save_known(dir: &Path, known: &HashMap<Key, (EndpointAddr, u64)>) {
    let mut list: Vec<&(EndpointAddr, u64)> = known.values().collect();
    list.sort_by_key(|(_, seen)| std::cmp::Reverse(*seen));
    list.truncate(MAX_KNOWN_PEERS);
    if let Ok(bytes) = serde_json::to_vec(&list) {
        let tmp = dir.join("peers.json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(tmp, dir.join("peers.json"));
        }
    }
}

impl Node {
    /// Open the room: load what we stored, start listening and go looking for
    /// the other members. Returns at once; [`Event`]s follow, starting with
    /// `Welcome` once the room is joined.
    pub async fn start(cfg: Config, events: mpsc::UnboundedSender<Event>, voice_sink: VoiceSink, video_sink: VideoSink) -> anyhow::Result<Node> {
        let name = normalize_room(&cfg.room).context("the room name needs letters or digits")?;
        let nick = proto::clean_name(&cfg.nick).context("the nick is empty")?;
        let keys = {
            let (name, password) = (name.clone(), cfg.password.clone());
            Arc::new(tokio::task::spawn_blocking(move || RoomKeys::derive(&name, &password)).await?)
        };
        let room_dir = cfg.data_dir.join(keys.dir_name());
        std::fs::create_dir_all(&room_dir).with_context(|| format!("cannot create {}", room_dir.display()))?;
        std::fs::create_dir_all(&cfg.files_dir).with_context(|| format!("cannot create {}", cfg.files_dir.display()))?;
        let log = OpLog::open(&room_dir.join("ops.jsonl"));
        let room = Room::fold(&name, log.sorted());
        let clock = log.sorted().last().map_or(0, |op| op.ts);

        let (endpoint, discovery, internet) = match &cfg.network {
            Network::Internet => {
                let endpoint = Endpoint::builder(presets::N0)
                    .secret_key(cfg.secret.clone())
                    .alpns(vec![ALPN.to_vec()])
                    .bind()
                    .await
                    .map_err(|e| anyhow!("cannot open the network: {e}"))?;
                let discovery = match RoomDht::start(keys.clone()) {
                    Ok(dht) => Discovery::Dht(dht),
                    Err(e) => {
                        eprintln!("tovuer: DHT unavailable ({e}); only remembered peers will be found");
                        Discovery::None
                    }
                };
                (endpoint, discovery, true)
            }
            Network::Local(net) => {
                let endpoint = Endpoint::builder(presets::Minimal)
                    .secret_key(cfg.secret.clone())
                    .alpns(vec![ALPN.to_vec()])
                    .relay_mode(RelayMode::Disabled)
                    .bind_addr("127.0.0.1:0")
                    .map_err(|e| anyhow!("bad address: {e}"))?
                    .bind()
                    .await
                    .map_err(|e| anyhow!("cannot open the network: {e}"))?;
                (endpoint, Discovery::Local(net.clone()), false)
            }
        };
        let me_state = PeerState { nick, version: cfg.version.clone(), ..PeerState::default() };
        let (stop, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            me: key_of(&endpoint.id()),
            keys,
            secret: cfg.secret,
            endpoint,
            discovery,
            internet,
            room_dir: room_dir.clone(),
            files_dir: cfg.files_dir,
            events,
            voice_sink,
            video_sink,
            core: Mutex::new(Core {
                room,
                log,
                clock,
                peers: HashMap::new(),
                dialing: HashSet::new(),
                retry: HashMap::new(),
                known: load_known(&room_dir),
                ids: HashMap::new(),
                next_id: ME + 1,
                me: me_state,
                welcomed: false,
                synced: None,
                lookup_done: None,
                created_room: false,
                stopped: false,
            }),
            stop,
        });
        tokio::spawn(inner.clone().accept_loop());
        tokio::spawn(inner.clone().join());
        Ok(Node { inner })
    }

    pub fn id(&self) -> EndpointId {
        self.inner.endpoint.id()
    }

    /// Normalized room name.
    pub fn room(&self) -> String {
        self.inner.keys.name.clone()
    }

    /// A command from the local client.
    pub fn send(&self, msg: ClientMsg) {
        self.inner.handle_client(msg);
    }

    /// A voice frame from our microphone: straight to everyone in our voice channel.
    pub fn voice(&self, frame: Bytes) {
        let core = self.inner.lock();
        let Some(room) = core.me.voice.as_ref() else { return };
        for peer in core.peers.values() {
            // people who switched sound off do not need our frames
            if peer.state.voice.as_ref() == Some(room) && !peer.state.deafened {
                let _ = peer.conn.send_datagram(frame.clone());
            }
        }
    }

    /// A frame from our camera or screen ([`proto::video`] format): to
    /// everyone in our voice channel who is watching the call, and — a
    /// screen — to whoever watches it from a text channel.
    pub fn video(&self, frame: Bytes) {
        let core = self.inner.lock();
        let (Some(room), Some(source)) = (core.me.voice.as_ref(), core.me.video) else { return };
        for peer in core.peers.values() {
            let in_call = peer.state.voice.as_ref() == Some(room) && peer.state.watching;
            let watches = source == VideoSource::Screen && peer.state.stream.as_deref().is_some_and(|n| same_nick(n, &core.me.nick));
            if in_call || watches {
                peer.video.send_replace(Some(frame.clone()));
            }
        }
    }

    /// Where a shared file lives on disk once we have it.
    pub fn file_path(&self, id: &str) -> Option<PathBuf> {
        valid_file_id(id).then(|| self.inner.files_dir.join(id))
    }

    /// Share a file in a text channel. `progress(done, total)`.
    pub async fn upload(
        &self,
        path: &Path,
        channel: &str,
        text: &str,
        reply_to: Option<u64>,
        progress: impl FnMut(u64, u64) + Send,
    ) -> Result<(), String> {
        self.inner.upload(path, channel, text, reply_to, progress).await
    }

    /// Get a shared file from whoever has it; returns its path in the file store.
    pub async fn fetch_file(&self, file: &FileMeta, progress: impl FnMut(u64, u64) + Send) -> Result<PathBuf, String> {
        self.inner.fetch_file(file, progress).await
    }

    /// Leave the room and close every connection.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, core: &Core, msgs: impl IntoIterator<Item = ServerMsg>) {
        // before the welcome the client knows nothing; the welcome carries it all
        if core.welcomed {
            for msg in msgs {
                let _ = self.events.send(Event::Msg(Box::new(msg)));
            }
        }
    }

    fn error(&self, core: &Core, code: ErrorCode, msg: &str) {
        self.emit(core, [ServerMsg::Error { code, msg: msg.to_string() }]);
    }

    fn stopped(&self) -> bool {
        *self.stop.borrow()
    }

    async fn shutdown(&self) {
        {
            let mut core = self.lock();
            if core.stopped {
                return;
            }
            core.stopped = true;
            for peer in core.peers.values() {
                peer.conn.close(0u32.into(), b"bye");
            }
            core.peers.clear();
        }
        self.stop.send_replace(true);
        self.endpoint.close().await;
    }

    fn reject(self: &Arc<Self>, code: ErrorCode, msg: String) {
        let _ = self.events.send(Event::Rejected { code, msg });
        let me = self.clone();
        tokio::spawn(async move { me.shutdown().await });
    }

    /// Sleep unless the node stops first. `false` — it stopped.
    async fn nap(&self, pause: Duration) -> bool {
        let mut stop = self.stop.subscribe();
        tokio::select! {
            _ = tokio::time::sleep(pause) => !self.stopped(),
            _ = stop.wait_for(|s| *s) => false,
        }
    }

    // ------------------------------------------------------------------ joining

    async fn join(self: Arc<Self>) {
        // remembered peers first: usually the quickest way back in
        let remembered: Vec<EndpointAddr> = self.lock().known.values().map(|(a, _)| a.clone()).collect();
        for addr in remembered {
            self.dial(addr);
        }
        tokio::spawn(self.clone().publish_loop());
        {
            let me = self.clone();
            tokio::spawn(async move {
                for addr in me.discovery.lookup(&me.keys).await {
                    me.dial(addr);
                }
                me.lock().lookup_done = Some(Instant::now());
            });
        }

        let started = Instant::now();
        loop {
            if self.stopped() {
                return;
            }
            let ready = {
                let core = self.lock();
                // someone caught us up: wait a moment for the peers they told us about
                let synced = core.synced.is_some_and(|at| core.dialing.is_empty() || at.elapsed() > SYNC_GRACE);
                let settled = core.lookup_done.is_some_and(|at| core.dialing.is_empty() || at.elapsed() > JOIN_GRACE);
                synced || settled || started.elapsed() > JOIN_WAIT
            };
            if ready {
                break;
            }
            if !self.nap(Duration::from_millis(100)).await {
                return;
            }
        }
        self.welcome();
        if self.stopped() {
            return;
        }
        tokio::spawn(self.clone().lookup_loop());
        tokio::spawn(self.clone().latency_loop());
        tokio::spawn(self.clone().resync_loop());
    }

    fn welcome(self: &Arc<Self>) {
        let mut core = self.lock();
        if core.stopped {
            return;
        }
        debug!("{} welcome: {} peers, synced {:?}, ops {}", short(&self.me), core.peers.len(), core.synced.map(|t| t.elapsed()), core.log.sorted().len());
        if core.room.is_banned(&self.me) {
            drop(core);
            return self.reject(ErrorCode::Kicked, "the owner removed you from this room".into());
        }
        let nick = core.me.nick.to_lowercase();
        if core.peers.values().any(|p| p.state.nick.to_lowercase() == nick) {
            let msg = format!("the nick {} is taken in this room right now — pick another", core.me.nick);
            drop(core);
            return self.reject(ErrorCode::NameTaken, msg);
        }
        if core.room.owner().is_none() {
            // nobody has made this room yet — it is ours
            let title = self.keys.name.clone();
            if let Err(msg) = self.create_op(&mut core, Body::Genesis { name: title }) {
                drop(core);
                return self.reject(ErrorCode::Storage, msg);
            }
            core.created_room = true;
        }
        core.welcomed = true;
        let owner = core.room.owner();
        let motd = match (core.peers.is_empty(), core.created_room) {
            (true, true) => Some(format!(
                "Room “{}” created. Friends join by typing the same room name and password.",
                self.keys.name
            )),
            (true, false) => Some("Nobody else is here right now — your messages reach the others when they come online.".into()),
            _ => None,
        };
        let welcome = ServerMsg::Welcome {
            id: ME,
            name: core.me.nick.clone(),
            server: core.room.title().to_string(),
            version: core.me.version.clone(),
            proto: proto::PROTO_VERSION,
            admin: owner == Some(self.me),
            channels: core.room.channels(),
            users: core.users(&self.me),
            max_file: MAX_FILE,
            motd,
        };
        self.emit(&core, [welcome]);
    }

    async fn publish_loop(self: Arc<Self>) {
        if self.internet {
            // the address is only useful to others once a relay knows us
            let _ = timeout(Duration::from_secs(10), self.endpoint.online()).await;
        }
        loop {
            self.discovery.publish(&self.keys, self.endpoint.addr()).await;
            if !self.nap(PUBLISH_EVERY).await {
                return;
            }
        }
    }

    async fn lookup_loop(self: Arc<Self>) {
        loop {
            let alone = self.lock().peers.is_empty();
            if !self.nap(if alone { LOOKUP_ALONE_EVERY } else { LOOKUP_EVERY }).await {
                return;
            }
            for addr in self.discovery.lookup(&self.keys).await {
                self.dial(addr);
            }
        }
    }

    async fn latency_loop(self: Arc<Self>) {
        loop {
            if !self.nap(LATENCY_EVERY).await {
                return;
            }
            let mut rtts: Vec<Duration> = {
                let core = self.lock();
                core.peers
                    .values()
                    .filter_map(|p| p.conn.paths().iter().find(|path| path.is_selected()).map(|path| path.rtt()))
                    .collect()
            };
            rtts.sort();
            let _ = self.events.send(Event::Latency(rtts.get(rtts.len() / 2).copied()));
        }
    }

    async fn resync_loop(self: Arc<Self>) {
        loop {
            if !self.nap(RESYNC_EVERY).await {
                return;
            }
            let core = self.lock();
            core.broadcast(&Frame::Heads { heads: core.log.heads() }, None);
        }
    }

    // -------------------------------------------------------------- connections

    fn hello(&self, core: &Core, remote: &Key) -> Frame {
        Frame::Hello {
            proto: proto::PROTO_VERSION,
            room: self.keys.id_hex(),
            proof: hex(&self.keys.proof(&self.me, remote)),
            state: core.me.clone(),
            addr: self.endpoint.addr(),
        }
    }

    fn check_hello(&self, remote: &Key, frame: Option<Frame>) -> Result<(PeerState, EndpointAddr), String> {
        match frame {
            Some(Frame::Hello { proto: version, room, proof, state, addr }) => {
                if version != proto::PROTO_VERSION {
                    return Err(format!("different tovuer protocol ({version} vs {}) — update tovuer", proto::PROTO_VERSION));
                }
                if room != self.keys.id_hex() {
                    return Err("different room".into());
                }
                if unhex::<32>(&proof) != Some(self.keys.proof(remote, &self.me)) {
                    return Err("wrong room key".into());
                }
                let id = EndpointId::from_bytes(remote).map_err(|e| e.to_string())?;
                let addr = if addr.id == id { addr } else { EndpointAddr::from_parts(id, addr.addrs) };
                Ok((clean_state(state), addr))
            }
            Some(Frame::Reject { reason }) => Err(reason),
            _ => Err("no hello".into()),
        }
    }

    fn dial(self: &Arc<Self>, addr: EndpointAddr) {
        let key = key_of(&addr.id);
        {
            let mut core = self.lock();
            if key == self.me || core.stopped || core.peers.contains_key(&key) || core.room.is_banned(&key) {
                return;
            }
            if core.dialing.contains(&key) {
                core.retry.insert(key, addr);
                return;
            }
            core.dialing.insert(key);
        }
        let me = self.clone();
        tokio::spawn(async move {
            debug!("{} dials {}", short(&me.me), short(&key));
            let tried = addr.clone();
            match timeout(DIAL_TIMEOUT, me.connect(addr)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => debug!("{} -> {} failed: {e:#}", short(&me.me), short(&key)),
                Err(_) => debug!("{} -> {} timed out", short(&me.me), short(&key)),
            }
            let again = {
                let mut core = me.lock();
                core.dialing.remove(&key);
                core.retry.remove(&key).filter(|a| *a != tried && !core.peers.contains_key(&key))
            };
            if let Some(addr) = again {
                me.dial(addr);
            }
        });
    }

    async fn connect(self: &Arc<Self>, addr: EndpointAddr) -> anyhow::Result<()> {
        let remote = key_of(&addr.id);
        let conn = self.endpoint.connect(addr, ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&[TAG_CONTROL]).await?;
        let hello = self.hello(&self.lock(), &remote);
        write_frame(&mut send, &hello).await?;
        let answer = timeout(HELLO_TIMEOUT, read_frame(&mut recv)).await??;
        match self.check_hello(&remote, answer) {
            Ok((state, addr)) => {
                self.register(conn, send, recv, state, addr, true);
                Ok(())
            }
            Err(reason) => {
                conn.close(1u32.into(), reason.as_bytes());
                bail!(reason)
            }
        }
    }

    async fn accept_loop(self: Arc<Self>) {
        while let Some(incoming) = self.endpoint.accept().await {
            let me = self.clone();
            tokio::spawn(async move {
                match timeout(HELLO_TIMEOUT * 2, me.accept_one(incoming)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => debug!("{} accept failed: {e:#}", short(&me.me)),
                    Err(_) => debug!("{} accept timed out", short(&me.me)),
                }
            });
        }
    }

    async fn accept_one(self: &Arc<Self>, incoming: Incoming) -> anyhow::Result<()> {
        let conn = incoming.await?;
        let remote = key_of(&conn.remote_id());
        if self.lock().room.is_banned(&remote) {
            conn.close(1u32.into(), b"banned");
            return Ok(());
        }
        let (mut send, mut recv) = timeout(HELLO_TIMEOUT, conn.accept_bi()).await??;
        let mut tag = [0u8; 1];
        recv.read_exact(&mut tag).await?;
        if tag[0] != TAG_CONTROL {
            bail!("expected the control stream");
        }
        let hello = timeout(HELLO_TIMEOUT, read_frame(&mut recv)).await??;
        match self.check_hello(&remote, hello) {
            Ok((state, addr)) => {
                let answer = self.hello(&self.lock(), &remote);
                write_frame(&mut send, &answer).await?;
                self.register(conn, send, recv, state, addr, false);
                Ok(())
            }
            Err(reason) => {
                let _ = write_frame(&mut send, &Frame::Reject { reason: reason.clone() }).await;
                let _ = send.finish();
                tokio::time::sleep(Duration::from_millis(200)).await;
                conn.close(1u32.into(), reason.as_bytes());
                bail!(reason)
            }
        }
    }

    fn register(self: &Arc<Self>, conn: Connection, send: SendStream, recv: RecvStream, state: PeerState, addr: EndpointAddr, dialed_by_me: bool) {
        let key = key_of(&conn.remote_id());
        let (tx, rx) = mpsc::unbounded_channel();
        let (video, video_rx) = watch::channel(None);
        let known = {
            let mut core = self.lock();
            if core.stopped || core.room.is_banned(&key) {
                conn.close(1u32.into(), b"bye");
                return;
            }
            let replaced = match core.peers.get(&key) {
                Some(old) => {
                    // both sides dialed at once: keep the one the smaller id made;
                    // an older connection is stale (the peer restarted) — the new one wins
                    let simultaneous = old.since.elapsed() < DUPLICATE_WINDOW;
                    if simultaneous && dialed_by_me != (self.me < key) {
                        conn.close(0u32.into(), b"duplicate");
                        return;
                    }
                    old.conn.close(0u32.into(), b"duplicate");
                    true
                }
                None => false,
            };
            let id = core.local_id(key);
            debug!("{} registered {} ({}, dialed by me: {dialed_by_me}, replaced: {replaced})", short(&self.me), short(&key), state.nick);
            let others: Vec<EndpointAddr> = core.peers.iter().filter(|(k, _)| **k != key).map(|(_, p)| p.addr.clone()).collect();
            let _ = tx.send(Frame::State { state: core.me.clone() });
            let _ = tx.send(Frame::Heads { heads: core.log.heads() });
            if !others.is_empty() {
                let _ = tx.send(Frame::Peers { addrs: others });
            }
            core.peers.insert(key, Peer { id, conn: conn.clone(), tx, video, state, addr: addr.clone(), since: Instant::now() });
            core.known.insert(key, (addr, proto::now_ms()));
            if let Some(user) = core.user(&key, &self.me) {
                let event = if replaced { ServerMsg::UserUpdate { user } } else { ServerMsg::UserJoin { user } };
                self.emit(&core, [event]);
            }
            core.known.clone()
        };
        let dir = self.room_dir.clone();
        tokio::task::spawn_blocking(move || save_known(&dir, &known));

        tokio::spawn(writer(send, rx));
        tokio::spawn(self.clone().reader(key, conn.clone(), recv));
        tokio::spawn(self.clone().datagrams(key, conn.clone()));
        tokio::spawn(video_writer(conn.clone(), video_rx));
        tokio::spawn(self.clone().video_frames(key, conn.clone()));
        tokio::spawn(self.clone().requests(conn));
    }

    fn unregister(&self, key: &Key, conn: &Connection) {
        let mut core = self.lock();
        debug!("{} lost {}", short(&self.me), short(key));
        if core.peers.get(key).is_some_and(|p| p.conn.stable_id() == conn.stable_id()) {
            let peer = core.peers.remove(key);
            if let Some(peer) = peer {
                self.emit(&core, [ServerMsg::UserLeave { id: peer.id }]);
            }
        }
    }

    async fn reader(self: Arc<Self>, key: Key, conn: Connection, mut recv: RecvStream) {
        while let Ok(Some(frame)) = read_frame(&mut recv).await {
            self.on_frame(key, frame);
        }
        conn.close(0u32.into(), b"bye");
        self.unregister(&key, &conn);
    }

    async fn datagrams(self: Arc<Self>, key: Key, conn: Connection) {
        while let Ok(data) = conn.read_datagram().await {
            let sender = {
                let core = self.lock();
                let Some(peer) = core.peers.get(&key) else { continue };
                match (&core.me.voice, &peer.state.voice) {
                    (Some(mine), Some(theirs)) if mine == theirs && !core.me.deafened => Some(peer.id),
                    _ => None,
                }
            };
            if let (Some(id), Some(frame)) = (sender, proto::voice::parse(&data)) {
                (self.voice_sink)(id, frame.seq, frame.is_end(), frame.payload);
            }
        }
    }

    /// Video from a peer: a stream per frame, taken only while they are in
    /// our voice channel with video on and we are watching.
    async fn video_frames(self: Arc<Self>, key: Key, conn: Connection) {
        while let Ok(mut recv) = conn.accept_uni().await {
            // (their id, only a screen is wanted from them)
            let sender = {
                let core = self.lock();
                core.peers.get(&key).and_then(|peer| {
                    let same = core.me.voice.is_some() && core.me.voice == peer.state.voice;
                    let in_call = same && core.me.watching && peer.state.video.is_some();
                    let watched = peer.state.video == Some(VideoSource::Screen) && core.me.stream.as_deref().is_some_and(|n| same_nick(n, &peer.state.nick));
                    (in_call || watched).then_some((peer.id, !in_call))
                })
            };
            let Some((id, screen_only)) = sender else {
                let _ = recv.stop(0u32.into());
                continue;
            };
            let sink = self.video_sink.clone();
            tokio::spawn(async move {
                let Ok(Ok(data)) = timeout(VIDEO_READ_TIMEOUT, recv.read_to_end(1 + proto::video::MAX_FRAME)).await else { return };
                let frame = data.get(1..).and_then(proto::video::parse);
                if data.first() == Some(&TAG_VIDEO) && frame.is_some_and(|f| !screen_only || f.source == VideoSource::Screen) {
                    sink(id, Bytes::from(data).slice(1..));
                }
            });
        }
    }

    async fn requests(self: Arc<Self>, conn: Connection) {
        while let Ok((send, recv)) = conn.accept_bi().await {
            let me = self.clone();
            tokio::spawn(async move {
                let _ = me.serve(send, recv).await;
            });
        }
    }

    // ---------------------------------------------------------------- frames

    fn on_frame(self: &Arc<Self>, from: Key, frame: Frame) {
        match frame {
            Frame::Ops { ops } => self.ingest(from, ops),
            Frame::Peers { addrs } => {
                for addr in addrs.into_iter().take(MAX_KNOWN_PEERS) {
                    self.dial(addr);
                }
            }
            frame => {
                let mut core = self.lock();
                let Some(peer) = core.peers.get(&from) else { return };
                let (id, nick, in_voice) = (peer.id, peer.state.nick.clone(), peer.state.voice.is_some());
                match frame {
                    Frame::State { state } => {
                        let state = clean_state(state);
                        if let Some(peer) = core.peers.get_mut(&from) {
                            peer.state = state;
                        }
                        if let Some(user) = core.user(&from, &self.me) {
                            self.emit(&core, [ServerMsg::UserUpdate { user }]);
                        }
                    }
                    Frame::Heads { heads } => {
                        let missing = core.log.missing_for(&heads);
                        if let Some(peer) = core.peers.get(&from) {
                            for chunk in missing.chunks(OPS_BATCH) {
                                let _ = peer.tx.send(Frame::Ops { ops: chunk.to_vec() });
                            }
                            let _ = peer.tx.send(Frame::Synced);
                        }
                    }
                    Frame::Synced => {
                        core.synced.get_or_insert_with(Instant::now);
                    }
                    Frame::Typing { channel } => {
                        if core.room.channel(&channel, ChannelKind::Text) {
                            self.emit(&core, [ServerMsg::Typing { channel, name: nick }]);
                        }
                    }
                    Frame::Speaking { on } if in_voice || !on => {
                        self.emit(&core, [ServerMsg::Speaking { id, on }]);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Operations from a peer: check, store, fold, pass on.
    fn ingest(self: &Arc<Self>, from: Key, ops: Vec<Op>) {
        // signatures are checked outside the lock — catching up can be thousands
        let ops: Vec<Op> = ops.into_iter().filter(|op| op.verify(&self.keys)).collect();
        let mut effects = Vec::new();
        {
            let mut core = self.lock();
            let mut gap = false;
            let mut forward = Vec::new();
            for op in ops {
                let Some(author) = op.author_key() else { continue };
                match core.log.offer(&op) {
                    Offer::Added => {
                        core.observe(op.ts);
                        let (events, effect) = core.room.apply(&op);
                        self.emit(&core, events);
                        effects.push(effect);
                        // straight from the author: others may not be connected to them
                        if author == from {
                            forward.push(op);
                        }
                    }
                    Offer::WriteFailed(msg) => {
                        if core.welcomed {
                            self.error(&core, ErrorCode::Storage, &msg);
                        } else {
                            drop(core);
                            self.reject(ErrorCode::Storage, msg);
                            return;
                        }
                        break;
                    }
                    Offer::Duplicate => {}
                    Offer::Gap => gap = true,
                }
            }
            if gap && let Some(peer) = core.peers.get(&from) {
                let _ = peer.tx.send(Frame::Heads { heads: core.log.heads() });
            }
            // the owner deleted the voice channel we are in: we are out of it
            // (and tell everyone, or we would linger in it as a ghost)
            if let Some(voice) = core.me.voice.clone()
                && !core.room.channel(&voice, ChannelKind::Voice)
            {
                self.set_presence(&mut core, leave_voice);
            }
            if !forward.is_empty() {
                core.broadcast(&Frame::Ops { ops: forward }, Some(&from));
            }
        }
        for effect in effects {
            self.on_effect(effect);
        }
    }

    fn on_effect(self: &Arc<Self>, effect: Effect) {
        match effect {
            Effect::None | Effect::OwnerSet(_) => {}
            Effect::Banned(key) if key == self.me => {
                self.reject(ErrorCode::Kicked, "the owner removed you from this room".into());
            }
            Effect::Banned(key) => {
                let mut core = self.lock();
                if let Some(peer) = core.peers.remove(&key) {
                    self.emit(&core, [ServerMsg::UserLeave { id: peer.id }]);
                    // dropping the peer lets its writer flush the ban itself;
                    // the connection closes once that had time to leave
                    let conn = peer.conn.clone();
                    drop(peer);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        conn.close(1u32.into(), b"banned");
                    });
                }
            }
            Effect::OwnerChanged(_) => {
                // fold again from scratch and show the client the corrected room
                let mut core = self.lock();
                core.room = Room::fold(&self.keys.name, core.log.sorted());
                let welcomed = core.welcomed;
                drop(core);
                if welcomed {
                    self.welcome();
                }
            }
        }
    }

    /// Make our own operation: sign, store, fold, send to everyone.
    fn create_op(&self, core: &mut Core, body: Body) -> Result<Effect, String> {
        let seq = core.log.head(&self.me) + 1;
        let ts = core.tick();
        let op = Op::new(&self.secret, &self.keys, seq, ts, &core.me.nick, &body);
        match core.log.offer(&op) {
            Offer::Added => {}
            Offer::WriteFailed(msg) => return Err(msg),
            other => return Err(format!("cannot append local operation {seq}: {other:?}")),
        }
        let (events, effect) = core.room.apply(&op);
        self.emit(core, events);
        core.broadcast(&Frame::Ops { ops: vec![op] }, None);
        Ok(effect)
    }

    fn set_presence(&self, core: &mut Core, change: impl FnOnce(&mut PeerState)) {
        let before = core.me.clone();
        change(&mut core.me);
        if core.me != before {
            core.broadcast(&Frame::State { state: core.me.clone() }, None);
            if let Some(user) = core.user(&self.me, &self.me) {
                self.emit(core, [ServerMsg::UserUpdate { user }]);
            }
        }
    }

    // ---------------------------------------------------------- local client

    fn handle_client(self: &Arc<Self>, msg: ClientMsg) {
        let mut core = self.lock();
        if core.stopped || !core.welcomed {
            return;
        }
        let owner = core.room.owner() == Some(self.me);
        let mut effect = Ok(Effect::None);
        match msg {
            ClientMsg::Send { channel, text, reply_to, style } => {
                if !core.room.channel(&channel, ChannelKind::Text) {
                    return self.error(&core, ErrorCode::NotFound, "no such text channel");
                }
                let Some(text) = proto::clean_text(&text, proto::MAX_TEXT_CHARS) else { return };
                let reply = reply_to.and_then(|id| core.reply_ref(id));
                let style = style.and_then(|s| s.clean());
                effect = self.create_op(&mut core, Body::Msg { channel, text, reply, file: None, style });
            }

            ClientMsg::Edit { id, text } => {
                let Some(text) = proto::clean_text(&text, proto::MAX_TEXT_CHARS) else { return };
                match core.room.message(id) {
                    None => return self.error(&core, ErrorCode::NotFound, "message not found"),
                    Some((m, author)) if author != self.me || m.system => {
                        return self.error(&core, ErrorCode::Forbidden, "you can only edit your own messages");
                    }
                    Some(_) => effect = self.create_op(&mut core, Body::Edit { id, text }),
                }
            }

            ClientMsg::Delete { id } => match core.room.message(id) {
                None => return self.error(&core, ErrorCode::NotFound, "message not found"),
                Some((_, author)) if author != self.me && !owner => {
                    return self.error(&core, ErrorCode::Forbidden, "you can only delete your own messages");
                }
                Some(_) => effect = self.create_op(&mut core, Body::Delete { id }),
            },

            ClientMsg::React { id, emoji } => {
                let Some(emoji) = clean_emoji(&emoji) else { return };
                let Some((m, _)) = core.room.message(id) else {
                    return self.error(&core, ErrorCode::NotFound, "message not found");
                };
                let on = !core.room.reacted(id, &emoji, &self.me);
                if on && !m.reactions.iter().any(|r| r.emoji == emoji) && core.room.reaction_count(id) >= proto::MAX_REACTIONS {
                    return;
                }
                effect = self.create_op(&mut core, Body::React { id, emoji, on });
            }

            ClientMsg::Typing { channel } => {
                if core.room.channel(&channel, ChannelKind::Text) {
                    core.broadcast(&Frame::Typing { channel }, None);
                }
            }

            ClientMsg::History { channel, before, limit } => {
                let limit = limit.unwrap_or(proto::HISTORY_PAGE).clamp(1, proto::HISTORY_PAGE) as usize;
                let (messages, more) = core.room.history(&channel, before, limit);
                self.emit(&core, [ServerMsg::History { channel, before, messages, more }]);
            }

            ClientMsg::VoiceJoin { channel } => {
                if !core.room.channel(&channel, ChannelKind::Voice) {
                    return self.error(&core, ErrorCode::NotFound, "no such voice channel");
                }
                self.set_presence(&mut core, |s| s.voice = Some(channel));
            }

            ClientMsg::VoiceLeave => {
                core.broadcast(&Frame::Speaking { on: false }, None);
                self.set_presence(&mut core, leave_voice);
            }

            ClientMsg::VoiceState { muted, deafened } => {
                self.set_presence(&mut core, |s| {
                    s.muted = muted || deafened;
                    s.deafened = deafened;
                });
            }

            ClientMsg::Speaking { on } => {
                if core.me.voice.is_some() {
                    core.broadcast(&Frame::Speaking { on }, None);
                }
            }

            ClientMsg::Video { source, watching, stream } => {
                let in_voice = core.me.voice.is_some();
                let stream = stream.and_then(|n| proto::clean_name(&n));
                self.set_presence(&mut core, |s| {
                    s.video = source.filter(|_| in_voice);
                    s.watching = watching && in_voice;
                    s.stream = stream;
                });
            }

            ClientMsg::ChannelCreate { name, kind } => {
                let Some(name) = proto::clean_channel(&name) else {
                    return self.error(&core, ErrorCode::BadRequest, "a channel name needs letters or digits");
                };
                if core.room.channel_like(&name).is_some() {
                    return self.error(&core, ErrorCode::BadRequest, "a channel with this name already exists");
                }
                if core.room.channels().len() >= MAX_CHANNELS {
                    return self.error(&core, ErrorCode::BadRequest, "too many channels already");
                }
                effect = self.create_op(&mut core, Body::ChannelCreate { name, kind });
            }

            ClientMsg::ChannelDelete { name } => {
                if !owner {
                    return self.error(&core, ErrorCode::Forbidden, "only the room owner can delete channels");
                }
                let channels = core.room.channels();
                let Some(target) = channels.iter().find(|c| c.name == name) else {
                    return self.error(&core, ErrorCode::NotFound, "no such channel");
                };
                let texts = channels.iter().filter(|c| c.kind == ChannelKind::Text).count();
                if target.kind == ChannelKind::Text && texts <= 1 {
                    return self.error(&core, ErrorCode::BadRequest, "the last text channel cannot be deleted");
                }
                effect = self.create_op(&mut core, Body::ChannelDelete { name });
                if effect.is_ok() && core.me.voice.as_ref().is_some_and(|v| !core.room.channel(v, ChannelKind::Voice)) {
                    self.set_presence(&mut core, leave_voice);
                }
            }

            ClientMsg::ChannelTopic { name, topic } => {
                if !core.room.channel(&name, ChannelKind::Text) {
                    return self.error(&core, ErrorCode::NotFound, "no such text channel");
                }
                let topic = proto::one_line(&topic, proto::MAX_TOPIC_CHARS);
                effect = self.create_op(&mut core, Body::ChannelTopic { name, topic });
            }

            ClientMsg::Kick { name } => {
                if !owner {
                    return self.error(&core, ErrorCode::Forbidden, "only the room owner can remove people");
                }
                let lower = name.to_lowercase();
                // the exact nick first: "bob" and "Bob" may both be online for a moment
                let online = core
                    .peers
                    .iter()
                    .find(|(_, p)| p.state.nick == name)
                    .or_else(|| core.peers.iter().find(|(_, p)| p.state.nick.to_lowercase() == lower))
                    .map(|(k, _)| *k);
                let target = online.or_else(|| core.room.nick_holder(&name, &self.me));
                match target {
                    Some(key) => {
                        let shown = core.peers.get(&key).map_or(name.clone(), |p| p.state.nick.clone());
                        effect = self.create_op(&mut core, Body::Ban { key: hex(&key), name: shown });
                    }
                    None if lower == core.me.nick.to_lowercase() => {
                        return self.error(&core, ErrorCode::BadRequest, "you cannot remove yourself");
                    }
                    None => return self.error(&core, ErrorCode::NotFound, "nobody with this nick in the room"),
                }
            }
        }
        match effect {
            Ok(effect) => {
                drop(core);
                self.on_effect(effect);
            }
            Err(msg) => self.error(&core, ErrorCode::Storage, &msg),
        }
    }

    // ------------------------------------------------------------------ files

    async fn upload(
        self: &Arc<Self>,
        path: &Path,
        channel: &str,
        text: &str,
        reply_to: Option<u64>,
        mut progress: impl FnMut(u64, u64) + Send,
    ) -> Result<(), String> {
        if !self.lock().room.channel(channel, ChannelKind::Text) {
            return Err("no such text channel".into());
        }
        let meta = tokio::fs::metadata(path).await.map_err(|_| format!("file not found: {}", path.display()))?;
        if meta.is_dir() {
            return Err(format!("this is a folder, not a file: {}", path.display()));
        }
        let total = meta.len();
        if total == 0 {
            return Err("the file is empty".into());
        }
        if total > MAX_FILE {
            return Err(format!("the file is too big: {} MB, the limit is {} MB", total >> 20, MAX_FILE >> 20));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());

        // copy into the store while hashing: the id is the content hash
        let part = self.files_dir.join(format!(".upload-{}.part", hex(&random::<8>())));
        let copied: Result<String, String> = async {
            let mut src = tokio::fs::File::open(path).await.map_err(|e| format!("cannot open: {e}"))?;
            let mut dst = tokio::fs::File::create(&part).await.map_err(|e| format!("cannot write: {e}"))?;
            let mut hasher = blake3::Hasher::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut done = 0u64;
            let mut last = Instant::now();
            loop {
                let n = src.read(&mut buf).await.map_err(|e| format!("cannot read: {e}"))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                dst.write_all(&buf[..n]).await.map_err(|e| format!("cannot write: {e}"))?;
                done += n as u64;
                if last.elapsed() > Duration::from_millis(100) {
                    last = Instant::now();
                    progress(done, total);
                }
            }
            dst.flush().await.map_err(|e| format!("cannot write: {e}"))?;
            progress(total, total);
            Ok(hex(&hasher.finalize().as_bytes()[..16]))
        }
        .await;
        let id = match copied {
            Ok(id) => id,
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e);
            }
        };
        let stored = self.files_dir.join(&id);
        if tokio::fs::metadata(&stored).await.is_ok_and(|m| m.len() == total) {
            let _ = tokio::fs::remove_file(&part).await;
        } else {
            tokio::fs::rename(&part, &stored).await.map_err(|e| format!("cannot store the file: {e}"))?;
        }

        let mut core = self.lock();
        if !core.room.channel(channel, ChannelKind::Text) {
            return Err("the channel was deleted meanwhile".into());
        }
        let text = proto::clean_text(text, proto::MAX_TEXT_CHARS).unwrap_or_default();
        let reply = reply_to.and_then(|id| core.reply_ref(id));
        let image = proto::is_image_name(&name);
        let file = FileMeta { id, name, size: total, image };
        self.create_op(&mut core, Body::Msg { channel: channel.to_string(), text, reply, file: Some(file), style: None })?;
        Ok(())
    }

    /// Connections to ask for a file: its author first, then everyone else.
    fn holders(&self, file_id: &str) -> Vec<Connection> {
        let core = self.lock();
        let author = core.room.file_author(file_id);
        let mut peers: Vec<(&Key, &Peer)> = core.peers.iter().collect();
        peers.sort_by_key(|(key, _)| Some(**key) != author);
        peers.into_iter().map(|(_, p)| p.conn.clone()).collect()
    }

    async fn fetch_file(self: &Arc<Self>, file: &FileMeta, mut progress: impl FnMut(u64, u64) + Send) -> Result<PathBuf, String> {
        if !valid_file_id(&file.id) {
            return Err("bad file id".into());
        }
        let path = self.files_dir.join(&file.id);
        if tokio::fs::metadata(&path).await.is_ok_and(|m| m.len() == file.size) {
            return Ok(path);
        }
        let holders = self.holders(&file.id);
        if holders.is_empty() {
            return Err("nobody who has this file is online".into());
        }
        let mut last_error = String::new();
        for conn in holders {
            match self.download(&conn, file, &path, &mut progress).await {
                Ok(()) => return Ok(path),
                Err(e) => last_error = e,
            }
        }
        Err(format!("could not get the file: {last_error}"))
    }

    async fn download(&self, conn: &Connection, file: &FileMeta, path: &Path, progress: &mut (impl FnMut(u64, u64) + Send)) -> Result<(), String> {
        let net = |e: &dyn std::fmt::Display| format!("network: {e}");
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| net(&e))?;
        send.write_all(&[TAG_FILE]).await.map_err(|e| net(&e))?;
        write_frame(&mut send, &Frame::FileReq { id: file.id.clone() }).await.map_err(|e| net(&e))?;
        let _ = send.finish();
        let size = match read_frame(&mut recv).await.map_err(|e| net(&e))? {
            Some(Frame::Blob { size, .. }) if size <= MAX_FILE => size,
            Some(Frame::Blob { .. }) => return Err("the file is too big".into()),
            _ => return Err("the peer does not have it".into()),
        };
        let part = path.with_extension(format!("{}.part", hex(&random::<4>())));
        let result: Result<(), String> = async {
            let mut out = tokio::fs::File::create(&part).await.map_err(|e| format!("cannot write: {e}"))?;
            let mut hasher = blake3::Hasher::new();
            let mut got = 0u64;
            let mut buf = vec![0u8; 64 * 1024];
            let mut last = Instant::now();
            while got < size {
                let want = buf.len().min((size - got) as usize);
                let n = match recv.read(&mut buf[..want]).await.map_err(|e| net(&e))? {
                    Some(n) => n,
                    None => return Err("the peer stopped sending".into()),
                };
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n]).await.map_err(|e| format!("cannot write: {e}"))?;
                got += n as u64;
                if last.elapsed() > Duration::from_millis(100) {
                    last = Instant::now();
                    progress(got, size);
                }
            }
            out.flush().await.map_err(|e| format!("cannot write: {e}"))?;
            if hex(&hasher.finalize().as_bytes()[..16]) != file.id {
                return Err("the file arrived damaged".into());
            }
            progress(size, size);
            Ok(())
        }
        .await;
        match result {
            Ok(()) => tokio::fs::rename(&part, path).await.map_err(|e| format!("cannot store the file: {e}")),
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(e)
            }
        }
    }

    /// Answer a file request from a peer (anything else gets `missing`).
    async fn serve(&self, mut send: SendStream, mut recv: RecvStream) -> anyhow::Result<()> {
        let mut tag = [0u8; 1];
        recv.read_exact(&mut tag).await?;
        let frame = timeout(HELLO_TIMEOUT, read_frame(&mut recv)).await??;
        let path = match (tag[0], frame) {
            (TAG_FILE, Some(Frame::FileReq { id })) if valid_file_id(&id) => Some(self.files_dir.join(id)),
            _ => None,
        };
        let file = match path {
            Some(path) => tokio::fs::File::open(path).await.ok(),
            None => None,
        };
        let Some(mut file) = file else {
            write_frame(&mut send, &Frame::Missing).await?;
            send.finish()?;
            return Ok(());
        };
        let size = file.metadata().await?.len();
        write_frame(&mut send, &Frame::Blob { size }).await?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            send.write_all(&buf[..n]).await?;
        }
        send.finish()?;
        // let the bytes leave before the stream is dropped
        let _ = timeout(Duration::from_secs(30), send.stopped()).await;
        Ok(())
    }
}

/// Nicks are told apart without case.
fn same_nick(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Out of the voice channel: no video either.
fn leave_voice(state: &mut PeerState) {
    state.voice = None;
    state.video = None;
    state.watching = false;
}

/// Our video to one peer, a stream per frame: the newest frame each time a
/// lane frees up. Ends when the peer is gone (its sender is dropped).
async fn video_writer(conn: Connection, mut frames: watch::Receiver<Option<Bytes>>) {
    let lanes = Arc::new(Semaphore::new(VIDEO_IN_FLIGHT));
    while frames.changed().await.is_ok() {
        let Ok(lane) = lanes.clone().acquire_owned().await else { return };
        // frames that came while waiting for the lane are skipped: take the newest
        let Some(frame) = frames.borrow_and_update().clone() else { continue };
        let conn = conn.clone();
        tokio::spawn(async move {
            let _lane = lane;
            let send = async {
                let mut send = conn.open_uni().await.ok()?;
                send.write_all(&[TAG_VIDEO]).await.ok()?;
                send.write_all(&frame).await.ok()?;
                send.finish().ok()?;
                // the lane is free once the peer has it all
                send.stopped().await.ok()
            };
            let _ = timeout(VIDEO_SEND_TIMEOUT, send).await;
        });
    }
}

async fn writer(mut send: SendStream, mut rx: mpsc::UnboundedReceiver<Frame>) {
    while let Some(frame) = rx.recv().await {
        if write_frame(&mut send, &frame).await.is_err() {
            break;
        }
    }
    let _ = send.finish();
}
