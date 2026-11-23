//! Real nodes in one process, talking over loopback QUIC. The room list is an
//! in-process [`LocalNet`] instead of the DHT; everything else is the real thing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::timeout;
use tovuer_node::{Config, Event, LocalNet, Network, Node, SecretKey};
use tovuer_proto::{ChannelKind, ClientMsg, ErrorCode, FileMeta, Message, ServerMsg, User};

const WAIT: Duration = Duration::from_secs(15);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tovuer-mesh-{tag}-{}", std::process::id()));
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

type Heard = Arc<Mutex<Vec<(u32, u16, Vec<u8>)>>>;

struct Peer {
    node: Node,
    events: mpsc::UnboundedReceiver<Event>,
    heard: Heard,
    me: User,
    users: Vec<User>,
    admin: bool,
    motd: Option<String>,
    client_version: Option<String>,
}

struct Opts<'a> {
    room: &'a str,
    password: &'a str,
    seed: u8,
    release: Option<PathBuf>,
}

impl<'a> Opts<'a> {
    fn new(seed: u8) -> Self {
        Self { room: "home", password: "", seed, release: None }
    }
}

async fn start(dir: &TempDir, net: &LocalNet, nick: &str, opts: Opts<'_>) -> (Node, mpsc::UnboundedReceiver<Event>, Heard) {
    let (tx, rx) = mpsc::unbounded_channel();
    let heard: Heard = Arc::default();
    let sink = {
        let heard = heard.clone();
        Arc::new(move |id: u32, seq: u16, _end: bool, data: &[u8]| heard.lock().unwrap().push((id, seq, data.to_vec())))
    };
    let home = dir.0.join(format!("peer-{}", opts.seed));
    let cfg = Config {
        room: opts.room.into(),
        password: opts.password.into(),
        nick: nick.into(),
        version: "test".into(),
        secret: SecretKey::from_bytes(&[opts.seed; 32]),
        data_dir: home.join("rooms"),
        files_dir: home.join("files"),
        release_dir: opts.release,
        network: Network::Local(net.clone()),
    };
    let node = Node::start(cfg, tx, sink).await.unwrap();
    (node, rx, heard)
}

async fn join(dir: &TempDir, net: &LocalNet, nick: &str, opts: Opts<'_>) -> Peer {
    let (node, mut events, heard) = start(dir, net, nick, opts).await;
    let welcome = next(&mut events, |e| matches!(e, Event::Msg(m) if matches!(**m, ServerMsg::Welcome { .. })) || matches!(e, Event::Rejected { .. })).await;
    let Event::Msg(msg) = welcome else { panic!("{nick} was not let in: {welcome:?}") };
    let ServerMsg::Welcome { id, name, users, admin, motd, client_version, .. } = *msg else { unreachable!() };
    let me = users.iter().find(|u| u.id == id).cloned().unwrap();
    assert_eq!(name, nick);
    Peer { node, events, heard, me, users, admin, motd, client_version }
}

