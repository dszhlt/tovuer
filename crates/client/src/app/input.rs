//! The input: a cursor over characters (not bytes), word movement, history
//! of sent lines, pasting, layout into lines.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, Default)]
pub struct Input {
    text: String,
    /// Cursor position in bytes, always on a character boundary.
    cursor: usize,
    /// Length limit in characters (0 — no limit).
    limit: usize,
    history: Vec<String>,
    /// Which history entry is being shown.
    recall: Option<usize>,
    /// Unfinished text, put aside while flipping through history.
    stash: String,
}

/// Where the cursor ended up after layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub lines: Vec<String>,
    pub cursor: (u16, u16),
}

impl Input {
    pub fn new(limit: usize) -> Self {
        Self { limit, ..Self::default() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Text left of the cursor — completion works on it.
    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.recall = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.recall = None;
    }

    /// Take the text to send: the input clears and the text goes into history.
    pub fn submit(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.recall = None;
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
            if self.history.len() > HISTORY_LIMIT {
                self.history.remove(0);
            }
        }
        text
    }

    pub fn insert(&mut self, s: &str) {
        // pasted text: no control characters, newlines made uniform
        let clean: String = s
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        let room = match self.limit {
            0 => usize::MAX,
            limit => limit.saturating_sub(self.text.chars().count()),
        };
        let clean: String = clean.chars().take(room).collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        self.recall = None;
    }

