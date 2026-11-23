//! The client's network task against real peers in the same process: real
//! nodes over loopback QUIC, rooms found through an in-process list instead
//! of the DHT. Chat, voice frames into the audio engine, files with previews,
//! updates from the room owner.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;
use tovuer::net::{self, NetConfig, NetEvent, NetHandle, NodeSetup};
use tovuer::settings::Paths;
use tovuer::transfer::{self, Remote, TaskEvent};
use tovuer_node::{LocalNet, Network, SecretKey};
use tovuer_proto::{ClientMsg, ErrorCode, Message, ServerMsg, User, voice};
use tovuer_voice::Engine;
use tovuer_voice::codec::{DEFAULT_BITRATE, Encoder, MAX_PACKET};

const WAIT: Duration = Duration::from_secs(15);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tovuer-live-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A client without the interface: network task, audio engine, event queue.
struct Peer {
    net: NetHandle,
    events: mpsc::UnboundedReceiver<(u64, NetEvent)>,
    engine: Arc<Engine>,
    paths: Paths,
    users: Vec<User>,
}

impl Peer {
    fn spawn(dir: &TempDir, net: &LocalNet, seed: u8, release: Option<PathBuf>) -> Self {
        let engine = Arc::new(Engine::new(|_| {}));
        let (tx, events) = mpsc::unbounded_channel();
        let paths = Paths::at(&dir.0.join(format!("peer-{seed}")));
        let setup = NodeSetup {
            version: "1.0".into(),
            secret: SecretKey::from_bytes(&[seed; 32]),
            data_dir: paths.rooms_dir(),
            files_dir: paths.file_cache(),
            release_dir: release,
            network: Network::Local(net.clone()),
        };
        let net = net::spawn(tx, engine.clone(), setup);
        Self { net, events, engine, paths, users: Vec::new() }
    }

    async fn event(&mut self) -> NetEvent {
        loop {
            let (session, event) = timeout(WAIT, self.events.recv()).await.expect("the network is silent").expect("the network task ended");
            if self.net.is_current(session) {
                return event;
            }
        }
    }

    async fn server_msg<T>(&mut self, mut pick: impl FnMut(ServerMsg) -> Option<T>) -> T {
        loop {
            if let NetEvent::Msg(msg) = self.event().await
                && let Some(found) = pick(*msg)
            {
                return found;
            }
        }
    }

    fn connect(&self, room: &str, name: &str, password: Option<&str>) {
        self.net.connect(NetConfig { room: room.into(), name: name.into(), password: password.map(str::to_string) });
    }

    async fn welcomed(&mut self) {
        self.users = self
            .server_msg(|m| match m {
                ServerMsg::Welcome { users, .. } => Some(users),
                _ => None,
            })
            .await;
    }

    async fn join(dir: &TempDir, net: &LocalNet, seed: u8, name: &str) -> Self {
        let mut peer = Self::spawn(dir, net, seed, None);
        peer.connect("home", name, None);
        peer.welcomed().await;
        peer
    }

    async fn chat_msg(&mut self) -> Message {
        self.server_msg(|m| match m {
            ServerMsg::Msg { msg } if !msg.system => Some(msg),
            _ => None,
        })
        .await
    }

    fn id_of(&self, name: &str) -> u32 {
        self.users.iter().find(|u| u.name == name).map(|u| u.id).expect("no such user")
    }

    fn remote(&self, events: transfer::Events) -> Remote {
        Remote { node: self.net.node().expect("joined"), events, paths: self.paths.clone() }
    }
}

