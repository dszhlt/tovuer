//! The client without a network: fills the app with believable data through
//! the same events the node sends. Used by interface tests and screenshots
//! (`cargo run -p tovuer --example snapshot`).

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tovuer_proto::{Channel, FileMeta, Message, Reaction, ReplyRef, ServerMsg, TextStyle, User};
use tovuer_voice::{Engine, StreamInfo};

use crate::app::overlay::{Overlay, Picker, SettingsView, Viewer};
use crate::app::{App, Launch, ThumbState, VoiceSession};
use crate::net::{NetCmd, NetEvent, NetHandle};
use crate::settings::Paths;
use crate::transfer::{Picture, TaskEvent};

/// An app connected to nothing. Also returns the "other ends": the commands
/// it sends to the network and the queue of background events.
pub fn blank(dir: &Path, launch: Launch) -> (App, mpsc::UnboundedReceiver<NetCmd>, mpsc::UnboundedReceiver<TaskEvent>) {
    let (net, sent) = NetHandle::recorded();
    let (tasks, task_rx) = mpsc::unbounded_channel();
    let engine = Arc::new(Engine::new(|_| {}));
    let app = App::new(launch, Paths::at(dir), engine, net, tasks);
    (app, sent, task_rx)
}

fn server(app: &mut App, msg: ServerMsg) {
    app.on_net(NetEvent::Msg(Box::new(msg)));
}

fn user(id: u32, name: &str, voice: Option<&str>) -> User {
    User { id, name: name.into(), voice: voice.map(str::to_string), muted: false, deafened: false, admin: id == 1 }
}

/// This afternoon minus `minutes_ago` — so date and time labels stay stable.
fn at(minutes_ago: i64) -> u64 {
    let noon = chrono::Local::now().date_naive().and_hms_opt(14, 30, 0).unwrap();
    let local = noon.and_local_timezone(chrono::Local).single().unwrap_or_else(chrono::Local::now);
    (local.timestamp_millis() - minutes_ago * 60_000) as u64
}

fn msg(id: u64, channel: &str, from: &str, minutes_ago: i64, text: &str) -> Message {
    Message {
        id,
        channel: channel.into(),
        from: from.into(),
        ts: at(minutes_ago),
        text: text.into(),
        reply: None,
        edited: None,
        file: None,
        reactions: Vec::new(),
        system: false,
        style: None,
    }
}

/// A message in someone's own style.
fn styled(style: TextStyle, m: Message) -> Message {
    Message { style: Some(style), ..m }
}

fn gradient(name: &str) -> TextStyle {
    let mut style = TextStyle { flow: true, ..TextStyle::default() };
    let _ = crate::app::style::set_gradient(&mut style, name);
    style
}

/// A stand-in picture: a sunset over the sea, drawn by a formula.
pub fn picture(w: u32, h: u32) -> Picture {
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let sun = ((fx - 0.5).powi(2) * 2.2 + (fy - 0.52).powi(2) * 5.0).sqrt();
            let glow = (1.0 - sun * 2.4).clamp(0.0, 1.0);
            let (r, g, b) = if fy < 0.55 {
                (40.0 + 200.0 * glow + 90.0 * fy, 30.0 + 110.0 * glow + 60.0 * fy, 90.0 + 40.0 * glow - 30.0 * fy)
            } else {
                let wave = ((fx * 40.0 + fy * 90.0).sin() * 0.5 + 0.5) * 30.0;
                (20.0 + 150.0 * glow + wave, 30.0 + 60.0 * glow + wave * 0.6, 70.0 + 30.0 * glow)
            };
            rgb.extend_from_slice(&[r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8]);
        }
    }
    Picture { w, h, rgb, full: (w, h) }
}

pub const IMAGE_ID: &str = "0123456789abcdef0123456789abcdef";

