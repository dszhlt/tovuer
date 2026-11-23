//! tovuer types shared by the client and the peer node.
//!
//! [`ClientMsg`] is what the interface asks its local node to do;
//! [`ServerMsg`] is what the node tells the interface. Both are tagged JSON
//! (`{"t":"send","channel":"general","text":"hi"}`). The voice frame format
//! is in [`voice`]. How peers talk to each other lives in `tovuer-node`.

use serde::{Deserialize, Serialize};

pub mod voice;

/// Protocol version. Peers with different versions do not talk to each other.
pub const PROTO_VERSION: u32 = 4;

pub const MAX_NAME_CHARS: usize = 24;
pub const MAX_CHANNEL_CHARS: usize = 24;
pub const MAX_TEXT_CHARS: usize = 4000;
pub const MAX_TOPIC_CHARS: usize = 120;
/// How many different emoji one message can carry.
pub const MAX_REACTIONS: usize = 20;
/// Default history page size, and the most one request returns.
pub const HISTORY_PAGE: u32 = 100;
/// How many characters of the quoted message go into a reply preview.
pub const REPLY_PREVIEW_CHARS: usize = 120;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Text,
    Voice,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub name: String,
    pub kind: ChannelKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
}

impl Channel {
    pub fn text(name: &str, topic: &str) -> Self {
        Self { name: name.into(), kind: ChannelKind::Text, topic: topic.into() }
    }

    pub fn voice(name: &str) -> Self {
        Self { name: name.into(), kind: ChannelKind::Voice, topic: String::new() }
    }
}

/// A member online. `id` is local to this client and marks their voice frames.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: u32,
    pub name: String,
    /// Voice channel they are in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deafened: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub admin: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    pub id: String,
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub image: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

/// Quote of the message being answered, carried in the reply itself so the
/// client need not look the original up in history.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReplyRef {
    pub id: u64,
    pub from: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u64,
    pub channel: String,
    pub from: String,
    /// Unix time, milliseconds.
    pub ts: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<FileMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    /// A service line (a channel was created and so on).
    #[serde(default, skip_serializing_if = "is_false")]
    pub system: bool,
}

/// Interface → node.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        channel: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<u64>,
    },
    Edit {
        id: u64,
        text: String,
    },
    Delete {
        id: u64,
    },
    /// Add or remove a reaction (a toggle).
    React {
        id: u64,
        emoji: String,
    },
    Typing {
        channel: String,
    },
    History {
        channel: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    VoiceJoin {
        channel: String,
    },
    VoiceLeave,
    VoiceState {
        muted: bool,
        deafened: bool,
    },
    Speaking {
        on: bool,
    },
    ChannelCreate {
        name: String,
        kind: ChannelKind,
    },
    ChannelDelete {
        name: String,
    },
    ChannelTopic {
        name: String,
        topic: String,
    },
    Kick {
        name: String,
    },
}

