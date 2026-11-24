//! The whole client without a terminal or a network: press keys, feed node
//! events, and look at what the app sent and what it drew.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedReceiver;
use tovuer::app::overlay::Overlay;
use tovuer::app::{App, Compose, Conn, Effect, Focus, Launch, LoginField, Screen, Tone};
use tovuer::net::{NetCmd, NetConfig, NetEvent};
use tovuer::settings::Paths;
use tovuer::transfer::TaskEvent;
use tovuer::{demo, ui};
use tovuer_proto::{Channel, ChannelKind, ClientMsg, ErrorCode, Message, ServerMsg};
use unicode_width::UnicodeWidthStr;

struct Harness {
    app: App,
    net: UnboundedReceiver<NetCmd>,
    _tasks: UnboundedReceiver<TaskEvent>,
    dir: PathBuf,
}

impl Harness {
    /// A freshly started client: the login screen.
    fn fresh(tag: &str) -> Self {
        Self::with(tag, Launch { version: "test".into(), ..Launch::default() })
    }

    fn with(tag: &str, launch: Launch) -> Self {
        let dir = std::env::temp_dir().join(format!("tovuer-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (app, net, tasks) = demo::blank(&dir, launch);
        Self { app, net, _tasks: tasks, dir }
    }

    /// A client in the room, with channels, people and a conversation.
    fn online(tag: &str) -> Self {
        let mut h = Self::fresh(tag);
        demo::populate(&mut h.app);
        h.cmds();
        h.app.effects.clear();
        h
    }

    fn key(&mut self, code: KeyCode) {
        self.app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn chord(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.app.on_key(KeyEvent::new(code, modifiers));
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    /// Type a line and press Enter.
    fn say(&mut self, text: &str) {
        self.typed(text);
        self.key(KeyCode::Enter);
    }

    fn cmds(&mut self) -> Vec<NetCmd> {
        std::iter::from_fn(|| self.net.try_recv().ok()).collect()
    }

    /// Everything sent to the node since the last call, "typing…" included.
    fn sent_raw(&mut self) -> Vec<ClientMsg> {
        self.cmds()
            .into_iter()
            .filter_map(|c| match c {
                NetCmd::Send(msg) => Some(msg),
                _ => None,
            })
            .collect()
    }

    /// Messages sent to the node since the last call (without the "typing…" notices).
    fn sent(&mut self) -> Vec<ClientMsg> {
        self.sent_raw().into_iter().filter(|m| !matches!(m, ClientMsg::Typing { .. })).collect()
    }

    fn connects(&mut self) -> Vec<NetConfig> {
        self.cmds()
            .into_iter()
            .filter_map(|c| match c {
                NetCmd::Connect { config, .. } => Some(config),
                _ => None,
            })
            .collect()
    }

    fn server(&mut self, msg: ServerMsg) {
        self.app.on_net(NetEvent::Msg(Box::new(msg)));
    }

    fn message(&mut self, id: u64, channel: &str, from: &str, text: &str) {
        self.server(ServerMsg::Msg {
            msg: Message {
                id,
                channel: channel.into(),
                from: from.into(),
                ts: tovuer_proto::now_ms(),
                text: text.into(),
                reply: None,
                edited: None,
                file: None,
                reactions: Vec::new(),
                system: false,
                style: None,
            },
        });
    }

    fn screen(&mut self, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        ui::render(&mut buf, area, &mut self.app);
        buf.reset();
        let cursor = ui::render(&mut buf, area, &mut self.app);
        if let Some((x, y)) = cursor {
            assert!(x < w && y < h, "cursor ({x},{y}) off the {w}x{h} screen");
        }
        let mut out = String::new();
        for y in 0..h {
            let mut x = 0;
            while x < w {
                let symbol = buf[(x, y)].symbol();
                out.push_str(symbol);
                x += UnicodeWidthStr::width(symbol).clamp(1, 2) as u16;
            }
            out.push('\n');
        }
        out
    }

    fn last_toast(&self) -> (Tone, String) {
        let t = self.app.toasts.back().expect("a toast was expected");
        (t.tone, t.text.clone())
    }

    fn channel(&self, name: &str) -> &tovuer::app::model::ChannelState {
        self.app.channels.iter().find(|c| c.name == name).expect("the channel exists")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn every_scene_renders_at_every_size() {
    let scenes = ["login", "main", "voice", "select", "palette", "help", "settings", "users", "react", "picker", "viewer", "typing", "toast"];
    let widths = [40u16, 47, 61, 62, 79, 80, 99, 100, 109, 110, 129, 130, 220];
    let heights = [12u16, 13, 17, 21, 22, 24, 36, 70];
    for (i, scene) in scenes.iter().enumerate() {
        for theme in ["tovuer", "terminal", "paper"] {
            let mut h = Harness::fresh(&format!("scene-{i}-{theme}"));
            h.app.settings.theme = theme.into();
            h.app.refresh_theme();
            demo::scene(&mut h.app, scene);
            for w in widths {
                for rows in heights {
                    let text = h.screen(w, rows);
                    assert!(!text.trim().is_empty(), "{scene} {theme} {w}x{rows}: an empty screen");
                }
            }
        }
    }
    // a tiny window gets a polite notice, not a panic
    let mut h = Harness::online("tiny");
    for (w, rows) in [(1, 1), (10, 3), (39, 30), (80, 11)] {
        let text = h.screen(w, rows);
        if w >= 22 {
            assert!(text.contains("the window is too small"), "{w}x{rows}");
        }
    }
}

#[test]
fn main_screen_shows_what_matters() {
    let mut h = Harness::online("main");
    let text = h.screen(130, 60);
    for must in [
        "◆ tovuer",
        "home · 4 online",
        "# general",
        "chat about anything",
        "ONLINE — 4",
        "yesterday",
        "today",
        "sunset.png",
        "1.8 MB",
        "kate is typing",
        "(edited)",
        "╭─ bob: who is in Lounge tonight",
        "👍 2",
        "Message #general…",
        "online · 23 ms",
        "the channel starts here",
        "◆ dana created the room",
    ] {
        assert!(text.contains(must), "“{must}” is not on screen:\n{text}");
    }
    // unread in the next channel: a mention counter in the list
    assert!(text.lines().any(|l| l.contains("# random") && l.contains("@1")), "{text}");

    // in a narrow window the panels go, the feed stays
    let narrow = h.screen(60, 20);
    assert!(narrow.contains("# general") && !narrow.contains("ONLINE —") && !narrow.contains("TEXT"));
    assert!(narrow.contains("Ctrl+K channels"), "{narrow}");
}

#[test]
fn login_validates_then_joins_and_handles_rejections() {
    let mut h = Harness::fresh("login");
    assert_eq!(h.app.screen, Screen::Login);
    let text = h.screen(100, 30);
    assert!(text.contains("Nick") && text.contains("Room") && text.contains("Room password"));

    h.key(KeyCode::Enter);
    assert!(h.app.login.error.as_deref().unwrap().contains("nick"));
    assert!(h.connects().is_empty());

    h.typed("  big boss ");
    h.key(KeyCode::Tab);
    assert_eq!(h.app.login.field, LoginField::Room);
    h.key(KeyCode::Enter);
    assert!(h.app.login.error.as_deref().unwrap().contains("room"), "a room name is required");
    // typed like a human: case and spaces do not matter
    h.typed("Our Place");
    h.key(KeyCode::Enter);
    let cfg = h.connects().pop().expect("the client started joining");
    assert_eq!((cfg.name.as_str(), cfg.room.as_str()), ("big_boss", "our-place"));
    assert_eq!(cfg.password, None);
    assert_eq!(h.app.screen, Screen::Main);
    assert!(matches!(h.app.conn, Conn::Connecting { .. }));
    // nick and room are remembered for next time
    let saved = Paths::at(&h.dir).load_settings();
    assert_eq!((saved.nick.as_str(), saved.room.as_str()), ("big_boss", "our-place"));

    h.app.on_net(NetEvent::Lost { reason: "cannot open the network".into(), retry_in: Duration::from_secs(4) });
    assert!(h.screen(100, 30).contains("cannot open the network"));

    h.app.on_net(NetEvent::Rejected { code: ErrorCode::NameTaken, msg: "the nick big_boss is taken".into() });
    assert_eq!((h.app.screen, h.app.login.field), (Screen::Login, LoginField::Nick));
    assert!(h.cmds().iter().any(|c| matches!(c, NetCmd::Stop)), "after a rejection the client stops trying");
    assert!(h.screen(100, 30).contains("is taken"));

    // the room password: hidden on screen, sent along
    h.key(KeyCode::Tab);
    h.key(KeyCode::Tab);
    assert_eq!(h.app.login.field, LoginField::Password);
    h.typed("sesame");
    assert!(!h.screen(100, 30).contains("sesame"), "the password is not shown");
    h.key(KeyCode::Enter);
    assert_eq!(h.connects().pop().unwrap().password.as_deref(), Some("sesame"));

    h.app.on_net(NetEvent::Rejected { code: ErrorCode::ProtoMismatch, msg: "update the client".into() });
    assert!(h.app.login.offer_update);

    h.app.on_net(NetEvent::Rejected { code: ErrorCode::Kicked, msg: "the owner removed you".into() });
    assert_eq!(h.app.login.field, LoginField::Nick);
    h.key(KeyCode::Esc);
    assert!(h.app.quit);
}

#[test]
fn friend_build_asks_only_for_a_nick() {
    let launch = Launch { version: "test".into(), room: "our-place".into(), password: Some("pw".into()), friend: true, ..Launch::default() };
    let mut h = Harness::with("friend", launch);
    let text = h.screen(100, 30);
    assert!(text.contains("Nick") && !text.contains("Room"));
    h.key(KeyCode::Tab);
    assert_eq!(h.app.login.field, LoginField::Nick, "nowhere else to go");
    h.say("guest");
    let cfg = h.connects().pop().unwrap();
    assert_eq!((cfg.room.as_str(), cfg.password.as_deref()), ("our-place", Some("pw")));
}

#[test]
fn name_on_command_line_skips_login() {
    let launch = Launch { version: "test".into(), room: "home".into(), name: Some("dana".into()), ..Launch::default() };
    let mut h = Harness::with("autologin", launch);
    assert_eq!(h.app.screen, Screen::Main);
    let cfg = h.connects().pop().unwrap();
    assert_eq!((cfg.name.as_str(), cfg.room.as_str()), ("dana", "home"));
}

#[test]
fn sending_text_commands_and_offline_safety() {
    let mut h = Harness::online("send");
    h.say("hi :fire: all");
    assert_eq!(h.sent(), vec![ClientMsg::Send { channel: "general".into(), text: "hi 🔥 all".into(), reply_to: None, style: None }]);
    assert!(h.app.input.is_empty());

    // a double slash is plain text that starts with a slash
    h.say("//shrug is not a command");
    assert_eq!(h.sent(), vec![ClientMsg::Send { channel: "general".into(), text: "/shrug is not a command".into(), reply_to: None, style: None }]);

    h.say("/me drinks tea");
    h.say("/shrug");
    let texts: Vec<String> = h.sent().into_iter().map(|m| match m {
        ClientMsg::Send { text, .. } => text,
        other => panic!("{other:?}"),
    }).collect();
    assert_eq!(texts, vec!["*drinks tea*", "¯\\_(ツ)_/¯"]);

    h.say("   ");
    assert!(h.sent().is_empty(), "nothing empty is sent");

    // #channel switches
    h.say("#random");
    assert_eq!(h.app.channel_name(), "random");
    assert!(h.sent().is_empty());
    h.say("#general");

    // the room is lost: what was typed is kept
    h.app.on_net(NetEvent::Lost { reason: "network down".into(), retry_in: Duration::from_secs(2) });
    h.say("an important message");
    assert!(h.sent().is_empty());
    assert_eq!(h.app.input.text(), "an important message");
    assert_eq!(h.last_toast().0, Tone::Warn);
    assert!(h.screen(120, 30).contains("offline · retry in"));
}

#[test]
fn slash_commands_change_settings() {
    let mut h = Harness::online("commands");
    h.say("/theme nord");
    assert_eq!((h.app.settings.theme.as_str(), h.app.theme.name), ("nord", "nord"));
    h.say("/theme no-such-theme");
    assert_eq!(h.app.settings.theme, "nord");
    assert_eq!(h.last_toast().0, Tone::Warn);
    h.say("/theme next");
    assert_eq!(h.app.settings.theme, "rose-pine");

    h.say("/denoise off");
    assert!(!h.app.settings.denoise);
    assert!(!h.app.engine.controls().denoise.load(std::sync::atomic::Ordering::Relaxed));
    h.say("/denoise");
    assert!(h.app.settings.denoise, "without an argument it toggles");
    h.say("/volume 150");
    assert_eq!(h.app.engine.controls().out_gain(), 1.5);
    h.say("/volume 900");
    assert_eq!(h.app.settings.volume, 150, "out of range is refused");
    h.say("/gain 200");
    assert_eq!(h.app.engine.controls().mic_gain(), 2.0);

    h.say("/vol bob 40");
    assert_eq!(h.app.settings.volume_of("bob"), 0.4);
    h.say("/mute BOB");
    assert!(h.app.settings.is_muted("bob"), "the nick is found in any case");
    h.say("/unmute bob");
    assert!(!h.app.settings.is_muted("bob"));
    h.say("/mute dana");
    assert!(!h.app.settings.is_muted("dana"));
    // all of this would survive a restart
    let saved = Paths::at(&h.dir).load_settings();
    assert_eq!((saved.theme.as_str(), saved.volume, saved.volumes.get("bob")), ("rose-pine", 150, Some(&40)));

    h.say("/new ideas");
    h.say("/newvoice Movies");
    h.say("/topic on topic only");
    h.say("/kick bob");
    assert_eq!(
        h.sent(),
        vec![
            ClientMsg::ChannelCreate { name: "ideas".into(), kind: ChannelKind::Text },
            ClientMsg::ChannelCreate { name: "Movies".into(), kind: ChannelKind::Voice },
            ClientMsg::ChannelTopic { name: "general".into(), topic: "on topic only".into() },
            ClientMsg::Kick { name: "bob".into() },
        ]
    );

    h.say("/delchannel random");
    assert!(matches!(h.app.overlay, Some(Overlay::Confirm(_))), "deleting a channel needs confirmation");
    h.key(KeyCode::Esc);
    assert!(h.sent().is_empty());
    h.say("/delchannel random");
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::ChannelDelete { name: "random".into() }]);

    h.say("/nosuch");
    assert!(h.last_toast().1.contains("no command"));
    h.say("/help");
    assert!(matches!(h.app.overlay, Some(Overlay::Help { .. })));
    assert!(h.screen(120, 40).contains("palette: find and run"));
    // help is long and scrolls; the commands are at the very end
    h.key(KeyCode::End);
    let end = h.screen(120, 40);
    assert!(end.contains("/sharp [on|off]") && end.contains("/install"), "{end}");
    h.key(KeyCode::Home);
    assert!(h.screen(120, 40).contains("GENERAL"));
    h.key(KeyCode::Esc);
    h.say("/files");
    assert!(h.screen(120, 40).contains("1. sunset.png — 1.8 MB, from oleg"));
    h.key(KeyCode::Esc);
    h.say("/q");
    assert!(h.app.quit);
}

#[test]
fn unread_counters_read_marks_and_notifications() {
    let mut h = Harness::online("unread");
    assert_eq!((h.channel("random").unread, h.channel("random").mentions), (3, 1));
    assert_eq!(h.channel("general").unread, 0);

    // the window title shows mentions
    h.app.on_tick();
    assert!(h.app.effects.iter().any(|e| matches!(e, Effect::Title(t) if t.starts_with("(@1) tovuer"))), "{:?}", h.app.effects);

    // the channel is open — everything is read, but the "new" line shows where to start
    h.chord(KeyCode::Down, KeyModifiers::ALT);
    assert_eq!(h.app.channel_name(), "random");
    assert_eq!((h.channel("random").unread, h.channel("random").first_unread), (0, Some(21)));
    assert!(h.screen(120, 40).contains("new messages"));
    h.app.flush();
    assert_eq!(Paths::at(&h.dir).load_state().last_read("home", "random"), Some(23));

    // a message in a channel that is not open
    h.app.effects.clear();
    h.message(30, "general", "bob", "an ordinary message");
    assert_eq!((h.channel("general").unread, h.channel("general").mentions), (1, 0));
    assert!(!h.app.effects.contains(&Effect::Bell), "no mention, no bell");
    h.message(31, "general", "bob", "hey @dana");
    assert_eq!(h.channel("general").mentions, 1);
    assert!(h.app.effects.contains(&Effect::Bell));
    // our own message does not count as unread
    h.message(32, "general", "dana", "from my phone");
    assert_eq!(h.channel("general").unread, 2);

    // in the open channel, at the bottom of the feed, new is read right away
    h.message(40, "random", "kate", "seen at once");
    assert_eq!(h.channel("random").unread, 0);
    // …but if the terminal window is not focused, it piles up
    h.app.on_focus(false);
    h.message(41, "random", "kate", "while you were away");
    assert_eq!(h.channel("random").unread, 1);
    h.app.on_focus(true);
    assert_eq!(h.channel("random").unread, 0);

    // a bell that is off does not ring
    h.app.settings.bell = false;
    h.app.effects.clear();
    h.message(50, "general", "kate", "@dana again");
    assert!(!h.app.effects.contains(&Effect::Bell));

    h.chord(KeyCode::Up, KeyModifiers::ALT);
    assert_eq!(h.app.channel_name(), "general");
    assert_eq!(h.channel("random").first_unread, None, "leaving drops the \"new\" line");
    h.chord(KeyCode::Char('2'), KeyModifiers::ALT);
    assert_eq!(h.app.channel_name(), "random");
}

#[test]
fn typing_notice_is_throttled_and_skips_commands() {
    let mut h = Harness::online("typing");
    h.typed("hi, how are you");
    let pings = h.sent_raw();
    assert_eq!(pings, vec![ClientMsg::Typing { channel: "general".into() }], "one \"typing\" per phrase, not per letter");
    h.key(KeyCode::Enter);
    h.sent_raw();
    // commands are private — the others are not told
    h.typed("/theme");
    assert!(h.sent_raw().is_empty());
    h.chord(KeyCode::Char('u'), KeyModifiers::CONTROL);
    // after sending, the next phrase is announced right away again
    h.typed("more");
    assert_eq!(h.sent_raw().len(), 1);

    // someone else's "typing" shows and goes away when their message arrives
    h.server(ServerMsg::Typing { channel: "general".into(), name: "bob".into() });
    assert!(h.screen(120, 40).contains("bob and kate are typing") || h.screen(120, 40).contains("kate and bob are typing"));
    h.message(90, "general", "bob", "done");
    h.message(91, "general", "kate", "me too");
    assert!(!h.screen(120, 40).contains("typing"));
}

#[test]
fn drafts_are_kept_per_channel() {
    let mut h = Harness::online("drafts");
    h.typed("an unfinished thought");
    h.chord(KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_eq!((h.app.channel_name(), h.app.input.text()), ("random", ""));
    h.typed("something else here");
    h.chord(KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(h.app.input.text(), "an unfinished thought");
    h.chord(KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert_eq!(h.app.input.text(), "something else here");
}

#[test]
fn reply_edit_delete_react_and_copy() {
    let mut h = Harness::online("actions");
    // Shift+Tab from the input goes to messages; the last one (ours) is selected
    h.key(KeyCode::BackTab);
    assert_eq!(h.app.focus, Focus::Messages);
    assert_eq!(h.channel("general").selected, Some(12));
    assert!(h.screen(120, 40).contains("SELECT"));

    h.key(KeyCode::Char('e'));
    assert_eq!(h.app.compose, Some(Compose::Edit { id: 12 }));
    assert_eq!(h.app.focus, Focus::Input);
    assert!(h.screen(120, 40).contains("editing a message"));
    h.typed(" 👌");
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::Edit { id: 12, text: "on it, it's a typo — fixing now 👌".into() }]);
    assert_eq!(h.app.compose, None);

    // ↑ on an empty line edits your last message; Esc changes your mind
    h.key(KeyCode::Up);
    assert_eq!(h.app.compose, Some(Compose::Edit { id: 12 }));
    h.key(KeyCode::Esc);
    assert!(h.app.compose.is_none() && h.app.input.is_empty());

    // a reply to someone else
    h.key(KeyCode::BackTab);
    h.key(KeyCode::Up);
    assert_eq!(h.channel("general").selected, Some(11));
    h.key(KeyCode::Char('e'));
    assert_eq!(h.app.compose, None, "someone else's message cannot be edited");
    assert_eq!(h.last_toast().0, Tone::Warn);
    h.key(KeyCode::Char('r'));
    assert!(matches!(&h.app.compose, Some(Compose::Reply { id: 11, from, .. }) if from == "bob"));
    assert!(h.screen(120, 40).contains("↪ bob: @dana could you check"));
    h.say("on it");
    assert_eq!(h.sent(), vec![ClientMsg::Send { channel: "general".into(), text: "on it".into(), reply_to: Some(11), style: None }]);

    // a reaction
    h.key(KeyCode::BackTab);
    h.key(KeyCode::Char('a'));
    assert!(matches!(h.app.overlay, Some(Overlay::React { msg: 12, sel: 0 })));
    h.key(KeyCode::Right);
    h.key(KeyCode::Down);
    h.key(KeyCode::Enter);
    let quick = tovuer::app::emoji::QUICK;
    assert_eq!(h.sent(), vec![ClientMsg::React { id: 12, emoji: quick[1 + tovuer::app::overlay::REACT_COLS].into() }]);
    assert!(h.app.overlay.is_none());

    // copying the text
    h.key(KeyCode::Char('y'));
    assert!(h.app.effects.contains(&Effect::Copy("on it, it's a typo — fixing now".into())));

    // deleting goes through a confirmation
    h.key(KeyCode::Char('d'));
    assert!(matches!(h.app.overlay, Some(Overlay::Confirm(_))));
    h.key(KeyCode::Char('n'));
    assert!(h.sent().is_empty());
    h.key(KeyCode::Delete);
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::Delete { id: 12 }]);

    // the node confirmed the delete: the selection moves to a neighbor
    h.server(ServerMsg::MsgDelete { channel: "general".into(), id: 12 });
    assert_eq!(h.channel("general").selected, Some(11));
    assert!(!h.screen(120, 40).contains("fixing now"));
    h.key(KeyCode::Esc);
    assert_eq!(h.app.focus, Focus::Input);

    // edits and reactions from others show on screen
    h.server(ServerMsg::MsgEdit { channel: "general".into(), id: 10, text: "corrected text".into(), edited: 1 });
    h.server(ServerMsg::MsgReact {
        channel: "general".into(),
        id: 10,
        reactions: vec![tovuer_proto::Reaction { emoji: "🎉".into(), users: vec!["kate".into(), "bob".into(), "dana".into()] }],
    });
    let text = h.screen(120, 40);
    assert!(text.contains("corrected text (edited)") && text.contains("🎉 3"), "{text}");
}

#[test]
fn completion_for_commands_names_and_emoji() {
    let mut h = Harness::online("complete");
    h.typed("/th");
    let comp = h.app.completion.clone().expect("suggestions show up on their own");
    assert_eq!(comp.items[0].insert, "/theme ");
    assert!(h.screen(120, 40).contains("change the color theme"));
    h.key(KeyCode::Tab);
    assert_eq!(h.app.input.text(), "/theme ");
    // after the command its values are offered right away
    assert!(h.app.completion.as_ref().is_some_and(|c| c.items.iter().any(|i| i.label == "nord")));
    h.typed("no");
    h.key(KeyCode::Tab);
    assert_eq!(h.app.input.text(), "/theme nord ");
    h.key(KeyCode::Enter);
    assert_eq!(h.app.settings.theme, "nord");

    h.typed("hey @bo");
    h.key(KeyCode::Tab);
    assert_eq!(h.app.input.text(), "hey @bob ");
    h.typed(":fir");
    assert!(h.app.completion.as_ref().is_some_and(|c| c.items[0].label.contains("🔥")));
    h.key(KeyCode::Tab);
    assert_eq!(h.app.input.text(), "hey @bob 🔥 ");
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::Send { channel: "general".into(), text: "hey @bob 🔥".into(), reply_to: None, style: None }]);

    // Enter with suggestions open completes the command instead of sending a stub
    h.typed("/he");
    h.key(KeyCode::Enter);
    assert_eq!(h.app.input.text(), "/help ");
    assert!(h.app.overlay.is_none());
    h.key(KeyCode::Enter);
    assert!(matches!(h.app.overlay, Some(Overlay::Help { .. })));
    h.key(KeyCode::Esc);

    // arrows move through the list, Esc closes it
    h.typed("/m");
    let first = h.app.completion.as_ref().unwrap().sel;
    h.key(KeyCode::Down);
    assert_ne!(h.app.completion.as_ref().unwrap().sel, first);
    h.key(KeyCode::Esc);
    assert!(h.app.completion.is_none());
    assert_eq!(h.app.focus, Focus::Input, "Esc closed only the suggestions");

    // plain text brings no suggestions; Tab then goes to the channels
    h.chord(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.typed("just text");
    assert!(h.app.completion.is_none());
    h.key(KeyCode::Tab);
    assert_eq!(h.app.focus, Focus::Sidebar);
}

#[test]
fn voice_presence_follows_keys_and_server() {
    let mut h = Harness::online("voice");
    h.say("/v lounge");
    assert_eq!(
        h.sent(),
        vec![ClientMsg::VoiceJoin { channel: "Lounge".into() }, ClientMsg::VoiceState { muted: false, deafened: false }]
    );
    assert_eq!(h.app.voice.as_ref().unwrap().channel, "Lounge");
    h.server(ServerMsg::UserUpdate {
        user: tovuer_proto::User { id: 1, name: "dana".into(), voice: Some("Lounge".into()), muted: false, deafened: false, admin: true },
    });
    let text = h.screen(120, 40);
    assert!(text.contains("AUDIO…"), "while the devices open, the console says so:\n{text}");

    h.chord(KeyCode::Char('m'), KeyModifiers::ALT);
    assert_eq!(h.sent(), vec![ClientMsg::VoiceState { muted: true, deafened: false }]);
    assert!(h.app.muted());
    h.chord(KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(h.sent(), vec![ClientMsg::VoiceState { muted: true, deafened: true }]);
    // a plain m is a letter, even on an empty line
    h.key(KeyCode::Char('m'));
    assert!(h.sent().is_empty() && h.app.muted());
    assert_eq!(h.app.input.text(), "m");
    h.key(KeyCode::Backspace);
    // unmuting the mic while deafened brings the sound back too
    h.chord(KeyCode::Char('m'), KeyModifiers::ALT);
    assert_eq!(h.sent(), vec![ClientMsg::VoiceState { muted: false, deafened: false }]);

    // in the channel list Enter on a voice channel joins or leaves
    h.key(KeyCode::Tab);
    assert_eq!(h.app.focus, Focus::Sidebar);
    h.key(KeyCode::End);
    h.key(KeyCode::Enter);
    assert_eq!(h.sent()[0], ClientMsg::VoiceJoin { channel: "Gaming".into() });
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::VoiceLeave]);
    assert!(h.app.voice.is_none());
    h.key(KeyCode::Esc);

    // the channel was deleted while we were in it
    h.say("/v Lounge");
    h.sent();
    h.server(ServerMsg::Channels { channels: vec![Channel::text("general", ""), Channel::voice("Gaming")] });
    assert!(h.app.voice.is_none());
    assert_eq!(h.last_toast().0, Tone::Warn);

    h.say("/v NoSuch");
    assert!(h.sent().is_empty());
    h.say("/v");
    assert!(h.last_toast().1.contains("/v <channel>"));
}

#[test]
fn letters_always_type_and_the_microphone_is_alt_m() {
    let mut h = Harness::online("quick");
    // a message may start with any letter: on an empty line too
    h.typed("Mm");
    assert_eq!(h.app.input.text(), "Mm");
    assert!(!h.app.muted());
    h.key(KeyCode::Backspace);
    h.key(KeyCode::Backspace);
    h.typed("u");
    assert_eq!(h.app.input.text(), "u");
    assert!(h.app.overlay.is_none());
    h.key(KeyCode::Backspace);
    h.chord(KeyCode::Char('m'), KeyModifiers::ALT);
    assert!(h.app.muted(), "Alt+M works while typing");
    h.chord(KeyCode::Char('m'), KeyModifiers::ALT);
    assert!(!h.app.muted());
    h.key(KeyCode::F(3));
    assert!(matches!(h.app.overlay, Some(Overlay::Users { .. })));
    // in the volume window: arrows change the volume, Enter mutes
    h.key(KeyCode::Left);
    h.key(KeyCode::Left);
    assert_eq!(h.app.settings.volumes.get("bob"), Some(&80));
    h.key(KeyCode::Char('0'));
    assert_eq!(h.app.settings.volumes.get("bob"), None, "100% is the default and is not stored");
    h.key(KeyCode::Down);
    h.key(KeyCode::Enter);
    assert!(h.app.settings.is_muted("kate"));
    assert!(h.screen(120, 40).contains("muted"));
    h.key(KeyCode::Esc);
    // text with an "m" types normally once the line is not empty
    h.typed("hm");
    h.typed("m");
    assert_eq!(h.app.input.text(), "hmm");
}

#[test]
fn reconnect_restores_voice_and_refetches_history() {
    let mut h = Harness::online("reconnect");
    h.say("/v Lounge");
    h.cmds();
    h.app.on_net(NetEvent::Lost { reason: "network down".into(), retry_in: Duration::from_secs(1) });
    assert!(matches!(h.app.conn, Conn::Waiting { .. }));
    assert!(h.app.voice.is_some(), "the voice channel is remembered — we go back to it ourselves");
    assert!(h.app.speaking.is_empty());

    h.app.on_net(NetEvent::Connecting { attempt: 1, server: "home".into() });
    h.server(ServerMsg::Welcome {
        id: 9,
        name: "dana".into(),
        server: "home".into(),
        version: "x".into(),
        proto: tovuer_proto::PROTO_VERSION,
        admin: false,
        channels: vec![Channel::text("general", "new topic"), Channel::text("random", ""), Channel::voice("Lounge")],
        users: vec![tovuer_proto::User { id: 9, name: "dana".into(), voice: None, muted: false, deafened: false, admin: false }],
        max_file: 1 << 20,
        client_version: Some("9.9.9".into()),
        motd: None,
    });
    assert_eq!(h.app.conn, Conn::Online);
    assert_eq!(h.app.my_id(), 9);
    let sent = h.sent();
    assert!(sent.contains(&ClientMsg::History { channel: "general".into(), before: None, limit: None }));
    assert!(sent.contains(&ClientMsg::History { channel: "random".into(), before: None, limit: None }));
    assert!(sent.contains(&ClientMsg::VoiceJoin { channel: "Lounge".into() }));
    assert!(h.app.toasts.iter().any(|t| t.text == "back in the room"));
    // the conversation and the place in it are kept, the topic is updated
    assert_eq!(h.channel("general").messages.len(), 12);
    assert_eq!(h.channel("general").topic, "new topic");
    assert_eq!(h.app.update_available.as_deref(), Some("9.9.9"));
    assert!(h.screen(140, 40).contains("version 9.9.9 is out: /update"));
}

fn old_message(id: u64, text: String) -> Message {
    Message {
        id,
        channel: "games".into(),
        from: if id % 3 == 0 { "kate".into() } else { "bob".into() },
        ts: 1_700_000_000_000 + id * 60_000,
        text,
        reply: None,
        edited: None,
        file: None,
        reactions: Vec::new(),
        system: false,
        style: None,
    }
}

#[test]
fn scrolling_to_the_top_asks_for_older_history_once() {
    let mut h = Harness::online("scroll");
    h.say("#games");
    let page: Vec<Message> = (100..140).map(|id| old_message(id, format!("message number {id}"))).collect();
    h.server(ServerMsg::History { before: None, channel: "games".into(), messages: page, more: true });
    h.sent();
    assert!(h.screen(100, 20).contains("message number 139"), "the feed opens at the latest messages");

    for _ in 0..40 {
        h.key(KeyCode::PageUp);
        h.screen(100, 20);
    }
    assert_eq!(h.sent(), vec![ClientMsg::History { channel: "games".into(), before: Some(100), limit: None }], "exactly one request");
    let before = h.screen(100, 20);
    assert!(before.contains("loading history…") && before.contains("message number 100"), "{before}");
    let row = |screen: &str, needle: &str| screen.lines().position(|l| l.contains(needle));

    // an older page arrived: the feed does not jump, reading continues in place
    let older: Vec<Message> = (60..100).map(|id| old_message(id, format!("message number {id}"))).collect();
    h.server(ServerMsg::History { before: Some(100), channel: "games".into(), messages: older, more: false });
    let after = h.screen(100, 20);
    assert_eq!(row(&before, "message number 103"), row(&after, "message number 103"), "{before}\n-----\n{after}");
    assert!(row(&after, "message number 103").is_some());

    // further up — no more requests, all the way to the start
    for _ in 0..40 {
        h.key(KeyCode::PageUp);
        h.screen(100, 20);
    }
    assert!(h.sent().is_empty());
    let top = h.screen(100, 20);
    assert!(top.contains("the channel starts here") && top.contains("message number 60"), "{top}");

    // down — the "to the latest" mark and one key combination back
    assert!(top.contains("to the latest"));
    h.chord(KeyCode::End, KeyModifiers::CONTROL);
    assert_eq!(h.channel("games").scroll, 0);
    let bottom = h.screen(100, 20);
    assert!(!bottom.contains("to the latest") && bottom.contains("message number 139"));
}

#[test]
fn new_messages_do_not_move_the_view_while_reading_old_ones() {
    let mut h = Harness::online("anchor");
    h.screen(100, 20);
    h.key(KeyCode::PageUp);
    let before = h.screen(100, 20);
    let scroll_before = h.channel("general").scroll;
    assert!(scroll_before > 0);
    let visible_line = before.lines().find(|l| l.contains("gorgeous") || l.contains("I'm in") || l.contains("meeting at")).map(str::trim_end).map(str::to_string);
    let visible_line = visible_line.expect("a familiar line is in view");
    let row_before = before.lines().position(|l| l.trim_end() == visible_line).unwrap();

    for i in 0..5 {
        h.message(200 + i, "general", "bob", &format!("new message {i} — long enough to take some room in the feed"));
    }
    let after = h.screen(100, 20);
    let row_after = after.lines().position(|l| l.trim_end() == visible_line).expect("the same line is still on screen");
    assert_eq!(row_before, row_after, "the feed did not move:\n{before}\n-----\n{after}");
    assert!(h.channel("general").scroll > scroll_before);
    assert_eq!(h.channel("general").unread, 5);
    assert!(after.contains("↓ 5 new"));
}

#[test]
fn pasting_a_file_path_offers_to_upload_it() {
    let mut h = Harness::online("paste");
    let file = h.dir.join("november report.txt");
    std::fs::create_dir_all(&h.dir).unwrap();
    std::fs::write(&file, b"data").unwrap();
    // this is how terminals paste a dropped file: with escaped spaces
    h.app.on_paste(file.display().to_string().replace(' ', "\\ "));
    assert_eq!(h.app.input.text(), format!("/file {}", file.display()));

    h.chord(KeyCode::Char('u'), KeyModifiers::CONTROL);
    h.app.on_paste("plain text\non two lines".into());
    assert_eq!(h.app.input.text(), "plain text\non two lines");
    assert!(h.screen(100, 30).contains("on two lines"), "the input grew for the second line");

    // pasting into the login form takes the first line only
    let mut fresh = Harness::fresh("paste-login");
    fresh.app.on_paste("dana\ngarbage".into());
    assert_eq!(fresh.app.login.nick.text(), "dana");
}

#[test]
fn narrow_window_uses_palette_instead_of_sidebar() {
    let mut h = Harness::online("narrow");
    h.screen(60, 20);
    h.key(KeyCode::Tab);
    assert!(matches!(h.app.overlay, Some(Overlay::Palette(_))), "no channel list — the palette opens");
    assert_eq!(h.app.focus, Focus::Input);
    h.typed("random");
    h.key(KeyCode::Enter);
    assert_eq!(h.app.channel_name(), "random");
    assert!(h.app.overlay.is_none());

    // in a wide window Tab goes to the channels, and Ctrl+K opens the palette
    h.screen(120, 30);
    h.key(KeyCode::Tab);
    assert_eq!(h.app.focus, Focus::Sidebar);
    h.chord(KeyCode::Char('k'), KeyModifiers::CONTROL);
    h.typed("mic off");
    h.key(KeyCode::Enter);
    assert!(h.app.muted());
}

#[test]
fn settings_window_edits_and_saves() {
    let mut h = Harness::online("settings");
    h.key(KeyCode::F(2));
    assert!(matches!(h.app.overlay, Some(Overlay::Settings(_))));
    // the first row is the theme: the right arrow flips and applies at once
    h.key(KeyCode::Right);
    assert_eq!(h.app.theme.name, "tokyo-night");
    h.key(KeyCode::Left);
    h.key(KeyCode::Left);
    assert_eq!(h.app.theme.name, "terminal", "it wraps around both ways");
    h.key(KeyCode::Down);
    h.key(KeyCode::Enter);
    assert!(h.app.settings.transparent);
    let text = h.screen(120, 40);
    assert!(text.contains("Transparent background") && text.contains("‹ on ›"), "{text}");
    // the mouse turns on at once, without a restart
    for _ in 0..6 {
        h.key(KeyCode::Down);
    }
    h.key(KeyCode::Char(' '));
    assert!(h.app.settings.mouse && h.app.effects.contains(&Effect::Mouse(true)));
    h.key(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    let saved = Paths::at(&h.dir).load_settings();
    assert!(saved.transparent && saved.mouse && saved.theme == "terminal");
}

#[test]
fn channel_list_follows_the_server() {
    let mut h = Harness::online("channels");
    h.chord(KeyCode::Down, KeyModifiers::ALT);
    assert_eq!(h.app.channel_name(), "random");
    h.typed("draft");
    // the open channel was deleted — move to the first one, without dragging the draft along
    h.server(ServerMsg::Channels {
        channels: vec![Channel::text("general", ""), Channel::text("fresh", "brand new"), Channel::voice("Lounge")],
    });
    assert_eq!(h.app.channel_name(), "general");
    assert_eq!(h.app.channels.len(), 3);
    assert_eq!(h.channel("general").messages.len(), 12, "the history of known channels is kept");
    let text = h.screen(120, 40);
    assert!(text.contains("# fresh") && !text.contains("# random"));

    // people come and go
    h.server(ServerMsg::UserJoin { user: tovuer_proto::User { id: 7, name: "yana".into(), voice: None, muted: false, deafened: false, admin: false } });
    assert!(h.screen(120, 40).contains("ONLINE — 5"));
    h.server(ServerMsg::UserLeave { id: 7 });
    h.server(ServerMsg::UserLeave { id: 2 });
    let text = h.screen(120, 40);
    assert!(text.contains("ONLINE — 3") && !text.contains("● bob") && !text.contains("◉ bob"), "{text}");
    // an error from the node is shown to the person
    h.server(ServerMsg::Error { code: ErrorCode::RateLimited, msg: "too fast".into() });
    assert_eq!(h.last_toast(), (Tone::Error, "too fast".into()));
}

#[test]
fn the_viewer_draws_real_pixels_when_the_terminal_can() {
    use std::sync::Arc;
    use ratatui_image::picker::{Picker, ProtocolType};
    use tovuer::app::overlay::Viewer;
    use tovuer::transfer::Sharp;

    let mut h = Harness::online("sharp");
    let file = tovuer_proto::FileMeta { id: "ab".repeat(16), name: "sunset.png".into(), size: 183_420, image: true };
    let show = |h: &mut Harness| {
        h.app.overlay = Some(Overlay::Viewer(Viewer { pic: Some(Arc::new(demo::picture(480, 270))), ..Viewer::new(file.clone()) }));
    };
    let draw = |h: &mut Harness| {
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        ui::render(&mut buf, area, &mut h.app);
        buf
    };
    let has_sixel = |buf: &Buffer| buf.content().iter().any(|c| c.symbol().contains("\x1bP"));
    let has_blocks = |buf: &Buffer| buf.content().iter().any(|c| c.symbol() == "▀");

    // a terminal with blocks only: blocks, and nothing to encode
    show(&mut h);
    let text = h.screen(120, 40);
    assert!(text.contains("480×270") && text.contains("o — open the original"), "{text}");
    assert!(has_blocks(&draw(&mut h)) && h.app.ui.want_sharp.is_none());

    // a terminal with Sixel: blocks stand in while the picture is encoded
    let mut graphics = Picker::halfblocks(); // its font: 10×20 px
    graphics.set_protocol_type(ProtocolType::Sixel);
    h.app.graphics = Some(graphics.clone());
    assert!(has_blocks(&draw(&mut h)));
    let (wanted, cells) = h.app.ui.want_sharp.take().expect("the viewer asks for a sharp picture");
    assert_eq!(wanted.id, file.id);
    draw(&mut h);
    assert!(h.app.ui.want_sharp.is_none(), "asked once per window size");

    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(480, 270, image::Rgb([200, 100, 50])));
    let sharp = Sharp::encode(&img, &graphics, cells.0, cells.1, [0, 0, 0]).unwrap();
    h.app.on_task(TaskEvent::Sharp { file_id: file.id.clone(), area: cells, result: Ok(Arc::new(sharp)) });
    let buf = draw(&mut h);
    assert!(has_sixel(&buf) && !has_blocks(&buf), "real pixels replace the blocks");
    draw(&mut h);
    assert!(!h.app.ui.repaint, "nothing changed on top of the picture");

    // a toast over a sixel picture: the whole screen is repainted, so no ghost stays
    h.app.on_task(TaskEvent::Opened { name: "sunset.png".into(), result: Ok(()) });
    assert!(h.last_toast().1.contains("opened sunset.png"));
    draw(&mut h);
    assert!(std::mem::take(&mut h.app.ui.repaint));

    // switched off: blocks again
    h.app.overlay = None;
    h.say("/sharp off");
    assert!(!h.app.settings.sharp);
    show(&mut h);
    let buf = draw(&mut h);
    assert!(has_blocks(&buf) && !has_sixel(&buf));
}

#[test]
fn feed_pictures_get_real_pixels_when_wholly_in_view() {
    use std::sync::Arc;
    use ratatui::buffer::CellDiffOption;
    use ratatui_image::picker::{Picker, ProtocolType};
    use tovuer::app::SharpThumb;
    use tovuer::transfer::Sharp;

    let mut h = Harness::online("feed-sharp");
    let draw = |h: &mut Harness| {
        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        ui::render(&mut buf, area, &mut h.app);
        buf
    };
    let sixel_at = |buf: &Buffer| buf.content().iter().position(|c| c.symbol().contains("\x1bP"));
    // blocks the terminal gets: under a picture they stay in the buffer, unsent
    let has_blocks = |buf: &Buffer| buf.content().iter().any(|c| c.symbol() == "▀" && c.diff_option != CellDiffOption::Skip);

    // blocks only: nothing is asked for
    assert!(has_blocks(&draw(&mut h)) && h.app.ui.want_sharp_thumbs.is_empty());

    // Sixel: the thumbnail is asked for at its size in cells, blocks meanwhile
    let mut graphics = Picker::halfblocks();
    graphics.set_protocol_type(ProtocolType::Sixel);
    h.app.graphics = Some(graphics.clone());
    assert!(has_blocks(&draw(&mut h)));
    let (file, cells) = h.app.ui.want_sharp_thumbs.pop().expect("the feed asks for a sharp thumbnail");
    assert_eq!(file.id, demo::IMAGE_ID);
    assert!(cells.0 > 10 && cells.1 > 3, "{cells:?}");
    h.app.sharp_thumbs.insert(file.id.clone(), SharpThumb::Loading(cells));
    draw(&mut h);
    assert!(h.app.ui.want_sharp_thumbs.is_empty(), "not asked twice while it is encoded");

    let img = demo::picture(480, 270).into_image().unwrap();
    let sharp = Sharp::encode_filled(&img, &graphics, cells.0, cells.1).unwrap();
    assert_eq!(sharp.size(), cells, "it covers exactly the thumbnail's cells");
    h.app.on_task(TaskEvent::SharpThumb { file_id: file.id.clone(), cells, result: Ok(Arc::new(sharp)) });
    let buf = draw(&mut h);
    let at = sixel_at(&buf).expect("real pixels in the feed");
    assert!(!has_blocks(&buf), "they replace the blocks");
    draw(&mut h);
    assert!(!h.app.ui.repaint, "nothing moved");

    // the feed scrolls by a line: the picture moves, the old one must not stay
    let current = h.app.current;
    h.app.channels[current].scroll += 1;
    let buf = draw(&mut h);
    assert!(std::mem::take(&mut h.app.ui.repaint));
    assert_eq!(sixel_at(&buf), Some(at + 120), "one line lower");

    // a window over the feed: blocks under it, no picture on top of the window
    h.chord(KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert!(matches!(h.app.overlay, Some(Overlay::Palette(_))));
    let buf = draw(&mut h);
    assert!(sixel_at(&buf).is_none() && std::mem::take(&mut h.app.ui.repaint));
    h.key(KeyCode::Esc);
    assert!(sixel_at(&draw(&mut h)).is_some(), "back once the window is closed");

    // a short window, scrolled line by line: a picture cut by an edge shows
    // as blocks, a whole one in real pixels — never both
    let (mut cut, mut whole) = (0, 0);
    for scroll in 0..40 {
        h.app.channels[current].scroll = scroll;
        h.app.channels[current].anchor = None; // or it holds the view in place
        let area = Rect::new(0, 0, 120, 18);
        let mut buf = Buffer::empty(area);
        ui::render(&mut buf, area, &mut h.app);
        match (sixel_at(&buf).is_some(), has_blocks(&buf)) {
            (true, true) => panic!("blocks and pixels at once, scroll {scroll}"),
            (true, false) => whole += 1,
            (false, true) => cut += 1,
            (false, false) => {}
        }
    }
    assert!(cut > 0 && whole > 0, "cut {cut}, whole {whole}");
}

#[test]
fn the_file_picker_previews_pictures_in_real_pixels() {
    use std::sync::Arc;
    use ratatui_image::picker::{Picker, ProtocolType};
    use tovuer::app::overlay::Peek;
    use tovuer::transfer::Sharp;

    let mut h = Harness::online("picker-sharp");
    let dir = h.dir.join("pics");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.png"), b"not really a picture").unwrap();
    std::fs::write(dir.join("b.txt"), b"text").unwrap();
    h.app.ui.last_dir = Some(dir.clone());
    let mut graphics = Picker::halfblocks();
    graphics.set_protocol_type(ProtocolType::Sixel);
    h.app.graphics = Some(graphics.clone());
    let draw = |h: &mut Harness| {
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        ui::render(&mut buf, area, &mut h.app);
        buf
    };
    let has_sixel = |buf: &Buffer| buf.content().iter().any(|c| c.symbol().contains("\x1bP"));

    h.chord(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let Some(Overlay::Picker(picker)) = &h.app.overlay else { panic!("the picker opens") };
    assert!(matches!(picker.peek, Some((ref p, Peek::Loading)) if *p == dir.join("a.png")));

    // the preview comes as blocks and in the terminal's graphics, same cells
    let thumb = demo::picture(480, 270).fit(40, 16);
    let img = demo::picture(480, 270).into_image().unwrap();
    let sharp = Sharp::encode_filled(&img, &graphics, thumb.w, thumb.rows()).unwrap();
    h.app.on_task(TaskEvent::Peek { path: dir.join("a.png"), result: Ok(thumb), sharp: Some(Arc::new(sharp)) });
    assert!(has_sixel(&draw(&mut h)), "real pixels in the preview");
    draw(&mut h);
    assert!(!h.app.ui.repaint);

    // off a picture: it goes, and the screen is repainted so no ghost stays
    h.key(KeyCode::Down);
    let buf = draw(&mut h);
    assert!(!has_sixel(&buf) && std::mem::take(&mut h.app.ui.repaint));
}

#[test]
fn videos_show_a_frame_and_their_length_and_play_outside() {
    let mut h = Harness::online("video");
    let clip = tovuer_proto::FileMeta { id: "cd".repeat(16), name: "clip.mp4".into(), size: 2_400_000, image: false };
    h.server(ServerMsg::Msg {
        msg: Message {
            id: 40,
            channel: "general".into(),
            from: "kate".into(),
            ts: tovuer_proto::now_ms(),
            text: String::new(),
            reply: None,
            edited: None,
            file: Some(clip.clone()),
            reactions: Vec::new(),
            system: false,
            style: None,
        },
    });

    // no ffmpeg: an ordinary file, nothing is fetched for it
    h.screen(120, 40);
    assert!(!h.app.ui.want_thumbs.iter().any(|f| f.id == clip.id));

    // with ffmpeg: a frame is wanted, then shown with the video's length
    h.app.video = true;
    h.app.ui.invalidate();
    let text = h.screen(120, 40);
    assert!(text.contains("loading the preview"), "{text}");
    assert!(h.app.ui.want_thumbs.iter().any(|f| f.id == clip.id));
    let mut thumb = demo::picture(320, 180).fit(tovuer::transfer::THUMB_COLS, tovuer::transfer::THUMB_ROWS);
    thumb.duration = Some(42);
    h.app.on_task(TaskEvent::Thumb { file_id: clip.id.clone(), result: Ok(thumb) });
    let text = h.screen(120, 40);
    assert!(text.contains("clip.mp4  2.3 MB · 0:42"), "{text}");
    assert!(text.contains("▀▀▀▀"), "the frame is drawn:\n{text}");

    // Enter plays it in the system's player instead of downloading it
    h.key(KeyCode::BackTab);
    assert_eq!(h.app.focus, Focus::Messages);
    h.key(KeyCode::Enter);
    assert!(h.last_toast().1.contains("cannot open the file"), "no node in a test, so it says why: {:?}", h.last_toast());
}

#[test]
fn channels_are_made_and_removed_from_the_list_the_palette_and_commands() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut h = Harness::online("make-channels");
    h.cmds();

    // "+" next to TEXT and VOICE; a click on it asks for a name
    h.app.settings.mouse = true;
    let text = h.screen(120, 40);
    let voice_line = text.lines().position(|l| l.trim_start().starts_with("VOICE")).expect("the VOICE header");
    let plus = text.lines().nth(voice_line).unwrap().chars().position(|c| c == '+').expect("a + next to VOICE");
    assert!(text.lines().any(|l| l.trim_start().starts_with("TEXT") && l.contains('+')), "{text}");
    h.app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: plus as u16, row: voice_line as u16, modifiers: KeyModifiers::NONE });
    assert!(h.screen(120, 40).contains("New voice channel"));
    h.say("party");
    assert_eq!(h.sent(), vec![ClientMsg::ChannelCreate { name: "party".into(), kind: ChannelKind::Voice }]);