async fn next(events: &mut mpsc::UnboundedReceiver<Event>, want: impl Fn(&Event) -> bool) -> Event {
    timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("node stopped");
            if want(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for an event")
}

impl Peer {
    async fn server(&mut self, want: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
        match next(&mut self.events, |e| matches!(e, Event::Msg(m) if want(m))).await {
            Event::Msg(m) => *m,
            _ => unreachable!(),
        }
    }

    async fn message(&mut self, text: &str) -> Message {
        match self.server(|m| matches!(m, ServerMsg::Msg { msg } if msg.text == text)).await {
            ServerMsg::Msg { msg } => msg,
            _ => unreachable!(),
        }
    }

    async fn joined(&mut self, nick: &str) -> User {
        match self.server(|m| matches!(m, ServerMsg::UserJoin { user } if user.name == nick)).await {
            ServerMsg::UserJoin { user } => user,
            _ => unreachable!(),
        }
    }

    async fn history(&mut self, channel: &str) -> Vec<Message> {
        self.node.send(ClientMsg::History { channel: channel.into(), before: None, limit: None });
        match self.server(|m| matches!(m, ServerMsg::History { channel: c, .. } if c == channel)).await {
            ServerMsg::History { messages, .. } => messages.into_iter().filter(|m| !m.system).collect(),
            _ => unreachable!(),
        }
    }

    fn say(&self, text: &str) {
        self.node.send(ClientMsg::Send { channel: "general".into(), text: text.into(), reply_to: None });
    }
}

#[tokio::test]
async fn chat_between_peers_and_history_for_latecomers() {
    let (dir, net) = (TempDir::new("chat"), LocalNet::new());
    let mut dana = join(&dir, &net, "dana", Opts::new(1)).await;
    assert!(dana.admin, "whoever opens an empty room owns it");
    assert!(dana.motd.as_deref().unwrap_or("").contains("created"));

    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    assert!(!bob.admin);
    assert!(bob.users.iter().any(|u| u.name == "dana" && u.admin));
    dana.joined("bob").await;

    dana.say("hi bob");
    let got = bob.message("hi bob").await;
    assert_eq!(got.from, "dana");
    let mine = dana.message("hi bob").await;
    assert_eq!(mine.id, got.id, "everyone sees the same id");

    bob.node.send(ClientMsg::Send { channel: "general".into(), text: "hey".into(), reply_to: Some(got.id) });
    let reply = dana.message("hey").await;
    assert_eq!(reply.reply.as_ref().map(|r| r.text.as_str()), Some("hi bob"));

    bob.node.send(ClientMsg::React { id: got.id, emoji: "🔥".into() });
    match dana.server(|m| matches!(m, ServerMsg::MsgReact { .. })).await {
        ServerMsg::MsgReact { reactions, .. } => assert_eq!(reactions[0].users, vec!["bob"]),
        _ => unreachable!(),
    }
    dana.node.send(ClientMsg::Edit { id: got.id, text: "hi bob!".into() });
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::MsgEdit { .. })).await, ServerMsg::MsgEdit { text, .. } if text == "hi bob!"));

    // bob may not edit someone else's message
    bob.node.send(ClientMsg::Edit { id: got.id, text: "hacked".into() });
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::Error { .. })).await, ServerMsg::Error { code: ErrorCode::Forbidden, .. }));

    // kate comes later and gets the whole history from whoever is online
    let mut kate = join(&dir, &net, "kate", Opts::new(3)).await;
    assert_eq!(kate.users.len(), 3);
    let texts: Vec<String> = kate.history("general").await.into_iter().map(|m| m.text).collect();
    assert_eq!(texts, vec!["hi bob!", "hey"]);

    // dana leaves; bob and kate still talk to each other directly
    dana.node.shutdown().await;
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::UserLeave { .. })).await, ServerMsg::UserLeave { id } if id == bob.users.iter().find(|u| u.name == "dana").unwrap().id));
    kate.say("still here?");
    bob.message("still here?").await;
}

#[tokio::test]
async fn offline_messages_arrive_later_and_history_survives_restarts() {
    let (dir, net) = (TempDir::new("offline"), LocalNet::new());
    let mut dana = join(&dir, &net, "dana", Opts::new(1)).await;
    let bob = join(&dir, &net, "bob", Opts::new(2)).await;
    dana.joined("bob").await;
    bob.node.shutdown().await;

    // nobody else is online: the message waits in dana's copy of the room
    dana.say("while you were away");
    dana.message("while you were away").await;

    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    let texts: Vec<String> = bob.history("general").await.into_iter().map(|m| m.text).collect();
    assert_eq!(texts, vec!["while you were away"]);
    bob.node.shutdown().await;
    dana.node.shutdown().await;

    // everyone is gone; bob opens the room alone and still has it all
    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    assert!(bob.motd.as_deref().unwrap_or("").contains("Nobody else"));
    assert!(!bob.admin, "dana still owns the room");
    let texts: Vec<String> = bob.history("general").await.into_iter().map(|m| m.text).collect();
    assert_eq!(texts, vec!["while you were away"]);
}

#[tokio::test]
async fn rooms_are_separated_by_name_and_password() {
    let (dir, net) = (TempDir::new("rooms"), LocalNet::new());
    let dana = join(&dir, &net, "dana", Opts { password: "secret", ..Opts::new(1) }).await;
    let other = join(&dir, &net, "bob", Opts::new(2)).await;
    assert!(other.admin, "no password — a different room, bob made it");
    assert_eq!(other.users.len(), 1);
    let friend = join(&dir, &net, "kate", Opts { password: "secret", ..Opts::new(3) }).await;
    assert!(friend.users.iter().any(|u| u.name == "dana"));
    assert!(!friend.users.iter().any(|u| u.name == "bob"));
    drop(dana);
}

