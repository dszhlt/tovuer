//! Pop-up windows over the chat and their own state.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use tovuer_proto::{ChannelKind, FileMeta};
use tovuer_voice::DeviceInfo;

use super::input::Input;
use crate::transfer::{Picture, Sharp, Thumb};

/// Emoji per row in the reaction window.
pub const REACT_COLS: usize = 6;

pub enum Overlay {
    Help { scroll: usize },
    Log { scroll: usize },
    Palette(Palette),
    Settings(SettingsView),
    Picker(Picker),
    Viewer(Viewer),
    Users { sel: usize },
    React { msg: u64, sel: usize },
    Confirm(Confirm),
    Prompt(Prompt),
}

// ------------------------------------------------------------ command palette

/// An action to pick in the palette (Ctrl+K).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Channel(String),
    Voice(String),
    LeaveVoice,
    Mute,
    Deafen,
    Upload,
    Settings,
    Users,
    Theme(String),
    NewChannel(ChannelKind),
    DeleteChannel(String),
    ToggleMembers,
    Help,
    Log,
    Update,
    Reconnect,
    Quit,
}

#[derive(Debug, Clone)]
pub struct Action {
    pub icon: &'static str,
    pub label: String,
    /// Hint on the right: a key or a note.
    pub hint: String,
    pub cmd: Cmd,
}

pub struct Palette {
    pub query: Input,
    pub actions: Vec<Action>,
    /// Indexes of `actions` that pass the filter, best first.
    pub shown: Vec<usize>,
    pub sel: usize,
}

/// How well `query` matches `text`: every letter of the query must appear
/// in order. Higher is better; `None` — no match.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let query = query.to_lowercase();
    let text = text.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    if let Some(at) = text.find(&query) {
        // a whole substring beats scattered letters, and at a word start even more
        let word_start = at == 0 || text[..at].ends_with(|c: char| !c.is_alphanumeric());
        return Some(1000 - at as i32 + if word_start { 500 } else { 0 });
    }
    let mut score = 0;
    let mut chars = text.chars().enumerate();
    let mut last: Option<usize> = None;
    for q in query.chars() {
        let (i, _) = chars.by_ref().find(|(_, c)| *c == q)?;
        score += match last {
            Some(prev) if i == prev + 1 => 15,
            _ => 5,
        };
        score -= i as i32 / 4;
        last = Some(i);
    }
    Some(score)
}

impl Palette {
    pub fn new(actions: Vec<Action>) -> Self {
        let shown = (0..actions.len()).collect();
        Self { query: Input::new(60), actions, shown, sel: 0 }
    }

    /// Rebuild the list for the current query.
    pub fn refilter(&mut self) {
        let query = self.query.text().trim().to_string();
        let mut scored: Vec<(i32, usize)> = self
            .actions
            .iter()
            .enumerate()
            .filter_map(|(i, a)| fuzzy(&query, &a.label).map(|s| (s, i)))
            .collect();
        // equal scores keep the original order
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.shown = scored.into_iter().map(|(_, i)| i).collect();
        self.sel = 0;
    }

    pub fn chosen(&self) -> Option<&Action> {
        self.shown.get(self.sel).map(|i| &self.actions[*i])
    }

    pub fn step(&mut self, delta: isize) {
        if !self.shown.is_empty() {
            self.sel = (self.sel as isize + delta).rem_euclid(self.shown.len() as isize) as usize;
        }
    }
}

// ------------------------------------------------------------------ settings

/// Rows of the settings window, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    Transparent,
    Members,
    Previews,
    Sharp,
    Mouse,
    Mic,
    Spk,
    Denoise,
    Agc,
    MicGain,
    Volume,
    Sensitivity,
    Monitor,
    Sounds,
    Bell,
    Notify,
    NotifyAll,
    QuickKeys,
}

