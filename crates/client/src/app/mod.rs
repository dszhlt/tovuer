//! Client state and all its logic. The screen is drawn by [`crate::ui`];
//! events (keys, network, background tasks, the timer) arrive here.

pub mod commands;
pub mod emoji;
mod events;
pub mod input;
mod keys;
pub mod model;
pub mod overlay;
pub mod style;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tovuer_proto::{ChannelKind, ClientMsg, FileMeta, Message, User, VideoSource};
use tovuer_voice::{Engine, Sound, StreamInfo};

use self::input::Input;
use self::model::ChannelState;
use self::overlay::{Action, Cmd, Confirm, ConfirmAction, Overlay, Palette, Picker, Prompt, PromptAction, Viewer};
use crate::net::{NetConfig, NetHandle};
use crate::ui::icons::Icon;
use crate::settings::{Paths, Settings, State};
use crate::transfer::{Events, Remote, TaskEvent, Thumb};
use crate::ui::UiState;
use crate::ui::theme::Theme;
use crate::video::{Capture, VideoFrame, source_name};

/// How the program was started: what is baked into the build and what came
/// from the command line.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    pub version: String,
    /// Room to suggest on the login screen.
    pub room: String,
    /// A build for a friend: the room is baked in, the login screen asks only for a nick.
    pub friend: bool,
    /// Nick from the command line — log in without the login screen.
    pub name: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Conn {
    /// Not in a room (the login screen).
    Offline,
    Connecting { attempt: u32, server: String },
    Online,
    /// Could not open the room; waiting for the next try.
    Waiting { reason: String, until: Instant },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub id: u32,
    pub name: String,
    pub admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub tone: Tone,
    pub text: String,
    pub until: Instant,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: chrono::DateTime<chrono::Local>,
    pub tone: Tone,
    pub text: String,
}

/// A file transfer in progress (for the progress bar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub key: String,
    pub name: String,
    pub done: u64,
    pub total: u64,
    pub upload: bool,
}

pub enum ThumbState {
    Loading,
    Ready(Arc<Thumb>),
    Failed,
}

/// A feed thumbnail in the terminal's own graphics.
pub enum SharpThumb {
    /// Being encoded for this many cells.
    Loading((u16, u16)),
    Ready(crate::motion::Pixels),
    /// Blocks stay.
    Failed,
}

/// Feed thumbnails encoded at the same time at most: each decodes a picture.
const SHARP_THUMBS_AT_ONCE: usize = 2;
/// Moving feed thumbnails kept in the terminal's graphics: each holds every
/// frame encoded; the oldest are dropped and encoded again when seen.
const MOVING_KEPT: usize = 6;

/// We are in a voice channel.
#[derive(Debug, Clone)]
pub struct VoiceSession {
    pub channel: String,
    pub since: Instant,
    /// The devices are still opening.
    pub starting: bool,
    pub info: Option<StreamInfo>,
}

/// The call view of our voice channel: everyone in it as tiles, with their
/// camera or screen where they show one.
#[derive(Default)]
pub struct Call {
    /// On screen instead of the feed.
    pub open: bool,
    /// One person big (user id).
    pub focus: Option<u32>,
    /// The tile under the cursor while picking tiles.
    pub sel: usize,
    /// The newest frame of each person showing video (ours under our id).
    pub frames: HashMap<u32, Arc<VideoFrame>>,
    /// Tiles in the terminal's own graphics.
    pub sharp: HashMap<u32, SharpTile>,
    /// A frame came since the last draw.
    pub fresh: bool,
}

/// A video tile in the terminal's own graphics: the last frame encoded.
#[derive(Default)]
pub struct SharpTile {
    pub ready: Option<(u32, Arc<crate::transfer::Sharp>)>,
    /// The size the screen last wanted, in cells.
    pub cells: Option<(u16, u16)>,
    /// A frame is being encoded.
    pub busy: bool,
    /// The terminal could not take it: blocks stay.
    pub failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Sidebar,
    /// Picking a message in the feed: reply, edit, open the attachment…
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Main,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginField {
    Nick,
    Room,
    Password,
}

pub struct LoginForm {
    pub nick: Input,
    pub room: Input,
    pub password: Input,
    pub field: LoginField,
    pub error: Option<String>,
    /// Show the password field (always, unless the room is baked into the build).
    pub ask_password: bool,
    /// Peers refused our protocol version — offer to update (F5).
    pub offer_update: bool,
}

/// A completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What to show in the list.
    pub label: String,
    pub hint: String,
    /// What replaces the typed text.
    pub insert: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte offset in the input where the replaced part starts.
    pub from: usize,
    pub items: Vec<Suggestion>,
    pub sel: usize,
}

/// Where the updated program goes right after it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejoin {
    pub room: String,
    pub nick: String,
    pub password: Option<String>,
}

/// What the input is for: a reply to a message or an edit of our own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compose {
    Reply { id: u64, from: String, preview: String },
    Edit { id: u64 },
}

/// Something to do to the terminal — the main loop does it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Bell,
    Copy(String),
    Title(String),
    Mouse(bool),
}

const TOAST_TTL: Duration = Duration::from_secs(4);
const ERROR_TTL: Duration = Duration::from_secs(7);
const MAX_TOASTS: usize = 4;
const MAX_LOG: usize = 1000;
const TYPING_EVERY: Duration = Duration::from_secs(3);

pub struct App {
    pub launch: Launch,
    pub paths: Paths,
    pub settings: Settings,
    pub state: State,
    pub theme: Theme,

    pub screen: Screen,
    pub login: LoginForm,
    pub conn: Conn,
    /// Room we are in (or joining), normalized name.
    pub room: String,
    pub server_name: String,
    /// Room-specific key for channel state and persistent read marks.
    room_scope: String,
    pub me: Option<Me>,
    pub max_file: u64,
    pub update_available: Option<String>,
    pub latency: Option<Duration>,

    pub channels: Vec<ChannelState>,
    /// Index of the open text channel.
    pub current: usize,
    /// Cursor in the channel list.
    pub side_sel: usize,
    pub users: Vec<User>,
    pub speaking: HashSet<u32>,
    pub voice: Option<VoiceSession>,
    pub call: Call,
    /// Our camera or screen, while it is on.
    pub capture: Option<Capture>,
    /// Whose shared screen we watch above the feed (a nick).
    pub watch: Option<String>,
    /// …over the whole window.
    pub watch_full: bool,

    pub focus: Focus,
    pub input: Input,
    pub compose: Option<Compose>,
    pub completion: Option<Completion>,
    pub overlay: Option<Overlay>,
    pub toasts: VecDeque<Toast>,
    pub log: Vec<LogLine>,
    pub transfers: Vec<Transfer>,
    pub thumbs: HashMap<String, ThumbState>,
    pub sharp_thumbs: HashMap<String, SharpThumb>,
    /// Which of `sharp_thumbs` move, oldest first.
    moving: VecDeque<String>,
    pub updating: Option<(u64, u64)>,

    pub engine: Arc<Engine>,
    pub net: NetHandle,
    pub tasks: Events,
    pub remote: Option<Remote>,
    /// The terminal's own graphics for sharp pictures; `None` — only blocks.
    pub graphics: Option<ratatui_image::picker::Picker>,
    /// Nerd Font glyphs or emoji for the voice icons.
    pub icons: crate::ui::icons::Icons,
    /// ffmpeg is installed: videos get a frame as a preview.
    pub video: bool,

    pub ui: UiState,
    pub effects: Vec<Effect>,
    /// The terminal window has focus.
    pub focused: bool,
    pub dirty: bool,
    /// Animation frame counter (spinners, the "typing" dots).
    pub frame: u64,
    pub quit: bool,
    /// Run this file after exiting (self-update).
    pub restart: Option<PathBuf>,
    /// …straight back into this room.
    pub rejoin: Option<Rejoin>,