#[tokio::test]
async fn nick_taken_and_kick() {
    let (dir, net) = (TempDir::new("kick"), LocalNet::new());
    let mut dana = join(&dir, &net, "dana", Opts::new(1)).await;
    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    dana.joined("bob").await;

    let (_node, mut events, _) = start(&dir, &net, "Bob", Opts::new(3)).await;
    match next(&mut events, |e| matches!(e, Event::Rejected { .. })).await {
        Event::Rejected { code, .. } => assert_eq!(code, ErrorCode::NameTaken),
        _ => unreachable!(),
    }

    bob.node.send(ClientMsg::Kick { name: "dana".into() });
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::Error { .. })).await, ServerMsg::Error { code: ErrorCode::Forbidden, .. }));

    dana.node.send(ClientMsg::Kick { name: "bob".into() });
    match next(&mut bob.events, |e| matches!(e, Event::Rejected { .. })).await {
        Event::Rejected { code, .. } => assert_eq!(code, ErrorCode::Kicked),
        _ => unreachable!(),
    }
    dana.server(|m| matches!(m, ServerMsg::UserLeave { .. })).await;

    // bob cannot come back with the same identity
    let (_node, mut events, _) = start(&dir, &net, "bob", Opts::new(2)).await;
    match next(&mut events, |e| matches!(e, Event::Rejected { .. })).await {
        Event::Rejected { code, .. } => assert_eq!(code, ErrorCode::Kicked),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn voice_goes_straight_to_the_same_channel() {
    let (dir, net) = (TempDir::new("voice"), LocalNet::new());
    let mut dana = join(&dir, &net, "dana", Opts::new(1)).await;
    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    let mut kate = join(&dir, &net, "kate", Opts::new(3)).await;
    dana.joined("kate").await;

    dana.node.send(ClientMsg::VoiceJoin { channel: "Lounge".into() });
    bob.node.send(ClientMsg::VoiceJoin { channel: "Lounge".into() });
    kate.node.send(ClientMsg::VoiceJoin { channel: "Gaming".into() });
    // everyone sees where everyone is
    for peer in [&mut dana, &mut kate] {
        peer.server(|m| matches!(m, ServerMsg::UserUpdate { user } if user.name == "bob" && user.voice.as_deref() == Some("Lounge"))).await;
    }
    bob.server(|m| matches!(m, ServerMsg::UserUpdate { user } if user.name == "dana" && user.voice.as_deref() == Some("Lounge"))).await;

    let dana_on_bob = bob.users.iter().find(|u| u.name == "dana").unwrap().id;
    let frame = tovuer_proto::voice::encode(0, 0, 7, &[1, 2, 3, 4]);
    timeout(WAIT, async {
        loop {
            dana.node.voice(bytes::Bytes::from(frame.clone()));
            if !bob.heard.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("bob never heard dana");
    assert_eq!(bob.heard.lock().unwrap()[0], (dana_on_bob, 7, vec![1, 2, 3, 4]));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(kate.heard.lock().unwrap().is_empty(), "another channel hears nothing");

    dana.node.send(ClientMsg::Speaking { on: true });
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::Speaking { .. })).await, ServerMsg::Speaking { id, on: true } if id == dana_on_bob));
    let _ = dana.me;
}

#[tokio::test]
async fn files_come_from_whoever_has_them() {
    let (dir, net) = (TempDir::new("files"), LocalNet::new());
    let mut dana = join(&dir, &net, "dana", Opts::new(1)).await;
    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    dana.joined("bob").await;

    let source = dir.0.join("cat.png");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&source, &data).unwrap();
    dana.node.upload(&source, "general", "look", None, |_, _| {}).await.unwrap();
    let msg = match bob.server(|m| matches!(m, ServerMsg::Msg { msg } if msg.file.is_some())).await {
        ServerMsg::Msg { msg } => msg,
        _ => unreachable!(),
    };
    let file: FileMeta = msg.file.unwrap();
    assert!(file.image && file.name == "cat.png" && file.size == data.len() as u64);
    assert_eq!(msg.text, "look");

    let path = bob.node.fetch_file(&file, |_, _| {}).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), data);

    // dana goes away; kate gets the file from bob, who now has a copy
    dana.node.shutdown().await;
    let kate = join(&dir, &net, "kate", Opts::new(3)).await;
    let path = kate.node.fetch_file(&file, |_, _| {}).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), data);

    let bogus = FileMeta { id: "0".repeat(32), name: "x".into(), size: 1, image: false };
    assert!(kate.node.fetch_file(&bogus, |_, _| {}).await.is_err());
    assert!(bob.node.upload(&dir.0.join("nope"), "general", "", None, |_, _| {}).await.is_err());
}