impl Setting {
    pub const ALL: [Setting; 19] = [
        Setting::Theme,
        Setting::Transparent,
        Setting::Members,
        Setting::Previews,
        Setting::Sharp,
        Setting::Mouse,
        Setting::Mic,
        Setting::Spk,
        Setting::Denoise,
        Setting::Agc,
        Setting::MicGain,
        Setting::Volume,
        Setting::Sensitivity,
        Setting::Monitor,
        Setting::Sounds,
        Setting::Bell,
        Setting::Notify,
        Setting::NotifyAll,
        Setting::QuickKeys,
    ];

    /// The section heading, if a section starts at this row.
    pub fn section(self) -> Option<&'static str> {
        match self {
            Setting::Theme => Some("LOOK"),
            Setting::Mic => Some("AUDIO"),
            Setting::Sounds => Some("NOTIFICATIONS"),
            Setting::QuickKeys => Some("CONTROLS"),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Setting::Theme => "Theme",
            Setting::Transparent => "Transparent background",
            Setting::Members => "Online panel",
            Setting::Previews => "Picture previews in chat",
            Setting::Sharp => "Sharp pictures",
            Setting::Mouse => "Mouse",
            Setting::Mic => "Microphone",
            Setting::Spk => "Speakers",
            Setting::Denoise => "Noise suppression",
            Setting::Agc => "Automatic mic level",
            Setting::MicGain => "Microphone gain",
            Setting::Volume => "Volume of others",
            Setting::Sensitivity => "Microphone sensitivity",
            Setting::Monitor => "Microphone test",
            Setting::Sounds => "Join and leave sounds",
            Setting::Bell => "Bell on mention",
            Setting::Notify => "Desktop notifications",
            Setting::NotifyAll => "Notify about every message",
            Setting::QuickKeys => "M and U on an empty line",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Setting::Theme => "← → flip through themes, changes show at once",
            Setting::Transparent => "do not paint the background: the terminal's own shows through",
            Setting::Members => "the member list on the right (always hidden in narrow windows)",
            Setting::Previews => "show thumbnails of shared pictures",
            Setting::Sharp => "the viewer draws real pixels (sixel/kitty) if the terminal can; o there opens the original",
            Setting::Mouse => "wheel and clicks; selecting text then needs Shift",
            Setting::Mic | Setting::Spk => "← → pick a device; “system default” follows the OS settings",
            Setting::Denoise => "removes room noise, fans and keyboard clatter",
            Setting::Agc => "a quiet voice is raised, a loud one is tamed",
            Setting::MicGain => "manual boost on top of the automatic level",
            Setting::Volume => "overall volume; per person — separately, key U",
            Setting::Sensitivity => "strict — only clear speech; sensitive — catches whispers too",
            Setting::Monitor => "hear yourself the way others do (headphones recommended)",
            Setting::Sounds => "a chime when someone joins or leaves the voice channel",
            Setting::Bell => "the terminal bell when someone @mentions you",
            Setting::Notify => "a desktop notification while the window is not focused",
            Setting::NotifyAll => "otherwise — mentions only",
            Setting::QuickKeys => "a lone M (microphone) or U (volumes) works while the line is empty",
        }
    }
}

#[derive(Default)]
pub struct SettingsView {
    pub sel: usize,
    /// System devices; `None` — the list is still being fetched.
    pub mics: Option<Vec<DeviceInfo>>,
    pub spks: Option<Vec<DeviceInfo>>,
}

/// The next device in a circle: "system default" (empty) → first → … → "system default".
pub fn cycle_device(current: &str, devices: &[DeviceInfo], delta: isize) -> String {
    let mut names: Vec<&str> = vec![""];
    names.extend(devices.iter().map(|d| d.name.as_str()));
    let lower = current.to_lowercase();
    let at = names
        .iter()
        .position(|n| *n == current)
        .or_else(|| (!current.is_empty()).then(|| names.iter().position(|n| n.to_lowercase().contains(&lower))).flatten())
        .unwrap_or(0);
    names[(at as isize + delta).rem_euclid(names.len() as isize) as usize].to_string()
}

// ------------------------------------------------------------------ file picker

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

pub enum Peek {
    Loading,
    Ready(Thumb),
    Failed,
}

