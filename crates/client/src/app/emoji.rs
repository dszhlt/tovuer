//! Emoji short codes: `:fire:` → 🔥. Only characters terminals reliably draw
//! two cells wide (no variation selectors).

pub const EMOJI: &[(&str, &str)] = &[
    ("+1", "👍"), ("-1", "👎"), ("ok", "👌"), ("clap", "👏"), ("wave", "👋"), ("pray", "🙏"),
    ("muscle", "💪"), ("handshake", "🤝"), ("eyes", "👀"), ("fire", "🔥"), ("100", "💯"),
    ("heart", "💖"), ("broken_heart", "💔"), ("star", "⭐"), ("sparkles", "✨"), ("zap", "⚡"),
    ("boom", "💥"), ("tada", "🎉"), ("rocket", "🚀"), ("check", "✅"), ("x", "❌"),
    ("question", "❓"), ("exclamation", "❗"), ("warning", "🚨"), ("bulb", "💡"), ("bell", "🔔"),
    ("smile", "😄"), ("grin", "😁"), ("joy", "😂"), ("rofl", "🤣"), ("wink", "😉"),
    ("blush", "😊"), ("cool", "😎"), ("heart_eyes", "😍"), ("kiss", "😘"), ("thinking", "🤔"),
    ("neutral", "😐"), ("eyeroll", "🙄"), ("smirk", "😏"), ("sweat", "😅"), ("sob", "😭"),
    ("cry", "😢"), ("angry", "😠"), ("rage", "😡"), ("scream", "😱"), ("wow", "😮"),
    ("sleepy", "😴"), ("sick", "🤢"), ("clown", "🤡"), ("skull", "💀"), ("ghost", "👻"),
    ("poop", "💩"), ("devil", "😈"), ("angel", "😇"), ("nerd", "🤓"), ("party", "🥳"),
    ("shrug", "🤷"), ("facepalm", "🤦"), ("salute", "🫡"), ("melt", "🫠"), ("hug", "🤗"),
    ("cat", "🐱"), ("dog", "🐶"), ("fox", "🦊"), ("bear", "🐻"), ("frog", "🐸"),
    ("monkey", "🐵"), ("unicorn", "🦄"), ("crab", "🦀"), ("snake", "🐍"), ("duck", "🦆"),
    ("pizza", "🍕"), ("burger", "🍔"), ("coffee", "☕"), ("beer", "🍺"), ("cake", "🍰"),
    ("popcorn", "🍿"), ("game", "🎮"), ("dice", "🎲"), ("trophy", "🏆"), ("medal", "🏅"),
    ("music", "🎵"), ("mic", "🎤"), ("headphones", "🎧"), ("movie", "🎬"), ("camera", "📷"),
    ("pc", "💻"), ("phone", "📱"), ("money", "💰"), ("gem", "💎"), ("gift", "🎁"),
    ("lock", "🔒"), ("key", "🔑"), ("link", "🔗"), ("clip", "📎"), ("pin", "📌"),
    ("book", "📖"), ("memo", "📝"), ("calendar", "📅"), ("clock", "⏰"), ("hourglass", "⏳"),
    ("sun", "🌞"), ("moon", "🌙"), ("rainbow", "🌈"), ("snow", "⛄"), ("tree", "🌲"),
    ("earth", "🌍"), ("car", "🚗"), ("plane", "🛫"), ("home", "🏠"), ("flag", "🚩"),
];

/// Quick reactions — the first row of the picker.
pub const QUICK: &[&str] = &["👍", "💖", "😂", "🔥", "😮", "😢", "👀", "🎉", "💯", "🤔", "👎", "✅"];

/// Replace every `:code:` with its emoji; unknown codes stay as they are.
pub fn expand(text: &str) -> String {
    if !text.contains(':') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let found = after.find(':').and_then(|end| {
            let code = &after[..end];
            EMOJI.iter().find(|(name, _)| *name == code).map(|(_, e)| (*e, end))
        });
        match found {
            Some((emoji, end)) => {
                out.push_str(emoji);
                rest = &after[end + 1..];
            }
            None => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Codes starting with `prefix` (then those containing it) — for completion.
pub fn matching(prefix: &str) -> Vec<(&'static str, &'static str)> {
    let prefix = prefix.to_lowercase();
    let mut starts: Vec<_> = EMOJI.iter().filter(|(n, _)| n.starts_with(&prefix)).copied().collect();
    let contains = EMOJI.iter().filter(|(n, _)| !n.starts_with(&prefix) && n.contains(&prefix)).copied();
    starts.extend(contains);
    starts
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn expands_known_codes_only() {
        assert_eq!(expand("go :fire: :tada:!"), "go 🔥 🎉!");
        assert_eq!(expand("time 12:30:45"), "time 12:30:45");
        assert_eq!(expand(":no_such_code: and :+1:"), ":no_such_code: and 👍");
        assert_eq!(expand("no colons"), "no colons");
        assert_eq!(expand("::fire::"), ":🔥:");
        assert_eq!(expand(":"), ":");
    }

    #[test]
    fn matching_prefers_prefix() {
        let m = matching("he");
        assert_eq!(m[0].0, "heart");
        assert!(m.iter().any(|(n, _)| *n == "broken_heart"));
        assert!(matching("zzzz").is_empty());
    }

    #[test]
    fn table_is_sane() {
        let mut names: Vec<&str> = EMOJI.iter().map(|(n, _)| *n).collect();
        names.sort();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total, "codes do not repeat");
        for (name, emoji) in EMOJI {
            assert_eq!(UnicodeWidthStr::width(*emoji), 2, ":{name}: must be 2 wide");
            assert!(!emoji.contains('\u{fe0f}'), ":{name}: without a variation selector");
        }
        for emoji in QUICK {
            assert_eq!(UnicodeWidthStr::width(*emoji), 2);
        }
    }
}
