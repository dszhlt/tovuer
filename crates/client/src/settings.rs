//! Settings and state between runs.
//!
//! `settings.json` — what the person chose (theme, devices, volumes).
//! `state.json` — what the client remembers for them (how far each channel
//! has been read).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    /// Do not paint the background — let the terminal's show through.
    pub transparent: bool,
    /// Part of the microphone / speakers name; empty — system default.
    pub mic: String,
    pub spk: String,
    /// Who only we do not hear.
    pub muted: Vec<String>,
    /// Volume of particular people, in percent.
    pub volumes: BTreeMap<String, u32>,
    /// The last nick — filled in on the login screen.
    pub nick: String,
    /// Last room and its password.
    pub room: String,
    pub password: String,
    pub denoise: bool,
    pub agc: bool,
    /// Manual microphone gain and overall volume, in percent.
    pub mic_gain: u32,
    pub volume: u32,
    /// Speech detector sensitivity: 0 — strict, 1 — normal, 2 — sensitive.
    pub sensitivity: u8,
    /// Join/leave/mute sounds.
    pub sounds: bool,
    /// The terminal bell on a mention.
    pub bell: bool,
    /// Desktop notifications (notify-send) while the window is not focused.
    pub notify: bool,
    /// Notify about every message, not only mentions.
    pub notify_all: bool,
    /// Picture thumbnails right in the chat.
    pub previews: bool,
    /// The picture viewer uses the terminal's own graphics (Sixel, Kitty), if it has them.
    pub sharp: bool,
    /// The online panel on the right.
    pub members: bool,
    pub mouse: bool,
    /// M and U on an empty input — mute the microphone and the volume menu.
    pub quick_keys: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "tovuer".into(),
            transparent: false,
            mic: String::new(),
            spk: String::new(),
            muted: Vec::new(),
            volumes: BTreeMap::new(),
            nick: String::new(),
            room: String::new(),
            password: String::new(),
            denoise: true,
            agc: true,
            mic_gain: 100,
            volume: 100,
            sensitivity: 1,
            sounds: true,
            bell: true,
            notify: true,
            notify_all: false,
            previews: true,
            sharp: true,
            members: true,
            mouse: false,
            quick_keys: true,
        }
    }
}

impl Settings {
    /// Speech detector threshold for the chosen sensitivity.
    pub fn vad_threshold(&self) -> f32 {
        match self.sensitivity {
            0 => 0.8,
            1 => tovuer_voice::DEFAULT_VAD_THRESHOLD,
            _ => 0.3,
        }
    }

    pub fn is_muted(&self, name: &str) -> bool {
        self.muted.iter().any(|m| m == name)
    }

    /// Someone's volume as a factor (1.0 — unchanged).
    pub fn volume_of(&self, name: &str) -> f32 {
        self.volumes.get(name).copied().unwrap_or(100).min(300) as f32 / 100.0
    }
}

/// Where things live on disk.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Settings and this install's identity.
    pub config: PathBuf,
    /// Logs; safe to delete.
    pub cache: PathBuf,
    /// Rooms (their history), shared files, client releases.
    pub data: PathBuf,
}