    // the channel list: n and v, with the keys shown in the status line
    h.key(KeyCode::Tab);
    assert!(h.screen(120, 40).contains("n new text · v new voice · Del delete"));
    h.key(KeyCode::Char('n'));
    h.say("memes");
    assert_eq!(h.sent(), vec![ClientMsg::ChannelCreate { name: "memes".into(), kind: ChannelKind::Text }]);

    // the palette deletes any channel, voice ones too — after a confirmation
    h.chord(KeyCode::Char('k'), KeyModifiers::CONTROL);
    h.typed("delete voice lounge");
    assert!(h.screen(120, 40).contains("Delete voice channel Lounge…"));
    h.key(KeyCode::Enter);
    assert!(matches!(h.app.overlay, Some(Overlay::Confirm(_))));
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::ChannelDelete { name: "Lounge".into() }]);

    // /delchannel without a name: the open channel; with one — any kind, any case
    h.key(KeyCode::Esc); // back to the input
    h.say("/delchannel");
    assert!(h.screen(120, 40).contains("#general"));
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::ChannelDelete { name: "general".into() }]);
    h.say("/delvoice gaming");
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::ChannelDelete { name: "Gaming".into() }]);
    h.say("/delchannel nosuch");
    assert!(h.last_toast().1.contains("no channel"));

    // someone who is not the owner sees no delete entries at all
    let mut guest = Harness::online("make-channels-guest");
    if let Some(me) = guest.app.me.as_mut() {
        me.admin = false;
    }
    guest.chord(KeyCode::Char('k'), KeyModifiers::CONTROL);
    guest.typed("delete");
    assert!(!guest.screen(120, 40).contains("Delete voice channel"));
    guest.key(KeyCode::Esc);
    guest.say("/delchannel random");
    assert!(guest.last_toast().1.contains("only the room owner"));
    guest.key(KeyCode::Tab);
    assert!(!guest.screen(120, 40).contains("Del delete"));
}

