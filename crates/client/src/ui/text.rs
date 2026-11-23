//! Message text: light markup (`**bold**`, `*italic*`, `` `code` ``,
//! `~~strike~~`, links, @mentions, quotes, code blocks) and wrapping by the
//! real width of characters (emoji, CJK).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

/// Width of a string in terminal cells.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Cut to `max` cells, ending with "…".
pub fn clip(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = width(g);
        if used + w > max - 1 {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// Cut from the left: "…tail/of/a/path" — for long paths.
pub fn clip_left(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail: Vec<&str> = Vec::new();
    let mut used = 0;
    for g in s.graphemes(true).rev() {
        let w = width(g);
        if used + w > max - 1 {
            break;
        }
        tail.push(g);
        used += w;
    }
    tail.reverse();
    format!("…{}", tail.concat())
}

/// What markup needs to know about its surroundings.
pub struct Markup<'a> {
    pub theme: &'a Theme,
    /// Our own nick — to highlight mentions of us.
    pub me: &'a str,
    pub base: Style,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Parse inline markup into styled pieces.
pub fn inline(text: &str, m: &Markup) -> Vec<Span<'static>> {
    let t = m.theme;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic, mut strike) = (false, false, false);

    let style_of = |bold: bool, italic: bool, strike: bool| {
        let mut s = m.base;
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if strike {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        s
    };
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), style_of(bold, italic, strike)));
            }
        };
    }

    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let at_word_start = !prev.is_some_and(is_word);

        // `code`: nothing inside is parsed
        if c == '`'
            && let Some(end) = rest[1..].find('`')
            && end > 0
        {
            flush!();
            let code = &rest[1..1 + end];
            spans.push(Span::styled(format!(" {code} "), Style::new().fg(t.warn).bg(t.raised)));
            i += end + 2;
            prev = Some('`');
            continue;
        }
        // links
        if at_word_start && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let mut end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            while end > 8 && rest[..end].ends_with(['.', ',', ';', ':', '!', '?', ')', '"', '»']) {
                end -= rest[..end].chars().next_back().map_or(1, char::len_utf8);
            }
            flush!();
            spans.push(Span::styled(
                rest[..end].to_string(),
                m.base.fg(t.info).add_modifier(Modifier::UNDERLINED),
            ));
            i += end;
            prev = Some('/');
            continue;
        }
        // @mentions
        if c == '@' && at_word_start {
            let after = &rest[1..];
            let me = m.me.to_lowercase();
            let lower = after.to_lowercase();
            let mine = !me.is_empty() && lower.starts_with(&me) && !lower[me.len()..].chars().next().is_some_and(is_word);
            let word_end = after.find(|c: char| !is_word(c)).unwrap_or(after.len());
            let everyone = matches!(lower.get(..word_end).unwrap_or(""), "all" | "everyone" | "here");
            let len = if mine {
                // the nick's length in the original text: case may differ, so count characters
                after.chars().take(m.me.chars().count()).map(char::len_utf8).sum()
            } else {
                word_end
            };
            if len > 0 {
                flush!();
                let style = if mine || everyone {
                    Style::new().fg(t.on_accent).bg(t.accent).add_modifier(Modifier::BOLD)
                } else {
                    m.base.fg(t.accent).add_modifier(Modifier::BOLD)
                };
                spans.push(Span::styled(format!("@{}", &after[..len]), style));
                i += 1 + len;
                prev = Some('x');
                continue;
            }
        }
        // paired markers apply only if a closing one follows
        let toggle = |marker: &str, on: bool| -> bool {
            if !rest.starts_with(marker) {
                return false;
            }
            let after = &rest[marker.len()..];
            if on {
                true
            } else {
                after.contains(marker) && !after.starts_with(char::is_whitespace) && !after.is_empty()
            }
        };
        if toggle("**", bold) {
            flush!();
            bold = !bold;
            i += 2;
            prev = Some('*');
            continue;
        }
        if toggle("~~", strike) {
            flush!();
            strike = !strike;
            i += 2;
            prev = Some('~');
            continue;
        }
        // a double marker without a pair ("**unclosed") is plain text, not two italics
        if (c == '*' || c == '_') && rest[1..].starts_with(c) {
            buf.push(c);
            buf.push(c);
            prev = Some(c);
            i += 2;
            continue;
        }
        let next_is_word = rest[c.len_utf8()..].chars().next().is_some_and(is_word);
        // "_" inside a word (snake_case) is a plain character
        let underscore_ok = c != '_' || if italic { !next_is_word } else { at_word_start };
        if (c == '*' || c == '_') && underscore_ok && toggle(&c.to_string(), italic) {
            flush!();
            italic = !italic;
            i += 1;
            prev = Some(c);
            continue;
        }

        buf.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    flush!();
    spans
}