impl Paths {
    /// Linux: `~/.config/tovuer`, `~/.cache/tovuer`, `~/.local/share/tovuer`;
    /// Windows: `%APPDATA%\tovuer` and `%LOCALAPPDATA%\tovuer`.
    pub fn system() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let config_root = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        let cache_root = dirs::cache_dir().unwrap_or_else(|| home.join(".cache"));
        let data_root = dirs::data_dir().unwrap_or_else(|| home.join(".local").join("share"));
        Self { config: config_root.join("tovuer"), cache: cache_root.join("tovuer"), data: data_root.join("tovuer") }
    }

    /// Everything under one folder — for tests and `--config-dir`.
    pub fn at(dir: &Path) -> Self {
        Self { config: dir.to_path_buf(), cache: dir.join("cache"), data: dir.join("data") }
    }

    /// The key that identifies this install to other members.
    pub fn identity_file(&self) -> PathBuf {
        self.config.join("identity.key")
    }

    /// Rooms this install has been in: their operation logs and known peers.
    pub fn rooms_dir(&self) -> PathBuf {
        self.data.join("rooms")
    }

    /// Client builds handed out to the room by `/update` (`./run.sh release` puts them here).
    pub fn release_dir(&self) -> PathBuf {
        self.data.join("release")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    pub fn state_file(&self) -> PathBuf {
        self.config.join("state.json")
    }

    /// Shared files, named by content hash. Kept, not cached: we hand them
    /// on to members who ask.
    pub fn file_cache(&self) -> PathBuf {
        self.data.join("files")
    }

    pub fn load_settings(&self) -> Settings {
        std::fs::read(self.settings_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) -> std::io::Result<()> {
        write_json(&self.settings_file(), settings)
    }

    pub fn load_state(&self) -> State {
        std::fs::read(self.state_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, state: &State) -> std::io::Result<()> {
        write_json(&self.state_file(), state)
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)
}

/// How far each channel has been read: the key is `room/channel`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct State {
    pub read: BTreeMap<String, u64>,
}

impl State {
    fn key(server: &str, channel: &str) -> String {
        format!("{server}/{channel}")
    }

    pub fn last_read(&self, server: &str, channel: &str) -> Option<u64> {
        self.read.get(&Self::key(server, channel)).copied()
    }

    /// Returns `true` if the mark moved forward.
    pub fn mark_read(&mut self, server: &str, channel: &str, id: u64) -> bool {
        let slot = self.read.entry(Self::key(server, channel)).or_insert(0);
        if id > *slot {
            *slot = id;
            true
        } else {
            false
        }
    }
}

/// Where downloads go: the Downloads folder, or the current folder without one.
pub fn download_dir() -> PathBuf {
    dirs::download_dir()
        .filter(|d| d.is_dir())
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")).filter(|d| d.is_dir()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A path in `dir` not taken by another file: `cat.png`, `cat (1).png`, `cat (2).png`…
pub fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Expand a leading `~/`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest)).unwrap_or_else(|| PathBuf::from(path)),
        None if path == "~" => dirs::home_dir().unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// "12.3 KB", "1.5 MB".
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tovuer-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_roundtrip_and_tolerate_unknown_or_missing_fields() {
        let dir = temp("roundtrip");
        let paths = Paths::at(&dir);
        assert_eq!(paths.load_settings(), Settings::default());

        let mut s = Settings { theme: "nord".into(), mic: "USB".into(), volume: 140, ..Settings::default() };
        s.volumes.insert("bob".into(), 60);
        paths.save_settings(&s).unwrap();
        assert_eq!(paths.load_settings(), s);
        assert_eq!(s.volume_of("bob"), 0.6);
        assert_eq!(s.volume_of("kate"), 1.0);

        // a file from a future version with an extra field and half of ours missing
        std::fs::write(paths.settings_file(), r#"{"theme":"mono","future":42}"#).unwrap();
        let loaded = paths.load_settings();
        assert_eq!(loaded.theme, "mono");
        assert!(loaded.denoise && loaded.quick_keys);

        std::fs::write(paths.settings_file(), "not json").unwrap();
        assert_eq!(paths.load_settings(), Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_marks_only_move_forward() {
        let mut st = State::default();
        assert_eq!(st.last_read("home", "general"), None);
        assert!(st.mark_read("home", "general", 10));
        assert!(!st.mark_read("home", "general", 7));
        assert!(st.mark_read("other", "general", 3));
        assert_eq!(st.last_read("home", "general"), Some(10));
        assert_eq!(st.last_read("other", "general"), Some(3));
    }

    #[test]
    fn free_path_never_overwrites() {
        let dir = temp("free");
        assert_eq!(free_path(&dir, "cat.png"), dir.join("cat.png"));
        std::fs::write(dir.join("cat.png"), b"1").unwrap();
        assert_eq!(free_path(&dir, "cat.png"), dir.join("cat (1).png"));
        std::fs::write(dir.join("cat (1).png"), b"2").unwrap();
        assert_eq!(free_path(&dir, "cat.png"), dir.join("cat (2).png"));
        std::fs::write(dir.join("README"), b"3").unwrap();
        assert_eq!(free_path(&dir, "README"), dir.join("README (1)"));
        std::fs::write(dir.join(".env"), b"4").unwrap();
        assert_eq!(free_path(&dir, ".env"), dir.join(".env (1)"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 << 20), "5.0 MB");
        assert_eq!(human_size(3 << 30), "3.0 GB");
    }

    #[test]
    fn sensitivity_maps_to_threshold() {
        let strict = Settings { sensitivity: 0, ..Settings::default() }.vad_threshold();
        let normal = Settings::default().vad_threshold();
        let keen = Settings { sensitivity: 2, ..Settings::default() }.vad_threshold();
        assert!(strict > normal && normal > keen);
    }
}
