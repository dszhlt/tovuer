//! Slash commands.

use std::sync::atomic::Ordering;

use tovuer_proto::{ChannelKind, ClientMsg, FileMeta};

use super::overlay::Overlay;
use super::{App, Effect, Tone};
use crate::settings::{expand_home, human_size};
use crate::ui::theme::Theme;

pub struct CommandSpec {
    /// The first name is the main one, the rest are aliases.
    pub names: &'static [&'static str],
    pub args: &'static str,
    pub help: &'static str,
}

const fn cmd(names: &'static [&'static str], args: &'static str, help: &'static str) -> CommandSpec {
    CommandSpec { names, args, help }
}

/// All commands — help and completion are built from this table.
pub const COMMANDS: &[CommandSpec] = &[
    cmd(&["help", "?"], "", "all keys and commands"),
    cmd(&["file", "upload"], "[path]", "send a file; without a path — the file picker"),
    cmd(&["files"], "", "files of this channel, numbered"),
    cmd(&["download", "dl"], "<number> [where]", "download a file from the /files list"),
    cmd(&["view"], "[number]", "open a picture; without a number — the latest"),
    cmd(&["open"], "[number]", "open a file in your system's app (a photo — in full quality)"),
    cmd(&["v", "voice"], "[channel]", "join a voice channel; without a name — leave"),
    cmd(&["m"], "", "turn your microphone on/off"),
    cmd(&["d", "deafen"], "", "turn sound on/off (hear no one)"),
    cmd(&["mute"], "<nick>", "stop hearing someone (only for you)"),
    cmd(&["unmute"], "<nick>", "hear someone again"),
    cmd(&["vol"], "<nick> <0-200>", "someone's volume, in percent"),
    cmd(&["volume"], "<0-200>", "overall volume of others"),
    cmd(&["gain"], "<50-300>", "your microphone gain"),
    cmd(&["denoise"], "[on|off]", "noise suppression"),
    cmd(&["agc"], "[on|off]", "automatic microphone level"),
    cmd(&["devices", "dev"], "", "list microphones and speakers"),
    cmd(&["mic"], "[name|default]", "pick a microphone; without a name — audio diagnostics"),
    cmd(&["spk"], "[name|default]", "pick speakers"),
    cmd(&["theme"], "[name|next]", "change the color theme"),
    cmd(&["transparent"], "[on|off]", "transparent background"),
    cmd(&["previews"], "[on|off]", "picture thumbnails in the chat"),
    cmd(&["sharp"], "[on|off]", "real pixels in the picture viewer (sixel/kitty)"),
    cmd(&["mouse"], "[on|off]", "mouse: wheel and clicks"),
    cmd(&["settings"], "", "the settings window"),
    cmd(&["new", "newtext"], "[name]", "create a text channel"),
    cmd(&["newvoice"], "[name]", "create a voice channel"),
    cmd(&["topic"], "<text>", "set the open channel's topic"),
    cmd(&["delchannel", "delvoice"], "[name]", "delete a text or voice channel; without a name — this one (room owner)"),
    cmd(&["kick"], "<nick>", "remove someone from the room (room owner)"),
    cmd(&["me"], "<action>", "a message in the third person"),
    cmd(&["shrug"], "[text]", "¯\\_(ツ)_/¯"),
    cmd(&["ping"], "", "latency to the other members"),
    cmd(&["room", "server"], "", "about the room and your node"),
    cmd(&["reconnect"], "", "rejoin the room"),
    cmd(&["log", "sys"], "", "the system log"),
    cmd(&["update", "upgrade"], "", "update the client from the room owner"),
    cmd(&["install"], "", "install the tovuer terminal command"),
    cmd(&["version", "ver"], "", "client version"),
    cmd(&["q", "quit", "exit"], "", "quit"),
];