fn welcome_to(room: &str, channels: Vec<Channel>) -> ServerMsg {
    ServerMsg::Welcome {
        id: 1, name: "dana".into(), server: room.into(), admin: false,
        proto: tovuer_proto::PROTO_VERSION, version: "test".into(), channels, users: vec![],
        max_file: 1 << 20, client_version: None, motd: None,
    }
}

#[test]
fn deleting_open_channel_restores_destination_draft() {
    let mut h = Harness::online("draft-delete");
    h.typed("general draft");
    h.app.open_channel_named("random");
    h.typed("random draft");
    h.server(ServerMsg::Channels { channels: vec![Channel::text("general", "")] });
    assert_eq!((h.app.channel_name(), h.app.input.text()), ("general", "general draft"));
    h.sent();
    h.key(KeyCode::Enter);
    assert_eq!(h.sent(), vec![ClientMsg::Send { channel: "general".into(), text: "general draft".into(), reply_to: None, style: None }]);
}

#[test]
fn reconnect_snapshot_reconciles_deleted_messages_and_pagination() {
    let mut h = Harness::online("sync-pages");
    let channels = vec![Channel::text("general", "")];
    let mut remaining = h.channel("general").messages.clone();
    remaining.retain(|m| m.id != 10);
    h.app.reconnect();
    h.server(welcome_to("home", channels.clone()));
    h.server(ServerMsg::History { channel: "general".into(), before: None, messages: remaining, more: false });
    assert!(h.channel("general").find(10).is_none());

    h.app.reconnect();
    h.server(welcome_to("home", channels));
    let messages = (51..=150).map(|id| Message { channel: "general".into(), ..old_message(id, format!("line {id}")) }).collect();
    h.server(ServerMsg::History { channel: "general".into(), before: None, messages, more: true });
    assert_eq!(h.channel("general").messages[0].id, 51);
    h.sent();
    h.app.set_focus(Focus::Messages);
    h.app.select_message(-200);
    assert_eq!(h.sent(), vec![ClientMsg::History { channel: "general".into(), before: Some(51), limit: None }]);
    let older = Message { channel: "general".into(), ..old_message(50, "older".into()) };
    h.server(ServerMsg::History { channel: "general".into(), before: Some(51), messages: vec![older], more: false });
    assert!(h.channel("general").find(50).is_some());
}