/// Node → interface.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// We are in the room: everything the interface needs to start.
    Welcome {
        /// Our own user id.
        id: u32,
        name: String,
        /// Room name.
        server: String,
        version: String,
        proto: u32,
        /// We own the room.
        #[serde(default)]
        admin: bool,
        channels: Vec<Channel>,
        users: Vec<User>,
        max_file: u64,
        /// Client release the room owner hands out (for the "update" hint).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_version: Option<String>,
        /// A note to show on entry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        motd: Option<String>,
    },
    Error {
        code: ErrorCode,
        msg: String,
    },
    UserJoin {
        user: User,
    },
    UserLeave {
        id: u32,
    },
    UserUpdate {
        user: User,
    },
    Msg {
        msg: Message,
    },
    MsgEdit {
        channel: String,
        id: u64,
        text: String,
        edited: u64,
    },
    MsgDelete {
        channel: String,
        id: u64,
    },
    MsgReact {
        channel: String,
        id: u64,
        reactions: Vec<Reaction>,
    },
    History {
        channel: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<u64>,
        messages: Vec<Message>,
        /// There are older messages than these.
        more: bool,
    },
    Typing {
        channel: String,
        name: String,
    },
    Speaking {
        id: u32,
        on: bool,
    },
    Channels {
        channels: Vec<Channel>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NameTaken,
    BadName,
    ProtoMismatch,
    RateLimited,
    Forbidden,
    NotFound,
    BadRequest,
    Kicked,
    Storage,
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    /// After this, retrying on our own is pointless — the person has to act.
    pub fn is_fatal(self) -> bool {
        matches!(self, Self::NameTaken | Self::BadName | Self::ProtoMismatch | Self::Kicked)
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Unix time, milliseconds.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Make a nick acceptable: no control characters or spaces, at most
/// [`MAX_NAME_CHARS`]. `None` if nothing is left.
pub fn clean_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut gap = false;
    for ch in raw.trim().chars() {
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push('_');
        }
        gap = false;
        out.push(ch);
    }
    let out: String = out
        .trim_start_matches(['@', '#', '/', ':'])
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Channel name: letters, digits, `-` and `_`; spaces become `-`.
pub fn clean_channel(raw: &str) -> Option<String> {
    let mut out = String::new();
    for ch in raw.trim().trim_start_matches('#').chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_whitespace() && !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(MAX_CHANNEL_CHARS).collect();
    (!out.is_empty()).then_some(out)
}

/// Message text: no control characters (except newline and tab), cut to
/// `max` characters. `None` if empty.
pub fn clean_text(raw: &str, max: usize) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .map(|c| if c == '\t' { ' ' } else { c })
        .take(max)
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c.is_whitespace());
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A single line without newlines — for channel topics, quotes and such.
pub fn one_line(raw: &str, max: usize) -> String {
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() || c == '\n' { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut s: String = flat.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// Is `name` mentioned: `@name` (any case) or `@all` / `@everyone` / `@here`.
pub fn mentions(text: &str, name: &str) -> bool {
    let lower = text.to_lowercase();
    let name = name.to_lowercase();
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut prev: Option<char> = None;
    for (i, c) in lower.char_indices() {
        // an "@" inside a word is an email address, not a mention
        if c == '@' && !prev.is_some_and(is_word) {
            let after = &lower[i + 1..];
            if !name.is_empty()
                && after.starts_with(&name)
                && !after[name.len()..].chars().next().is_some_and(is_word)
            {
                return true;
            }
            let end = after.find(|c: char| !is_word(c)).unwrap_or(after.len());
            if matches!(&after[..end], "all" | "everyone" | "here") {
                return true;
            }
        }
        prev = Some(c);
    }
    false
}

/// By its extension, a picture the client can show.
pub fn is_image_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp"].iter().any(|ext| lower.ends_with(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_msg_wire_format_is_tagged() {
        let m = ClientMsg::Send { channel: "general".into(), text: "hi".into(), reply_to: None };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, r#"{"t":"send","channel":"general","text":"hi"}"#);
        assert_eq!(serde_json::from_str::<ClientMsg>(&s).unwrap(), m);

        let leave = serde_json::to_string(&ClientMsg::VoiceLeave).unwrap();
        assert_eq!(leave, r#"{"t":"voice_leave"}"#);
        assert_eq!(serde_json::from_str::<ClientMsg>(&leave).unwrap(), ClientMsg::VoiceLeave);
    }

    #[test]
    fn server_msg_roundtrip() {
        let msg = Message {
            id: 7,
            channel: "general".into(),
            from: "bob".into(),
            ts: 1_700_000_000_000,
            text: "look".into(),
            reply: Some(ReplyRef { id: 3, from: "dana".into(), text: "what is it?".into() }),
            edited: None,
            file: Some(FileMeta { id: "ab".into(), name: "pic.png".into(), size: 10, image: true }),
            reactions: vec![Reaction { emoji: "🔥".into(), users: vec!["dana".into()] }],
            system: false,
        };
        let s = serde_json::to_string(&ServerMsg::Msg { msg: msg.clone() }).unwrap();
        assert_eq!(serde_json::from_str::<ServerMsg>(&s).unwrap(), ServerMsg::Msg { msg });
    }

    #[test]
    fn unknown_error_code_does_not_break_parsing() {
        let m: ServerMsg = serde_json::from_str(r#"{"t":"error","code":"from_the_future","msg":"x"}"#).unwrap();
        assert_eq!(m, ServerMsg::Error { code: ErrorCode::Unknown, msg: "x".into() });
        assert!(!ErrorCode::Unknown.is_fatal());
        assert!(ErrorCode::NameTaken.is_fatal());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  dana  ").as_deref(), Some("dana"));
        assert_eq!(clean_name("big  boss").as_deref(), Some("big_boss"));
        assert_eq!(clean_name("@#/evil").as_deref(), Some("evil"));
        assert_eq!(clean_name("a\u{7}b\u{1b}c").as_deref(), Some("abc"));
        assert_eq!(clean_name(" \t "), None);
        assert_eq!(clean_name(&"é".repeat(60)).unwrap().chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn channels_are_cleaned() {
        assert_eq!(clean_channel("#Games and memes").as_deref(), Some("Games-and-memes"));
        assert_eq!(clean_channel("  lounge  ").as_deref(), Some("lounge"));
        assert_eq!(clean_channel("a/b\\c").as_deref(), Some("abc"));
        assert_eq!(clean_channel("###"), None);
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean_text("  hello\n\n", 100).as_deref(), Some("hello"));
        assert_eq!(clean_text("a\u{0}b\tc", 100).as_deref(), Some("ab c"));
        assert_eq!(clean_text("   ", 100), None);
        assert_eq!(clean_text("héllo", 3).as_deref(), Some("hél"));
        assert_eq!(one_line("one\ntwo   three", 100), "one two three");
        assert_eq!(one_line("abcdefgh", 5), "abcd…");
    }

    #[test]
    fn mentions_are_detected() {
        assert!(mentions("hey @dana look", "dana"));
        assert!(mentions("@DANA!", "dana"));
        assert!(mentions("@all voice in 5", "bob"));
        assert!(mentions("mail a@b.c and @bob", "bob"));
        assert!(!mentions("@danaik hi", "dana"));
        assert!(!mentions("dana without the at", "dana"));
        assert!(!mentions("@", "dana"));
        assert!(!mentions("trailing @", "dana"));
    }

    #[test]
    fn image_names() {
        assert!(is_image_name("Shot.PNG"));
        assert!(is_image_name("a.b.webp"));
        assert!(!is_image_name("notes.txt"));
        assert!(!is_image_name("png"));
    }
}