/// "on/1/yes" → true, "off/0/no" → false, empty → toggle.
fn switch(arg: &str, current: bool) -> Option<bool> {
    match arg.trim().to_lowercase().as_str() {
        "" => Some(!current),
        "on" | "1" | "yes" | "true" => Some(true),
        "off" | "0" | "no" | "false" => Some(false),
        _ => None,
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

impl App {
    /// Files of the open channel in the order they appeared.
    pub fn channel_files(&self) -> Vec<(FileMeta, String)> {
        self.channel()
            .map(|c| c.messages.iter().filter_map(|m| m.file.clone().map(|f| (f, m.from.clone()))).collect())
            .unwrap_or_default()
    }

    /// A file by its `/files` number (from one) or by the start of its id.
    fn file_by_ref(&self, reference: &str) -> Option<FileMeta> {
        let files = self.channel_files();
        if let Ok(n) = reference.parse::<usize>() {
            return files.get(n.checked_sub(1)?).map(|(f, _)| f.clone());
        }
        files.into_iter().map(|(f, _)| f).find(|f| reference.len() >= 4 && f.id.starts_with(reference))
    }

    fn show_log(&mut self) {
        self.overlay = Some(Overlay::Log { scroll: 0 });
    }

    pub fn run_command(&mut self, line: &str) {
        let line = line.trim();
        let (name, arg) = line[1..].split_once(char::is_whitespace).unwrap_or((&line[1..], ""));
        let arg = arg.trim();
        let name = name.to_lowercase();
        let Some(spec) = COMMANDS.iter().find(|c| c.names.contains(&name.as_str())) else {
            self.warn(format!("no command /{name} — F1 shows them all"));
            return;
        };
        match spec.names[0] {
            "help" => self.overlay = Some(Overlay::Help { scroll: 0 }),
            "q" => self.quit = true,
            "log" => self.show_log(),
            "settings" => self.open_settings(),
            "update" => self.start_update(),
            "install" => self.install_command(),
            "reconnect" => self.reconnect(),
            "version" => self.toast(Tone::Info, format!("tovuer {}", self.launch.version)),

            "file" => {
                if arg.is_empty() {
                    self.open_picker();
                } else {
                    self.upload(App::resolve_file(arg));
                }
            }
            "files" => {
                let files = self.channel_files();
                if files.is_empty() {
                    self.toast(Tone::Info, format!("no files in #{} yet", self.channel_name()));
                    return;
                }
                self.note(Tone::Info, format!("files in #{}:", self.channel_name()));
                for (i, (file, from)) in files.iter().enumerate() {
                    self.note(Tone::Info, format!("  {:>2}. {} — {}, from {from}", i + 1, file.name, human_size(file.size)));
                }
                self.note(Tone::Info, "download: /download <number> [where] · view a picture: /view <number> · open in your app: /open <number>");
                self.show_log();
            }
            "download" => {
                let (reference, target) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
                match self.file_by_ref(reference) {
                    Some(file) => {
                        let target = target.trim().trim_matches('"');
                        self.save_file(file, (!target.is_empty()).then(|| expand_home(target)));
                    }
                    None if reference.is_empty() => self.warn("usage: /download <number> [where] — numbers are in /files"),
                    None => self.warn(format!("no file “{reference}” — numbers are in /files")),
                }
            }
            "view" => {
                let file = if arg.is_empty() {
                    self.images().pop()
                } else {
                    self.file_by_ref(arg).filter(|f| f.image)
                };
                match file {
                    Some(file) => self.open_file(file),
                    None if arg.is_empty() => self.warn("no pictures in this channel yet"),
                    None => self.warn(format!("“{arg}” is not a picture from the /files list")),
                }
            }
            "open" => {
                let file = if arg.is_empty() {
                    self.channel_files().pop().map(|(f, _)| f)
                } else {
                    self.file_by_ref(arg)
                };
                match file {
                    Some(file) => self.open_outside(file),
                    None if arg.is_empty() => self.warn("no files in this channel yet"),
                    None => self.warn(format!("no file “{arg}” — numbers are in /files")),
                }
            }

            "v" => {
                if arg.is_empty() {
                    if self.voice.is_some() {
                        self.leave_voice();
                    } else {
                        self.warn("usage: /v <channel> — e.g. /v Lounge");
                    }
                } else {
                    self.join_voice(arg.trim_start_matches('#'));
                }
            }
            "m" => self.toggle_mute(),
            "d" => self.toggle_deafen(),
            "mute" | "unmute" => {
                let mute = spec.names[0] == "mute";
                if arg.is_empty() {
                    self.overlay = Some(Overlay::Users { sel: 0 });
                    return;
                }
                let name = self.user_named(arg).map(|u| u.name.clone()).unwrap_or_else(|| arg.to_string());
                if name == self.my_name() {
                    self.warn("you cannot mute yourself — that is what M is for");
                    return;
                }
                self.set_peer_muted(&name, mute);
                self.ok(if mute { format!("{name} muted — only for you") } else { format!("you hear {name} again") });
            }
            "vol" => {
                let (who, value) = arg.rsplit_once(char::is_whitespace).unwrap_or((arg, ""));
                match (self.user_named(who.trim()).map(|u| u.name.clone()), value.trim().trim_end_matches('%').parse::<u32>()) {
                    (Some(name), Ok(percent)) if percent <= 200 => {
                        self.set_peer_volume(&name, percent);
                        self.ok(format!("{name}'s volume: {percent}%"));
                    }
                    _ => self.warn("usage: /vol <nick> <0-200> — or the volume window on F3"),
                }
            }
            "volume" | "gain" => {
                let is_volume = spec.names[0] == "volume";
                let (lo, hi) = if is_volume { (0, 200) } else { (50, 300) };
                match arg.trim_end_matches('%').parse::<u32>() {
                    Ok(v) if (lo..=hi).contains(&v) => {
                        if is_volume {
                            self.settings.volume = v;
                        } else {
                            self.settings.mic_gain = v;
                        }
                        self.apply_audio_settings();
                        self.save_settings();
                        self.ok(if is_volume { format!("volume of others: {v}%") } else { format!("microphone gain: {v}%") });
                    }
                    _ => self.warn(format!("usage: /{} <{lo}-{hi}>", spec.names[0])),
                }
            }
            "denoise" | "agc" | "transparent" | "previews" | "sharp" | "mouse" => {
                let s = &mut self.settings;
                let slot = match spec.names[0] {
                    "denoise" => &mut s.denoise,
                    "agc" => &mut s.agc,
                    "transparent" => &mut s.transparent,
                    "previews" => &mut s.previews,
                    "sharp" => &mut s.sharp,
                    _ => &mut s.mouse,
                };
                let Some(value) = switch(arg, *slot) else {
                    self.warn(format!("usage: /{} on|off", spec.names[0]));
                    return;
                };
                *slot = value;
                let label = match spec.names[0] {
                    "denoise" => "noise suppression",
                    "agc" => "automatic microphone level",
                    "transparent" => "transparent background",
                    "previews" => "picture previews",
                    "sharp" if value && self.graphics.is_none() => "sharp pictures (this terminal can only draw blocks — /open shows the original)",
                    "sharp" => "sharp pictures",
                    _ => {
                        self.effects.push(Effect::Mouse(value));
                        "mouse"
                    }
                };
                self.apply_audio_settings();
                self.refresh_theme();
                self.save_settings();
                self.ok(format!("{label}: {}", on_off(value)));
            }

            "devices" => {
                match tovuer_voice::list_devices() {
                    Ok((mics, spks)) => {
                        let mark = |d: &tovuer_voice::DeviceInfo, chosen: &str| {
                            let picked = if chosen.is_empty() { d.is_default } else { d.name.to_lowercase().contains(&chosen.to_lowercase()) };
                            format!("  {} {}{}", if picked { "✓" } else { " " }, d.name, if d.is_default { "  (system default)" } else { "" })
                        };
                        self.note(Tone::Info, "microphones:");
                        for d in &mics {
                            self.note(Tone::Info, mark(d, &self.settings.mic.clone()));
                        }
                        self.note(Tone::Info, "speakers:");
                        for d in &spks {
                            self.note(Tone::Info, mark(d, &self.settings.spk.clone()));
                        }
                        self.note(Tone::Info, "pick: /mic <part of the name> · /spk <part of the name> · reset: /mic default · easier: F2");
                    }
                    Err(e) => self.note(Tone::Error, format!("could not list audio devices: {e}")),
                }
                self.show_log();
            }
            "mic" | "spk" => {
                let is_mic = spec.names[0] == "mic";
                if arg.is_empty() {
                    if is_mic {
                        self.voice_diagnostics();
                    } else {
                        self.run_command("/devices");
                    }
                    return;
                }
                let value = if arg.eq_ignore_ascii_case("default") { String::new() } else { arg.to_string() };
                let shown = if value.is_empty() { "system default".to_string() } else { value.clone() };
                if is_mic {
                    self.settings.mic = value;
                } else {
                    self.settings.spk = value;
                }
                self.save_settings();
                self.restart_engine();
                self.ok(format!("{}: {shown}", if is_mic { "microphone" } else { "speakers" }));
            }

            "theme" => match arg {
                "" => {
                    self.toast(Tone::Info, format!("theme {} — available: {}", self.settings.theme, Theme::names().join(", ")));
                }
                "next" => {
                    let next = Theme::next_after(&self.settings.theme);
                    self.set_theme(next);
                    self.ok(format!("theme: {next}"));
                }
                name => {
                    if self.set_theme(&name.to_lowercase()) {
                        self.ok(format!("theme: {}", name.to_lowercase()));
                    } else {
                        self.warn(format!("no theme “{name}” — available: {}", Theme::names().join(", ")));
                    }
                }
            },

            "new" | "newvoice" => {
                let kind = if spec.names[0] == "new" { ChannelKind::Text } else { ChannelKind::Voice };
                if arg.is_empty() {
                    self.prompt_new_channel(kind);
                } else if self.online() {
                    self.net.send(ClientMsg::ChannelCreate { name: arg.to_string(), kind });
                } else {
                    self.warn("not in the room");
                }
            }
            "topic" => match self.channel().map(|c| c.name.clone()) {
                Some(name) if self.online() => self.net.send(ClientMsg::ChannelTopic { name, topic: arg.to_string() }),
                _ => self.warn("not in the room"),
            },
            "delchannel" => {
                let wanted = arg.trim_start_matches('#').to_lowercase();
                let name = if wanted.is_empty() {
                    self.channel().map(|c| c.name.clone())
                } else {
                    self.channels.iter().find(|c| c.name.to_lowercase() == wanted).map(|c| c.name.clone())
                };
                match name {
                    Some(name) => self.ask_delete_channel(&name),
                    None => self.warn(format!("no channel “{arg}”")),
                }
            }
            "kick" => {
                if arg.is_empty() {
                    self.warn("usage: /kick <nick>");
                } else if !self.is_admin() {
                    self.warn("only the room owner can remove people");
                } else {
                    self.net.send(ClientMsg::Kick { name: arg.to_string() });
                }
            }

            "me" | "shrug" => {
                let text = if spec.names[0] == "me" {
                    if arg.is_empty() {
                        self.warn("usage: /me <what you are doing>");
                        return;
                    }
                    format!("*{arg}*")
                } else {
                    format!("{arg} ¯\\_(ツ)_/¯").trim().to_string()
                };
                match self.channel().map(|c| c.name.clone()) {
                    Some(channel) if self.online() => {
                        self.net.send(ClientMsg::Send { channel, text, reply_to: None });
                        self.scroll_to_bottom();
                    }
                    _ => self.warn("not in the room"),
                }
            }

            "ping" => match self.latency {
                Some(rtt) => self.toast(Tone::Info, format!("round trip to the others: {} ms", rtt.as_millis())),
                None => self.warn("nobody else is here to measure latency to"),
            },
            "room" => {
                let owner = if self.is_admin() { ", you own it" } else { "" };
                self.note(Tone::Info, format!("room “{}”{owner}", self.room));
                let peers = self.users.len().saturating_sub(1);
                self.note(Tone::Info, format!("{peers} other member(s) online, files up to {}", human_size(self.max_file)));
                if let Some(id) = self.net.node().map(|n| n.id()) {
                    self.note(Tone::Info, format!("your node id: {}", id.fmt_short()));
                }
                if let Some(v) = &self.update_available {
                    self.note(Tone::Info, format!("the room owner has client {v}, you have {} — /update", self.launch.version));
                }
                self.show_log();
            }
            other => self.warn(format!("/{other} does nothing yet")),
        }
    }

    /// `/mic` with no argument: what the microphone hears and what goes out.
    fn voice_diagnostics(&mut self) {
        let ctl = self.engine.controls().clone();
        match self.engine.info() {
            Some(info) => {
                self.note(
                    Tone::Info,
                    format!(
                        "audio on ({}): microphone “{}” {} Hz, output “{}” {} Hz",
                        info.host,
                        info.mic.as_deref().unwrap_or("none"),
                        info.mic_rate,
                        info.spk.as_deref().unwrap_or("none"),
                        info.spk_rate
                    ),
                );
            }
            None => self.note(Tone::Info, "audio is off — join a voice channel or turn on the mic test in F2"),
        }
        let (captured, played) = (ctl.captured.load(Ordering::Relaxed), ctl.played.load(Ordering::Relaxed));
        self.note(
            Tone::Info,
            format!(
                "{captured} frames from the microphone, {played} to the output (a frame is 20 ms); level {:.4}, peak {:.4}",
                ctl.mic_level(),
                ctl.mic_peak(),
            ),
        );
        self.note(
            Tone::Info,
            format!(
                "{} frames sent, {} cut as silence{}",
                ctl.sent.load(Ordering::Relaxed),
                ctl.gated.load(Ordering::Relaxed),
                if self.muted() { " — the microphone is OFF" } else { "" }
            ),
        );
        if self.engine.running() && captured == 0 {
            self.note(Tone::Warn, "the microphone is open but sends no sound — pick another device in F2");
        }
        for peer in self.engine.peer_stats() {
            let name = self.user(peer.id).map_or_else(|| format!("#{}", peer.id), |u| u.name.clone());
            self.note(Tone::Info, format!("  {name}: buffer {} ms, {} frames queued", peer.delay_ms, peer.queued));
        }
        self.note(Tone::Info, "speech usually gives a level of 0.05–0.3; silence after noise suppression is almost exactly zero");
        self.show_log();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_parse() {
        assert_eq!(switch("", false), Some(true));
        assert_eq!(switch("", true), Some(false));
        assert_eq!(switch("ON", false), Some(true));
        assert_eq!(switch("off", true), Some(false));
        assert_eq!(switch("maybe", true), None);
    }

    #[test]
    fn command_table_has_no_duplicate_names() {
        let mut names: Vec<&str> = COMMANDS.iter().flat_map(|c| c.names.iter().copied()).collect();
        let total = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), total);
        assert!(COMMANDS.iter().all(|c| !c.help.is_empty() && !c.names.is_empty()));
    }
}
