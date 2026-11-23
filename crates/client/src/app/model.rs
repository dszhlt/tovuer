//! Chat data on the client side: channels with their messages, read marks,
//! scrolling and drafts.

use std::time::{Duration, Instant};

use tovuer_proto::{Channel, ChannelKind, Message, Reaction};

/// How long "typing…" lasts without being renewed.
pub const TYPING_TTL: Duration = Duration::from_secs(5);
/// Messages by one author within this time go without repeating the header.
pub const GROUP_WINDOW_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct ChannelState {
    pub name: String,
    pub kind: ChannelKind,
    pub topic: String,
    /// Ascending by id.
    pub messages: Vec<Message>,
    /// The first history page has arrived.
    pub loaded: bool,
    /// There are messages older than the loaded ones.
    pub more: bool,
    /// A request for older history is already out.
    pub loading: bool,
    /// The last read message.
    pub last_read: u64,
    pub unread: u32,
    pub mentions: u32,
    /// The first unread message when the channel was opened — the "new" line goes above it.
    pub first_unread: Option<u64>,
    /// Lines hidden below the bottom edge (0 — the feed is at the bottom).
    pub scroll: usize,
    /// The message at the bottom edge and the line in it: keeps the feed still
    /// while new messages arrive below and we read older ones.
    pub anchor: Option<(u64, usize)>,
    /// The selected message (message mode).
    pub selected: Option<u64>,
    /// On the next draw, scroll the selected message into view.
    pub reveal: bool,
    pub typing: Vec<(String, Instant)>,
    /// Unfinished text — each channel has its own.
    pub draft: String,
}

impl ChannelState {
    pub fn new(ch: &Channel) -> Self {
        Self {
            name: ch.name.clone(),
            kind: ch.kind,
            topic: ch.topic.clone(),
            messages: Vec::new(),
            loaded: false,
            more: false,
            loading: false,
            last_read: 0,
            unread: 0,
            mentions: 0,
            first_unread: None,
            scroll: 0,
            anchor: None,
            selected: None,
            reveal: false,
            typing: Vec::new(),
            draft: String::new(),
        }
    }

    pub fn is_text(&self) -> bool {
        self.kind == ChannelKind::Text
    }

    pub fn last_id(&self) -> u64 {
        self.messages.last().map_or(0, |m| m.id)
    }

    fn position(&self, id: u64) -> Result<usize, usize> {
        self.messages.binary_search_by_key(&id, |m| m.id)
    }