pub struct Picker {
    pub dir: PathBuf,
    entries: Vec<Entry>,
    /// Indexes of `entries` that pass the filter.
    pub shown: Vec<usize>,
    pub sel: usize,
    pub hidden: bool,
    pub error: Option<String>,
    /// Typed text — keeps the files that contain it.
    pub filter: String,
    /// Preview of the picture under the cursor.
    pub peek: Option<(PathBuf, Peek)>,
}

impl Picker {
    pub fn open(dir: &Path) -> Self {
        let mut picker = Self {
            dir: dir.to_path_buf(),
            entries: Vec::new(),
            shown: Vec::new(),
            sel: 0,
            hidden: false,
            error: None,
            filter: String::new(),
            peek: None,
        };
        picker.load(dir);
        picker
    }

    /// Read a folder: folders first, then files, alphabetically.
    pub fn load(&mut self, dir: &Path) {
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(format!("{}: {e}", dir.display()));
                return;
            }
        };
        let mut entries: Vec<Entry> = read
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if !self.hidden && name.starts_with('.') {
                    return None;
                }
                // metadata() follows links: a link to a folder acts as a folder
                let meta = std::fs::metadata(e.path()).ok();
                Some(Entry {
                    name,
                    dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                    size: meta.as_ref().map_or(0, |m| m.len()),
                    modified: meta.and_then(|m| m.modified().ok()),
                })
            })
            .collect();
        entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        self.dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.entries = entries;
        self.error = None;
        self.filter.clear();
        self.refilter();
    }

    pub fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.shown = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| needle.is_empty() || e.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        self.sel = self.sel.min(self.shown.len().saturating_sub(1));
        if !needle.is_empty() {
            self.sel = 0;
        }
    }

    pub fn entry(&self, shown_index: usize) -> Option<&Entry> {
        self.shown.get(shown_index).map(|i| &self.entries[*i])
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entry(self.sel)
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        self.current().map(|e| self.dir.join(&e.name))
    }

    pub fn step(&mut self, delta: isize) {
        if !self.shown.is_empty() {
            self.sel = (self.sel as isize + delta).clamp(0, self.shown.len() as isize - 1) as usize;
        }
    }

    /// Go one folder up, with the cursor on the folder we came from.
    pub fn up(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else { return };
        let came_from = self.dir.file_name().map(|n| n.to_string_lossy().into_owned());
        self.load(&parent);
        if let Some(name) = came_from
            && let Some(at) = self.shown.iter().position(|i| self.entries[*i].name == name)
        {
            self.sel = at;
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.hidden = !self.hidden;
        let dir = self.dir.clone();
        self.load(&dir);
    }
}

// ------------------------------------------------------------------- picture

pub struct Viewer {
    pub file: FileMeta,
    pub pic: Option<Arc<Picture>>,
    pub error: Option<String>,
    /// The picture already fitted to the window (redone when the size changes).
    pub fitted: Option<(u16, u16, Thumb)>,
    /// The picture in the terminal's own graphics, for a window of `.0` cells.
    pub sharp: Option<((u16, u16), Arc<Sharp>)>,
    /// The window size a sharp picture was last asked for (encoding takes a moment).
    pub sharp_asked: Option<(u16, u16)>,
}

impl Viewer {
    pub fn new(file: FileMeta) -> Self {
        Self { file, pic: None, error: None, fitted: None, sharp: None, sharp_asked: None }
    }
}

// --------------------------------------------------------- confirm / prompt

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    DeleteMessage(u64),
    DeleteChannel(String),
}

pub struct Confirm {
    pub title: String,
    pub text: String,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    NewChannel(ChannelKind),
    Topic(String),
    SaveAs(FileMeta),
}

pub struct Prompt {
    pub title: String,
    pub hint: String,
    pub input: Input,
    pub action: PromptAction,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(label: &str) -> Action {
        Action { icon: "", label: label.into(), hint: String::new(), cmd: Cmd::Help }
    }

