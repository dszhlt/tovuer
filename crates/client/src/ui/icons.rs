//! Icons: plain single-color glyphs from a Nerd Font when the system has
//! one — they take the theme's colors — and emoji otherwise.

use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Icons {
    #[default]
    Emoji,
    Nerd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// A voice channel, being in one.
    Voice,
    /// A muted microphone, leaving voice.
    Muted,
    /// Sound turned off: hearing no one.
    Deafened,
    Mic,
    /// Turning the sound on and off.
    Sound,
    Admin,
    /// A file attached to a message.
    Attachment,
    Picture,
    Folder,
    File,
    Search,
    Theme,
    Settings,
    Edit,
    Loading,
}

impl Icon {
    pub const ALL: [Icon; 15] = [
        Icon::Voice,
        Icon::Muted,
        Icon::Deafened,
        Icon::Mic,
        Icon::Sound,
        Icon::Admin,
        Icon::Attachment,
        Icon::Picture,
        Icon::Folder,
        Icon::File,
        Icon::Search,
        Icon::Theme,
        Icon::Settings,
        Icon::Edit,
        Icon::Loading,
    ];

    /// The emoji and the Nerd Font glyph (its name in the font in the comment).
    /// A glyph is followed by a space it may draw over, as terminals let
    /// Nerd glyphs do: two cells, like an emoji.
    fn forms(self) -> (&'static str, &'static str) {
        match self {
            Icon::Voice => ("🔊", "\u{f485} "),       // oct-unmute
            Icon::Muted => ("🔇", "\u{f466} "),       // oct-mute
            Icon::Deafened => ("🔕", "\u{f07ce} "),   // md-headphones_off
            Icon::Mic => ("🎤", "\u{ec12} "),         // cod-mic
            Icon::Sound => ("🎧", "\u{ee58} "),       // fa-headphones_simple
            Icon::Admin => ("👑", "\u{f11d0} "),      // md-crown_outline
            Icon::Attachment => ("📎", "\u{f4fb} "),  // oct-paperclip
            Icon::Picture => ("📷", "\u{f4e5} "),     // oct-image
            Icon::Folder => ("📁", "\u{f413} "),      // oct-file_directory
            Icon::File => ("📄", "\u{f4a5} "),        // oct-file
            Icon::Search => ("🔍", "\u{f422} "),      // oct-search
            Icon::Theme => ("🎨", "\u{f0e0c} "),      // md-palette_outline
            Icon::Settings => ("⚙", "\u{f423} "),     // oct-gear
            Icon::Edit => ("✎", "\u{f448} "),         // oct-pencil
            Icon::Loading => ("⏳", "\u{f4e3} "),     // oct-hourglass
        }
    }
}

impl Icons {
    pub fn get(self, icon: Icon) -> &'static str {
        let (emoji, nerd) = icon.forms();
        match self {
            Icons::Emoji => emoji,
            Icons::Nerd => nerd,
        }
    }

    /// The icon and a space after it: three cells, the text starts clear of it.
    pub fn spaced(self, icon: Icon) -> String {
        format!("{} ", self.get(icon))
    }

    /// `TOVUER_ICONS=nerd|emoji` decides; otherwise Nerd glyphs when a font on
    /// the system has all of them (terminals fall back to it for missing glyphs).
    pub fn detect() -> Icons {
        match std::env::var("TOVUER_ICONS").as_deref() {
            Ok("nerd") => return Icons::Nerd,
            Ok("emoji") => return Icons::Emoji,
            _ => {}
        }
        if cfg!(windows) {
            return Icons::Emoji;
        }
        let found = Command::new("fc-list")
            .args([&format!(":charset={}", charset()), "family"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty());
        if found { Icons::Nerd } else { Icons::Emoji }
    }
}

/// Every glyph used, as fontconfig wants it: "f485 f466 …".
fn charset() -> String {
    let codes: Vec<String> = Icon::ALL.iter().filter_map(|i| i.forms().1.chars().next()).map(|c| format!("{:x}", c as u32)).collect();
    codes.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text;

    #[test]
    fn a_nerd_glyph_takes_the_room_of_an_emoji() {
        for icon in Icon::ALL {
            assert_eq!(text::width(Icons::Nerd.get(icon)), 2, "{icon:?}");
            assert!(text::width(Icons::Emoji.get(icon)) <= 2, "{icon:?}");
            assert_eq!(text::width(&Icons::Nerd.spaced(icon)), 3);
        }
        assert!(charset().starts_with("f485 f466 f07ce"));
    }
}
