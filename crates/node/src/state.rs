//! Room state folded from the operation log. Every peer folds the same
//! operations, so everyone sees the same channels and messages. Operations
//! may arrive in any order between authors: edits and reactions for a message
//! not seen yet wait for it, last write wins for edits, topics and reactions.
//!
//! [`Room::apply`] returns what changed as [`ServerMsg`]s for the local client.

use std::collections::{BTreeMap, HashMap};

use tovuer_proto::{self as proto, Channel, ChannelKind, FileMeta, Message, Reaction, ServerMsg};

use crate::ops::{Body, Key, Op};
use crate::util::unhex;

/// Author shown on service lines ("bob created #memes").
pub const SYSTEM_NAME: &str = "tovuer";
/// Most channels a room can have.
pub const MAX_CHANNELS: usize = 50;
const MAX_EMOJI_BYTES: usize = 32;

/// Channels every room starts with.
const DEFAULT_CHANNELS: &[(&str, ChannelKind, &str)] = &[
    ("general", ChannelKind::Text, "chat about anything"),
    ("random", ChannelKind::Text, "memes, links, pictures"),
    ("Lounge", ChannelKind::Voice, ""),
    ("Gaming", ChannelKind::Voice, ""),
];

#[derive(Debug, Clone)]
struct Chan {
    name: String,
    kind: ChannelKind,
    topic: String,
    topic_order: Option<(u64, Key, u64)>,
    created: Option<(u64, Key, u64)>,
    deleted: Option<u64>,
    /// Sort key: when the channel first appeared.
    first: u64,
}

impl Chan {
    fn pending(name: String) -> Self {
        Self {
            name, kind: ChannelKind::Text, topic: String::new(), topic_order: None,
            created: None, deleted: None, first: u64::MAX,
        }
    }

    fn alive(&self) -> bool {
        self.created.map(|order| order.0) > self.deleted
    }
}

#[derive(Debug, Clone)]
struct ReactMark {
    ts: u64,
    on: bool,
    nick: String,
}

#[derive(Debug, Clone)]
struct Entry {
    msg: Message,
    author: Key,
    edit_ts: u64,
    reacts: BTreeMap<String, BTreeMap<Key, ReactMark>>,
}

impl Entry {
    fn reactions(&self) -> Vec<Reaction> {
        let mut list: Vec<(u64, Reaction)> = self
            .reacts
            .iter()
            .filter_map(|(emoji, marks)| {
                let mut on: Vec<&ReactMark> = marks.values().filter(|m| m.on).collect();
                on.sort_by_key(|m| m.ts);
                let first = on.first()?.ts;
                Some((first, Reaction { emoji: emoji.clone(), users: on.iter().map(|m| m.nick.clone()).collect() }))
            })
            .collect();
        list.sort_by_key(|(first, _)| *first);
        list.into_iter().map(|(_, r)| r).collect()
    }

    fn snapshot(&self) -> Message {
        Message { reactions: self.reactions(), ..self.msg.clone() }
    }
}

/// Operation on a message that has not arrived yet.
#[derive(Debug, Clone)]
struct Pending {
    author: Key,
    ts: u64,
    nick: String,
    body: Body,
}

#[derive(Debug, Default)]
pub struct Room {
    title: String,
    /// Author of the earliest genesis and when it was made.
    owner: Option<(u64, Key)>,
    channels: BTreeMap<String, Chan>,
    messages: HashMap<String, BTreeMap<u64, Entry>>,
    /// Message id → channel.
    index: HashMap<u64, String>,
    deleted: std::collections::HashSet<u64>,
    pending: HashMap<u64, Vec<Pending>>,
    bans: HashMap<Key, (u64, String)>,
    nicks: HashMap<Key, (u64, String)>,
    /// File id → who posted it (they are the first to ask for the bytes).
    files: HashMap<String, Key>,
}

/// What an operation means for the node itself, beyond client events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Banned(Key),
    OwnerSet(Key),
    /// An earlier genesis showed up: the room has a different owner, and
    /// everything the old one did must be checked again — fold from scratch.
    OwnerChanged(Key),
}