    #[test]
    fn fuzzy_ranks_whole_and_prefix_matches_first() {
        assert!(fuzzy("gen", "# general").unwrap() > fuzzy("gen", "go to general").unwrap());
        assert!(fuzzy("the", "Theme: nord").unwrap() > fuzzy("the", "other settings").unwrap());
        assert!(fuzzy("gnrl", "# general").is_some(), "scattered letters in order match");
        assert!(fuzzy("xyz", "# general").is_none());
        assert_eq!(fuzzy("", "anything"), Some(0));
        assert!(fuzzy("GEN", "# general").is_some(), "case does not matter");
    }

    #[test]
    fn palette_filters_and_wraps_selection() {
        let mut p = Palette::new(vec![action("Channel # general"), action("Channel # random"), action("Settings")]);
        assert_eq!(p.shown, vec![0, 1, 2]);
        p.step(-1);
        assert_eq!(p.sel, 2, "up from the first goes to the last");
        p.query.set("chan");
        p.refilter();
        assert_eq!(p.shown, vec![0, 1]);
        assert_eq!(p.chosen().unwrap().label, "Channel # general");
        p.query.set("zzz");
        p.refilter();
        assert!(p.chosen().is_none());
        p.step(1);
        assert_eq!(p.sel, 0);
    }

    #[test]
    fn devices_cycle_through_system_default() {
        let devs = vec![
            DeviceInfo { name: "USB PnP Audio Device Mono".into(), is_default: true },
            DeviceInfo { name: "Built-in Audio".into(), is_default: false },
        ];
        assert_eq!(cycle_device("", &devs, 1), "USB PnP Audio Device Mono");
        assert_eq!(cycle_device("Built-in Audio", &devs, 1), "");
        assert_eq!(cycle_device("", &devs, -1), "Built-in Audio");
        // a saved part of the name finds its device
        assert_eq!(cycle_device("usb pnp", &devs, 1), "Built-in Audio");
        assert_eq!(cycle_device("unplugged", &devs, 1), "USB PnP Audio Device Mono");
        assert_eq!(cycle_device("", &[], 1), "");
    }

    #[test]
    fn settings_sections_cover_all_rows() {
        assert_eq!(Setting::ALL.iter().filter(|s| s.section().is_some()).count(), 4);
        assert!(Setting::ALL[0].section().is_some(), "the first row opens a section");
        for s in Setting::ALL {
            assert!(!s.label().is_empty() && !s.help().is_empty());
        }
    }

    #[test]
    fn picker_lists_filters_and_walks() {
        let root = std::env::temp_dir().join(format!("tovuer-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Pictures")).unwrap();
        std::fs::create_dir_all(root.join("archive")).unwrap();
        std::fs::write(root.join("b.txt"), b"12345").unwrap();
        std::fs::write(root.join("A.png"), b"1").unwrap();
        std::fs::write(root.join(".secret"), b"1").unwrap();
        std::fs::write(root.join("Pictures/cat.png"), b"1").unwrap();

        let mut p = Picker::open(&root);
        let names = |p: &Picker| (0..p.shown.len()).map(|i| p.entry(i).unwrap().name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&p), vec!["archive", "Pictures", "A.png", "b.txt"], "folders first, case-insensitive");
        assert_eq!(p.entry(3).unwrap().size, 5);

        p.toggle_hidden();
        assert!(names(&p).contains(&".secret".to_string()));
        p.toggle_hidden();

        p.filter = "PNG".into();
        p.refilter();
        assert_eq!(names(&p), vec!["A.png"]);
        assert_eq!(p.current_path().unwrap(), p.dir.join("A.png"));

        p.load(&root.join("Pictures"));
        assert_eq!(names(&p), vec!["cat.png"]);
        assert!(p.filter.is_empty(), "moving to a folder resets the filter");
        p.up();
        assert_eq!(p.current().unwrap().name, "Pictures", "the cursor is on the folder we came from");

        p.step(100);
        assert_eq!(p.sel, 3);
        p.step(-100);
        assert_eq!(p.sel, 0);

        p.load(&root.join("no-such-folder"));
        assert!(p.error.is_some());
        assert_eq!(names(&p).len(), 4, "on an error the old list stays");
        std::fs::remove_dir_all(root).unwrap();
    }
}