async fn task_event<T>(rx: &mut mpsc::UnboundedReceiver<TaskEvent>, mut pick: impl FnMut(TaskEvent) -> Option<T>) -> T {
    loop {
        let event = timeout(WAIT, rx.recv()).await.expect("the background task is silent").expect("queue closed");
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

#[tokio::test]
async fn chat_and_voice_between_two_clients() {
    let (dir, rooms) = (TempDir::new("chat"), LocalNet::new());
    let mut dana = Peer::join(&dir, &rooms, 1, "dana").await;
    let mut bob = Peer::join(&dir, &rooms, 2, "bob").await;

    dana.net.send(ClientMsg::Send { channel: "general".into(), text: "hello".into(), reply_to: None });
    let got = bob.chat_msg().await;
    assert_eq!((got.from.as_str(), got.text.as_str()), ("dana", "hello"));

    // latency to the other members is measured on its own
    let rtt = timeout(WAIT, async {
        loop {
            if let NetEvent::Latency(Some(rtt)) = bob.event().await {
                return rtt;
            }
        }
    })
    .await
    .expect("no latency measured");
    assert!(rtt < Duration::from_secs(1));

    // voice: real Opus packets go from one engine straight to the other
    for peer in [&dana, &bob] {
        peer.net.send(ClientMsg::VoiceJoin { channel: "Lounge".into() });
    }
    let dana_id = bob.id_of("dana");
    bob.server_msg(|m| matches!(m, ServerMsg::UserUpdate { user } if user.id == dana_id && user.voice.is_some()).then_some(())).await;
    dana.server_msg(|m| matches!(m, ServerMsg::UserUpdate { user } if user.name == "bob" && user.voice.is_some()).then_some(())).await;

    let mut encoder = Encoder::new(DEFAULT_BITRATE).unwrap();
    let mut packet = [0u8; MAX_PACKET];
    for seq in 0..10u16 {
        let mut pcm = [0.0f32; tovuer_voice::FRAME];
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = 0.3 * ((seq as usize * tovuer_voice::FRAME + i) as f32 * 0.06).sin();
        }
        let len = encoder.encode(&pcm, &mut packet).unwrap();
        let flags = if seq == 9 { voice::FLAG_END } else { 0 };
        dana.net.voice(voice::encode(flags, 0, seq, &packet[..len]));
    }
    let queued = timeout(WAIT, async {
        loop {
            let stats = bob.engine.peer_stats();
            if let Some(peer) = stats.iter().find(|p| p.id == dana_id)
                && peer.queued == 10
            {
                return peer.queued;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the frames never reached the other engine");
    assert_eq!(queued, 10);
    assert!(dana.engine.peer_stats().is_empty(), "your own voice does not come back");
}

#[tokio::test]
async fn the_password_is_part_of_the_room() {
    let (dir, rooms) = (TempDir::new("password"), LocalNet::new());
    let mut dana = Peer::spawn(&dir, &rooms, 1, None);
    dana.connect("home", "dana", Some("sesame"));
    dana.welcomed().await;

    let mut bob = Peer::spawn(&dir, &rooms, 2, None);
    bob.connect("home", "bob", Some("wrong"));
    bob.welcomed().await;
    assert_eq!(bob.users.len(), 1, "a wrong password is simply another, empty room");

    // the task leaves that room and joins with the new details
    bob.connect("Home", "bob", Some("sesame"));
    bob.welcomed().await;
    assert!(bob.users.iter().any(|u| u.name == "dana"));
}

#[tokio::test]
async fn a_taken_nick_is_rejected_and_another_works() {
    let (dir, rooms) = (TempDir::new("nick"), LocalNet::new());
    let _dana = Peer::join(&dir, &rooms, 1, "dana").await;
    let mut other = Peer::spawn(&dir, &rooms, 2, None);
    other.connect("home", "DANA", None);
    let code = timeout(WAIT, async {
        loop {
            if let NetEvent::Rejected { code, .. } = other.event().await {
                return code;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, ErrorCode::NameTaken);
    assert!(other.net.node().is_none(), "a rejected node is gone");

    other.connect("home", "dana2", None);
    other.welcomed().await;
    assert_eq!(other.users.len(), 2);
}

#[tokio::test]
async fn files_travel_between_peers_with_previews() {
    let (dir, rooms) = (TempDir::new("files"), LocalNet::new());
    let mut dana = Peer::join(&dir, &rooms, 1, "dana").await;
    let mut bob = Peer::join(&dir, &rooms, 2, "bob").await;
    let (tasks, mut task_rx) = mpsc::unbounded_channel();
    let up = dana.remote(tasks.clone());
    let down = bob.remote(tasks.clone());

    // a 64×32 PNG in two colors
    let mut img = image::RgbImage::new(64, 32);
    for (x, _, px) in img.enumerate_pixels_mut() {
        *px = if x < 32 { image::Rgb([220, 40, 40]) } else { image::Rgb([40, 40, 220]) };
    }
    let picture = dir.0.join("flag.png");
    img.save(&picture).unwrap();
    up.upload(1, picture.clone(), "general".into(), "look".into(), None);
    let error = task_event(&mut task_rx, |e| match e {
        TaskEvent::UploadDone { id: 1, error, .. } => Some(error),
        _ => None,
    })
    .await;
    assert_eq!(error, None);
    let posted = bob.chat_msg().await;
    let file = posted.file.clone().expect("a message with an attachment");
    assert_eq!((posted.text.as_str(), file.name.as_str(), file.image), ("look", "flag.png", true));

    // a thumbnail for the feed: fetched from dana and shrunk
    down.thumb(file.clone());
    let thumb = task_event(&mut task_rx, |e| match e {
        TaskEvent::Thumb { result, .. } => Some(result),
        _ => None,
    })
    .await
    .unwrap();
    assert!(thumb.w <= transfer::THUMB_COLS && thumb.rows() <= transfer::THUMB_ROWS);
    assert_eq!(thumb.at(0, 0), [220, 40, 40]);
    assert_eq!(thumb.at(thumb.w - 1, 0), [40, 40, 220]);
    assert!(bob.paths.file_cache().join(&file.id).is_file(), "bob keeps a copy to share");

    // saving: byte for byte, existing files are not overwritten
    let target = dir.0.join("downloads");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("flag.png"), b"old").unwrap();
    down.save(file.clone(), Some(target.clone()));
    let saved = task_event(&mut task_rx, |e| match e {
        TaskEvent::Saved { result, .. } => Some(result),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(saved, target.join("flag (1).png"));
    assert_eq!(std::fs::read(&saved).unwrap(), std::fs::read(&picture).unwrap());
    assert_eq!(std::fs::read(target.join("flag.png")).unwrap(), b"old");

    // a file over the limit is refused before anything is sent, with a clear reason
    let huge = dir.0.join("huge.bin");
    std::fs::File::create(&huge).unwrap().set_len(tovuer_node::MAX_FILE + 1).unwrap();
    up.upload(2, huge, "general".into(), String::new(), None);
    let error = task_event(&mut task_rx, |e| match e {
        TaskEvent::UploadDone { id: 2, error, .. } => Some(error),
        _ => None,
    })
    .await;
    assert!(error.unwrap().contains("too big"));
    let _ = dana.chat_msg().await;
}

#[tokio::test]
async fn updates_come_from_the_owner_and_never_install_garbage() {
    let (dir, rooms) = (TempDir::new("update"), LocalNet::new());
    let release = dir.0.join("release");
    std::fs::create_dir_all(&release).unwrap();
    let mut owner = Peer::spawn(&dir, &rooms, 1, Some(release.clone()));
    owner.connect("home", "dana", None);
    owner.welcomed().await;
    let bob = Peer::join(&dir, &rooms, 2, "bob").await;
    let (tasks, mut task_rx) = mpsc::unbounded_channel();
    let check = |current: &str| transfer::update(bob.net.node().unwrap(), current.to_string(), tasks.clone());
    async fn done(rx: &mut mpsc::UnboundedReceiver<TaskEvent>) -> Result<Option<(PathBuf, String)>, String> {
        task_event(rx, |e| match e {
            TaskEvent::UpdateDone(result) => Some(result),
            _ => None,
        })
        .await
    }

    // the owner has not released anything
    check("1.0");
    assert!(done(&mut task_rx).await.unwrap_err().contains("no build"));

    // same version — nothing to download
    std::fs::write(release.join("VERSION"), "1.0\n").unwrap();
    std::fs::write(release.join(transfer::asset_name()), b"\x7fELF").unwrap();
    check("1.0");
    assert_eq!(done(&mut task_rx).await, Ok(None));

    // a newer version, but what lies there is not a program — our file is left alone
    std::fs::write(release.join("VERSION"), "2.0").unwrap();
    std::fs::write(release.join(transfer::asset_name()), "<html>502 Bad Gateway</html>".repeat(20_000)).unwrap();
    let me = std::env::current_exe().unwrap();
    let before = std::fs::metadata(&me).unwrap().len();
    check("1.0");
    assert!(done(&mut task_rx).await.unwrap_err().contains("not a program"));
    assert_eq!(std::fs::metadata(&me).unwrap().len(), before, "the executable is untouched");
}
