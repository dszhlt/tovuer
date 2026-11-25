//! Keyboard, mouse and pasting.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tovuer_proto::{ChannelKind, ClientMsg, VideoSource};

use super::overlay::{ConfirmAction, Overlay, Prompt, PromptAction, REACT_COLS, Setting, cycle_camera, cycle_device};
use super::{App, Completion, Compose, Effect, Focus, LoginField, Screen, Suggestion, Tone, commands, emoji};
use crate::ui::Hit;
use crate::ui::theme::Theme;

fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT)
}

/// A single letter without modifiers (either case).
impl App {
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.dirty = true;
        if is_ctrl(&key) && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q')) {
            self.quit = true;
            return;
        }
        if self.screen == Screen::Login {
            return self.login_key(key);
        }
        if self.me.is_none() {
            return self.splash_key(key);
        }
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
        // a screen over the whole window: Esc brings the chat back; letters do not type blindly
        if self.stream_full() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => self.watch_full = false,
                _ if is_alt(&key) || is_ctrl(&key) || matches!(key.code, KeyCode::F(_)) => {
                    self.global_key(&key);
                }
                _ => {}
            }
            return;
        }
        if self.global_key(&key) {
            return;
        }
        match self.focus {
            Focus::Input => self.input_key(key),
            Focus::Sidebar => self.sidebar_key(key),
            Focus::Messages => self.messages_key(key),
        }
    }

    fn login_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Down => self.login_step(1),
            KeyCode::BackTab | KeyCode::Up => self.login_step(-1),
            KeyCode::Enter => self.submit_login(),
            KeyCode::F(5) if self.login.offer_update => self.start_update(),
            _ => {
                if self.login_input().key(key) {
                    self.login.error = None;
                }
            }
        }
    }

    /// The "looking for the room…" screen, before the room is joined.
    fn splash_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.back_to_login(None);
                self.login.field = if self.launch.friend { LoginField::Nick } else { LoginField::Room };
            }
            // the same keys on a Russian keyboard layout work too
            KeyCode::Enter | KeyCode::Char('r') | KeyCode::Char('к') => self.reconnect(),
            KeyCode::Char('q') | KeyCode::Char('й') => self.quit = true,
            _ => {}
        }
    }

    /// Keys that work anywhere on the main screen. `true` — handled.
    fn global_key(&mut self, key: &KeyEvent) -> bool {
        let (ctrl, alt) = (is_ctrl(key), is_alt(key));
        let page = (self.ui.chat_rows as isize - 2).max(1);
        match key.code {
            KeyCode::F(1) => self.overlay = Some(Overlay::Help { scroll: 0 }),
            KeyCode::F(2) => self.open_settings(),
            KeyCode::F(3) => self.overlay = Some(Overlay::Users { sel: 0 }),
            KeyCode::F(4) => self.open_style(),
            KeyCode::Char('k') if ctrl => self.open_palette(),
            KeyCode::Char('o') if ctrl => self.open_picker(),
            KeyCode::Char('l') if ctrl => self.overlay = Some(Overlay::Log { scroll: 0 }),
            KeyCode::Char('m') if alt => self.toggle_mute(),
            KeyCode::Char('d') if alt => self.toggle_deafen(),
            KeyCode::Char('c') if alt => self.toggle_call(),
            KeyCode::Char('w') if alt => self.toggle_watch(),
            KeyCode::Char('f') if alt => self.toggle_stream_full(),
            KeyCode::Char('v') if alt => self.toggle_video(VideoSource::Camera),
            KeyCode::Char('s') if alt => self.toggle_video(VideoSource::Screen),
            KeyCode::Up if alt => self.cycle_channel(-1),
            KeyCode::Down if alt => self.cycle_channel(1),
            KeyCode::Char('p') if ctrl => self.cycle_channel(-1),
            KeyCode::Char('n') if ctrl => self.cycle_channel(1),
            KeyCode::Char(c @ '1'..='9') if alt => {
                let n = c as usize - '1' as usize;
                let target = self.channels.iter().enumerate().filter(|(_, ch)| ch.is_text()).map(|(i, _)| i).nth(n);
                if let Some(i) = target {
                    self.open_channel(i);
                }
            }
            KeyCode::PageUp if self.focus != Focus::Messages => self.scroll_chat(page),
            KeyCode::PageDown if self.focus != Focus::Messages => self.scroll_chat(-page),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_chat(1),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => self.scroll_chat(-1),
            KeyCode::End if ctrl => self.scroll_to_bottom(),
            KeyCode::BackTab => self.set_focus(match self.focus {
                Focus::Input => Focus::Messages,
                Focus::Messages => Focus::Sidebar,
                Focus::Sidebar => Focus::Input,
            }),
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------------------- the input

    fn input_key(&mut self, key: KeyEvent) {
        let (ctrl, alt) = (is_ctrl(&key), is_alt(&key));
        if let Some(comp) = self.completion.as_mut() {
            match key.code {
                KeyCode::Up => {
                    comp.sel = comp.sel.checked_sub(1).unwrap_or(comp.items.len() - 1);
                    return;
                }
                KeyCode::Down => {
                    comp.sel = (comp.sel + 1) % comp.items.len();
                    return;
                }
                KeyCode::Tab => return self.accept_completion(),
                KeyCode::Esc => {
                    self.completion = None;
                    return;
                }
                KeyCode::Enter if !alt => {
                    // Enter takes the suggestion unless it is already what was typed
                    // (case aside: "/v lounge" is a finished command)
                    let typed = &self.input.before_cursor()[comp.from.min(self.input.cursor())..];
                    let chosen = comp.items[comp.sel].insert.trim_end();
                    if typed.to_lowercase() != chosen.to_lowercase() {
                        return self.accept_completion();
                    }
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => self.input.insert("\n"),
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => {
                if let Some(compose) = self.compose.take() {
                    if matches!(compose, Compose::Edit { .. }) {
                        self.input.clear();
                    }
                } else {
                    self.set_focus(Focus::Sidebar);
                }
                return;
            }
            KeyCode::Tab => {
                self.completion = self.suggest();
                match self.completion.as_ref().map(|c| c.items.len()) {
                    Some(1) => self.accept_completion(),
                    Some(_) => {}
                    None => self.set_focus(Focus::Sidebar),
                }
                return;
            }
            KeyCode::Up if ctrl => {
                self.input.recall_prev();
            }
            KeyCode::Down if ctrl => {
                self.input.recall_next();
            }
            KeyCode::Up if self.input.is_empty() => {
                // up arrow on an empty line edits your last message
                match self.my_last_message() {
                    Some(msg) => self.edit_message(&msg),
                    None => self.scroll_chat(1),
                }
                return;
            }
            KeyCode::Down if self.input.is_empty() => {
                self.scroll_chat(-1);
                return;
            }
            _ => {
                if !self.input.key(key) {
                    return;
                }
            }
        }
        self.completion = self.suggest().filter(|c| self.auto_popup(c));
        self.typing_ping();
    }

    /// Whether to show suggestions on their own (without Tab) for this input.
    fn auto_popup(&self, comp: &Completion) -> bool {
        let typed = &self.input.before_cursor()[comp.from.min(self.input.cursor())..];
        // a suggestion equal to what is typed only gets in the way
        !(comp.items.len() == 1 && comp.items[0].insert.trim_end() == typed)
    }

    fn accept_completion(&mut self) {
        if let Some(comp) = self.completion.take()
            && let Some(item) = comp.items.get(comp.sel)
        {
            self.input.replace_before_cursor(comp.from, &item.insert);
            // after a command whose argument is a list, offer the values right away
            self.completion = self.suggest().filter(|c| self.auto_popup(c) && c.from != comp.from);
        }
    }

    /// What can be completed at the cursor.
    pub fn suggest(&self) -> Option<Completion> {
        let before = self.input.before_cursor();
        let make = |from: usize, items: Vec<Suggestion>| (!items.is_empty()).then_some(Completion { from, items, sel: 0 });

        if let Some(rest) = before.strip_prefix('/')
            && !before.starts_with("//")
        {
            return match rest.split_once(' ') {
                None => {
                    let mut matching: Vec<&commands::CommandSpec> =
                        commands::COMMANDS.iter().filter(|c| c.names.iter().any(|n| n.starts_with(rest))).collect();
                    // a fully typed command name goes first: Enter runs exactly that
                    matching.sort_by_key(|c| !c.names.contains(&rest));
                    let items = matching
                        .into_iter()
                        .map(|c| Suggestion {
                            label: format!("/{} {}", c.names[0], c.args).trim_end().to_string(),
                            hint: c.help.to_string(),
                            insert: format!("/{} ", c.names[0]),
                        })
                        .take(8)
                        .collect();
                    make(0, items)
                }
                Some((cmd, arg)) if !arg.contains(' ') => {
                    let from = before.len() - arg.len();
                    let lower = arg.to_lowercase();
                    let values: Vec<String> = match cmd {
                        "theme" => Theme::names().iter().map(|n| n.to_string()).collect(),
                        "v" | "voice" => self.channels.iter().filter(|c| !c.is_text()).map(|c| c.name.clone()).collect(),
                        "mute" | "unmute" | "vol" | "kick" => self.others().iter().map(|u| u.name.clone()).collect(),
                        "denoise" => ["off", "soft", "normal", "strong", "ai"].iter().map(|s| s.to_string()).collect(),
                        "agc" | "mouse" | "transparent" | "previews" | "animate" | "sharp" => vec!["on".into(), "off".into()],
                        "watch" => std::iter::once("off".to_string()).chain(self.live().iter().map(|u| u.name.clone())).collect(),
                        "color" => std::iter::once("off").chain(super::style::COLORS.iter().map(|c| c.0)).map(str::to_string).collect(),
                        "gradient" => std::iter::once("off").chain(super::style::GRADIENTS.iter().map(|g| g.0)).map(str::to_string).collect(),
                        _ => Vec::new(),
                    };
                    let items = values
                        .into_iter()
                        .filter(|v| v.to_lowercase().starts_with(&lower))
                        .map(|v| Suggestion { label: v.clone(), hint: String::new(), insert: format!("{v} ") })
                        .take(8)
                        .collect();
                    make(from, items)
                }
                _ => None,
            };
        }

        let from = before.rfind(char::is_whitespace).map_or(0, |i| i + before[i..].chars().next().map_or(1, char::len_utf8));
        let token = &before[from..];
        if let Some(part) = token.strip_prefix('@') {
            let lower = part.to_lowercase();
            let mut names: Vec<String> = self.others().iter().map(|u| u.name.clone()).collect();
            names.push("all".into());
            let items = names
                .into_iter()
                .filter(|n| n.to_lowercase().starts_with(&lower))
                .map(|n| Suggestion { label: format!("@{n}"), hint: String::new(), insert: format!("@{n} ") })
                .take(8)
                .collect();
            return make(from, items);
        }
        if let Some(part) = token.strip_prefix(':')
            && part.chars().count() >= 2
            && !part.contains(':')
        {
            let items = emoji::matching(part)
                .into_iter()
                .map(|(name, e)| Suggestion { label: format!("{e} :{name}:"), hint: String::new(), insert: format!("{e} ") })
                .take(8)
                .collect();
            return make(from, items);
        }
        if from == 0
            && let Some(part) = token.strip_prefix('#')
        {
            let lower = part.to_lowercase();
            let items = self
                .channels
                .iter()
                .filter(|c| c.is_text() && c.name.to_lowercase().starts_with(&lower))
                .map(|c| Suggestion { label: format!("#{}", c.name), hint: c.topic.clone(), insert: format!("#{}", c.name) })
                .take(8)
                .collect();
            return make(from, items);
        }
        None
    }

    // --------------------------------------------------------------------- channels

    fn sidebar_key(&mut self, key: KeyEvent) {
        let last = self.channels.len().saturating_sub(1);
        let under = self.channels.get(self.side_sel).map(|c| (c.name.clone(), c.kind));
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.side_sel = self.side_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.side_sel = (self.side_sel + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => self.side_sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.side_sel = last,
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => match under {
                Some((_, ChannelKind::Text)) => {
                    self.open_channel(self.side_sel);
                    self.set_focus(Focus::Input);
                }
                Some((name, ChannelKind::Voice)) => self.toggle_voice(&name),
                None => {}
            },
            KeyCode::Tab => self.set_focus(Focus::Messages),
            KeyCode::Esc | KeyCode::Char('i') => self.set_focus(Focus::Input),
            KeyCode::Char('m') => self.toggle_mute(),
            KeyCode::Char('d') => self.toggle_deafen(),
            KeyCode::Char('u') => self.overlay = Some(Overlay::Users { sel: 0 }),
            KeyCode::Char('n') => self.prompt_new_channel(ChannelKind::Text),
            KeyCode::Char('v') => self.prompt_new_channel(ChannelKind::Voice),
            KeyCode::Char('t') => {
                if let Some((name, ChannelKind::Text)) = under {
                    self.prompt_topic(&name);
                }
            }
            KeyCode::Delete | KeyCode::Char('x') => {
                if let Some((name, _)) = under {
                    self.ask_delete_channel(&name);
                }
            }
            _ => {}
        }
        // the channel list may have emptied after focus moved to messages — leave the selection
        if self.focus == Focus::Messages && self.channel().is_none_or(|c| c.messages.is_empty()) {
            self.set_focus(Focus::Input);
        }
    }

    pub fn prompt_topic(&mut self, channel: &str) {
        let mut input = super::input::Input::new(tovuer_proto::MAX_TOPIC_CHARS);
        if let Some(ch) = self.channels.iter().find(|c| c.is_text() && c.name == channel) {
            input.set(&ch.topic);
        }
        self.overlay = Some(Overlay::Prompt(Prompt {
            title: format!("Topic of #{channel}"),
            hint: "one line; empty removes the topic".into(),
            input,
            action: PromptAction::Topic(channel.to_string()),
        }));
    }

    pub fn ask_delete_channel(&mut self, name: &str) {
        if !self.is_admin() {
            self.warn("only the room owner can delete channels");
            return;
        }
        let Some(ch) = self.channels.iter().find(|c| c.name == name) else { return };
        let text = if ch.is_text() {
            format!("#{name} — its whole history and files will be gone for good")
        } else {
            format!("voice {name} — everyone inside will be disconnected")
        };
        self.overlay = Some(Overlay::Confirm(super::overlay::Confirm {
            title: "Delete the channel?".into(),
            text,
            action: ConfirmAction::DeleteChannel(name.to_string()),
        }));
    }

    // ------------------------------------------------------------------ messages

    /// Picking a tile in the call (the feed's place, Shift+Tab gets here).
    fn tiles_key(&mut self, key: KeyEvent) {
        let members: Vec<u32> = self.call_members().iter().map(|u| u.id).collect();
        let last = members.len().saturating_sub(1);
        let sel = self.call.sel.min(last);
        match key.code {
            KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => self.call.sel = sel.saturating_sub(1),
            KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => self.call.sel = (sel + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => self.call.sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.call.sel = last,
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('f') => {
                if let Some(id) = members.get(sel) {
                    self.spotlight(*id);
                }
            }
            KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q') | KeyCode::Tab => {
                if self.call.focus.is_some() && key.code == KeyCode::Esc {
                    self.call.focus = None;
                } else {
                    self.set_focus(Focus::Input);
                }
            }
            _ => {}
        }
    }

    fn messages_key(&mut self, key: KeyEvent) {
        if self.call_open() {
            return self.tiles_key(key);
        }
        let selected = self.channel().and_then(|c| c.selected_msg()).cloned();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select_message(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select_message(1),
            KeyCode::PageUp => self.select_message(-8),
            KeyCode::PageDown => self.select_message(8),
            KeyCode::Home | KeyCode::Char('g') => self.select_message(-100_000),
            KeyCode::End | KeyCode::Char('G') => self.select_message(100_000),
            KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q') | KeyCode::Tab => {
                self.set_focus(Focus::Input);
                self.scroll_to_bottom();
            }
            _ => {
                let Some(msg) = selected else { return };
                match key.code {
                    KeyCode::Char('r') => self.reply_to(&msg),
                    KeyCode::Char('e') => self.edit_message(&msg),
                    KeyCode::Char('d') | KeyCode::Delete => self.ask_delete(&msg),
                    KeyCode::Char('a') | KeyCode::Char('+') | KeyCode::Char('=') => {
                        self.overlay = Some(Overlay::React { msg: msg.id, sel: 0 });
                    }
                    KeyCode::Enter => match msg.file {
                        Some(file) => self.open_file(file),
                        None => self.reply_to(&msg),
                    },
                    KeyCode::Char('o') => match msg.file {
                        Some(file) => self.open_outside(file),
                        None => self.warn("this message has no file"),
                    },
                    KeyCode::Char('s') => match msg.file {
                        Some(file) => self.save_file(file, None),
                        None => self.warn("this message has no file"),
                    },
                    KeyCode::Char('y') | KeyCode::Char('c') => {
                        let text = match (&msg.file, msg.text.is_empty()) {
                            (Some(f), true) => f.name.clone(),
                            _ => msg.text.clone(),
                        };
                        self.effects.push(Effect::Copy(text));
                        self.toast(Tone::Ok, "message text copied");
                    }
                    _ => {}
                }
            }
        }
    }

    // ---------------------------------------------------------------------- overlays

    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(mut overlay) = self.overlay.take() else { return };
        let list_rows = self.ui.list_rows.max(1) as isize;
        // the log scrolls from the bottom (newest), help from the top
        let dir: isize = if matches!(overlay, Overlay::Log { .. }) { -1 } else { 1 };
        let keep = match &mut overlay {
            Overlay::Help { scroll } | Overlay::Log { scroll } => {
                let mut shift = |by: isize| *scroll = (*scroll as isize + by * dir).max(0) as usize;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::F(1) => false,
                    KeyCode::Up | KeyCode::Char('k') => {
                        shift(-1);
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        shift(1);
                        true
                    }
                    KeyCode::PageUp => {
                        shift(-list_rows);
                        true
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        shift(list_rows);
                        true
                    }
                    KeyCode::Home => {
                        *scroll = if dir < 0 { usize::MAX / 2 } else { 0 };
                        true
                    }
                    KeyCode::End => {
                        *scroll = if dir < 0 { 0 } else { usize::MAX / 2 };
                        true
                    }
                    _ => true,
                }
            }

            Overlay::Palette(palette) => match key.code {
                KeyCode::Esc => false,
                KeyCode::Up | KeyCode::BackTab => {
                    palette.step(-1);
                    true
                }
                KeyCode::Down | KeyCode::Tab => {
                    palette.step(1);
                    true
                }
                KeyCode::Char('p') if is_ctrl(&key) => {
                    palette.step(-1);
                    true
                }
                KeyCode::Char('n') if is_ctrl(&key) => {
                    palette.step(1);
                    true
                }
                KeyCode::Enter => {
                    if let Some(cmd) = palette.chosen().map(|a| a.cmd.clone()) {
                        self.run_cmd(cmd);
                    }
                    false
                }
                _ => {
                    if palette.query.key(key) {
                        palette.refilter();
                    }
                    true
                }
            },

            Overlay::Settings(view) => {
                let last = Setting::ALL.len() - 1;
                let which = Setting::ALL[view.sel.min(last)];
                let (mics, spks) = (view.mics.clone().unwrap_or_default(), view.spks.clone().unwrap_or_default());
                let cameras = view.cameras.clone().unwrap_or_default();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(2) => {
                        self.stop_monitor();
                        false
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        view.sel = view.sel.saturating_sub(1);
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        view.sel = (view.sel + 1).min(last);
                        true
                    }
                    KeyCode::Home => {
                        view.sel = 0;
                        true
                    }
                    KeyCode::End => {
                        view.sel = last;
                        true
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        self.change_setting(which, -1, &mics, &spks, &cameras);
                        true
                    }
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => {
                        self.change_setting(which, 1, &mics, &spks, &cameras);
                        true
                    }
                    _ => true,
                }
            }

            Overlay::Picker(picker) => {
                let mut upload = None;
                let stay = match key.code {
                    KeyCode::Esc => false,
                    KeyCode::Up => {
                        picker.step(-1);
                        true
                    }
                    KeyCode::Down => {
                        picker.step(1);
                        true
                    }
                    KeyCode::PageUp => {
                        picker.step(-list_rows);
                        true
                    }
                    KeyCode::PageDown => {
                        picker.step(list_rows);
                        true
                    }
                    KeyCode::Home => {
                        picker.step(-1_000_000);
                        true
                    }
                    KeyCode::End => {
                        picker.step(1_000_000);
                        true
                    }
                    KeyCode::Left => {
                        picker.up();
                        true
                    }
                    KeyCode::Backspace => {
                        if picker.filter.pop().is_some() {
                            picker.refilter();
                        } else {
                            picker.up();
                        }
                        true
                    }
                    KeyCode::Enter | KeyCode::Right => match (picker.current().map(|e| e.dir), picker.current_path()) {
                        (Some(true), Some(path)) => {
                            picker.load(&path);
                            true
                        }
                        (Some(false), Some(path)) if key.code == KeyCode::Enter => {
                            upload = Some(path);
                            false
                        }
                        _ => true,
                    },
                    KeyCode::Char('.') if picker.filter.is_empty() => {
                        picker.toggle_hidden();
                        true
                    }
                    KeyCode::Char('~') if picker.filter.is_empty() => {
                        if let Some(home) = dirs::home_dir() {
                            picker.load(&home);
                        }
                        true
                    }
                    KeyCode::Char(c) if !is_ctrl(&key) && !is_alt(&key) => {
                        picker.filter.push(c);
                        picker.refilter();
                        true
                    }
                    _ => true,
                };
                self.ui.last_dir = Some(picker.dir.clone());
                if let Some(path) = upload {
                    self.upload(path);
                }
                stay
            }

            Overlay::Viewer(viewer) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => false,
                KeyCode::Char('s') => {
                    let file = viewer.file.clone();
                    self.save_file(file, None);
                    true
                }
                KeyCode::Char('o') => {
                    let file = viewer.file.clone();
                    self.open_outside(file);
                    true
                }
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l') => {
                    let forward = matches!(key.code, KeyCode::Right | KeyCode::Char('l'));
                    let images = self.images();
                    let at = images.iter().position(|f| f.id == viewer.file.id);
                    let next = at.and_then(|i| if forward { images.get(i + 1) } else { i.checked_sub(1).and_then(|p| images.get(p)) });
                    if let Some(file) = next.cloned() {
                        self.open_file(file);
                        return;
                    }
                    true
                }
                _ => true,
            },

            Overlay::Users { sel } => {
                let others: Vec<String> = self.others().iter().map(|u| u.name.clone()).collect();
                *sel = (*sel).min(others.len().saturating_sub(1));
                let name = others.get(*sel).cloned();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(3) => false,
                    KeyCode::Up | KeyCode::Char('k') => {
                        *sel = sel.saturating_sub(1);
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *sel = (*sel + 1).min(others.len().saturating_sub(1));
                        true
                    }
                    _ => {
                        if let Some(name) = name {
                            let volume = self.settings.volumes.get(&name).copied().unwrap_or(100);
                            match key.code {
                                KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => {
                                    self.set_peer_volume(&name, volume.saturating_sub(10));
                                }
                                KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('+') | KeyCode::Char('=') => {
                                    self.set_peer_volume(&name, (volume + 10).min(200));
                                }
                                KeyCode::Char('0') => self.set_peer_volume(&name, 100),
                                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('m') => {
                                    let muted = self.settings.is_muted(&name);
                                    self.set_peer_muted(&name, !muted);
                                }
                                _ => {}
                            }
                        }
                        true
                    }
                }
            }

            Overlay::React { msg, sel } => {
                let count = emoji::QUICK.len();
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => false,
                    KeyCode::Left | KeyCode::Char('h') => {
                        *sel = (*sel + count - 1) % count;
                        true
                    }
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                        *sel = (*sel + 1) % count;
                        true
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *sel = (*sel + count - REACT_COLS) % count;
                        true
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        *sel = (*sel + REACT_COLS) % count;
                        true
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        let emoji = emoji::QUICK[*sel % count].to_string();
                        self.net.send(ClientMsg::React { id: *msg, emoji });
                        false
                    }
                    _ => true,
                }
            }

            Overlay::Confirm(confirm) => match key.code {
                // 'н' and 'т' are y and n on a Russian keyboard layout
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('н') => {
                    match confirm.action.clone() {
                        ConfirmAction::DeleteMessage(id) => self.net.send(ClientMsg::Delete { id }),
                        ConfirmAction::DeleteChannel(name) => self.net.send(ClientMsg::ChannelDelete { name }),
                    }
                    false
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('т') | KeyCode::Char('q') => false,
                _ => true,
            },

            Overlay::Style(view) => self.style_key(view, key),

            Overlay::Prompt(prompt) => match key.code {
                KeyCode::Esc => {
                    // back from your own color to the style window
                    if matches!(prompt.action, PromptAction::Color | PromptAction::Gradient) {
                        self.overlay = Some(Overlay::Style(super::style::StyleView { sel: self.ui.style_sel }));
                    }
                    false
                }
                KeyCode::Enter => {
                    let value = prompt.input.text().trim().to_string();
                    match prompt.action.clone() {
                        PromptAction::NewChannel(kind) => {
                            if value.is_empty() {
                                self.overlay = Some(overlay);
                                return;
                            }
                            self.net.send(ClientMsg::ChannelCreate { name: value, kind });
                        }
                        PromptAction::Topic(name) => self.net.send(ClientMsg::ChannelTopic { name, topic: value }),
                        PromptAction::SaveAs(file) => {
                            let target = (!value.is_empty()).then(|| crate::settings::expand_home(&value));
                            self.save_file(file, target);
                        }
                        action @ (PromptAction::Color | PromptAction::Gradient) => {
                            if !self.apply_custom(&action, &value) {
                                self.overlay = Some(overlay);
                                return;
                            }
                        }
                    }
                    false
                }
                _ => {
                    prompt.input.key(key);
                    true
                }
            },
        };
        // the action may have opened another window itself (palette → settings) — keep it
        if keep && self.overlay.is_none() {
            self.overlay = Some(overlay);
        }
        self.peek_current();
        if self.overlay.is_none() && self.reading_bottom() {
            self.mark_current_read();
        }
    }

    fn stop_monitor(&mut self) {
        if self.engine.monitor() {
            self.engine.set_monitor(false);
            if self.voice.is_none() {
                self.stop_engine_now_public();
            }
        }
    }

    fn stop_engine_now_public(&self) {
        let engine = self.engine.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(move || engine.stop());
        } else {
            engine.stop();
        }
    }

    /// A setting's current value, as shown in the window.
    pub fn setting_value(&self, which: Setting) -> String {
        let on = |b: bool| if b { "on" } else { "off" }.to_string();
        let device = |name: &str| if name.is_empty() { "system default".to_string() } else { name.to_string() };
        let s = &self.settings;
        match which {
            Setting::Theme => s.theme.clone(),
            Setting::Transparent => on(s.transparent),
            Setting::Members => on(s.members),
            Setting::Previews => on(s.previews),
            Setting::Animate => on(s.animate),
            Setting::MyStyle => super::style::describe(&s.style),
            Setting::Styles => ["plain", "still", "flowing"][s.styles.min(2) as usize].to_string(),
            Setting::Sharp => match &self.graphics {
                Some(g) if s.sharp => format!("on · {}", crate::transfer::graphics_name(g.protocol_type())),
                Some(_) => "off".into(),
                None => "blocks only in this terminal".into(),
            },
            Setting::Mouse => on(s.mouse),
            Setting::Mic => device(&s.mic),
            Setting::Spk => device(&s.spk),
            Setting::Denoise => {
                use tovuer_voice::neural::State;
                match (self.denoise_mode(), self.engine.controls().neural_state()) {
                    ("ai", Some(State::Loading)) if self.engine.running() => "ai · loading…".into(),
                    ("ai", Some(State::Failed)) => "ai · unavailable, RNNoise works".into(),
                    (mode, _) => mode.to_string(),
                }
            }
            Setting::Agc => on(s.agc),
            Setting::MicGain => format!("{}%", s.mic_gain),
            Setting::Volume => format!("{}%", s.volume),
            Setting::Sensitivity => ["strict", "normal", "sensitive"][s.sensitivity.min(2) as usize].to_string(),
            Setting::Monitor => on(self.engine.monitor()),
            Setting::Camera => match s.camera.as_str() {
                "" => "the first one".into(),
                crate::video::TEST_DEVICE => "test picture".into(),
                name => name.to_string(),
            },
            Setting::Sounds => on(s.sounds),
            Setting::Bell => on(s.bell),
            Setting::Notify => on(s.notify),
            Setting::NotifyAll => on(s.notify_all),
            Setting::Updates => on(s.check_updates),
        }
    }

    /// Change a setting by `delta` (switches just flip).
    pub fn change_setting(
        &mut self,
        which: Setting,
        delta: isize,
        mics: &[tovuer_voice::DeviceInfo],
        spks: &[tovuer_voice::DeviceInfo],
        cameras: &[String],
    ) {
        let step = |value: u32, lo: u32, hi: u32| (value as i64 + delta as i64 * 10).clamp(lo as i64, hi as i64) as u32;
        match which {
            Setting::Theme => {
                let names = Theme::names();
                let at = names.iter().position(|n| *n == self.settings.theme).unwrap_or(0);
                self.settings.theme = names[(at as isize + delta).rem_euclid(names.len() as isize) as usize].to_string();
                self.refresh_theme();
            }
            Setting::Transparent => {
                self.settings.transparent = !self.settings.transparent;
                self.refresh_theme();
            }
            Setting::Members => self.settings.members = !self.settings.members,
            Setting::Previews => {
                self.settings.previews = !self.settings.previews;
                self.ui.invalidate();
            }
            Setting::Animate => self.set_animate(!self.settings.animate),
            Setting::Sharp => self.settings.sharp = !self.settings.sharp,
            Setting::MyStyle => self.open_style(),
            Setting::Styles => {
                self.settings.styles = (self.settings.styles as isize + delta).rem_euclid(3) as u8;
                self.ui.invalidate();
            }
            Setting::Mouse => {
                self.settings.mouse = !self.settings.mouse;
                self.effects.push(Effect::Mouse(self.settings.mouse));
            }
            Setting::Mic => {
                self.settings.mic = cycle_device(&self.settings.mic, mics, delta);
                self.restart_engine();
            }
            Setting::Spk => {
                self.settings.spk = cycle_device(&self.settings.spk, spks, delta);
                self.restart_engine();
            }
            Setting::Denoise => {
                const MODES: [&str; 5] = ["off", "soft", "normal", "strong", "ai"];
                let at = MODES.iter().position(|m| *m == self.denoise_mode()).unwrap_or(2);
                self.set_denoise_mode(MODES[(at as isize + delta).rem_euclid(MODES.len() as isize) as usize]);
            }
            Setting::Agc => self.settings.agc = !self.settings.agc,
            Setting::MicGain => self.settings.mic_gain = step(self.settings.mic_gain, 50, 300),
            Setting::Volume => self.settings.volume = step(self.settings.volume, 0, 200),
            Setting::Sensitivity => {
                self.settings.sensitivity = (self.settings.sensitivity as isize + delta).clamp(0, 2) as u8;
            }
            Setting::Monitor => {
                let on = !self.engine.monitor();
                self.engine.set_monitor(on);
                if on && !self.engine.running() {
                    self.start_engine_public();
                } else if !on && self.voice.is_none() {
                    self.stop_engine_now_public();
                }
            }
            Setting::Camera => {
                self.settings.camera = cycle_camera(&self.settings.camera, cameras, delta);
                // a camera that is on switches over
                if self.sharing() == Some(tovuer_proto::VideoSource::Camera) {
                    self.set_video(None);
                    self.set_video(Some(tovuer_proto::VideoSource::Camera));
                }
            }
            Setting::Sounds => self.settings.sounds = !self.settings.sounds,
            Setting::Bell => self.settings.bell = !self.settings.bell,
            Setting::Notify => self.settings.notify = !self.settings.notify,
            Setting::NotifyAll => self.settings.notify_all = !self.settings.notify_all,
            Setting::Updates => self.settings.check_updates = !self.settings.check_updates,
        }
        self.apply_audio_settings();
        self.save_settings();
        self.dirty = true;
    }

    fn start_engine_public(&mut self) {
        let engine = self.engine.clone();
        let tasks = self.tasks.clone();
        let (mic, spk) = (self.settings.mic.clone(), self.settings.spk.clone());
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::spawn_blocking(move || {
                let result = engine.start(&mic, &spk).map_err(|e| e.to_string());
                let _ = tasks.send(crate::transfer::TaskEvent::Voice(result));
            });
        }
    }

    // ------------------------------------------------------------- paste and mouse

    pub fn on_paste(&mut self, text: String) {
        self.dirty = true;
        if self.screen == Screen::Login {
            let line = text.lines().next().unwrap_or("").to_string();
            self.login_input().insert(&line);
            return;
        }
        match self.overlay.as_mut() {
            Some(Overlay::Prompt(prompt)) => {
                prompt.input.insert(text.lines().next().unwrap_or(""));
                return;
            }
            Some(Overlay::Palette(palette)) => {
                palette.query.insert(text.lines().next().unwrap_or(""));
                palette.refilter();
                return;
            }
            Some(_) => return,
            None => {}
        }
        // a file dropped into the window arrives as a pasted path — offer to send it
        let candidate = text.trim().trim_matches(['"', '\'']).replace("\\ ", " ");
        let looks_like_path = !candidate.contains('\n') && (candidate.starts_with('/') || candidate.starts_with("~/") || candidate.get(1..3) == Some(":\\"));
        if self.input.is_empty() && looks_like_path && crate::settings::expand_home(&candidate).is_file() {
            self.input.set(&format!("/file {candidate}"));
            self.toast(Tone::Info, "that is a file — Enter sends it to the channel");
        } else {
            self.input.insert(&text);
        }
        self.set_focus(Focus::Input);
        self.completion = None;
    }

    pub fn on_mouse(&mut self, event: MouseEvent) {
        if !self.settings.mouse || self.screen != Screen::Main || self.me.is_none() {
            return;
        }
        match event.kind {
            MouseEventKind::ScrollUp if self.overlay.is_none() => self.scroll_chat(3),
            MouseEventKind::ScrollDown if self.overlay.is_none() => self.scroll_chat(-3),
            MouseEventKind::Down(MouseButton::Left) if self.overlay.is_none() => {
                let hit = self
                    .ui
                    .hits
                    .iter()
                    .rev()
                    .find(|(area, _)| {
                        event.column >= area.x
                            && event.column < area.x + area.width
                            && event.row >= area.y
                            && event.row < area.y + area.height
                    })
                    .map(|(_, hit)| hit.clone());
                match hit {
                    Some(Hit::Channel(i)) => {
                        self.side_sel = i;
                        match self.channels.get(i).map(|c| (c.name.clone(), c.kind)) {
                            Some((_, ChannelKind::Text)) => {
                                self.open_channel(i);
                                self.set_focus(Focus::Input);
                            }
                            Some((name, ChannelKind::Voice)) => self.toggle_voice(&name),
                            None => {}
                        }
                    }
                    Some(Hit::Message(id)) => {
                        if let Some(ch) = self.channel_mut() {
                            ch.selected = Some(id);
                        }
                        self.focus = Focus::Messages;
                    }
                    Some(Hit::Input) => self.set_focus(Focus::Input),
                    Some(Hit::NewChannel(kind)) => self.prompt_new_channel(kind),
                    Some(Hit::Live) => self.toggle_watch(),
                    Some(Hit::Stream) => self.toggle_stream_full(),
                    Some(Hit::Tile(id)) => {
                        if let Some(at) = self.call_members().iter().position(|u| u.id == id) {
                            self.call.sel = at;
                        }
                        self.spotlight(id);
                    }
                    None => {}
                }
                self.dirty = true;
            }
            _ => {}
        }
    }
}