    title: String,
    next_upload: u64,
    last_typing: Option<Instant>,
    sent_speaking: bool,
    last_speaking: Instant,
    state_dirty: Option<Instant>,
    last_voice_retry: Option<Instant>,
    welcomed_before: bool,
}

impl App {
    pub fn new(launch: Launch, paths: Paths, engine: Arc<Engine>, net: NetHandle, tasks: Events) -> Self {
        let settings = paths.load_settings();
        let state = paths.load_state();
        let theme = resolve_theme(&settings);

        let mut login = LoginForm {
            nick: Input::new(tovuer_proto::MAX_NAME_CHARS),
            room: Input::new(tovuer_node::room::MAX_ROOM_CHARS),
            password: Input::new(100),
            field: LoginField::Nick,
            error: None,
            ask_password: false,
            offer_update: false,
        };
        login.nick.set(launch.name.as_deref().unwrap_or(&settings.nick));
        let room = if launch.friend || !launch.room.is_empty() { &launch.room } else { &settings.room };
        login.room.set(room);
        let use_saved_password = !launch.friend && crate::net::normalize_room(room) == crate::net::normalize_room(&settings.room);
        let password = launch.password.as_deref().unwrap_or(if use_saved_password { &settings.password } else { "" });
        // the password is part of the room: always ask, unless it is baked in
        login.ask_password = !launch.friend;
        login.password.set(password);

        let mut app = Self {
            room: launch.room.clone(),
            launch,
            paths,
            settings,
            state,
            theme,
            screen: Screen::Login,
            login,
            conn: Conn::Offline,
            server_name: String::new(),
            room_scope: String::new(),
            me: None,
            max_file: 50 << 20,
            update_available: None,
            latency: None,
            channels: Vec::new(),
            current: 0,
            side_sel: 0,
            users: Vec::new(),
            speaking: HashSet::new(),
            voice: None,
            call: Call::default(),
            capture: None,
            watch: None,
            watch_full: false,
            focus: Focus::Input,
            input: Input::new(tovuer_proto::MAX_TEXT_CHARS),
            compose: None,
            completion: None,
            overlay: None,
            toasts: VecDeque::new(),
            log: Vec::new(),
            transfers: Vec::new(),
            thumbs: HashMap::new(),
            sharp_thumbs: HashMap::new(),
            moving: VecDeque::new(),
            updating: None,
            engine,
            net,
            tasks,
            remote: None,
            graphics: None,
            icons: Default::default(),
            video: false,
            ui: UiState::default(),
            effects: Vec::new(),
            focused: true,
            dirty: true,
            frame: 0,
            quit: false,
            restart: None,
            rejoin: None,
            title: String::new(),
            next_upload: 1,
            last_typing: None,
            sent_speaking: false,
            last_speaking: Instant::now(),
            state_dirty: None,
            last_voice_retry: None,
            welcomed_before: false,
        };
        app.apply_audio_settings();
        app.note(Tone::Info, format!("tovuer {} — F1: help, Ctrl+K: commands", app.launch.version));
        if app.launch.name.is_some() {
            app.submit_login();
        }
        app
    }

    // ------------------------------------------------------------------ notices

    /// Write to the system log (Ctrl+L) without a toast.
    pub fn note(&mut self, tone: Tone, text: impl Into<String>) {
        self.log.push(LogLine { at: chrono::Local::now(), tone, text: text.into() });
        if self.log.len() > MAX_LOG {
            let extra = self.log.len() - MAX_LOG;
            self.log.drain(..extra);
        }
        self.dirty = true;
    }

    /// Show a toast (and write it to the log).
    pub fn toast(&mut self, tone: Tone, text: impl Into<String>) {
        let text = text.into();
        self.note(tone, text.clone());
        let ttl = if tone == Tone::Error { ERROR_TTL } else { TOAST_TTL };
        // the same text again is not stacked — it just lasts longer
        if let Some(last) = self.toasts.back_mut()
            && last.text == text
        {
            last.until = Instant::now() + ttl;
            return;
        }
        self.toasts.push_back(Toast { tone, text, until: Instant::now() + ttl });
        while self.toasts.len() > MAX_TOASTS {
            self.toasts.pop_front();
        }
    }