    /// Replace `[from, cursor)` with `with` — this is how a completion goes in.
    pub fn replace_before_cursor(&mut self, from: usize, with: &str) {
        let from = from.min(self.cursor);
        self.text.replace_range(from..self.cursor, with);
        self.cursor = from + with.len();
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..].graphemes(true).next().map_or(self.text.len(), |g| self.cursor + g.len())
    }

    fn word_left(&self) -> usize {
        let left = &self.text[..self.cursor];
        let trimmed = left.trim_end_matches(|c: char| !c.is_alphanumeric());
        trimmed.rfind(|c: char| !c.is_alphanumeric()).map_or(0, |i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8))
    }

    fn word_right(&self) -> usize {
        let right = &self.text[self.cursor..];
        let skipped = right.len() - right.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let word = &right[skipped..];
        self.cursor + skipped + word.find(|c: char| !c.is_alphanumeric()).unwrap_or(word.len())
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i)
    }

    /// The previous history entry. `false` — nothing further back.
    pub fn recall_prev(&mut self) -> bool {
        let next = match self.recall {
            None if self.history.is_empty() => return false,
            None => self.history.len() - 1,
            Some(0) => return false,
            Some(i) => i - 1,
        };
        if self.recall.is_none() {
            self.stash = self.text.clone();
        }
        self.recall = Some(next);
        self.text = self.history[next].clone();
        self.cursor = self.text.len();
        true
    }

    pub fn recall_next(&mut self) -> bool {
        let Some(i) = self.recall else { return false };
        if i + 1 < self.history.len() {
            self.recall = Some(i + 1);
            self.text = self.history[i + 1].clone();
        } else {
            self.recall = None;
            self.text = std::mem::take(&mut self.stash);
        }
        self.cursor = self.text.len();
        true
    }

    /// Handle an editing key. `false` — the key is not for the input.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => {
                let mut buf = [0u8; 4];
                self.insert(c.encode_utf8(&mut buf));
            }
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Char('w') if ctrl => {
                let from = self.word_left();
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            KeyCode::Char('u') if ctrl => {
                let from = self.line_start();
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            KeyCode::Backspace if ctrl || alt => {
                let from = self.word_left();
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            KeyCode::Backspace => {
                let from = self.prev_boundary();
                self.text.replace_range(from..self.cursor, "");
                self.cursor = from;
            }
            KeyCode::Delete => {
                let to = if ctrl { self.word_right() } else { self.next_boundary() };
                self.text.replace_range(self.cursor..to, "");
            }
            KeyCode::Left if ctrl || alt => self.cursor = self.word_left(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.prev_boundary(),
            KeyCode::Right => self.cursor = self.next_boundary(),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            _ => return false,
        }
        if !matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End) {
            self.recall = None;
        }
        true
    }

    /// Lay the text out in lines `width` wide and find the cursor.
    /// `mask` — show dots instead of characters (a password).
    pub fn layout(&self, width: usize, mask: bool) -> Layout {
        let width = width.max(1);
        let mut lines = vec![String::new()];
        let mut used = 0usize;
        let mut cursor = (0u16, 0u16);
        let mut seen_cursor = false;
        for (i, g) in self.text.grapheme_indices(true) {
            if i >= self.cursor && !seen_cursor {
                seen_cursor = true;
                cursor = (used as u16, (lines.len() - 1) as u16);
            }
            if g == "\n" {
                lines.push(String::new());
                used = 0;
                continue;
            }
            let shown = if mask { "•" } else { g };
            let w = UnicodeWidthStr::width(shown);
            if used + w > width {
                lines.push(String::new());
                used = 0;
                // the cursor was before this character — it moves along with it
                if i == self.cursor {
                    cursor = (0, (lines.len() - 1) as u16);
                }
            }
            lines.last_mut().expect("there is a line").push_str(shown);
            used += w;
        }
        if !seen_cursor {
            // the cursor is at the very end; if the line is full, on a new line
            if used >= width {
                lines.push(String::new());
                used = 0;
            }
            cursor = (used as u16, (lines.len() - 1) as u16);
        }
        Layout { lines, cursor }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::new(0);
        for c in text.chars() {
            input.key(key(KeyCode::Char(c)));
        }
        input
    }

    #[test]
    fn typing_and_deleting_by_grapheme() {
        let mut i = typed("héllo👍🏽");
        assert_eq!(i.text(), "héllo👍🏽");
        i.key(key(KeyCode::Backspace));
        assert_eq!(i.text(), "héllo", "an emoji with a modifier is deleted whole");
        i.key(key(KeyCode::Left));
        i.key(key(KeyCode::Left));
        i.key(key(KeyCode::Char('!')));
        assert_eq!(i.text(), "hél!lo");
        i.key(key(KeyCode::Delete));
        assert_eq!(i.text(), "hél!o");
        i.key(key(KeyCode::Home));
        i.key(key(KeyCode::Backspace));
        assert_eq!(i.text(), "hél!o", "nothing to delete at the start");
        i.key(key(KeyCode::End));
        i.key(key(KeyCode::Delete));
        assert_eq!(i.text(), "hél!o");
    }

    #[test]
    fn word_navigation_and_kill() {
        let mut i = typed("one twö, six");
        i.key(ctrl(KeyCode::Left));
        assert_eq!(i.before_cursor(), "one twö, ");
        i.key(ctrl(KeyCode::Left));
        assert_eq!(i.before_cursor(), "one ");
        i.key(ctrl(KeyCode::Right));
        assert_eq!(i.before_cursor(), "one twö");
        i.key(ctrl(KeyCode::Char('w')));
        assert_eq!(i.text(), "one , six");
        i.key(key(KeyCode::End));
        i.key(ctrl(KeyCode::Char('u')));
        assert_eq!(i.text(), "");
        assert!(!i.key(key(KeyCode::Enter)), "Enter is not the editor's business");
        assert!(!i.key(key(KeyCode::Up)));
    }

    #[test]
    fn paste_is_sanitised_and_limited() {
        let mut i = Input::new(10);
        i.insert("a\tb\r\nc\u{7}\u{1b}[31m");
        assert_eq!(i.text(), "a b\nc[31m");
        i.insert("0123456789");
        assert_eq!(i.text().chars().count(), 10, "the length limit holds");
    }

    #[test]
    fn history_recall_keeps_draft() {
        let mut i = Input::new(0);
        assert!(!i.recall_prev());
        for text in ["first", "second", "second", "  "] {
            i.set(text);
            i.submit();
        }
        i.set("draft");
        assert!(i.recall_prev());
        assert_eq!(i.text(), "second");
        assert!(i.recall_prev());
        assert_eq!(i.text(), "first", "repeats and blanks stay out of history");
        assert!(!i.recall_prev());
        assert!(i.recall_next());
        assert!(i.recall_next());
        assert_eq!(i.text(), "draft", "the unfinished text comes back");
        assert!(!i.recall_next());
    }

    #[test]
    fn layout_wraps_and_places_cursor() {
        let i = typed("àbcdéfgh");
        let l = i.layout(5, false);
        assert_eq!(l.lines, vec!["àbcdé", "fgh"]);
        assert_eq!(l.cursor, (3, 1));

        let mut i = typed("àbcdé");
        assert_eq!(i.layout(5, false), Layout { lines: vec!["àbcdé".into(), "".into()], cursor: (0, 1) });
        i.key(key(KeyCode::Left));
        assert_eq!(i.layout(5, false).cursor, (4, 0));
        i.key(key(KeyCode::Home));
        assert_eq!(i.layout(5, false).cursor, (0, 0));

        let mut i = Input::new(0);
        i.insert("one\ntwo");
        assert_eq!(i.layout(10, false), Layout { lines: vec!["one".into(), "two".into()], cursor: (3, 1) });
        assert_eq!(typed("🔥🔥🔥").layout(5, false).lines, vec!["🔥🔥", "🔥"]);
        assert_eq!(Input::new(0).layout(5, false), Layout { lines: vec!["".into()], cursor: (0, 0) });
    }

    #[test]
    fn password_is_masked() {
        let l = typed("secret").layout(20, true);
        assert_eq!(l.lines, vec!["••••••"]);
        assert_eq!(l.cursor, (6, 0));
    }

    #[test]
    fn completion_replaces_the_typed_prefix() {
        let mut i = typed("héllo @di");
        let from = i.text().rfind('@').unwrap();
        i.replace_before_cursor(from, "@dana ");
        assert_eq!(i.text(), "héllo @dana ");
        assert_eq!(i.cursor(), i.text().len());
    }
}
