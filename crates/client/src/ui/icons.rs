//! Icons that have a plain, single-color version in Nerd Fonts. They take the
//! theme's colors, unlike emoji; without such a font the emoji stay.

use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Icons {
    #[default]
    Emoji,
    Nerd,
}

/// The Nerd Font glyphs used: Octicons "unmute" and "mute".
const NERD_VOICE: &str = "\u{f485} ";
const NERD_MUTED: &str = "\u{f466} ";

impl Icons {
    /// A voice channel, being in one. Two cells wide either way: a Nerd glyph
    /// is followed by a space it may draw over.
    pub fn voice(self) -> &'static str {
        match self {
            Icons::Emoji => "🔊",
            Icons::Nerd => NERD_VOICE,
        }
    }

    /// Before a voice channel's name in the list, with the gap after it.
    pub fn voice_channel(self) -> &'static str {
        match self {
            Icons::Emoji => "🔊 ",
            Icons::Nerd => "\u{f485}  ",
        }
    }

    /// A muted microphone, leaving voice.
    pub fn muted(self) -> &'static str {
        match self {
            Icons::Emoji => "🔇",
            Icons::Nerd => NERD_MUTED,
        }
    }

    /// `TOVUER_ICONS=nerd|emoji` decides; otherwise Nerd glyphs when a font on
    /// the system has them (terminals fall back to it for missing glyphs).
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
            .args([":charset=f466 f485", "family"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty());
        if found { Icons::Nerd } else { Icons::Emoji }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text;

    #[test]
    fn both_sets_take_the_same_room() {
        for icons in [Icons::Emoji, Icons::Nerd] {
            assert_eq!(text::width(icons.voice()), 2);
            assert_eq!(text::width(icons.muted()), 2);
            assert_eq!(text::width(icons.voice_channel()), 3);
        }
    }
}