struct Cell {
    text: String,
    style: Style,
    w: usize,
    space: bool,
}

fn cells_of(spans: &[Span<'static>]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for span in spans {
        for g in span.content.graphemes(true) {
            cells.push(Cell { text: g.to_string(), style: span.style, w: width(g), space: g == " " });
        }
    }
    cells
}

fn line_of(cells: Vec<Cell>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for cell in cells {
        match spans.last_mut() {
            Some(last) if last.style == cell.style => last.content.to_mut().push_str(&cell.text),
            _ => spans.push(Span::styled(cell.text, cell.style)),
        }
    }
    Line::from(spans)
}

/// Wrap by words to width `max`; words that are too long are cut by character.
pub fn wrap(spans: &[Span<'static>], max: usize) -> Vec<Line<'static>> {
    let max = max.max(1);
    let mut lines: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut used = 0usize;
    let mut last_space: Option<usize> = None;
    for cell in cells_of(spans) {
        if used + cell.w > max && cell.w > 0 {
            let cur = lines.last_mut().expect("there is always a current line");
            if cell.space {
                // a space at a line break just disappears
                lines.push(Vec::new());
                used = 0;
                last_space = None;
                continue;
            }
            match last_space {
                Some(at) => {
                    let tail = cur.split_off(at + 1);
                    cur.pop();
                    used = tail.iter().map(|c| c.w).sum();
                    lines.push(tail);
                }
                None => {
                    lines.push(Vec::new());
                    used = 0;
                }
            }
            last_space = None;
            if used + cell.w > max {
                lines.push(Vec::new());
                used = 0;
            }
        }
        let cur = lines.last_mut().expect("there is always a current line");
        if cell.space {
            last_space = Some(cur.len());
        }
        used += cell.w;
        cur.push(cell);
    }
    lines.into_iter().map(line_of).collect()
}

/// A whole message text → lines no wider than `max`.
pub fn message_lines(text: &str, m: &Markup, max: usize) -> Vec<Line<'static>> {
    let t = m.theme;
    let mut out = Vec::new();
    let mut in_code = false;
    for raw in text.split('\n') {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            // text on the same line after the opening ``` is already code
            let rest = raw.trim_start().trim_start_matches('`').trim();
            if rest.is_empty() || !in_code {
                continue;
            }
            out.extend(code_lines(rest, t, max));
            continue;
        }
        if in_code {
            out.extend(code_lines(raw, t, max));
        } else if let Some(quote) = raw.strip_prefix("> ").or_else(|| raw.strip_prefix(">")) {
            let quoted = Markup { theme: t, me: m.me, base: m.base.fg(t.dim) };
            for line in wrap(&inline(quote, &quoted), max.saturating_sub(2).max(1)) {
                let mut spans = vec![Span::styled("▎ ", Style::new().fg(t.line))];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        } else {
            out.extend(wrap(&inline(raw, m), max));
        }
    }
    if out.is_empty() {
        out.push(Line::default());
    }
    out
}

/// A code line: no markup, on the raised background, padded to the full width.
fn code_lines(raw: &str, t: &Theme, max: usize) -> Vec<Line<'static>> {
    let style = Style::new().fg(t.text).bg(t.raised);
    let inner = max.saturating_sub(2).max(1);
    let text = raw.replace('\t', "    ");
    wrap(&[Span::styled(text, style)], inner)
        .into_iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| width(&s.content)).sum();
            let mut spans = vec![Span::styled(" ", style)];
            spans.extend(line.spans);
            spans.push(Span::styled(" ".repeat(inner.saturating_sub(w) + 1), style));
            Line::from(spans)
        })
        .collect()
}