#[test]
fn newly_discovered_text_channel_fetches_early_messages() {
    let mut h = Harness::online("channel-early-msg");
    h.message(100, "travel", "bob", "reservations");
    let channels = vec![Channel::text("general", ""), Channel::text("travel", "")];
    h.server(ServerMsg::Channels { channels: channels.clone() });
    h.server(ServerMsg::Channels { channels });
    assert_eq!(h.sent(), vec![ClientMsg::History { channel: "travel".into(), before: None, limit: None }]);
    let msg = Message { channel: "travel".into(), ..old_message(100, "reservations".into()) };
    h.server(ServerMsg::History { channel: "travel".into(), before: None, messages: vec![msg], more: false });
    assert_eq!(h.channel("travel").find(100).unwrap().text, "reservations");
}

#[test]
fn switching_rooms_resets_history_drafts_and_read_marks() {
    let mut h = Harness::online("rooms");
    h.typed("old draft");
    h.app.back_to_login(None);
    // Even an identical name with a different password denotes a distinct room.
    h.app.login.nick.set("dana");
    h.app.login.room.set("home");
    h.app.login.password.set("another room");
    h.app.submit_login();
    assert!(h.app.channels.is_empty() && h.app.input.is_empty());
    h.app.on_session_event((0, NetEvent::Msg(Box::new(welcome_to("old", vec![Channel::text("old", "")])))));
    assert!(h.app.channels.is_empty());
    h.server(welcome_to("home", vec![Channel::text("general", "")]));
    assert_eq!(h.channel("general").last_read, 0);
    h.server(ServerMsg::History { channel: "general".into(), before: None, messages: vec![], more: false });
    assert!(h.channel("general").messages.is_empty());
}