impl Room {
    pub fn new(title: &str) -> Self {
        let mut room = Room { title: title.to_string(), ..Room::default() };
        for (i, (name, kind, topic)) in DEFAULT_CHANNELS.iter().enumerate() {
            room.channels.insert(
                name.to_string(),
                Chan {
                    name: name.to_string(),
                    kind: *kind,
                    topic: topic.to_string(),
                    topic_order: None,
                    created: Some((0, [0; 32], 0)),
                    deleted: None,
                    first: i as u64,
                },
            );
        }
        room
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn owner(&self) -> Option<Key> {
        self.owner.map(|(_, key)| key)
    }

    /// Fold operations from scratch, oldest first.
    pub fn fold<'a>(title: &str, ops: impl IntoIterator<Item = &'a Op>) -> Self {
        let mut room = Room::new(title);
        for op in ops {
            room.apply(op);
        }
        room
    }

    pub fn is_banned(&self, key: &Key) -> bool {
        self.bans.contains_key(key)
    }

    pub fn file_author(&self, file_id: &str) -> Option<Key> {
        self.files.get(file_id).copied()
    }

    /// Apply the limit to a deterministic view. Keep concurrent creations
    /// in the log and state so that delivery order cannot decide the last slot.
    fn visible_channels(&self) -> Vec<&Chan> {
        let mut live: Vec<_> = self.channels.values().filter(|c| c.alive()).collect();
        live.sort_unstable_by_key(|c| (c.first, &c.name));
        live.truncate(MAX_CHANNELS);
        live
    }

    /// Live channels: text first, then voice, each in the order they appeared.
    pub fn channels(&self) -> Vec<Channel> {
        let mut live = self.visible_channels();
        live.sort_by_key(|c| c.kind == ChannelKind::Voice);
        live.into_iter().map(|c| Channel { name: c.name.clone(), kind: c.kind, topic: c.topic.clone() }).collect()
    }

    pub fn channel(&self, name: &str, kind: ChannelKind) -> bool {
        self.visible_channels().iter().any(|c| c.name == name && c.kind == kind)
    }

    /// A live channel with this name in any letter case.
    pub fn channel_like(&self, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        self.visible_channels().into_iter().find(|c| c.name.to_lowercase() == lower).map(|c| c.name.clone())
    }

