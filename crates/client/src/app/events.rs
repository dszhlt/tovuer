//! Reacting to events: the network, background tasks, the timer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tovuer_proto::{Channel, ClientMsg, ErrorCode, Message, ServerMsg, User};
use tovuer_voice::Sound;

use super::model::ChannelState;
use super::overlay::{Overlay, Peek};
use super::{App, Conn, Effect, Focus, LoginField, Me, Screen, ThumbState, Tone, Transfer};
use crate::net::NetEvent;
use crate::transfer::{Remote, TaskEvent};

/// "Talking / quiet" goes out no more often than this — not an event per syllable.
const SPEAKING_MIN_GAP: Duration = Duration::from_millis(150);
/// Read marks are written to disk no more often than this.
const STATE_SAVE_DELAY: Duration = Duration::from_secs(2);
const VOICE_RETRY_GAP: Duration = Duration::from_secs(5);

impl App {
    pub fn on_session_event(&mut self, (session, event): (u64, NetEvent)) {
        if self.net.is_current(session) {
            self.on_net(event);
        }
    }

    pub fn on_net(&mut self, event: NetEvent) {
        self.dirty = true;
        match event {
            NetEvent::Connecting { attempt, server } => {
                self.room = server.clone();
                self.conn = Conn::Connecting { attempt, server };
            }
            NetEvent::Latency(rtt) => self.latency = rtt,
            NetEvent::Lost { reason, retry_in } => {
                let was_online = self.online();
                self.conn = Conn::Waiting { reason: reason.clone(), until: Instant::now() + retry_in };
                self.latency = None;
                self.speaking.clear();
                for ch in &mut self.channels {
                    ch.typing.clear();
                    ch.loading = false;
                }
                if was_online {
                    self.warn(format!("lost the room: {reason} — reconnecting"));
                } else {
                    self.note(Tone::Warn, format!("could not join: {reason}"));
                }
            }
            NetEvent::Rejected { code, msg } => self.on_rejected(code, msg),
            NetEvent::Msg(msg) => self.on_server(*msg),
        }
    }

    fn on_rejected(&mut self, code: ErrorCode, msg: String) {
        self.back_to_login(Some(msg));
        match code {
            ErrorCode::ProtoMismatch => self.login.offer_update = true,
            _ => self.login.field = LoginField::Nick,
        }
    }

    /// Bring the channel list in line with the new one, keeping the state of known channels.
    fn set_channels(&mut self, channels: Vec<Channel>, refresh: bool) {
        let current = self.channel().map(|c| c.name.clone());
        let side = self.channels.get(self.side_sel).map(|c| (c.name.clone(), c.kind));
        let mut old: Vec<ChannelState> = std::mem::take(&mut self.channels);
        self.channels = channels
            .iter()
            .map(|ch| match old.iter().position(|o| o.name == ch.name && o.kind == ch.kind) {
                Some(i) => {
                    let mut kept = old.swap_remove(i);
                    kept.topic = ch.topic.clone();
                    kept
                }
                None => ChannelState::new(ch),
            })
            .collect();
        let first_text = self.channels.iter().position(|c| c.is_text()).unwrap_or(0);
        let reopened = current.and_then(|name| self.channels.iter().position(|c| c.is_text() && c.name == name));
        self.current = reopened.unwrap_or(first_text);
        if reopened.is_none() {
            // the open channel was deleted — its unfinished text has nowhere to go
            self.compose = None;
            self.completion = None;
            self.input.set(self.channels.get(self.current).map_or("", |c| c.draft.as_str()));
            if self.focus == Focus::Messages {
                self.focus = Focus::Input;
            }
        }
        self.side_sel = side
            .and_then(|(name, kind)| self.channels.iter().position(|c| c.name == name && c.kind == kind))
            .unwrap_or(self.current);
        self.ui.invalidate();
        for ch in self.channels.iter_mut().filter(|c| c.is_text()) {
            if refresh || (!ch.loaded && !ch.loading) {
                ch.last_read = self.state.last_read(&self.room_scope, &ch.name).unwrap_or(0);
                ch.loaded = false;
                ch.loading = true;
                self.net.send(ClientMsg::History { channel: ch.name.clone(), before: None, limit: None });
            }
        }
    }