/// Fill the app: room "home", four people online, a lively conversation.
pub fn populate(app: &mut App) {
    app.room = "home".into();
    app.screen = crate::app::Screen::Main;
    server(
        app,
        ServerMsg::Welcome {
            id: 1,
            name: "dana".into(),
            server: "home".into(),
            version: crate::VERSION.into(),
            proto: tovuer_proto::PROTO_VERSION,
            admin: true,
            channels: vec![
                Channel::text("general", "chat about anything"),
                Channel::text("random", "memes, links, pictures"),
                Channel::text("games", ""),
                Channel::voice("Lounge"),
                Channel::voice("Gaming"),
            ],
            users: vec![user(1, "dana", None), user(2, "bob", Some("Lounge")), user(3, "kate", Some("Lounge")), user(4, "oleg", None)],
            max_file: 50 << 20,
            client_version: None,
            motd: None,
        },
    );

    let image = FileMeta { id: IMAGE_ID.into(), name: "sunset.png".into(), size: 1_843_200, image: true };
    let mut history = vec![
        Message { system: true, ..msg(1, "general", "tovuer", 60 * 26, "dana created the room") },
        msg(2, "general", "bob", 60 * 25, "hi all! finally a proper chat in the terminal"),
        msg(3, "general", "kate", 60 * 25 - 2, "oh, there is even **markup** here and `code`"),
        msg(4, "general", "bob", 95, "who is in Lounge tonight? want to show you the new map"),
        msg(5, "general", "bob", 94, "meeting at nine"),
        Message {
            reply: Some(ReplyRef { id: 4, from: "bob".into(), text: "who is in Lounge tonight? want to show you the new map".into() }),
            reactions: vec![Reaction { emoji: "👍".into(), users: vec!["bob".into(), "dana".into()] }, Reaction { emoji: "🔥".into(), users: vec!["oleg".into()] }],
            ..msg(6, "general", "kate", 80, "I'm in! just finishing a report")
        },
        Message { edited: Some(at(40)), ..msg(7, "general", "dana", 41, "me too. by the way, there is no server now — the room lives on all of us") },
        Message { file: Some(image), ..msg(8, "general", "oleg", 30, "look what I shot yesterday") },
        styled(gradient("candy"), msg(9, "general", "kate", 28, "gorgeous 😍 where is that?")),
        msg(10, "general", "oleg", 27, "by the bay, the rest goes here: https://example.com/album/2026"),
        msg(11, "general", "bob", 12, "@dana could you check why the build fails:\n```\nerror[E0599]: no method named `fit` found\n  --> src/ui/chat.rs:42:18\n```"),
    ];
    history.push(styled(TextStyle { bold: true, ..gradient("aurora") }, msg(12, "general", "dana", 3, "on it, it's a typo — fixing now")));
    server(app, ServerMsg::History { before: None, channel: "general".into(), messages: history, more: false });
    server(
        app,
        ServerMsg::History { before: None, channel: "random".into(), messages: vec![msg(20, "random", "kate", 200, "first message in random")], more: false },
    );
    server(app, ServerMsg::History { before: None, channel: "games".into(), messages: Vec::new(), more: false });

    app.thumbs.insert(
        IMAGE_ID.into(),
        ThumbState::Ready(Arc::new(picture(480, 270).fit(crate::transfer::THUMB_COLS, crate::transfer::THUMB_ROWS))),
    );
    // new messages in the next channel — unread counters appear
    for (id, text) in [(21, "found something funny"), (22, "@dana this one is about you 😄"), (23, "and one more")] {
        server(app, ServerMsg::Msg { msg: msg(id, "random", "bob", 1, text) });
    }
    server(app, ServerMsg::Speaking { id: 2, on: true });
    server(app, ServerMsg::Typing { channel: "general".into(), name: "kate".into() });
    app.on_net(NetEvent::Latency(Some(std::time::Duration::from_millis(23))));
    app.toasts.clear();
    app.dirty = true;
}

/// Pretend we are in a voice channel.
pub fn in_voice(app: &mut App, channel: &str) {
    if let Some(me) = app.users.iter().find(|u| u.id == 1).cloned() {
        server(app, ServerMsg::UserUpdate { user: User { voice: Some(channel.into()), ..me } });
    }
    app.voice = Some(VoiceSession {
        channel: channel.into(),
        since: Instant::now() - std::time::Duration::from_secs(754),
        starting: false,
        info: Some(StreamInfo {
            host: "PulseAudio".into(),
            mic: Some("USB PnP Audio Device Mono".into()),
            spk: Some("Built-in Audio Analog Stereo".into()),
            mic_rate: 48_000,
            spk_rate: 48_000,
            warnings: Vec::new(),
        }),
    });
}

/// A named scene for a screenshot. An unknown name gives the main screen.
pub fn scene(app: &mut App, name: &str) {
    if name == "login" {
        return;
    }
    populate(app);
    match name {
        "voice" => in_voice(app, "Lounge"),
        "select" => {
            in_voice(app, "Lounge");
            app.set_focus(crate::app::Focus::Messages);
            app.select_message(-4);
        }
        "palette" => app.open_palette(),
        "help" => app.overlay = Some(Overlay::Help { scroll: 0 }),
        "settings" => app.overlay = Some(Overlay::Settings(SettingsView { sel: 0, mics: Some(Vec::new()), spks: Some(Vec::new()) })),
        "style" => {
            app.settings.style = TextStyle { bold: true, ..gradient("aurora") };
            app.open_style();
            app.frame = 6;
        }
        "users" => app.overlay = Some(Overlay::Users { sel: 0 }),
        "react" => app.overlay = Some(Overlay::React { msg: 9, sel: 3 }),
        "picker" => app.overlay = Some(Overlay::Picker(Picker::open(&std::env::temp_dir()))),
        "viewer" => {
            let file = FileMeta { id: IMAGE_ID.into(), name: "sunset.png".into(), size: 1_843_200, image: true };
            app.overlay = Some(Overlay::Viewer(Viewer { pic: Some(Arc::new(picture(480, 270))), ..Viewer::new(file) }));
        }
        "typing" => {
            app.input.set("/th");
            app.completion = app.suggest();
        }
        "toast" => {
            app.ok("sent: sunset.png");
            app.warn("lost the room: a peer stopped answering — reconnecting");
        }
        _ => {}
    }
}