    pub fn ok(&mut self, text: impl Into<String>) {
        self.toast(Tone::Ok, text);
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.toast(Tone::Warn, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.toast(Tone::Error, text);
    }

    // ------------------------------------------------------------------ data access

    pub fn online(&self) -> bool {
        self.conn == Conn::Online
    }

    pub fn my_name(&self) -> &str {
        self.me.as_ref().map_or("", |m| m.name.as_str())
    }

    pub fn my_id(&self) -> u32 {
        self.me.as_ref().map_or(0, |m| m.id)
    }

    pub fn is_admin(&self) -> bool {
        self.me.as_ref().is_some_and(|m| m.admin)
    }

    pub fn channel(&self) -> Option<&ChannelState> {
        self.channels.get(self.current).filter(|c| c.is_text())
    }

    pub fn channel_mut(&mut self) -> Option<&mut ChannelState> {
        self.channels.get_mut(self.current).filter(|c| c.is_text())
    }

    pub fn channel_named(&mut self, name: &str) -> Option<&mut ChannelState> {
        self.channels.iter_mut().find(|c| c.is_text() && c.name == name)
    }

    pub fn channel_name(&self) -> &str {
        self.channel().map_or("", |c| c.name.as_str())
    }

    pub fn user(&self, id: u32) -> Option<&User> {
        self.users.iter().find(|u| u.id == id)
    }

    pub fn user_named(&self, name: &str) -> Option<&User> {
        let lower = name.to_lowercase();
        self.users.iter().find(|u| u.name.to_lowercase() == lower)
    }

    /// Who is in voice channel `name`.
    pub fn voice_members(&self, name: &str) -> Vec<&User> {
        self.users.iter().filter(|u| u.voice.as_deref() == Some(name)).collect()
    }

    /// Everyone but us, alphabetically — for the volume menu.
    pub fn others(&self) -> Vec<&User> {
        let me = self.my_id();
        self.users.iter().filter(|u| u.id != me).collect()
    }

    pub fn muted(&self) -> bool {
        self.engine.controls().muted.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn deafened(&self) -> bool {
        self.engine.controls().deafened.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn total_unread(&self) -> (u32, u32) {
        self.channels.iter().fold((0, 0), |(u, m), c| (u + c.unread, m + c.mentions))
    }

    // ------------------------------------------------------------------ login

    fn login_fields(&self) -> Vec<LoginField> {
        let mut fields = vec![LoginField::Nick];
        if !self.launch.friend {
            fields.push(LoginField::Room);
        }
        if self.login.ask_password {
            fields.push(LoginField::Password);
        }
        fields
    }

    pub fn login_step(&mut self, delta: isize) {
        let fields = self.login_fields();
        let at = fields.iter().position(|f| *f == self.login.field).unwrap_or(0);
        self.login.field = fields[(at as isize + delta).rem_euclid(fields.len() as isize) as usize];
    }

    pub fn login_input(&mut self) -> &mut Input {
        match self.login.field {
            LoginField::Nick => &mut self.login.nick,
            LoginField::Room => &mut self.login.room,
            LoginField::Password => &mut self.login.password,
        }
    }

    /// Check the login form and start joining the room.
    pub fn submit_login(&mut self) {
        let Some(nick) = tovuer_proto::clean_name(self.login.nick.text()) else {
            self.login.error = Some("type a nick — others will see it".into());
            self.login.field = LoginField::Nick;
            return;
        };
        let typed = if self.launch.friend { self.launch.room.clone() } else { self.login.room.text().to_string() };
        let Some(room) = crate::net::normalize_room(&typed) else {
            self.login.error = Some("type a room name — friends who type the same one end up together".into());
            self.login.field = LoginField::Room;
            return;
        };
        let password = if self.launch.friend {
            self.launch.password.clone().unwrap_or_default()
        } else {
            self.login.password.text().to_string()
        };
        self.login.nick.set(&nick);
        self.login.error = None;
        self.login.offer_update = false;

        // Passwordless rooms retain their existing read-mark keys. Protected
        // rooms use the protocol identity, never the display name or password.
        let scope = if password.is_empty() {
            room.clone()
        } else {
            format!("private:{}", tovuer_node::room::RoomKeys::derive(&room, &password).id_hex())
        };
        if self.room_scope != scope {
            self.channels.clear();
            self.current = 0;
            self.side_sel = 0;
            self.input = Input::new(tovuer_proto::MAX_TEXT_CHARS);
            self.compose = None;
            self.completion = None;
            self.focus = Focus::Input;
            self.ui.invalidate();
            self.ui.want_thumbs.clear();
            self.ui.want_sharp = None;
            self.toasts.clear();
            self.log.clear();
            self.thumbs.clear();
            self.transfers.clear();
            self.last_typing = None;
            self.update_available = None;
            self.server_name.clear();
            self.room_scope = scope;
        }

        self.settings.nick = nick.clone();
        if !self.launch.friend {
            self.settings.room = room.clone();
            self.settings.password = password.clone();
            self.login.room.set(&room);
        }
        self.save_settings();

        self.room = room.clone();
        self.screen = Screen::Main;
        self.conn = Conn::Connecting { attempt: 1, server: room.clone() };
        self.net.connect(NetConfig { room, name: nick, password: (!password.is_empty()).then_some(password) });
        self.dirty = true;
    }

    /// Back to the login screen (change nick or room) and stop trying.
    pub fn back_to_login(&mut self, error: Option<String>) {
        self.net.stop();
        self.remote = None;
        self.latency = None;
        self.leave_voice_locally();
        self.conn = Conn::Offline;
        self.screen = Screen::Login;
        self.login.error = error;
        self.overlay = None;
        self.me = None;
        self.users.clear();
        self.speaking.clear();
        self.watch = None;
        // the next entry is a fresh one, not "back in the room"
        self.welcomed_before = false;
        self.dirty = true;
    }

    // ------------------------------------------------------------------ channels

    /// Open the text channel at this index of the full list.
    pub fn open_channel(&mut self, index: usize) {
        if !self.channels.get(index).is_some_and(|c| c.is_text()) {
            return;
        }
        let me = self.my_name().to_string();
        if index != self.current {
            // leaving: keep the unfinished text and drop the "new" line
            let draft = self.input.text().to_string();
            if let Some(old) = self.channel_mut() {
                old.draft = draft;
                old.first_unread = None;
                old.selected = None;
            }
            self.current = index;
            let draft = self.channels[index].draft.clone();
            self.input.set(&draft);
            self.compose = None;
            self.completion = None;
        }
        self.side_sel = index;
        let ch = &mut self.channels[index];
        ch.mark_first_unread(&me);
        ch.scroll = 0;
        ch.anchor = None;
        self.mark_current_read();
        self.dirty = true;
    }

    pub fn open_channel_named(&mut self, name: &str) -> bool {
        let lower = name.trim_start_matches('#').to_lowercase();
        match self.channels.iter().position(|c| c.is_text() && c.name.to_lowercase() == lower) {
            Some(i) => {
                self.open_channel(i);
                true
            }
            None => false,
        }
    }

    /// Next or previous text channel, wrapping around.
    pub fn cycle_channel(&mut self, delta: isize) {
        let texts: Vec<usize> = self.channels.iter().enumerate().filter(|(_, c)| c.is_text()).map(|(i, _)| i).collect();
        if texts.is_empty() {
            return;
        }
        let at = texts.iter().position(|i| *i == self.current).unwrap_or(0);
        self.open_channel(texts[(at as isize + delta).rem_euclid(texts.len() as isize) as usize]);
    }

    /// Mark the open channel read (and remember it between runs).
    pub fn mark_current_read(&mut self) {
        let server = self.room_scope.clone();
        let Some(ch) = self.channel_mut() else { return };
        let had_unread = ch.unread > 0;
        if let Some(id) = ch.mark_read() {
            let name = ch.name.clone();
            if self.state.mark_read(&server, &name, id) {
                self.state_dirty.get_or_insert_with(Instant::now);
            }
        }
        if had_unread {
            self.dirty = true;
        }
    }

    /// The feed is at the bottom and visible — new messages are read right away.
    pub fn reading_bottom(&self) -> bool {
        self.focused
            && self.screen == Screen::Main
            && self.overlay.is_none()
            && !self.call_open()
            && self.channel().is_some_and(|c| c.scroll == 0)
    }

    /// Scroll the feed by `delta` lines (positive — towards older).
    pub fn scroll_chat(&mut self, delta: isize) {
        let (rows, total) = (self.ui.chat_rows as usize, self.ui.chat_total);
        let online = self.online();
        let Some(ch) = self.channel_mut() else { return };
        let max = total.saturating_sub(rows);
        let wanted = (ch.scroll as isize + delta).max(0) as usize;
        // hit the top of what is loaded — ask for an older page
        let request = (wanted >= max && delta > 0 && ch.more && !ch.loading && online).then(|| {
            ch.loading = true;
            ClientMsg::History { channel: ch.name.clone(), before: ch.messages.first().map(|m| m.id), limit: None }
        });
        ch.scroll = wanted.min(max);
        ch.anchor = None;
        if let Some(request) = request {
            self.net.send(request);
        }
        if self.channel().is_some_and(|c| c.scroll == 0) {
            self.mark_current_read();
        }
        self.dirty = true;
    }

    pub fn scroll_to_bottom(&mut self) {
        if let Some(ch) = self.channel_mut() {
            ch.scroll = 0;
            ch.anchor = None;
        }
        self.mark_current_read();
        self.dirty = true;
    }

    /// Move the message selection; load more history at the top.
    pub fn select_message(&mut self, delta: isize) {
        let online = self.online();
        let Some(ch) = self.channel_mut() else { return };
        let hit_top = ch.select_step(delta);
        let request = (hit_top && delta < 0 && ch.more && !ch.loading && online).then(|| {
            ch.loading = true;
            ClientMsg::History { channel: ch.name.clone(), before: ch.messages.first().map(|m| m.id), limit: None }
        });
        if let Some(request) = request {
            self.net.send(request);
        }
        self.dirty = true;
    }

    pub fn set_focus(&mut self, focus: Focus) {
        if focus == Focus::Messages && self.call_open() {
            // in the call the feed is the tiles: pick one
            self.call.sel = self.call.sel.min(self.call_members().len().saturating_sub(1));
        } else if focus == Focus::Messages {
            match self.channel_mut() {
                Some(ch) if !ch.messages.is_empty() => {
                    if ch.selected.is_none() {
                        ch.select_last();
                    }
                    ch.reveal = true;
                }
                _ => return,
            }
        } else if let Some(ch) = self.channel_mut() {
            ch.selected = None;
        }
        if focus == Focus::Sidebar {
            if self.ui.side_hidden {
                // a narrow window has no channel list — the palette stands in for it
                self.open_palette();
                return;
            }
            self.side_sel = self.side_sel.min(self.channels.len().saturating_sub(1));
        }
        self.focus = focus;
        self.completion = None;
        self.dirty = true;
    }

    // ------------------------------------------------------------------ voice

    /// Join a voice channel; joining the same one again leaves it.
    pub fn toggle_voice(&mut self, name: &str) {
        if self.voice.as_ref().is_some_and(|v| v.channel == name) {
            self.leave_voice();
        } else {
            self.join_voice(name);
        }
    }

    pub fn join_voice(&mut self, name: &str) {
        let Some(channel) = self.channels.iter().find(|c| !c.is_text() && c.name.to_lowercase() == name.to_lowercase()) else {
            self.warn(format!("there is no voice channel “{name}”"));
            return;
        };
        let name = channel.name.clone();
        if !self.online() {
            self.warn("not in the room yet — cannot join voice");
            return;
        }
        let me = self.my_id();
        self.call.frames.retain(|id, _| *id == me);
        self.call.sharp.retain(|id, _| *id == me);
        self.call.focus = None;
        self.net.send(ClientMsg::VoiceJoin { channel: name.clone() });
        let running = self.engine.running();
        self.voice = Some(VoiceSession { channel: name, since: Instant::now(), starting: !running, info: self.engine.info() });
        self.send_voice_state();
        self.sync_voice_peers();
        if running {
            self.play(Sound::Join);
        } else {
            self.start_engine();
        }
        self.dirty = true;
    }

    pub fn leave_voice(&mut self) {
        if self.voice.is_none() {
            return;
        }
        self.net.send(ClientMsg::VoiceLeave);
        self.leave_voice_locally();
    }

    /// Close the audio without telling the node (it already knows or is gone).
    pub fn leave_voice_locally(&mut self) {
        if self.voice.take().is_some() {
            self.stop_engine();
            self.sent_speaking = false;
            self.dirty = true;
        }
        // no voice channel, no call: the camera goes off too; a screen
        // watched from the chat stays
        self.capture = None;
        let watched = self.watched().map(|u| u.id);
        let mut call = std::mem::take(&mut self.call);
        call.frames.retain(|id, _| Some(*id) == watched);
        self.call = Call { frames: call.frames, ..Call::default() };
        if self.focus == Focus::Messages && self.channel().is_none_or(|c| c.selected.is_none()) {
            self.focus = Focus::Input;
        }
    }

    fn start_engine(&mut self) {
        let engine = self.engine.clone();
        let tasks = self.tasks.clone();
        let (mic, spk) = (self.settings.mic.clone(), self.settings.spk.clone());
        // opening devices can take hundreds of milliseconds — do not block the interface
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(move || {
                let result = engine.start(&mic, &spk).map_err(|e| e.to_string());
                let _ = tasks.send(TaskEvent::Voice(result));
            });
        }
    }

    fn stop_engine(&mut self) {
        let engine = self.engine.clone();
        if !engine.monitor() {
            if tokio::runtime::Handle::try_current().is_ok() {
                tokio::task::spawn_blocking(move || engine.stop());
            } else {
                engine.stop();
            }
        }
    }

    /// Reopen the devices (the microphone or speakers changed).
    pub fn restart_engine(&mut self) {
        if !self.engine.running() {
            return;
        }
        let engine = self.engine.clone();
        let tasks = self.tasks.clone();
        let (mic, spk) = (self.settings.mic.clone(), self.settings.spk.clone());
        if let Some(v) = self.voice.as_mut() {
            v.starting = true;
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(move || {
                engine.stop();
                let result = engine.start(&mic, &spk).map_err(|e| e.to_string());
                let _ = tasks.send(TaskEvent::Voice(result));
            });
        }
    }

    pub fn play(&self, sound: Sound) {
        if self.settings.sounds {
            self.engine.play(sound);
        }
    }

    fn send_voice_state(&self) {
        if self.voice.is_some() {
            self.net.send(ClientMsg::VoiceState { muted: self.muted(), deafened: self.deafened() });
        }
    }

    pub fn toggle_mute(&mut self) {
        let ctl = self.engine.controls().clone();
        let now = !self.muted();
        if !now && self.deafened() {
            // unmuting the mic while deafened brings the sound back too
            ctl.deafened.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        ctl.muted.store(now, std::sync::atomic::Ordering::Relaxed);
        self.play(if now { Sound::Mute } else { Sound::Unmute });
        self.send_voice_state();
        self.note(Tone::Info, if now { "microphone off" } else { "microphone on" });
    }

    pub fn toggle_deafen(&mut self) {
        let ctl = self.engine.controls().clone();
        let now = !self.deafened();
        ctl.deafened.store(now, std::sync::atomic::Ordering::Relaxed);
        self.play(if now { Sound::Deafen } else { Sound::Undeafen });
        self.send_voice_state();
        self.note(Tone::Info, if now { "sound off: you hear no one and no one hears you" } else { "sound on" });
    }

    // ------------------------------------------------------------------ video

    /// The call view is on screen.
    pub fn call_open(&self) -> bool {
        self.call.open && self.voice.is_some()
    }

    /// What we show: the camera, the screen or nothing.
    pub fn sharing(&self) -> Option<VideoSource> {
        self.capture.as_ref().map(|c| c.source)
    }

    /// Who the call shows, in tile order: us first (even before the node
    /// has confirmed where we are), then the others.
    pub fn call_members(&self) -> Vec<User> {
        let Some(session) = &self.voice else { return Vec::new() };
        let me = self.my_id();
        let mut members: Vec<User> = Vec::new();
        if let Some(user) = self.user(me) {
            members.push(User { voice: Some(session.channel.clone()), video: self.sharing(), ..user.clone() });
        }
        members.extend(self.voice_members(&session.channel).into_iter().filter(|u| u.id != me).cloned());
        members
    }

    /// Someone in our voice channel shows video (and it is not us).
    fn shows_video_to_us(&self, id: u32) -> bool {
        id != self.my_id() && self.user(id).is_some_and(|u| u.video.is_some() && self.in_my_room(u))
    }

    /// Open or close the call view of our voice channel.
    pub fn toggle_call(&mut self) {
        if self.voice.is_none() {
            self.warn("join a voice channel first — the call is there");
            return;
        }
        self.set_call_open(!self.call.open);
    }

    pub fn set_call_open(&mut self, open: bool) {
        if self.voice.is_none() || self.call.open == open {
            return;
        }
        self.call.open = open;
        if !open {
            // the frames of others stop coming: drop them (not of a screen watched from the chat)
            let keep = [Some(self.my_id()), self.watched().map(|u| u.id)];
            self.call.frames.retain(|id, _| keep.contains(&Some(*id)));
            self.call.sharp.clear();
            self.call.focus = None;
            if self.focus == Focus::Messages {
                self.focus = Focus::Input;
            }
        }
        self.send_video_state();
        self.dirty = true;
    }

    /// Tell the node what we show and what we watch.
    pub(crate) fn send_video_state(&self) {
        if self.online() {
            let stream = self.watch.clone();
            self.net.send(ClientMsg::Video { source: self.sharing(), watching: self.call.open && self.voice.is_some(), stream });
        }
    }

    /// Others sharing their screen right now — live streams anyone can watch.
    pub fn live(&self) -> Vec<&User> {
        let me = self.my_id();
        self.users.iter().filter(|u| u.id != me && u.video == Some(VideoSource::Screen) && u.voice.is_some()).collect()
    }

    /// The screen we watch above the feed, while it is shared.
    pub fn watched(&self) -> Option<&User> {
        let nick = self.watch.as_deref()?;
        self.user_named(nick).filter(|u| u.video == Some(VideoSource::Screen) && u.id != self.my_id())
    }

    /// Watch someone's shared screen above the feed (`None` — stop).
    pub fn watch_stream(&mut self, nick: Option<String>) {
        let nick = match nick {
            Some(nick) => match self.user_named(&nick) {
                Some(u) if u.id == self.my_id() => {
                    self.warn("that is your own screen — Alt+C shows it in the call");
                    return;
                }
                Some(u) if u.video == Some(VideoSource::Screen) => Some(u.name.clone()),
                Some(u) => {
                    self.warn(format!("{} is not sharing a screen right now", u.name));
                    return;
                }
                None => {
                    self.warn(format!("nobody called “{nick}” is online"));
                    return;
                }
            },
            None => None,
        };
        if nick == self.watch {
            return;
        }
        if let Some(old) = self.watched().map(|u| u.id) {
            // frames of the call stay; the panel's own go
            if !self.call_open() || !self.shows_video_to_us(old) {
                self.forget_video(old);
            }
        }
        self.watch = nick;
        self.watch_full &= self.watch.is_some();
        if let Some(nick) = &self.watch {
            self.note(Tone::Info, format!("watching {nick}'s screen"));
        }
        self.send_video_state();
        self.dirty = true;
    }

    /// The watched screen fills the window.
    pub fn stream_full(&self) -> bool {
        self.watch_full && self.watched().is_some() && !self.call_open() && self.overlay.is_none()
    }

    /// Alt+F: the watched screen over the whole window, or back to the chat.
    pub fn toggle_stream_full(&mut self) {
        if self.watched().is_none() {
            self.watch_full = false;
            self.warn("nothing to show big — Alt+W watches a shared screen");
            return;
        }
        self.watch_full = !self.watch_full;
        self.dirty = true;
    }

    /// Alt+W: watch a live screen, the next one, or stop.
    pub fn toggle_watch(&mut self) {
        let live: Vec<String> = self.live().iter().map(|u| u.name.clone()).collect();
        let next = match self.watched().map(|u| u.name.clone()) {
            Some(now) => live.iter().position(|n| *n == now).and_then(|i| live.get(i + 1)).cloned(),
            None if live.is_empty() => {
                self.warn("nobody is sharing a screen right now");
                return;
            }
            None => live.first().cloned(),
        };
        self.watch_stream(next);
    }

    /// Turn the camera or the screen on, or off if it is the one on. One at a time.
    pub fn toggle_video(&mut self, source: VideoSource) {
        let on = self.sharing() != Some(source);
        self.set_video(on.then_some(source));
    }

    pub fn set_video(&mut self, source: Option<VideoSource>) {
        if source.is_some() && self.voice.is_none() {
            self.warn("join a voice channel first — video goes to the people there");
            return;
        }
        if self.sharing() == source {
            return;
        }
        let was = self.sharing();
        self.capture = None; // stops what was on
        let me = self.my_id();
        self.call.frames.remove(&me);
        self.call.sharp.remove(&me);
        match source {
            Some(source) => {
                let device = match source {
                    VideoSource::Camera => self.settings.camera.clone(),
                    VideoSource::Screen => self.settings.screen.clone(),
                };
                let net = self.net.clone();
                self.capture = Some(Capture::start(source, &device, move |frame| net.video(frame), self.tasks.clone()));
                let channel = self.voice.as_ref().map(|v| v.channel.clone()).unwrap_or_default();
                match source {
                    VideoSource::Camera => self.ok(format!("camera on — {channel} sees you in the call")),
                    VideoSource::Screen => self.warn(format!("sharing your screen — {channel} sees all of it")),
                }
                // you want to see what you show
                if !self.call.open {
                    self.call.open = true;
                }
            }
            None => {
                if let Some(was) = was {
                    self.note(Tone::Info, format!("{} off", source_name(was)));
                }
            }
        }
        self.send_video_state();
        self.dirty = true;
    }

    /// A video frame: ours, or of someone in the call.
    pub fn on_video(&mut self, from: Option<u32>, frame: Arc<VideoFrame>) {
        let watched = self.watched().map(|u| u.id);
        let id = match from {
            Some(id) if self.call_open() && self.shows_video_to_us(id) => id,
            Some(id) if Some(id) == watched && frame.source == VideoSource::Screen => id,
            None if self.sharing() == Some(frame.source) => self.my_id(),
            _ => return,
        };
        self.call.frames.insert(id, frame);
        self.call.fresh |= self.call_open() || Some(id) == watched;
        // real pixels: start on the frame now rather than at the next draw
        self.encode_sharp_tile(id);
    }

    /// Our camera or screen stopped by itself.
    pub fn on_video_ended(&mut self, source: VideoSource, error: Option<String>) {
        if self.sharing() != Some(source) {
            return;
        }
        self.capture = None;
        let me = self.my_id();
        self.call.frames.remove(&me);
        self.call.sharp.remove(&me);
        self.send_video_state();
        match error {
            Some(e) => self.error(format!("{} off: {e}", source_name(source))),
            None => self.note(Tone::Info, format!("{} off", source_name(source))),
        }
    }

    /// Someone's video went away (off, left the channel, went offline).
    pub fn forget_video(&mut self, id: u32) {
        self.call.frames.remove(&id);
        self.call.sharp.remove(&id);
        if self.call.focus == Some(id) {
            self.call.focus = None;
        }
    }

    /// Make one tile big, or go back to all of them.
    pub fn spotlight(&mut self, id: u32) {
        self.call.focus = if self.call.focus == Some(id) { None } else { Some(id) };
        self.dirty = true;
    }

    /// Apply our personal settings for a peer to the engine.
    fn apply_peer(&self, user: &User) {
        self.engine.set_peer_muted(user.id, self.settings.is_muted(&user.name));
        self.engine.set_peer_volume(user.id, self.settings.volume_of(&user.name));
    }

    /// Bring the engine in line with who is in our voice channel.
    pub fn sync_voice_peers(&self) {
        let Some(session) = &self.voice else { return };
        let me = self.my_id();
        let room: Vec<&User> = self.voice_members(&session.channel).into_iter().filter(|u| u.id != me).collect();
        let ids: Vec<u32> = room.iter().map(|u| u.id).collect();
        self.engine.retain_peers(&ids);
        for user in room {
            self.apply_peer(user);
        }
    }

    pub fn set_peer_muted(&mut self, name: &str, muted: bool) {
        self.settings.muted.retain(|m| m != name);
        if muted {
            self.settings.muted.push(name.to_string());
            self.settings.muted.sort();
        }
        if let Some(user) = self.user_named(name) {
            self.apply_peer(user);
        }
        self.save_settings();
        self.dirty = true;
    }

    pub fn set_peer_volume(&mut self, name: &str, percent: u32) {
        let percent = percent.min(300);
        if percent == 100 {
            self.settings.volumes.remove(name);
        } else {
            self.settings.volumes.insert(name.to_string(), percent);
        }
        if let Some(user) = self.user_named(name) {
            self.apply_peer(user);
        }
        self.save_settings();
        self.dirty = true;
    }

    // ------------------------------------------------------------------ settings

    pub fn save_settings(&mut self) {
        if let Err(e) = self.paths.save_settings(&self.settings) {
            self.note(Tone::Warn, format!("could not save settings: {e}"));
        }
    }

    /// Noise suppression as one word: off, soft, normal or strong.
    pub fn denoise_mode(&self) -> &'static str {
        if self.settings.denoise { tovuer_voice::Strength::from_u8(self.settings.denoise_level).name() } else { "off" }
    }

    /// Set noise suppression by name; `false` — no such mode.
    pub fn set_denoise_mode(&mut self, mode: &str) -> bool {
        let (on, level) = match mode {
            "off" => (false, self.settings.denoise_level),
            "soft" => (true, 0),
            "normal" | "on" => (true, 1),
            "strong" => (true, 2),
            "ai" => (true, 3),
            _ => return false,
        };
        self.settings.denoise = on;
        self.settings.denoise_level = level;
        self.apply_audio_settings();
        self.save_settings();
        true
    }

    /// Hand volumes, noise suppression and the rest from settings to the engine.
    pub fn apply_audio_settings(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let ctl = self.engine.controls();
        ctl.denoise.store(self.settings.denoise, Relaxed);
        ctl.set_denoise_strength(tovuer_voice::Strength::from_u8(self.settings.denoise_level));
        ctl.agc.store(self.settings.agc, Relaxed);
        ctl.set_mic_gain(self.settings.mic_gain as f32 / 100.0);
        ctl.set_out_gain(self.settings.volume as f32 / 100.0);
        ctl.set_vad_threshold(self.settings.vad_threshold());
    }

    pub fn set_theme(&mut self, name: &str) -> bool {
        if !Theme::exists(name) {
            return false;
        }
        self.settings.theme = name.to_string();
        self.refresh_theme();
        self.save_settings();
        true
    }

    pub fn refresh_theme(&mut self) {
        self.theme = resolve_theme(&self.settings);
        self.ui.invalidate();
        self.dirty = true;
    }

    // ------------------------------------------------------------------ files

    /// Share a file in the open channel.
    pub fn upload(&mut self, path: PathBuf) {
        let Some(remote) = self.remote.clone().filter(|_| self.online()) else {
            self.warn("not in the room — cannot send the file");
            return;
        };
        let Some(channel) = self.channel().map(|c| c.name.clone()) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let id = self.next_upload;
        self.next_upload += 1;
        let reply = match self.compose.take() {
            Some(Compose::Reply { id, .. }) => Some(id),
            other => {
                self.compose = other;
                None
            }
        };
        self.transfers.push(Transfer { key: format!("up{id}"), name, done: 0, total: 0, upload: true });
        remote.upload(id, path, channel, String::new(), reply);
        self.dirty = true;
    }

    /// Find a file by name: as typed first, then in the usual folders.
    pub fn resolve_file(typed: &str) -> PathBuf {
        let path = crate::settings::expand_home(typed.trim().trim_matches('"'));
        if path.exists() || path.components().count() > 1 {
            return path;
        }
        let home = dirs::home_dir();
        let places = [
            std::env::current_dir().ok(),
            dirs::picture_dir(),
            dirs::download_dir(),
            dirs::desktop_dir(),
            dirs::document_dir(),
            home,
        ];
        places.into_iter().flatten().map(|dir| dir.join(&path)).find(|p| p.exists()).unwrap_or(path)
    }

    pub fn save_file(&mut self, file: FileMeta, target: Option<PathBuf>) {
        let Some(remote) = self.remote.clone() else {
            self.warn("not in the room — cannot download");
            return;
        };
        self.transfers.push(Transfer { key: file.id.clone(), name: file.name.clone(), done: 0, total: file.size, upload: false });
        remote.save(file, target);
        self.dirty = true;
    }

    /// Open an attachment: a picture full screen, a video in the system's
    /// player, anything else is downloaded.
    pub fn open_file(&mut self, file: FileMeta) {
        if crate::transfer::is_video(&file) {
            self.open_outside(file);
            return;
        }
        if !file.image {
            self.save_file(file, None);
            return;
        }
        let Some(remote) = self.remote.clone() else {
            self.warn("not in the room — cannot open the picture");
            return;
        };
        self.overlay = Some(Overlay::Viewer(Viewer::new(file.clone())));
        remote.picture(file);
        self.dirty = true;
    }

    /// Open the original file with the system's app: an image viewer shows a
    /// photo in full quality, whatever the terminal can draw.
    pub fn open_outside(&mut self, file: FileMeta) {
        let Some(remote) = self.remote.clone() else {
            self.warn("not in the room — cannot open the file");
            return;
        };
        self.transfers.push(Transfer { key: file.id.clone(), name: file.name.clone(), done: 0, total: file.size, upload: false });
        remote.open_outside(file);
        self.dirty = true;
    }

    /// Whether pictures are drawn with the terminal's own graphics.
    pub fn sharp_pictures(&self) -> bool {
        self.settings.sharp && self.graphics.is_some()
    }

    /// Pictures of the open channel in order — for flipping in the viewer.
    pub fn images(&self) -> Vec<FileMeta> {
        self.channel()
            .map(|c| c.messages.iter().filter_map(|m| m.file.clone()).filter(|f| f.image).collect())
            .unwrap_or_default()
    }

    /// A moving thumbnail was encoded: keep it, dropping the oldest beyond the limit.
    pub fn keep_moving(&mut self, file_id: &str) {
        self.moving.retain(|id| id != file_id);
        self.moving.push_back(file_id.to_string());
        while self.moving.len() > MOVING_KEPT {
            if let Some(old) = self.moving.pop_front() {
                self.sharp_thumbs.remove(&old);
            }
        }
    }

    /// GIFs and videos play in the feed, or stand still.
    pub fn set_animate(&mut self, on: bool) {
        self.settings.animate = on;
        // real pixels were encoded moving or still: again, the other way
        self.sharp_thumbs.clear();
        self.moving.clear();
        self.save_settings();
        self.dirty = true;
    }

    /// Whether moving previews move right now: on, and the window is looked at.
    pub fn animating_previews(&self) -> bool {
        self.settings.animate && self.focused
    }

    /// The interface wants thumbnails of these pictures — queue them.
    pub fn fetch_wanted_thumbs(&mut self) {
        let wanted = std::mem::take(&mut self.ui.want_thumbs);
        let Some(remote) = self.remote.clone() else { return };
        if let (Some((file, area)), Some(graphics)) = (self.ui.want_sharp.take(), self.graphics.clone()) {
            let backdrop = match self.theme.raised {
                ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
                _ => [24, 24, 28],
            };
            remote.sharp(file, graphics, area, backdrop);
        }
        self.encode_sharp_video();
        if let Some(graphics) = self.graphics.clone() {
            for (file, cells) in std::mem::take(&mut self.ui.want_sharp_thumbs) {
                let busy = self.sharp_thumbs.values().filter(|s| matches!(s, SharpThumb::Loading(_))).count();
                if busy >= SHARP_THUMBS_AT_ONCE {
                    break; // the rest is asked for again on the next frame
                }
                if matches!(self.sharp_thumbs.get(&file.id), Some(SharpThumb::Loading(c)) if *c == cells) {
                    continue;
                }
                self.sharp_thumbs.insert(file.id.clone(), SharpThumb::Loading(cells));
                remote.sharp_thumb(file, graphics.clone(), cells, self.settings.animate);
            }
        }
        for file in wanted {
            if self.thumbs.contains_key(&file.id) {
                continue;
            }
            self.thumbs.insert(file.id.clone(), ThumbState::Loading);
            remote.thumb(file);
        }
    }

    /// The video tiles the screen asked for in the terminal's own graphics.
    fn encode_sharp_video(&mut self) {
        for (user, cells) in std::mem::take(&mut self.ui.want_sharp_video) {
            self.call.sharp.entry(user).or_default().cells = Some(cells);
            self.encode_sharp_tile(user);
        }
    }

    /// Encode the newest frame of a tile for the terminal's own graphics,
    /// unless one is being encoded already (it asks again when done).
    pub fn encode_sharp_tile(&mut self, user: u32) {
        let shown = self.call_open() || self.watched().is_some_and(|u| u.id == user);
        let Some(graphics) = self.graphics.clone().filter(|_| self.sharp_pictures() && shown) else { return };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let Some(frame) = self.call.frames.get(&user).cloned() else { return };
        let Some(tile) = self.call.sharp.get_mut(&user) else { return };
        let Some(cells) = tile.cells else { return };
        let current = tile.ready.as_ref().is_some_and(|(seq, s)| *seq == frame.seq && s.size() == cells);
        if tile.busy || tile.failed || current {
            return;
        }
        tile.busy = true;
        let backdrop = match self.theme.raised {
            ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
            _ => [24, 24, 28],
        };
        let mirror = user == self.my_id() && frame.source == VideoSource::Camera;
        let tasks = self.tasks.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::video::encode_sharp(&frame.image, mirror, &graphics, cells.0, cells.1, backdrop, crate::video::kitty_id(user));
            let _ = tasks.send(TaskEvent::SharpVideo { user, seq: frame.seq, cells, result: result.map(Arc::new) });
        });
    }