    fn on_server(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { id, name, server, admin, channels, users, max_file, client_version, motd, .. } => {
                let reconnect = self.welcomed_before && self.server_name == server;
                self.me = Some(Me { id, name: name.clone(), admin });
                self.server_name = server;
                self.max_file = max_file;
                self.conn = Conn::Online;
                if self.room_scope.is_empty() {
                    self.room_scope = self.room.clone();
                }
                self.set_channels(channels, true);
                self.users = users;
                self.sort_users();
                self.speaking.clear();
                self.remote = self.net.node().map(|node| Remote { node, events: self.tasks.clone(), paths: self.paths.clone() });

                // a restarted node knows nothing about our voice channel — tell it again
                match self.voice.as_ref().map(|v| v.channel.clone()) {
                    Some(channel) if self.channels.iter().any(|c| !c.is_text() && c.name == channel) => {
                        self.net.send(ClientMsg::VoiceJoin { channel });
                        self.net.send(ClientMsg::VoiceState { muted: self.muted(), deafened: self.deafened() });
                        self.sync_voice_peers();
                    }
                    Some(_) => self.leave_voice_locally(),
                    None => {}
                }

                if reconnect {
                    self.ok("back in the room");
                } else {
                    let role = if admin { " (owner)" } else { "" };
                    self.ok(format!("you are in as {name}{role}"));
                    if let Some(motd) = motd.filter(|m| !m.trim().is_empty()) {
                        self.toast(Tone::Info, motd);
                    }
                }
                self.welcomed_before = true;
                match client_version {
                    Some(v) if v != self.launch.version => {
                        if self.update_available.as_ref() != Some(&v) {
                            self.toast(Tone::Info, format!("the room owner has version {v} — update with /update"));
                        }
                        self.update_available = Some(v);
                    }
                    _ => self.update_available = None,
                }
            }

            ServerMsg::Error { msg, .. } => self.error(msg),

            ServerMsg::UserJoin { user } => {
                self.note(Tone::Info, format!("{} came online", user.name));
                self.users.retain(|u| u.id != user.id);
                self.users.push(user);
                self.sort_users();
            }

            ServerMsg::UserLeave { id } => {
                self.speaking.remove(&id);
                if let Some(at) = self.users.iter().position(|u| u.id == id) {
                    let gone = self.users.remove(at);
                    self.note(Tone::Info, format!("{} went offline", gone.name));
                    for ch in &mut self.channels {
                        ch.clear_typing(&gone.name);
                    }
                    if self.in_my_room(&gone) {
                        self.play(Sound::Leave);
                        self.sync_voice_peers();
                    }
                }
            }

            ServerMsg::UserUpdate { user } => self.on_user_update(user),

            ServerMsg::Msg { msg } => self.on_message(msg),

            ServerMsg::MsgEdit { channel, id, text, edited } => {
                if let Some(ch) = self.channel_named(&channel) {
                    ch.edit(id, text, edited);
                }
            }

            ServerMsg::MsgDelete { channel, id } => {
                let me = self.my_name().to_string();
                if let Some(ch) = self.channel_named(&channel) {
                    ch.remove(id);
                    ch.recount(&me);
                }
                if matches!(self.compose, Some(super::Compose::Reply { id: rid, .. } | super::Compose::Edit { id: rid }) if rid == id) {
                    self.compose = None;
                }
                if self.focus == Focus::Messages && self.channel().is_some_and(|c| c.selected.is_none()) {
                    self.set_focus(Focus::Input);
                }
            }

            ServerMsg::MsgReact { channel, id, reactions } => {
                if let Some(ch) = self.channel_named(&channel) {
                    ch.set_reactions(id, reactions);
                }
            }

            ServerMsg::History { channel, before, messages, more } => {
                let me = self.my_name().to_string();
                let server = self.room_scope.clone();
                let known = self.state.last_read(&server, &channel).is_some();
                let is_current = self.channel_name() == channel;
                let Some(ch) = self.channel_named(&channel) else { return };
                let first_page = !ch.loaded;
                if before.is_some() && first_page {
                    return; // an old pagination request overtaken by a refresh
                }
                ch.merge_history(before, messages, more);
                if first_page && !known {
                    // first time in this room: old messages do not count as unread
                    ch.last_read = ch.last_id();
                }
                ch.recount(&me);
                if first_page && is_current {
                    ch.mark_first_unread(&me);
                }
                let last = ch.last_id();
                let lost_selection = is_current && ch.selected.is_none();
                if lost_selection && self.focus == Focus::Messages {
                    self.set_focus(Focus::Input);
                }
                if first_page && !known && last > 0 && self.state.mark_read(&server, &channel, last) {
                    self.state_dirty.get_or_insert_with(Instant::now);
                }
                if is_current && self.reading_bottom() {
                    self.mark_current_read();
                }
            }

            ServerMsg::Typing { channel, name } => {
                if name != self.my_name()
                    && let Some(ch) = self.channel_named(&channel)
                {
                    ch.note_typing(&name, Instant::now());
                }
            }

            ServerMsg::Speaking { id, on } => {
                if on {
                    self.speaking.insert(id);
                } else {
                    self.speaking.remove(&id);
                }
            }

            ServerMsg::Channels { channels } => {
                self.set_channels(channels, false);
                if let Some(session) = &self.voice
                    && !self.channels.iter().any(|c| !c.is_text() && c.name == session.channel)
                {
                    self.leave_voice_locally();
                    self.warn("the voice channel was deleted — you left it");
                }
            }
        }
    }

    fn sort_users(&mut self) {
        self.users.sort_by_key(|u| u.name.to_lowercase());
    }

    fn in_my_room(&self, user: &User) -> bool {
        match (&self.voice, &user.voice) {
            (Some(session), Some(room)) => session.channel == *room && user.id != self.my_id(),
            _ => false,
        }
    }

    fn on_user_update(&mut self, user: User) {
        let before = self.user(user.id).cloned();
        let was_here = before.as_ref().is_some_and(|u| self.in_my_room(u));
        let is_here = self.in_my_room(&user);
        if user.voice.is_none() {
            self.speaking.remove(&user.id);
        }
        // we were taken out of the voice channel (it was deleted) — close the audio
        if user.id == self.my_id() && user.voice.is_none() && self.voice.is_some() && self.online() {
            let expected = self.voice.as_ref().map(|v| v.channel.clone());
            if before.as_ref().and_then(|u| u.voice.clone()) == expected {
                self.leave_voice_locally();
            }
        }
        match self.users.iter_mut().find(|u| u.id == user.id) {
            Some(slot) => *slot = user,
            None => {
                self.users.push(user);
                self.sort_users();
            }
        }
        if was_here != is_here {
            self.play(if is_here { Sound::Join } else { Sound::Leave });
        }
        if was_here || is_here {
            self.sync_voice_peers();
        }
    }

    fn on_message(&mut self, msg: Message) {
        let me = self.my_name().to_string();
        let mine = msg.from == me;
        let mentioned = !mine && !msg.system && tovuer_proto::mentions(&msg.text, &me);
        let is_current = self.channel_name() == msg.channel;
        let seen = is_current && self.reading_bottom();
        let (channel, from) = (msg.channel.clone(), msg.from.clone());
        let preview = match (&msg.file, msg.text.is_empty()) {
            (Some(f), true) => format!("📎 {}", f.name),
            _ => tovuer_proto::one_line(&msg.text, 120),
        };
        let system = msg.system;

        let Some(ch) = self.channel_named(&channel) else { return };
        ch.clear_typing(&from);
        let fresh = ch.insert(msg);
        if !fresh {
            return;
        }
        if mine && is_current {
            ch.first_unread = None;
            ch.scroll = 0;
            ch.anchor = None;
        }
        if !mine && !system && !seen {
            ch.unread += 1;
            if mentioned {
                ch.mentions += 1;
            }
            if is_current {
                ch.mark_first_unread(&me);
            }
        }
        if seen || mine {
            if is_current {
                self.mark_current_read();
            } else if mine {
                // our own message in another channel (sent from another window) is not unread
            }
        }

        // notify only when the person cannot see the message right now
        if mine || system || seen {
            return;
        }
        if mentioned {
            if self.settings.bell {
                self.effects.push(Effect::Bell);
            }
            self.play(Sound::Mention);
        }
        if self.settings.notify && !self.focused && (mentioned || self.settings.notify_all) {
            crate::transfer::desktop_notify(&format!("{from} · #{channel}"), &preview);
        }
    }

    pub fn on_task(&mut self, event: TaskEvent) {
        self.dirty = true;
        match event {
            TaskEvent::Upload { id, sent, total } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.key == format!("up{id}")) {
                    t.done = sent;
                    t.total = total;
                }
            }
            TaskEvent::UploadDone { id, name, error } => {
                self.transfers.retain(|t| t.key != format!("up{id}"));
                match error {
                    None => self.ok(format!("sent: {name}")),
                    Some(e) => self.error(format!("{name} was not sent: {e}")),
                }
            }
            TaskEvent::Download { file_id, got, total } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.key == file_id) {
                    t.done = got;
                    if total > 0 {
                        t.total = total;
                    }
                }
            }
            TaskEvent::Saved { name, result } => {
                self.transfers.retain(|t| t.upload || t.name != name);
                match result {
                    Ok(path) => self.ok(format!("saved: {}", path.display())),
                    Err(e) => self.error(format!("{name} was not downloaded: {e}")),
                }
            }
            TaskEvent::Thumb { file_id, result } => {
                let state = match result {
                    Ok(thumb) => ThumbState::Ready(Arc::new(thumb)),
                    Err(e) => {
                        self.note(Tone::Warn, format!("no preview: {e}"));
                        ThumbState::Failed
                    }
                };
                self.thumbs.insert(file_id, state);
            }
            TaskEvent::Sharp { file_id, area, result } => {
                if let Some(Overlay::Viewer(viewer)) = self.overlay.as_mut()
                    && viewer.file.id == file_id
                {
                    match result {
                        Ok(sharp) => viewer.sharp = Some((area, sharp)),
                        // blocks stay; the same size is not asked again
                        Err(e) => self.note(Tone::Warn, format!("no sharp picture: {e}")),
                    }
                }
            }
            TaskEvent::Opened { name, result } => {
                self.transfers.retain(|t| t.upload || t.name != name);
                match result {
                    Ok(()) => self.ok(format!("opened {name} in your system's app")),
                    Err(e) => self.error(format!("{name} did not open: {e} — s saves it to Downloads")),
                }
            }
            TaskEvent::Picture { file_id, result, .. } => {
                if let Some(Overlay::Viewer(viewer)) = self.overlay.as_mut()
                    && viewer.file.id == file_id
                {
                    viewer.fitted = None;
                    match result {
                        Ok(pic) => viewer.pic = Some(pic),
                        Err(e) => viewer.error = Some(e),
                    }
                }
            }
            TaskEvent::Peek { path, result } => {
                if let Some(Overlay::Picker(picker)) = self.overlay.as_mut()
                    && picker.peek.as_ref().is_some_and(|(p, _)| *p == path)
                {
                    picker.peek = Some((path, result.map(Peek::Ready).unwrap_or(Peek::Failed)));
                }
            }
            TaskEvent::UpdateProgress { got, total } => self.updating = Some((got, total)),
            TaskEvent::UpdateDone(result) => {
                self.updating = None;
                match result {
                    Ok(None) => self.ok(format!("you already have the latest version ({})", self.launch.version)),
                    Ok(Some((exe, version))) => {
                        self.ok(format!("updated to {version} — restarting"));
                        self.restart = Some(exe);
                        self.quit = true;
                    }
                    Err(e) => self.error(format!("update failed: {e}")),
                }
            }
            TaskEvent::Voice(result) => self.on_voice_started(result),
            TaskEvent::Devices(result) => {
                if let Some(Overlay::Settings(view)) = self.overlay.as_mut() {
                    match result {
                        Ok((mics, spks)) => {
                            view.mics = Some(mics);
                            view.spks = Some(spks);
                        }
                        Err(e) => {
                            view.mics = Some(Vec::new());
                            view.spks = Some(Vec::new());
                            self.warn(format!("could not list audio devices: {e}"));
                        }
                    }
                }
            }
        }
    }

    fn on_voice_started(&mut self, result: Result<tovuer_voice::StreamInfo, String>) {
        match result {
            Ok(info) => {
                let Some(session) = self.voice.as_mut() else {
                    // we left while the devices were opening (or this is a mic test)
                    if !self.engine.monitor() {
                        self.stop_engine_now();
                    }
                    return;
                };
                let first = session.starting;
                session.starting = false;
                session.info = Some(info.clone());
                for warning in &info.warnings {
                    self.warn(warning.clone());
                }
                let devices = format!(
                    "audio: {} → {} ({})",
                    info.mic.as_deref().unwrap_or("no microphone"),
                    info.spk.as_deref().unwrap_or("no output"),
                    info.host
                );
                self.note(Tone::Info, devices);
                self.sync_voice_peers();
                if first {
                    self.play(Sound::Join);
                }
            }
            Err(e) => {
                if self.voice.is_some() {
                    self.error(format!("audio did not open: {e} — text chat still works"));
                    self.net.send(ClientMsg::VoiceLeave);
                    self.voice = None;
                } else {
                    self.error(format!("audio did not open: {e}"));
                    self.engine.set_monitor(false);
                }
            }
        }
    }

    fn stop_engine_now(&self) {
        let engine = self.engine.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(move || engine.stop());
        }
    }

    /// The timer (~12 times a second): animations, timeouts, delayed saves.
    pub fn on_tick(&mut self) {
        let now = Instant::now();
        self.frame = self.frame.wrapping_add(1);

        if self.ui.animating {
            self.dirty = true;
        }
        let before = self.toasts.len();
        self.toasts.retain(|t| t.until > now);
        if self.toasts.len() != before {
            self.dirty = true;
        }
        for ch in &mut self.channels {
            if ch.expire_typing(now) {
                self.dirty = true;
            }
        }

        // "talking / quiet" follows the microphone gate
        if self.voice.is_some() && self.online() {
            let speaking = self.engine.controls().speaking.load(std::sync::atomic::Ordering::Relaxed);
            if speaking != self.sent_speaking && self.last_speaking.elapsed() >= SPEAKING_MIN_GAP {
                self.sent_speaking = speaking;
                self.last_speaking = now;
                self.net.send(ClientMsg::Speaking { on: speaking });
                let me = self.my_id();
                if speaking {
                    self.speaking.insert(me);
                } else {
                    self.speaking.remove(&me);
                }
            }
        }

        // a device dropped out (headset unplugged) — try to reopen
        if let Some(problem) = self.engine.take_error() {
            self.warn(format!("{problem} — reopening audio"));
            if self.last_voice_retry.is_none_or(|t| t.elapsed() > VOICE_RETRY_GAP) {
                self.last_voice_retry = Some(now);
                self.restart_engine();
            }
        }

        if self.reading_bottom() && self.channel().is_some_and(|c| c.unread > 0) {
            self.mark_current_read();
        }
        if self.state_dirty.is_some_and(|since| since.elapsed() > STATE_SAVE_DELAY) {
            self.state_dirty = None;
            if let Err(e) = self.paths.save_state(&self.state) {
                self.note(Tone::Warn, format!("could not save read marks: {e}"));
            }
        }
        self.refresh_title();

        // redraw only if something on screen is moving
        let animated = self.engine.running()
            || !self.transfers.is_empty()
            || self.updating.is_some()
            || !matches!(self.conn, Conn::Online | Conn::Offline)
            || self.channel().is_some_and(|c| !c.typing.is_empty())
            || matches!(&self.overlay, Some(Overlay::Viewer(v)) if v.pic.is_none() && v.error.is_none())
            || (self.screen == Screen::Main && self.me.is_none());
        if animated {
            self.dirty = true;
        }
    }

    /// The terminal window gained or lost focus.
    pub fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.reading_bottom() {
            self.mark_current_read();
        }
        self.dirty = true;
    }

    /// Update the progress of a file being fetched for a preview or the viewer.
    pub fn track_download(&mut self, file_id: &str, name: &str, total: u64) {
        if !self.transfers.iter().any(|t| t.key == file_id) {
            self.transfers.push(Transfer { key: file_id.to_string(), name: name.to_string(), done: 0, total, upload: false });
        }
    }
}