/// The plain text of a line (for tests and copying).
pub fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::by_name("tovuer")
    }

    fn parse(text: &str) -> Vec<Span<'static>> {
        let t = theme();
        inline(text, &Markup { theme: &t, me: "dana", base: Style::new() })
    }

    fn texts(spans: &[Span<'static>]) -> Vec<String> {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn clipping_respects_wide_characters() {
        assert_eq!(clip("hello", 10), "hello");
        assert_eq!(clip("hello world", 6), "hello…");
        assert_eq!(clip("🔥🔥🔥🔥", 5), "🔥🔥…");
        assert_eq!(clip("abc", 0), "");
        assert_eq!(clip_left("/home/dana/Pictures", 9), "…Pictures");
        assert_eq!(clip_left("short", 9), "short");
        assert!(width(&clip("日本語のテキスト", 7)) <= 7);
    }

    #[test]
    fn bold_italic_strike_code() {
        let s = parse("this is **important**, *very* and ~~not~~ `let x = 1;`");
        assert_eq!(texts(&s), vec!["this is ", "important", ", ", "very", " and ", "not", " ", " let x = 1; "]);
        assert!(s[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(s[3].style.add_modifier.contains(Modifier::ITALIC));
        assert!(s[5].style.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(s[7].style.bg, Some(theme().raised));
    }

    #[test]
    fn unpaired_markers_stay_literal() {
        assert_eq!(texts(&parse("2 * 3 = 6")), vec!["2 * 3 = 6"]);
        assert_eq!(texts(&parse("a star* alone")), vec!["a star* alone"]);
        assert_eq!(texts(&parse("snake_case_name and my_var")), vec!["snake_case_name and my_var"]);
        assert_eq!(texts(&parse("one ` backtick")), vec!["one ` backtick"]);
        assert_eq!(texts(&parse("**unclosed")), vec!["**unclosed"]);
        // no markup inside code
        assert_eq!(texts(&parse("`**not bold**`")), vec![" **not bold** "]);
    }

    #[test]
    fn links_and_mentions() {
        let s = parse("look https://example.com/a?b=1, @dana and @bob!");
        assert_eq!(texts(&s), vec!["look ", "https://example.com/a?b=1", ", ", "@dana", " and ", "@bob", "!"]);
        assert!(s[1].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(s[3].style.bg, Some(theme().accent), "a mention of us is filled");
        assert_eq!(s[5].style.bg, None, "someone else's is just colored");
        // an email is not a mention; @all mentions everyone
        assert_eq!(texts(&parse("mail a@dana.kz")), vec!["mail a@dana.kz"]);
        assert_eq!(parse("@all over here")[0].style.bg, Some(theme().accent));
        assert_eq!(texts(&parse("@DANA")), vec!["@DANA"]);
        assert_eq!(parse("@DANA")[0].style.bg, Some(theme().accent));
        assert_eq!(texts(&parse("a lonely @ sign")), vec!["a lonely @ sign"]);
    }

    fn wrapped(text: &str, max: usize) -> Vec<String> {
        wrap(&[Span::raw(text.to_string())], max).iter().map(plain).collect()
    }

    #[test]
    fn wraps_by_words() {
        assert_eq!(wrapped("one two six seven", 8), vec!["one two", "six", "seven"]);
        assert_eq!(wrapped("exactly10c", 10), vec!["exactly10c"]);
        assert_eq!(wrapped("", 10), vec![""]);
    }

    #[test]
    fn long_words_are_cut_and_width_is_never_exceeded() {
        assert_eq!(wrapped("aaaaaaaaaaaa", 5), vec!["aaaaa", "aaaaa", "aa"]);
        assert_eq!(wrapped("I aaaaaaaaa", 5), vec!["I", "aaaaa", "aaaa"]);
        for max in 1..30 {
            for line in wrapped("short and aaaaaaaveryverylongword with 🔥🔥🔥 emoji and 日本語 characters", max) {
                assert!(width(&line) <= max.max(2), "“{line}” is wider than {max}");
            }
        }
    }

    #[test]
    fn wrapping_keeps_styles() {
        let t = theme();
        let spans = inline("regular **strong words** tail", &Markup { theme: &t, me: "", base: Style::new() });
        let lines = wrap(&spans, 14);
        assert_eq!(lines.iter().map(plain).collect::<Vec<_>>(), vec!["regular strong", "words tail"]);
        assert!(lines[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(lines[1].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(!lines[1].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn blocks_quotes_and_code_fences() {
        let t = theme();
        let m = Markup { theme: &t, me: "", base: Style::new() };
        let lines = message_lines("hello\n> quote **here**\n```\nlet a = *b*;\n```\nend", &m, 20);
        let text: Vec<String> = lines.iter().map(plain).collect();
        assert_eq!(text[0], "hello");
        assert_eq!(text[1], "▎ quote here");
        assert_eq!(text[2].trim_end(), " let a = *b*;", "stars stay as they are in a code block");
        assert_eq!(width(&text[2]), 20, "the code background reaches the edge");
        assert_eq!(text[3], "end");
        assert_eq!(message_lines("", &m, 10).len(), 1);
        // code on the same line as the fence
        assert_eq!(plain(&message_lines("```fn main() {}", &m, 30)[0]).trim(), "fn main() {}");
    }
}