    pub fn find(&self, id: u64) -> Option<&Message> {
        self.position(id).ok().map(|i| &self.messages[i])
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut Message> {
        self.position(id).ok().map(|i| &mut self.messages[i])
    }

    /// Insert a message in its place. `false` — it was already there (updated).
    pub fn insert(&mut self, msg: Message) -> bool {
        match self.position(msg.id) {
            Ok(i) => {
                self.messages[i] = msg;
                false
            }
            Err(i) => {
                self.messages.insert(i, msg);
                true
            }
        }
    }

    /// A page of history (the latest or an older one).
    pub fn merge_history(&mut self, before: Option<u64>, messages: Vec<Message>, more: bool) {
        if before.is_none() {
            // The node sends snapshots and live events in order. Replace our
            // window, including any deletes or gaps accumulated while offline.
            self.messages = messages;
            self.selected = self.selected.filter(|id| self.find(*id).is_some());
            self.first_unread = self.first_unread.filter(|id| self.find(*id).is_some());
            if self.anchor.is_some_and(|(id, _)| self.find(id).is_none()) {
                self.anchor = None;
                self.scroll = 0;
            }
        } else {
            for msg in messages {
                self.insert(msg);
            }
        }
        self.more = more;
        self.loaded = true;
        self.loading = false;
    }

    pub fn remove(&mut self, id: u64) -> Option<Message> {
        let i = self.position(id).ok()?;
        if self.selected == Some(id) {
            // the selection moves to a neighbor instead of vanishing
            self.selected = self.messages.get(i + 1).or_else(|| i.checked_sub(1).and_then(|p| self.messages.get(p))).map(|m| m.id);
        }
        if self.first_unread == Some(id) {
            self.first_unread = self.messages.get(i + 1).map(|m| m.id);
        }
        Some(self.messages.remove(i))
    }

    pub fn edit(&mut self, id: u64, text: String, edited: u64) -> bool {
        match self.find_mut(id) {
            Some(m) => {
                m.text = text;
                m.edited = Some(edited);
                true
            }
            None => false,
        }
    }

    pub fn set_reactions(&mut self, id: u64, reactions: Vec<Reaction>) -> bool {
        match self.find_mut(id) {
            Some(m) => {
                m.reactions = reactions;
                true
            }
            None => false,
        }
    }

    /// Recount unread messages from the `last_read` mark.
    pub fn recount(&mut self, me: &str) {
        let fresh = self.messages.iter().filter(|m| m.id > self.last_read && m.from != me && !m.system);
        let (mut unread, mut mentions) = (0, 0);
        for m in fresh {
            unread += 1;
            if tovuer_proto::mentions(&m.text, me) {
                mentions += 1;
            }
        }
        self.unread = unread;
        self.mentions = mentions;
    }

    /// Remember where the unread part starts (for the "new" line).
    pub fn mark_first_unread(&mut self, me: &str) {
        if self.first_unread.is_none() {
            self.first_unread =
                self.messages.iter().find(|m| m.id > self.last_read && m.from != me && !m.system).map(|m| m.id);
        }
    }

    /// Everything is read. Returns the id read up to, if the mark moved.
    pub fn mark_read(&mut self) -> Option<u64> {
        self.unread = 0;
        self.mentions = 0;
        let last = self.last_id();
        if last > self.last_read {
            self.last_read = last;
            Some(last)
        } else {
            None
        }
    }

    /// Move the selection by `delta` messages (negative — towards older).
    /// Returns `true` if it hit the oldest loaded message.
    pub fn select_step(&mut self, delta: isize) -> bool {
        if self.messages.is_empty() {
            self.selected = None;
            return true;
        }
        let last = self.messages.len() - 1;
        let current = self.selected.and_then(|id| self.position(id).ok()).unwrap_or(last);
        let target = (current as isize + delta).clamp(0, last as isize) as usize;
        let hit_top = current as isize + delta < 0 || (target == 0 && delta < 0);
        self.selected = Some(self.messages[target].id);
        self.reveal = true;
        hit_top
    }

    pub fn select_last(&mut self) {
        self.selected = self.messages.last().map(|m| m.id);
        self.reveal = true;
    }

    pub fn selected_msg(&self) -> Option<&Message> {
        self.selected.and_then(|id| self.find(id))
    }

    pub fn note_typing(&mut self, name: &str, now: Instant) {
        self.typing.retain(|(n, _)| n != name);
        self.typing.push((name.to_string(), now + TYPING_TTL));
    }

    pub fn clear_typing(&mut self, name: &str) {
        self.typing.retain(|(n, _)| n != name);
    }

    /// Drop expired "typing" marks. `true` — something changed.
    pub fn expire_typing(&mut self, now: Instant) -> bool {
        let before = self.typing.len();
        self.typing.retain(|(_, until)| *until > now);
        self.typing.len() != before
    }

    /// Does message `i` need a header with the nick (or does it continue the previous one).
    pub fn starts_group(&self, i: usize) -> bool {
        let Some(prev) = i.checked_sub(1).map(|p| &self.messages[p]) else { return true };
        let cur = &self.messages[i];
        prev.from != cur.from
            || prev.system
            || cur.system
            || cur.reply.is_some()
            || cur.ts.saturating_sub(prev.ts) > GROUP_WINDOW_MS
            || !same_day(prev.ts, cur.ts)
    }
}

/// Local date of unix milliseconds.
pub fn local_date(ts: u64) -> chrono::NaiveDate {
    local_time(ts).date_naive()
}

pub fn local_time(ts: u64) -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::from_timestamp_millis(ts as i64)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
}