    fn first_text_channel(&self) -> Option<String> {
        self.channels().into_iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.name)
    }

    /// The message and who wrote it.
    pub fn message(&self, id: u64) -> Option<(Message, Key)> {
        let channel = self.index.get(&id)?;
        let entry = self.messages.get(channel)?.get(&id)?;
        Some((entry.snapshot(), entry.author))
    }

    /// Has `key` currently got reaction `emoji` on message `id`.
    pub fn reacted(&self, id: u64, emoji: &str, key: &Key) -> bool {
        self.index
            .get(&id)
            .and_then(|ch| self.messages.get(ch)?.get(&id))
            .and_then(|e| e.reacts.get(emoji)?.get(key))
            .is_some_and(|m| m.on)
    }

    pub fn reaction_count(&self, id: u64) -> usize {
        self.message(id).map_or(0, |(m, _)| m.reactions.len())
    }

    /// A page of history: up to `limit` messages before `before`, oldest first.
    pub fn history(&self, channel: &str, before: Option<u64>, limit: usize) -> (Vec<Message>, bool) {
        let Some(list) = self.messages.get(channel) else { return (Vec::new(), false) };
        let range: Vec<&Entry> = match before {
            Some(b) => list.range(..b).map(|(_, e)| e).collect(),
            None => list.values().collect(),
        };
        let start = range.len().saturating_sub(limit);
        (range[start..].iter().map(|e| e.snapshot()).collect(), start > 0)
    }

    /// Key that last used this nick (any letter case), not counting `except`.
    pub fn nick_holder(&self, nick: &str, except: &Key) -> Option<Key> {
        let lower = nick.to_lowercase();
        self.nicks
            .iter()
            .filter(|(key, _)| *key != except && !self.bans.contains_key(*key))
            .find(|(_, (_, n))| n.to_lowercase() == lower)
            .map(|(key, _)| *key)
    }

    fn insert(&mut self, entry: Entry) {
        let (id, channel) = (entry.msg.id, entry.msg.channel.clone());
        if let Some(f) = &entry.msg.file {
            self.files.entry(f.id.clone()).or_insert(entry.author);
        }
        self.index.insert(id, channel.clone());
        self.messages.entry(channel).or_default().insert(id, entry);
    }

    fn system(&mut self, op: &Op, channel: Option<String>, text: String) -> Option<ServerMsg> {
        let channel = channel?;
        let msg = Message {
            id: op.msg_id(),
            channel,
            from: SYSTEM_NAME.into(),
            ts: op.ts,
            text,
            reply: None,
            edited: None,
            file: None,
            reactions: Vec::new(),
            system: true,
            style: None,
        };
        if self.index.contains_key(&msg.id) {
            return None;
        }
        self.insert(Entry { msg: msg.clone(), author: [0; 32], edit_ts: 0, reacts: BTreeMap::new() });
        Some(ServerMsg::Msg { msg })
    }

    /// Fold one verified operation. Returns client events and what it means
    /// for the node.
    pub fn apply(&mut self, op: &Op) -> (Vec<ServerMsg>, Effect) {
        let Some(author) = op.author_key() else { return (Vec::new(), Effect::None) };
        if let Some((ban_ts, _)) = self.bans.get(&author)
            && op.ts >= *ban_ts
        {
            return (Vec::new(), Effect::None);
        }
        let nick = proto::clean_name(&op.nick).unwrap_or_else(|| "anon".into());
        match self.nicks.get(&author) {
            Some((ts, _)) if *ts > op.ts => {}
            _ => {
                self.nicks.insert(author, (op.ts, nick.clone()));
            }
        }

        let mut out = Vec::new();
        let mut effect = Effect::None;
        match op.body() {
            Body::Genesis { name } => {
                // the earliest genesis wins: someone who could not find the
                // room for a moment and "created" it again loses once they meet
                let candidate = (op.ts, author);
                match self.owner {
                    Some(current) if current <= candidate => {}
                    Some(_) => effect = Effect::OwnerChanged(author),
                    None => {
                        self.owner = Some(candidate);
                        if let Some(title) = proto::clean_text(&name, 64) {
                            self.title = proto::one_line(&title, 64);
                        }
                        effect = Effect::OwnerSet(author);
                        let target = self.first_text_channel();
                        out.extend(self.system(op, target, format!("{nick} created the room")));
                    }
                }
            }

            Body::Msg { channel, text, reply, file, style } => {
                let id = op.msg_id();
                if self.deleted.contains(&id) || self.index.contains_key(&id) {
                    return (out, effect);
                }
                let text = proto::clean_text(&text, proto::MAX_TEXT_CHARS).unwrap_or_default();
                let file = file.and_then(clean_file);
                let Some(channel) = proto::clean_channel(&channel) else { return (out, effect) };
                if text.is_empty() && file.is_none() {
                    return (out, effect);
                }
                let reply = reply.map(|r| proto::ReplyRef {
                    id: r.id,
                    from: proto::clean_name(&r.from).unwrap_or_default(),
                    text: proto::one_line(&r.text, proto::REPLY_PREVIEW_CHARS),
                });
                let msg = Message {
                    id,
                    channel: channel.clone(),
                    from: nick.clone(),
                    ts: op.ts,
                    text,
                    reply,
                    edited: None,
                    file,
                    reactions: Vec::new(),
                    system: false,
                    style: style.and_then(|s| s.clean()),
                };
                self.insert(Entry { msg, author, edit_ts: 0, reacts: BTreeMap::new() });
                // operations that arrived before the message itself
                let mut waiting = self.pending.remove(&id).unwrap_or_default();
                waiting.sort_by_key(|p| p.ts);
                for p in waiting {
                    self.apply_to_message(id, p);
                }
                if let Some((msg, _)) = self.message(id) {
                    out.push(ServerMsg::Msg { msg });
                }
            }

            body @ (Body::Edit { .. } | Body::Delete { .. } | Body::React { .. }) => {
                let id = match &body {
                    Body::Edit { id, .. } | Body::Delete { id } | Body::React { id, .. } => *id,
                    _ => unreachable!(),
                };
                let pending = Pending { author, ts: op.ts, nick: nick.clone(), body };
                if self.index.contains_key(&id) {
                    out.extend(self.apply_to_message(id, pending));
                } else if !self.deleted.contains(&id) {
                    self.pending.entry(id).or_default().push(pending);
                }
            }

            Body::ChannelCreate { name, kind } => {
                let Some(name) = proto::clean_channel(&name) else { return (out, effect) };
                let before = self.channels();
                let chan = self.channels.entry(name.clone()).or_insert_with(|| Chan::pending(name.clone()));
                let order = (op.ts, author, op.seq);
                chan.first = chan.first.min(op.ts);
                if Some(order) > chan.created {
                    chan.created = Some(order);
                    chan.kind = kind;
                }
                let channels = self.channels();
                if channels != before {
                    out.push(ServerMsg::Channels { channels });
                }
                let (target, note) = match kind {
                    ChannelKind::Text => (Some(name.clone()), format!("{nick} created channel #{name}")),
                    ChannelKind::Voice => (self.first_text_channel(), format!("{nick} created voice channel {name}")),
                };
                out.extend(self.system(op, target, note));
            }

            Body::ChannelTopic { name, topic } => {
                let Some(name) = proto::clean_channel(&name) else { return (out, effect) };
                let visible = self.channel(&name, ChannelKind::Text);
                let chan = self.channels.entry(name.clone()).or_insert_with(|| Chan::pending(name));
                let order = (op.ts, author, op.seq);
                if Some(order) > chan.topic_order {
                    chan.topic = proto::one_line(&topic, proto::MAX_TOPIC_CHARS);
                    chan.topic_order = Some(order);
                    if visible {
                        out.push(ServerMsg::Channels { channels: self.channels() });
                    }
                }
            }

            Body::ChannelDelete { name } => {
                if Some(author) != self.owner() {
                    return (out, effect);
                }
                let Some(name) = proto::clean_channel(&name) else { return (out, effect) };
                let before = self.channels();
                let chan = self.channels.entry(name.clone()).or_insert_with(|| Chan::pending(name.clone()));
                chan.deleted = Some(chan.deleted.unwrap_or(0).max(op.ts));
                let channels = self.channels();
                if channels != before {
                    out.push(ServerMsg::Channels { channels });
                }
                let target = self.first_text_channel();
                out.extend(self.system(op, target, format!("{nick} deleted channel {name}")));
            }

            Body::Ban { key, name } => {
                let Some(target) = unhex::<32>(&key) else { return (out, effect) };
                if Some(author) == self.owner() && Some(target) != self.owner() && !self.bans.contains_key(&target) {
                    self.bans.insert(target, (op.ts, proto::clean_name(&name).unwrap_or_default()));
                    effect = Effect::Banned(target);
                }
            }

            Body::Unknown => {}
        }
        (out, effect)
    }

    fn apply_to_message(&mut self, id: u64, p: Pending) -> Option<ServerMsg> {
        let owner = self.owner();
        let channel = self.index.get(&id)?.clone();
        let entry = self.messages.get_mut(&channel)?.get_mut(&id)?;
        match p.body {
            Body::Edit { text, .. } => {
                let text = proto::clean_text(&text, proto::MAX_TEXT_CHARS)?;
                if entry.author != p.author || entry.msg.system || p.ts <= entry.edit_ts {
                    return None;
                }
                entry.msg.text = text.clone();
                entry.msg.edited = Some(p.ts);
                entry.edit_ts = p.ts;
                Some(ServerMsg::MsgEdit { channel, id, text, edited: p.ts })
            }
            Body::Delete { .. } => {
                if entry.author != p.author && Some(p.author) != owner {
                    return None;
                }
                self.messages.get_mut(&channel)?.remove(&id);
                self.index.remove(&id);
                self.deleted.insert(id);
                Some(ServerMsg::MsgDelete { channel, id })
            }
            Body::React { emoji, on, .. } => {
                let emoji = clean_emoji(&emoji)?;
                let marks = entry.reacts.entry(emoji).or_default();
                if marks.get(&p.author).is_some_and(|m| m.ts >= p.ts) {
                    return None;
                }
                marks.insert(p.author, ReactMark { ts: p.ts, on, nick: p.nick });
                Some(ServerMsg::MsgReact { channel, id, reactions: entry.reactions() })
            }
            _ => None,
        }
    }
}