    /// `/update`: the newest release from GitHub. Works outside a room too.
    pub fn start_update(&mut self) {
        if self.updating.is_some() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            self.error("cannot update right now");
            return;
        }
        self.updating = Some((0, 0));
        self.toast(Tone::Info, "looking for a new version on GitHub…");
        crate::update::update(self.launch.version.clone(), self.tasks.clone());
    }

    /// Before the updated program takes over: back into this room, and a
    /// build with the room built in keeps it — the update is a general build.
    fn prepare_restart(&mut self) {
        let password = if self.launch.friend { self.launch.password.clone() } else { Some(self.settings.password.clone()) };
        let password = password.filter(|p| !p.is_empty());
        if self.launch.friend {
            self.settings.room = self.launch.room.clone();
            self.settings.password = password.clone().unwrap_or_default();
            self.save_settings();
        }
        self.rejoin = (self.screen == Screen::Main && !self.room.is_empty() && self.me.is_some())
            .then(|| Rejoin { room: self.room.clone(), nick: self.my_name().to_string(), password });
    }

    // ------------------------------------------------------------------ messages

    /// Start a reply to a message.
    pub fn reply_to(&mut self, msg: &Message) {
        let quote = match (&msg.file, msg.text.is_empty()) {
            (Some(f), true) => f.name.clone(),
            _ => msg.text.clone(),
        };
        self.compose = Some(Compose::Reply { id: msg.id, from: msg.from.clone(), preview: tovuer_proto::one_line(&quote, 80) });
        self.set_focus(Focus::Input);
    }

    /// Start editing our own message.
    pub fn edit_message(&mut self, msg: &Message) {
        if msg.from != self.my_name() || msg.system {
            self.warn("you can only edit your own messages");
            return;
        }
        if msg.text.is_empty() {
            self.warn("this message has no text to edit");
            return;
        }
        self.compose = Some(Compose::Edit { id: msg.id });
        self.input.set(&msg.text);
        self.set_focus(Focus::Input);
    }

    pub fn ask_delete(&mut self, msg: &Message) {
        if msg.from != self.my_name() && !self.is_admin() {
            self.warn("you can only delete your own messages");
            return;
        }
        let what = match (&msg.file, msg.text.is_empty()) {
            (Some(f), true) => f.name.clone(),
            _ => tovuer_proto::one_line(&msg.text, 60),
        };
        self.overlay = Some(Overlay::Confirm(Confirm {
            title: "Delete the message?".into(),
            text: format!("{}: {what}", msg.from),
            action: ConfirmAction::DeleteMessage(msg.id),
        }));
        self.dirty = true;
    }

    /// Our last message with text — for "edit the last one".
    pub fn my_last_message(&self) -> Option<Message> {
        let me = self.my_name();
        self.channel()?.messages.iter().rev().find(|m| m.from == me && !m.system && !m.text.is_empty()).cloned()
    }

    /// Send what was typed: a command, an edit or a plain message.
    pub fn submit(&mut self) {
        let raw = self.input.submit();
        self.completion = None;
        let text = raw.trim();
        if text.is_empty() {
            // an empty Enter while editing or replying just cancels
            self.compose = None;
            self.dirty = true;
            return;
        }
        if let Some(Compose::Edit { id }) = self.compose.clone() {
            self.compose = None;
            if self.online() {
                self.net.send(ClientMsg::Edit { id, text: emoji::expand(text) });
            } else {
                self.warn("not in the room — the edit was not sent");
                self.input.set(&raw);
                self.compose = Some(Compose::Edit { id });
            }
            return;
        }
        if text.starts_with('/') && !text.starts_with("//") {
            self.run_command(text);
            return;
        }
        // "#channel" jumps to that channel
        if let Some(name) = text.strip_prefix('#')
            && !name.contains(char::is_whitespace)
            && self.open_channel_named(name)
        {
            return;
        }
        let Some(channel) = self.channel().map(|c| c.name.clone()) else { return };
        if !self.online() {
            // keep what was typed: put it back into the input
            self.input.set(&raw);
            self.warn("not in the room — the message was not sent");
            return;
        }
        let reply_to = match self.compose.take() {
            Some(Compose::Reply { id, .. }) => Some(id),
            _ => None,
        };
        let body = text.strip_prefix('/').filter(|_| text.starts_with("//")).unwrap_or(text);
        let style = self.settings.style.clean();
        self.net.send(ClientMsg::Send { channel, text: emoji::expand(body), reply_to, style });
        self.last_typing = None;
        if let Some(ch) = self.channel_mut() {
            ch.first_unread = None;
            ch.draft.clear();
        }
        self.scroll_to_bottom();
    }

    /// Tell the others "typing…" (once every few seconds at most).
    pub fn typing_ping(&mut self) {
        let text = self.input.text();
        if text.trim().is_empty() || text.starts_with('/') || !self.online() || self.compose.as_ref().is_some_and(|c| matches!(c, Compose::Edit { .. })) {
            return;
        }
        if self.last_typing.is_some_and(|t| t.elapsed() < TYPING_EVERY) {
            return;
        }
        let Some(channel) = self.channel().map(|c| c.name.clone()) else { return };
        self.last_typing = Some(Instant::now());
        self.net.send(ClientMsg::Typing { channel });
    }

    // ------------------------------------------------------------------ overlays

    pub fn open_picker(&mut self) {
        let dir = match &self.overlay {
            Some(Overlay::Picker(p)) => p.dir.clone(),
            _ => self.ui.last_dir.clone().or_else(dirs::home_dir).or_else(|| std::env::current_dir().ok()).unwrap_or_default(),
        };
        self.overlay = Some(Overlay::Picker(Picker::open(&dir)));
        self.peek_current();
        self.dirty = true;
    }

    /// Ask for a preview if the file picker's cursor is on a picture.
    pub fn peek_current(&mut self) {
        let tasks = self.tasks.clone();
        let graphics = self.graphics.clone().filter(|_| self.settings.sharp);
        let video = self.video;
        let Some(Overlay::Picker(picker)) = self.overlay.as_mut() else { return };
        // a video is not read whole for a frame: any size will do
        let target = picker
            .current()
            .filter(|e| !e.dir && ((tovuer_proto::is_image_name(&e.name) && e.size <= 32 << 20) || (video && tovuer_proto::is_video_name(&e.name))))
            .and_then(|_| picker.current_path());
        match target {
            Some(path) => {
                if picker.peek.as_ref().is_some_and(|(p, _)| *p == path) {
                    return;
                }
                picker.peek = Some((path.clone(), overlay::Peek::Loading));
                if tokio::runtime::Handle::try_current().is_ok() {
                    crate::transfer::peek(tasks, path, 40, 16, graphics);
                }
            }
            None => picker.peek = None,
        }
    }

    pub fn open_settings(&mut self) {
        self.overlay = Some(Overlay::Settings(overlay::SettingsView::default()));
        let tasks = self.tasks.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            let cams = tasks.clone();
            tokio::task::spawn_blocking(move || {
                let result = tovuer_voice::list_devices().map_err(|e| e.to_string());
                let _ = tasks.send(TaskEvent::Devices(result));
            });
            tokio::task::spawn_blocking(move || {
                let names = crate::video::cameras().into_iter().map(|c| c.name).collect();
                let _ = cams.send(TaskEvent::Cameras(names));
            });
        }
        self.dirty = true;
    }

    pub fn open_palette(&mut self) {
        self.overlay = Some(Overlay::Palette(Palette::new(self.palette_actions())));
        self.dirty = true;
    }

    fn palette_actions(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        let act = |icon: &'static str, label: String, hint: &str, cmd: Cmd| Action { icon, label, hint: hint.to_string(), cmd };
        for ch in self.channels.iter().filter(|c| c.is_text()) {
            let hint = if ch.unread > 0 { format!("{} new", ch.unread) } else { String::new() };
            actions.push(act("#", format!("Go to #{}", ch.name), &hint, Cmd::Channel(ch.name.clone())));
        }
        let watched = self.watched().map(|u| u.name.clone());
        for user in self.live() {
            if Some(&user.name) != watched.as_ref() {
                let label = format!("Watch {}'s screen", user.name);
                actions.push(act(self.icons.get(Icon::Screen), label, "Alt+W", Cmd::Watch(Some(user.name.clone()))));
            }
        }
        if let Some(name) = watched {
            actions.push(act(self.icons.get(Icon::Screen), format!("Stop watching {name}'s screen"), "/watch off", Cmd::Watch(None)));
        }
        if self.voice.is_some() {
            let call = if self.call.open { "Back to the chat" } else { "Open the call: cameras and screens" };
            actions.push(act(self.icons.get(Icon::Voice), call.into(), "Alt+C", Cmd::Call));
            let camera = if self.sharing() == Some(VideoSource::Camera) { "Turn the camera off" } else { "Turn the camera on" };
            actions.push(act(self.icons.get(Icon::Camera), camera.into(), "Alt+V", Cmd::Video(VideoSource::Camera)));
            let screen = if self.sharing() == Some(VideoSource::Screen) { "Stop sharing the screen" } else { "Share your screen" };
            actions.push(act(self.icons.get(Icon::Screen), screen.into(), "Alt+S", Cmd::Video(VideoSource::Screen)));
            actions.push(act(self.icons.get(Icon::Muted), "Leave the voice channel".into(), "", Cmd::LeaveVoice));
        }
        for ch in self.channels.iter().filter(|c| !c.is_text()) {
            if self.voice.as_ref().is_some_and(|v| v.channel == ch.name) {
                continue;
            }
            let inside = self.voice_members(&ch.name).len();
            let hint = if inside > 0 { format!("{inside} inside") } else { String::new() };
            actions.push(act(self.icons.get(Icon::Voice), format!("Join voice {}", ch.name), &hint, Cmd::Voice(ch.name.clone())));
        }
        let mic = if self.muted() { "Turn the microphone on" } else { "Turn the microphone off" };
        actions.push(act(self.icons.get(Icon::Mic), mic.into(), "Alt+M", Cmd::Mute));
        let sound = if self.deafened() { "Turn sound on" } else { "Turn sound off (hear no one)" };
        actions.push(act(self.icons.get(Icon::Sound), sound.into(), "Alt+D", Cmd::Deafen));
        actions.push(act(self.icons.get(Icon::Attachment), "Send a file…".into(), "Ctrl+O", Cmd::Upload));
        actions.push(act(self.icons.get(Icon::Voice), "Peer volumes…".into(), "F3", Cmd::Users));
        actions.push(act(self.icons.get(Icon::Settings), "Settings…".into(), "F2", Cmd::Settings));
        actions.push(act(self.icons.get(Icon::Edit), "Your message style: color, gradient, markup…".into(), "F4", Cmd::Style));
        actions.push(act("#", "Create a text channel…".into(), "/new", Cmd::NewChannel(ChannelKind::Text)));
        actions.push(act(self.icons.get(Icon::Voice), "Create a voice channel…".into(), "/newvoice", Cmd::NewChannel(ChannelKind::Voice)));
        let members = if self.settings.members { "Hide the online panel" } else { "Show the online panel" };
        actions.push(act("☰", members.into(), "", Cmd::ToggleMembers));
        for name in Theme::names() {
            if name != self.settings.theme {
                actions.push(act(self.icons.get(Icon::Theme), format!("Theme: {name}"), "", Cmd::Theme(name.to_string())));
            }
        }
        // deleting is rarer than anything above: at the end, found by typing "delete"
        if self.is_admin() {
            for ch in &self.channels {
                let label = if ch.is_text() { format!("Delete channel #{}…", ch.name) } else { format!("Delete voice channel {}…", ch.name) };
                actions.push(act("×", label, "/delchannel", Cmd::DeleteChannel(ch.name.clone())));
            }
        }
        actions.push(act("↻", "Update tovuer from GitHub".into(), "/update", Cmd::Update));
        actions.push(act("↻", "Rejoin the room".into(), "", Cmd::Reconnect));
        actions.push(act("≡", "System log".into(), "Ctrl+L", Cmd::Log));
        actions.push(act("?", "Help".into(), "F1", Cmd::Help));
        actions.push(act("×", "Quit tovuer".into(), "Ctrl+Q", Cmd::Quit));
        actions
    }

    pub fn run_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Channel(name) => {
                self.open_channel_named(&name);
                self.set_focus(Focus::Input);
            }
            Cmd::Voice(name) => self.join_voice(&name),
            Cmd::Call => self.toggle_call(),
            Cmd::Watch(nick) => self.watch_stream(nick),
            Cmd::Video(source) => self.toggle_video(source),
            Cmd::LeaveVoice => self.leave_voice(),
            Cmd::Mute => self.toggle_mute(),
            Cmd::Deafen => self.toggle_deafen(),
            Cmd::Upload => self.open_picker(),
            Cmd::Settings => self.open_settings(),
            Cmd::Style => self.open_style(),
            Cmd::Users => self.overlay = Some(Overlay::Users { sel: 0 }),
            Cmd::Theme(name) => {
                self.set_theme(&name);
            }
            Cmd::NewChannel(kind) => self.prompt_new_channel(kind),
            Cmd::DeleteChannel(name) => self.ask_delete_channel(&name),
            Cmd::ToggleMembers => {
                self.settings.members = !self.settings.members;
                self.save_settings();
            }
            Cmd::Help => self.overlay = Some(Overlay::Help { scroll: 0 }),
            Cmd::Log => self.overlay = Some(Overlay::Log { scroll: 0 }),
            Cmd::Update => self.start_update(),
            Cmd::Reconnect => self.reconnect(),
            Cmd::Quit => self.quit = true,
        }
        self.dirty = true;
    }

    pub fn reconnect(&mut self) {
        let Some(me) = self.me.as_ref().map(|m| m.name.clone()).or_else(|| Some(self.settings.nick.clone()).filter(|n| !n.is_empty())) else {
            return;
        };
        let password = if self.launch.friend { self.launch.password.clone().unwrap_or_default() } else { self.settings.password.clone() };
        self.conn = Conn::Connecting { attempt: 1, server: self.room.clone() };
        self.net.connect(NetConfig { room: self.room.clone(), name: me, password: (!password.is_empty()).then_some(password) });
        self.dirty = true;
    }

    /// Install the `tovuer` terminal command (same as `tovuer --install`).
    pub fn install_command(&mut self) {
        match crate::install::install() {
            Ok(done) => {
                let ready = done.path_via.is_empty();
                for line in done.lines() {
                    self.note(Tone::Info, line);
                }
                self.ok(if ready {
                    "done: start tovuer by typing tovuer"
                } else {
                    "done: type tovuer in a new terminal window"
                });
            }
            Err(e) => self.error(format!("could not install the command: {e:#}")),
        }
    }

    pub fn prompt_new_channel(&mut self, kind: ChannelKind) {
        let title = match kind {
            ChannelKind::Text => "New text channel",
            ChannelKind::Voice => "New voice channel",
        };
        self.overlay = Some(Overlay::Prompt(Prompt {
            title: title.into(),
            hint: "name: letters, digits, dashes".into(),
            input: Input::new(tovuer_proto::MAX_CHANNEL_CHARS),
            action: PromptAction::NewChannel(kind),
        }));
        self.dirty = true;
    }

    /// Terminal window title: the channel and the unread count.
    pub fn refresh_title(&mut self) {
        let (unread, mentions) = self.total_unread();
        let mark = match (mentions, unread) {
            (0, 0) => String::new(),
            (0, n) => format!("({n}) "),
            (m, _) => format!("(@{m}) "),
        };
        let place = match self.screen {
            Screen::Main if self.me.is_some() => format!(" — #{}", self.channel_name()),
            _ => String::new(),
        };
        let title = format!("{mark}tovuer{place}");
        if title != self.title {
            self.title = title.clone();
            self.effects.push(Effect::Title(title));
        }
    }

    /// Save everything unsaved (before quitting).
    pub fn flush(&mut self) {
        let draft = self.input.text().to_string();
        if let Some(ch) = self.channel_mut() {
            ch.draft = draft;
        }
        if self.state_dirty.take().is_some() {
            let _ = self.paths.save_state(&self.state);
        }
    }
}

fn resolve_theme(settings: &Settings) -> Theme {
    let theme = Theme::by_name(&settings.theme);
    if settings.transparent { theme.transparent() } else { theme }
}