pub fn same_day(a: u64, b: u64) -> bool {
    local_date(a) == local_date(b)
}

/// "today", "yesterday", "November 10", "March 3, 2025".
pub fn day_label(ts: u64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
        "December",
    ];
    let date = local_date(ts);
    let today = now.date_naive();
    if date == today {
        "today".into()
    } else if Some(date) == today.pred_opt() {
        "yesterday".into()
    } else if date.year() == today.year() {
        format!("{} {}", MONTHS[date.month0() as usize], date.day())
    } else {
        format!("{} {}, {}", MONTHS[date.month0() as usize], date.day(), date.year())
    }
}

/// "1 message", "5 messages".
pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "3:07", "1:02:45" — time spent in a voice channel.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: u64, from: &str, text: &str) -> Message {
        Message {
            id,
            channel: "general".into(),
            from: from.into(),
            ts: 1_700_000_000_000 + id * 1000,
            text: text.into(),
            reply: None,
            edited: None,
            file: None,
            reactions: Vec::new(),
            system: false,
            style: None,
        }
    }

    fn channel() -> ChannelState {
        ChannelState::new(&Channel::text("general", ""))
    }

    #[test]
    fn messages_stay_sorted_and_deduplicated() {
        let mut ch = channel();
        assert!(ch.insert(msg(5, "a", "five")));
        assert!(ch.insert(msg(2, "a", "two")));
        assert!(ch.insert(msg(9, "a", "nine")));
        assert!(!ch.insert(msg(5, "a", "five-2")), "a repeat updates instead of duplicating");
        assert_eq!(ch.messages.iter().map(|m| m.id).collect::<Vec<_>>(), vec![2, 5, 9]);
        assert_eq!(ch.find(5).unwrap().text, "five-2");
        assert_eq!(ch.last_id(), 9);
    }

    #[test]
    fn history_pages_merge_with_live_messages() {
        let mut ch = channel();
        // a live message came before the history page
        ch.insert(msg(12, "bob", "fresh"));
        ch.merge_history(None, vec![msg(10, "a", "x"), msg(11, "a", "y"), msg(12, "bob", "fresh")], true);
        assert!(ch.loaded && ch.more);
        assert_eq!(ch.messages.len(), 3);
        // an older page — the last one
        ch.loading = true;
        ch.merge_history(Some(10), vec![msg(8, "a", "p"), msg(9, "a", "q")], false);
        assert!(!ch.more && !ch.loading);
        assert_eq!(ch.messages.iter().map(|m| m.id).collect::<Vec<_>>(), vec![8, 9, 10, 11, 12]);
    }

    #[test]
    fn unread_counts_only_others_and_tracks_mentions() {
        let mut ch = channel();
        for (id, from, text) in [(1, "bob", "one"), (2, "dana", "me myself"), (3, "bob", "hey @dana"), (4, "kate", "yo")] {
            ch.insert(msg(id, from, text));
        }
        ch.last_read = 1;
        ch.recount("dana");
        assert_eq!((ch.unread, ch.mentions), (2, 1));
        ch.mark_first_unread("dana");
        assert_eq!(ch.first_unread, Some(3));
        assert_eq!(ch.mark_read(), Some(4));
        assert_eq!((ch.unread, ch.mentions, ch.last_read), (0, 0, 4));
        assert_eq!(ch.mark_read(), None, "marking again moves nothing");
        assert_eq!(ch.first_unread, Some(3), "the \"new\" line stays while the channel is open");
    }

    #[test]
    fn selection_moves_and_survives_deletion() {
        let mut ch = channel();
        for id in 1..=4 {
            ch.insert(msg(id, "a", "x"));
        }
        assert!(!ch.select_step(-1));
        assert_eq!(ch.selected, Some(3), "the first step up starts from the last message");
        assert!(!ch.select_step(-1));
        assert!(ch.select_step(-5), "hit the start — time to load more history");
        assert_eq!(ch.selected, Some(1));
        ch.select_step(100);
        assert_eq!(ch.selected, Some(4));

        ch.remove(4);
        assert_eq!(ch.selected, Some(3), "after a delete a neighbor is selected");
        ch.selected = Some(1);
        ch.remove(1);
        assert_eq!(ch.selected, Some(2));
        ch.remove(2);
        ch.remove(3);
        assert_eq!(ch.selected, None);
        assert!(ch.select_step(-1));
    }

    #[test]
    fn edits_and_reactions_apply_by_id() {
        let mut ch = channel();
        ch.insert(msg(1, "a", "tpyo"));
        assert!(ch.edit(1, "typo".into(), 42));
        assert_eq!((ch.find(1).unwrap().text.as_str(), ch.find(1).unwrap().edited), ("typo", Some(42)));
        assert!(ch.set_reactions(1, vec![Reaction { emoji: "🔥".into(), users: vec!["b".into()] }]));
        assert_eq!(ch.find(1).unwrap().reactions.len(), 1);
        assert!(!ch.edit(7, "no".into(), 1));
        assert!(!ch.set_reactions(7, vec![]));
    }

    #[test]
    fn grouping_rules() {
        let mut ch = channel();
        ch.insert(msg(1, "bob", "one"));
        ch.insert(msg(2, "bob", "two"));
        ch.insert(msg(3, "kate", "three"));
        let mut late = msg(4, "kate", "an hour later");
        late.ts += 3_600_000;
        ch.insert(late);
        let mut reply = msg(5, "kate", "reply");
        reply.ts = ch.messages[3].ts + 1000;
        reply.reply = Some(tovuer_proto::ReplyRef { id: 1, from: "bob".into(), text: "one".into() });
        ch.insert(reply);
        let groups: Vec<bool> = (0..5).map(|i| ch.starts_group(i)).collect();
        assert_eq!(groups, vec![true, false, true, true, true]);
    }

    #[test]
    fn typing_expires() {
        let mut ch = channel();
        let now = Instant::now();
        ch.note_typing("bob", now);
        ch.note_typing("bob", now);
        ch.note_typing("kate", now);
        assert_eq!(ch.typing.len(), 2);
        assert!(!ch.expire_typing(now + Duration::from_secs(1)));
        ch.clear_typing("kate");
        assert!(ch.expire_typing(now + TYPING_TTL + Duration::from_millis(1)));
        assert!(ch.typing.is_empty());
    }

    #[test]
    fn russian_plurals_and_clock() {
        let p = |n| plural(n, "message", "messages");
        assert_eq!(p(1), "1 message");
        assert_eq!(p(0), "0 messages");
        assert_eq!(p(21), "21 messages");
        assert_eq!(clock(Duration::from_secs(187)), "3:07");
        assert_eq!(clock(Duration::from_secs(3765)), "1:02:45");
    }

    #[test]
    fn day_labels() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 10, 15, 0, 0).unwrap();
        let at = |y, m, d| chrono::Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap().timestamp_millis() as u64;
        assert_eq!(day_label(at(2026, 11, 10), now), "today");
        assert_eq!(day_label(at(2026, 11, 9), now), "yesterday");
        assert_eq!(day_label(at(2026, 3, 3), now), "March 3");
        assert_eq!(day_label(at(2025, 12, 31), now), "December 31, 2025");
        assert!(same_day(at(2026, 1, 1), at(2026, 1, 1) + 1000));
        assert!(!same_day(at(2026, 1, 1), at(2026, 1, 2)));
    }
}