pub fn clean_emoji(raw: &str) -> Option<String> {
    let emoji = raw.trim();
    (!emoji.is_empty() && emoji.len() <= MAX_EMOJI_BYTES && !emoji.chars().any(|c| c.is_whitespace() || c.is_control()))
        .then(|| emoji.to_string())
}

/// File metadata from another peer: id must be a content hash, the name a
/// plain file name.
fn clean_file(file: FileMeta) -> Option<FileMeta> {
    if file.id.len() != 32 || !file.id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let name: String = file
        .name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .take(200)
        .collect();
    let name = name.trim().trim_start_matches('.').to_string();
    let name = if name.is_empty() { "file".to_string() } else { name };
    let image = proto::is_image_name(&name);
    Some(FileMeta { id: file.id.to_lowercase(), name, size: file.size, image })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::RoomKeys;
    use iroh::SecretKey;

    struct Author {
        secret: SecretKey,
        seq: u64,
        nick: String,
    }

    impl Author {
        fn new(n: u8, nick: &str) -> Self {
            Self { secret: SecretKey::from_bytes(&[n; 32]), seq: 0, nick: nick.into() }
        }
        fn key(&self) -> Key {
            *self.secret.public().as_bytes()
        }
        fn op(&mut self, ts: u64, body: Body) -> Op {
            self.seq += 1;
            Op::new(&self.secret, &RoomKeys::derive("t", ""), self.seq, ts, &self.nick, &body)
        }
    }

    fn msg(channel: &str, text: &str) -> Body {
        Body::Msg { channel: channel.into(), text: text.into(), reply: None, file: None, style: None }
    }

    fn texts(room: &Room, channel: &str) -> Vec<String> {
        room.history(channel, None, 100).0.into_iter().filter(|m| !m.system).map(|m| m.text).collect()
    }

    #[test]
    fn messages_are_ordered_by_time_whatever_the_arrival_order() {
        let (mut a, mut b) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let late = b.op(2000, msg("general", "second"));
        let early = a.op(1000, msg("general", "first"));
        let mut room = Room::new("home");
        room.apply(&late);
        let (events, _) = room.apply(&early);
        assert!(matches!(&events[..], [ServerMsg::Msg { msg }] if msg.text == "first" && msg.from == "dana"));
        assert_eq!(texts(&room, "general"), vec!["first", "second"]);
        // a duplicate changes nothing
        assert!(room.apply(&early).0.is_empty());
    }

    #[test]
    fn edits_and_deletes_respect_authorship_and_ownership() {
        let (mut owner, mut bob, mut eve) = (Author::new(1, "dana"), Author::new(2, "bob"), Author::new(3, "eve"));
        let mut room = Room::new("home");
        assert_eq!(room.apply(&owner.op(10, Body::Genesis { name: "home".into() })).1, Effect::OwnerSet(owner.key()));
        // a later genesis does not steal the room
        assert_eq!(room.apply(&eve.op(50, Body::Genesis { name: "mine".into() })).1, Effect::None);
        assert_eq!(room.owner(), Some(owner.key()));

        let m = bob.op(100, msg("general", "hello"));
        let id = m.msg_id();
        room.apply(&m);
        room.apply(&eve.op(110, Body::Edit { id, text: "hacked".into() }));
        assert_eq!(texts(&room, "general"), vec!["hello"]);
        let (events, _) = room.apply(&bob.op(120, Body::Edit { id, text: "hello!".into() }));
        assert!(matches!(&events[..], [ServerMsg::MsgEdit { text, .. }] if text == "hello!"));
        // an older edit arriving later loses
        room.apply(&bob.op(115, Body::Edit { id, text: "stale".into() }));
        assert_eq!(texts(&room, "general"), vec!["hello!"]);

        room.apply(&eve.op(130, Body::Delete { id }));
        assert_eq!(texts(&room, "general").len(), 1, "only author or owner deletes");
        room.apply(&owner.op(140, Body::Delete { id }));
        assert!(texts(&room, "general").is_empty());
        // the message cannot come back
        assert!(room.apply(&m).0.is_empty());
    }

    #[test]
    fn operations_wait_for_their_message() {
        let (mut a, mut b) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let m = a.op(100, msg("general", "pic"));
        let id = m.msg_id();
        let react = b.op(110, Body::React { id, emoji: "🔥".into(), on: true });
        let edit = a.op(120, Body::Edit { id, text: "pic!".into() });
        let mut room = Room::new("home");
        assert!(room.apply(&react).0.is_empty());
        assert!(room.apply(&edit).0.is_empty());
        let (events, _) = room.apply(&m);
        let ServerMsg::Msg { msg: got } = &events[0] else { panic!("{events:?}") };
        assert_eq!(got.text, "pic!");
        assert_eq!(got.reactions, vec![Reaction { emoji: "🔥".into(), users: vec!["bob".into()] }]);

        // a delete that arrives first wins over the message
        let doomed = a.op(200, msg("general", "oops"));
        let mut room = Room::new("home");
        room.apply(&a.op(210, Body::Delete { id: doomed.msg_id() }));
        assert!(room.apply(&doomed).0.is_empty());
    }

    #[test]
    fn reactions_toggle_per_user() {
        let (mut a, mut b) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let mut room = Room::new("home");
        let m = a.op(100, msg("general", "x"));
        let id = m.msg_id();
        room.apply(&m);
        room.apply(&b.op(110, Body::React { id, emoji: "👍".into(), on: true }));
        room.apply(&a.op(120, Body::React { id, emoji: "👍".into(), on: true }));
        room.apply(&a.op(125, Body::React { id, emoji: "🔥".into(), on: true }));
        assert!(room.reacted(id, "👍", &b.key()));
        let (events, _) = room.apply(&b.op(130, Body::React { id, emoji: "👍".into(), on: false }));
        let ServerMsg::MsgReact { reactions, .. } = &events[0] else { panic!() };
        assert_eq!(reactions[0], Reaction { emoji: "👍".into(), users: vec!["dana".into()] });
        assert_eq!(reactions.len(), 2);
        assert!(room.apply(&b.op(1, Body::React { id, emoji: "x y".into(), on: true })).0.is_empty());
    }

    #[test]
    fn channels_lifecycle() {
        let (mut owner, mut bob) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let mut room = Room::new("home");
        room.apply(&owner.op(1, Body::Genesis { name: "home".into() }));
        let (events, _) = room.apply(&bob.op(10, Body::ChannelCreate { name: "#Memes and cats".into(), kind: ChannelKind::Text }));
        assert!(matches!(&events[0], ServerMsg::Channels { channels } if channels.iter().any(|c| c.name == "Memes-and-cats")));
        assert!(matches!(&events[1], ServerMsg::Msg { msg } if msg.system && msg.text.contains("bob created")));
        room.apply(&bob.op(20, Body::ChannelTopic { name: "random".into(), topic: "all\nthings".into() }));
        assert_eq!(room.channels().iter().find(|c| c.name == "random").unwrap().topic, "all things");

        room.apply(&bob.op(30, Body::ChannelDelete { name: "random".into() }));
        assert!(room.channel("random", ChannelKind::Text), "only the owner deletes channels");
        room.apply(&owner.op(40, Body::ChannelDelete { name: "random".into() }));
        assert!(!room.channel("random", ChannelKind::Text));
        let names: Vec<String> = room.channels().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["general", "Memes-and-cats", "Lounge", "Gaming"]);
        // recreated later — it is back
        room.apply(&bob.op(50, Body::ChannelCreate { name: "random".into(), kind: ChannelKind::Text }));
        assert!(room.channel("random", ChannelKind::Text));
        assert_eq!(room.channel_like("GENERAL").as_deref(), Some("general"));
    }

    #[test]
    fn channel_deletion_can_arrive_before_creation() {
        let (mut owner, mut bob) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let genesis = owner.op(1, Body::Genesis { name: "home".into() });
        let create = bob.op(2, Body::ChannelCreate { name: "travel".into(), kind: ChannelKind::Text });
        let delete = owner.op(3, Body::ChannelDelete { name: "travel".into() });
        let expected = Room::fold("home", [&genesis, &create, &delete]);
        let mut reversed = Room::fold("home", [&genesis, &delete, &create]);
        assert_eq!(expected.channels(), reversed.channels());
        assert!(!reversed.channel("travel", ChannelKind::Text));
        assert_eq!(expected.history("general", None, 10), reversed.history("general", None, 10));
        reversed.apply(&bob.op(4, Body::ChannelCreate { name: "travel".into(), kind: ChannelKind::Text }));
        assert!(reversed.channel("travel", ChannelKind::Text));
    }

    #[test]
    fn concurrent_last_channel_slot_converges_and_can_be_freed() {
        let (mut a, mut b) = (Author::new(1, "alice"), Author::new(2, "bob"));
        let keys = RoomKeys::derive("t", "");
        let mut initial = vec![a.op(1, Body::Genesis { name: "home".into() })];
        let create = |name: String| Body::ChannelCreate { name, kind: ChannelKind::Text };
        for i in 0..45 {
            a.seq += 1;
            initial.push(Op::new(&a.secret, &keys, a.seq, 2+i, "alice", &create(format!("ch{i}"))));
        }
        let alpha = a.op(100, create("alpha".into()));
        let beta = b.op(100, create("beta".into()));
        let mut left = Room::fold("home", initial.iter().chain([&alpha, &beta]));
        let mut right = Room::fold("home", initial.iter().chain([&beta, &alpha]));
        assert_eq!(left.channels(), right.channels());
        assert_eq!(left.channels().len(), MAX_CHANNELS);
        assert!(left.channel("alpha", ChannelKind::Text));
        assert!(right.channel_like("beta").is_none());
        let delete = a.op(101, Body::ChannelDelete { name: "alpha".into() });
        for room in [&mut left, &mut right] {
            room.apply(&delete);
            assert!(room.channel("beta", ChannelKind::Text));
            assert_eq!(room.channels().len(), MAX_CHANNELS);
        }
    }

    #[test]
    fn earliest_genesis_wins_after_a_refold() {
        let (mut real, mut late) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let rogue = late.op(500, Body::Genesis { name: "home".into() });
        let rogue_delete = late.op(510, Body::ChannelDelete { name: "random".into() });
        let genesis = real.op(100, Body::Genesis { name: "home".into() });
        let mut room = Room::new("home");
        room.apply(&rogue);
        room.apply(&rogue_delete);
        assert!(!room.channel("random", ChannelKind::Text), "bob thought he owned it");
        assert_eq!(room.apply(&genesis).1, Effect::OwnerChanged(real.key()));
        let mut all = [rogue, rogue_delete, genesis];
        all.sort_by_key(|op| (op.ts, op.author.clone(), op.seq));
        let room = Room::fold("home", all.iter());
        assert_eq!(room.owner(), Some(real.key()));
        assert!(room.channel("random", ChannelKind::Text), "bob's delete no longer counts");
    }

    #[test]
    fn bans_silence_the_author() {
        let (mut owner, mut bob) = (Author::new(1, "dana"), Author::new(2, "bob"));
        let mut room = Room::new("home");
        room.apply(&owner.op(1, Body::Genesis { name: "home".into() }));
        room.apply(&bob.op(10, msg("general", "before")));
        let ban = owner.op(20, Body::Ban { key: crate::util::hex(&bob.key()), name: "bob".into() });
        assert_eq!(room.apply(&ban).1, Effect::Banned(bob.key()));
        room.apply(&bob.op(30, msg("general", "after")));
        assert_eq!(texts(&room, "general"), vec!["before"]);
        assert!(room.is_banned(&bob.key()));
        // the owner cannot be banned, and only the owner bans
        assert_eq!(room.apply(&bob.op(40, Body::Ban { key: crate::util::hex(&owner.key()), name: "x".into() })).1, Effect::None);
    }

    #[test]
    fn history_pages_and_nicks() {
        let mut a = Author::new(1, "dana");
        let mut room = Room::new("home");
        let ids: Vec<u64> = (0..5)
            .map(|i| {
                let op = a.op(100 + i, msg("general", &format!("m{i}")));
                room.apply(&op);
                op.msg_id()
            })
            .collect();
        let (page, more) = room.history("general", None, 2);
        assert_eq!((page.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), more), (vec!["m3", "m4"], true));
        let (page, more) = room.history("general", Some(ids[3]), 10);
        assert_eq!((page.len(), more), (3, false));
        assert_eq!(room.history("nope", None, 10), (Vec::new(), false));
        assert_eq!(room.nick_holder("DANA", &[9; 32]), Some(a.key()));
        assert_eq!(room.nick_holder("dana", &a.key()), None);
    }

    #[test]
    fn files_from_peers_are_sanitized() {
        let mut a = Author::new(1, "dana");
        let mut room = Room::new("home");
        let file = FileMeta { id: "AB".repeat(16), name: "../../.bashrc".into(), size: 10, image: false };
        room.apply(&a.op(1, Body::Msg { channel: "general".into(), text: String::new(), reply: None, file: Some(file), style: None }));
        let (page, _) = room.history("general", None, 10);
        let f = page[0].file.as_ref().unwrap();
        assert_eq!((f.id.as_str(), f.name.as_str()), ("ab".repeat(16).as_str(), "bashrc"));
        assert_eq!(room.file_author(&"ab".repeat(16)), Some(a.key()));
        let bad = FileMeta { id: "zz".into(), name: "x".into(), size: 1, image: false };
        room.apply(&a.op(2, Body::Msg { channel: "general".into(), text: String::new(), reply: None, file: Some(bad), style: None }));
        assert_eq!(room.history("general", None, 10).0.len(), 1);
    }
}