#[tokio::test]
async fn channels_and_owner_powers() {
    let (dir, net) = (TempDir::new("channels"), LocalNet::new());
    let mut dana = join(&dir, &net, "dana", Opts::new(1)).await;
    let mut bob = join(&dir, &net, "bob", Opts::new(2)).await;
    dana.joined("bob").await;

    bob.node.send(ClientMsg::ChannelCreate { name: "memes".into(), kind: ChannelKind::Text });
    let has_memes = |m: &ServerMsg| matches!(m, ServerMsg::Channels { channels } if channels.iter().any(|c| c.name == "memes"));
    dana.server(has_memes).await;
    bob.node.send(ClientMsg::ChannelDelete { name: "memes".into() });
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::Error { .. })).await, ServerMsg::Error { code: ErrorCode::Forbidden, .. }));
    dana.node.send(ClientMsg::ChannelDelete { name: "memes".into() });
    bob.server(|m| matches!(m, ServerMsg::Channels { channels } if !channels.iter().any(|c| c.name == "memes"))).await;

    // a voice channel too, with bob inside: when the owner deletes it, bob is
    // out of it for everyone — no ghost lingering in a channel that is gone
    bob.node.send(ClientMsg::ChannelCreate { name: "party".into(), kind: ChannelKind::Voice });
    dana.server(|m| matches!(m, ServerMsg::Channels { channels } if channels.iter().any(|c| c.name == "party" && c.kind == ChannelKind::Voice))).await;
    bob.node.send(ClientMsg::VoiceJoin { channel: "party".into() });
    dana.server(|m| matches!(m, ServerMsg::UserUpdate { user } if user.name == "bob" && user.voice.as_deref() == Some("party"))).await;
    dana.node.send(ClientMsg::ChannelDelete { name: "party".into() });
    bob.server(|m| matches!(m, ServerMsg::Channels { channels } if !channels.iter().any(|c| c.name == "party"))).await;
    bob.server(|m| matches!(m, ServerMsg::UserUpdate { user } if user.name == "bob" && user.voice.is_none())).await;
    dana.server(|m| matches!(m, ServerMsg::UserUpdate { user } if user.name == "bob" && user.voice.is_none())).await;

    bob.say("oops");
    let oops = dana.message("oops").await;
    dana.node.send(ClientMsg::Delete { id: oops.id });
    assert!(matches!(bob.server(|m| matches!(m, ServerMsg::MsgDelete { .. })).await, ServerMsg::MsgDelete { id, .. } if id == oops.id));
}

#[tokio::test]
async fn update_comes_from_the_owner() {
    let (dir, net) = (TempDir::new("update"), LocalNet::new());
    let release = dir.0.join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(release.join("VERSION"), "2026-11-22\n").unwrap();
    std::fs::write(release.join("tovuer-linux-amd64"), b"\x7fELF new build").unwrap();
    let _dana = join(&dir, &net, "dana", Opts { release: Some(release), ..Opts::new(1) }).await;
    let bob = join(&dir, &net, "bob", Opts::new(2)).await;
    assert_eq!(bob.client_version.as_deref(), Some("2026-11-22"));

    let (version, data) = bob.node.fetch_update("tovuer-linux-amd64", "old", |_, _| {}).await.unwrap().unwrap();
    assert_eq!((version.as_str(), data.as_slice()), ("2026-11-22", &b"\x7fELF new build"[..]));
    assert_eq!(bob.node.fetch_update("tovuer-linux-amd64", "2026-11-22", |_, _| {}).await, Ok(None));
    assert!(bob.node.fetch_update("tovuer-windows-amd64.exe", "old", |_, _| {}).await.is_err());
    assert!(bob.node.fetch_update("../../etc/passwd", "old", |_, _| {}).await.is_err());
}