#[test]
fn explicit_room_overrides_saved_credentials() {
    let h = Harness::fresh("explicit-room");
    let mut settings = h.app.settings.clone();
    settings.room = "old".into();
    settings.password = "saved password".into();
    h.app.paths.save_settings(&settings).unwrap();
    for (arg, expected_room, expected_password) in [("new", "new", None), ("", "old", Some("saved password"))] {
        let launch = Launch { room: arg.into(), name: Some("alice".into()), ..Launch::default() };
        let (_, mut cmds, _) = demo::blank(&h.dir, launch);
        let NetCmd::Connect { config, .. } = cmds.try_recv().unwrap() else { panic!("expected autologin") };
        assert_eq!((config.room.as_str(), config.password.as_deref()), (expected_room, expected_password));
        h.app.paths.save_settings(&settings).unwrap();
    }
}

#[test]
fn your_own_message_style_travels_with_what_you_send() {
    use tovuer_proto::TextStyle;
    let mut h = Harness::online("style");
    h.cmds();

    // F4: the style window; ← → walk the ready colors, Enter takes your own
    h.key(KeyCode::F(4));
    assert!(h.screen(120, 40).contains("Your message style"));
    h.key(KeyCode::Right);
    assert_eq!(h.app.settings.style.color.as_deref(), Some("#ff5f6d"), "the first ready color: red");
    h.key(KeyCode::Enter);
    assert!(h.screen(120, 40).contains("Your color"));
    h.key(KeyCode::Backspace);
    h.typed("nonsense");
    h.key(KeyCode::Enter);
    assert!(h.last_toast().1.contains("not a color"), "a wrong color keeps the prompt open");
    for _ in 0.."#ff5f6nonsense".len() {
        h.key(KeyCode::Backspace);
    }
    h.typed("#123abc");
    h.key(KeyCode::Enter);
    assert!(matches!(h.app.overlay, Some(Overlay::Style(_))), "back in the style window");
    assert_eq!(h.app.settings.style.color.as_deref(), Some("#123abc"));
    // a gradient (it wins over the color), bold on, then close: it is saved
    h.key(KeyCode::Down);
    h.key(KeyCode::Right);
    assert_eq!(h.app.settings.style.gradient.len(), 3, "sunset");
    for _ in 0..2 {
        h.key(KeyCode::Down);
    }
    h.key(KeyCode::Enter);
    assert!(h.app.settings.style.bold);
    let text = h.screen(120, 40);
    assert!(text.contains("sunset") && text.contains("‹ on ›"), "{text}");
    h.key(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(Paths::at(&h.dir).load_settings().style, h.app.settings.style);

    // what you send carries the style; a flowing gradient keeps the screen moving
    h.say("look at me");
    let sent = h.sent();
    let ClientMsg::Send { style: Some(style), .. } = &sent[0] else { panic!("no style in {sent:?}") };
    assert_eq!((style.gradient.len(), style.flow, style.bold, style.color.is_none()), (3, true, true, true));
    h.typed("typing shows it too");
    h.screen(120, 40);
    assert!(h.app.ui.animating, "the input previews the flowing gradient");
    h.app.dirty = false;
    h.app.on_tick();
    assert!(h.app.dirty, "each tick redraws while something flows");

    // commands do the same
    h.app.input.set("");
    h.say("/color sky");
    assert_eq!((h.app.settings.style.color.as_deref(), h.app.settings.style.gradient.is_empty()), (Some("#48dbfb"), true));
    assert!(h.last_toast().1.contains("sky, bold"));
    h.say("/gradient #ff0000 #00ff00 #0000ff");
    assert_eq!(h.app.settings.style.gradient, vec!["#ff0000", "#00ff00", "#0000ff"]);
    h.say("/gradient blurple");
    assert!(h.last_toast().1.contains("not a color"));
    h.say("/style");
    assert!(matches!(h.app.overlay, Some(Overlay::Style(_))));
    for _ in 0..7 {
        h.key(KeyCode::Down);
    }
    h.key(KeyCode::Enter);
    h.key(KeyCode::Esc);
    assert_eq!(h.app.settings.style.clean(), None, "back to plain");
    h.say("plain again");
    assert!(matches!(&h.sent()[0], ClientMsg::Send { style: None, .. }));

    // someone else's style: drawn in their colors, unless you turn styles off
    let styled = Message {
        id: 500,
        channel: "general".into(),
        from: "kate".into(),
        ts: tovuer_proto::now_ms(),
        text: "shiny".into(),
        reply: None,
        edited: None,
        file: None,
        reactions: Vec::new(),
        system: false,
        style: Some(TextStyle { color: Some("#ff0000".into()), italic: true, ..TextStyle::default() }),
    };
    h.server(ServerMsg::Msg { msg: styled });
    let find = |h: &mut Harness| {
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        ui::render(&mut buf, area, &mut h.app);
        let at = (0..40).find_map(|y| {
            let row: String = (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect();
            row.find("shiny").map(|x| (x as u16, y))
        });
        let (x, y) = at.expect("the message is on screen");
        buf[(x, y)].clone()
    };
    let cell = find(&mut h);
    assert_eq!(cell.fg, ratatui::style::Color::Rgb(255, 0, 0));
    assert!(cell.modifier.contains(ratatui::style::Modifier::ITALIC));
    h.app.settings.styles = 0;
    h.app.ui.invalidate();
    let cell = find(&mut h);
    assert_eq!(cell.fg, h.app.theme.text, "styles of others are off: plain text");
}
