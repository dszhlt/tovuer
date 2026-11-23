//! The middle column: the channel header, the message feed, the "typing…"
//! line, the input and completion suggestions.

use std::hash::{Hash, Hasher};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tovuer_proto::Message;

use super::icons::{Icon, Icons};
use super::look::{Paint, look, paint_row};
use super::text::{Markup, clip, message_lines, width};
use super::theme::Theme;
use super::{Hit, fill, frame, hrule, put, put_center, put_line, put_right, spinner};
use crate::app::model::{ChannelState, day_label, local_time, plural, same_day};
use crate::app::{App, Compose, Focus, ThumbState};
use crate::settings::human_size;
use crate::transfer::{AUTO_PREVIEW_LIMIT, Thumb};

/// The laid-out lines of one message and a fingerprint of what they came from.
pub struct Laid {
    sig: u64,
    lines: Vec<Line<'static>>,
    /// The author's gradient over the text lines (`body`), applied when drawing.
    paint: Option<Paint>,
    body: std::ops::Range<usize>,
    /// The widest text line — a still gradient stretches over it.
    widest: usize,
}

/// The input grows no taller than this — beyond it the text scrolls inside.
const INPUT_MAX_LINES: usize = 6;

/// A line in front of a message that is not part of it.
enum Pre {
    Blank,
    Day(String),
    New,
    /// The note above the oldest loaded message.
    Top(&'static str),
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App) -> Option<(u16, u16)> {
    let t = app.theme.clone();
    if area.width < 12 || area.height < 8 {
        return None;
    }
    // header
    let (name, topic) = app.channel().map(|c| (c.name.clone(), c.topic.clone())).unwrap_or_default();
    let title = Style::new().fg(t.text).bg(t.bg).add_modifier(Modifier::BOLD);
    let mut x = put(buf, area.x + 2, area.y, "# ", Style::new().fg(t.faint).bg(t.bg), 2);
    x = put(buf, x, area.y, &clip(&name, area.width as usize - 6), title, area.width - 4);
    if !topic.is_empty() && x + 6 < area.right() {
        x = put(buf, x + 2, area.y, "│ ", Style::new().fg(t.line).bg(t.bg), 2);
        let room = area.right().saturating_sub(x + 2) as usize;
        put(buf, x, area.y, &clip(&topic, room), Style::new().fg(t.dim).bg(t.bg), room as u16);
    }
    hrule(buf, area.x, area.y + 1, area.width, Style::new().fg(t.line).bg(t.bg));

    // bottom: the input (grows with the text), above it the reply/edit line and "typing…"
    let input_w = area.width.saturating_sub(6) as usize;
    let layout = app.input.layout(input_w, false);
    let input_lines = layout.lines.len().clamp(1, INPUT_MAX_LINES);
    let input_h = input_lines as u16 + 2;
    let compose_h = u16::from(app.compose.is_some());
    let input_area = Rect::new(area.x + 1, area.bottom() - input_h, area.width - 2, input_h);
    let compose_y = input_area.y - compose_h;
    let typing_y = compose_y - 1;
    let msgs = Rect::new(area.x, area.y + 2, area.width, typing_y.saturating_sub(area.y + 2));

    messages(buf, msgs, app);
    typing_line(buf, Rect::new(area.x, typing_y, area.width, 1), app);
    if let Some(compose) = &app.compose {
        compose_bar(buf, Rect::new(area.x + 1, compose_y, area.width - 2, 1), compose, &t, app.icons);
    }

    // the input
    let focused = app.focus == Focus::Input;
    let border = Style::new().fg(if focused { t.accent } else { t.line }).bg(t.bg);
    frame(buf, input_area, border, None);
    app.ui.hits.push((input_area, Hit::Input));
    let text_x = input_area.x + 2;
    // long text: show a window of lines around the cursor
    let first = (layout.cursor.1 as usize + 1).saturating_sub(input_lines);
    if app.input.is_empty() {
        let hint = if app.online() { format!("Message #{name}…") } else { "Not in the room yet…".to_string() };
        put(buf, text_x, input_area.y + 1, &clip(&hint, input_w), Style::new().fg(t.faint).bg(t.bg), input_w as u16);
    } else {
        let is_command = app.input.text().starts_with('/') && !app.input.text().starts_with("//");
        // what you type already looks the way it will be sent
        let mine = if is_command { None } else { app.settings.style.clean() };
        let shown = look(mine.as_ref(), &t, true);
        let style = if is_command { Style::new().fg(t.info).bg(t.bg) } else { Style::new().bg(t.bg).patch(shown.base) };
        let widest = layout.lines.iter().map(|l| width(l)).max().unwrap_or(0);
        for (row, line) in layout.lines.iter().skip(first).take(input_lines).enumerate() {
            let y = input_area.y + 1 + row as u16;
            put(buf, text_x, y, line, style, input_w as u16);
            if let Some(paint) = &shown.paint {
                paint_row(buf, Rect::new(text_x, y, width(line).min(input_w) as u16, 1), paint, first + row, widest, app.frame);
                app.ui.animating |= paint.flow;
            }
        }
    }
    let used = app.input.text().chars().count();
    if used * 10 >= tovuer_proto::MAX_TEXT_CHARS * 9 {
        let counter = format!(" {used}/{} ", tovuer_proto::MAX_TEXT_CHARS);
        put_right(buf, input_area.right() - 2, input_area.bottom() - 1, &counter, Style::new().fg(t.warn).bg(t.bg), input_area.x + 2);
    }
    if app.completion.is_some() {
        completion_popup(buf, area, input_area.y - compose_h, app);
    }

    focused.then(|| (text_x + layout.cursor.0, input_area.y + 1 + (layout.cursor.1 as usize - first) as u16))
}

fn typing_line(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    let Some(ch) = app.channel() else { return };
    let mut right = area.right();
    if ch.scroll > 0 {
        let label = if ch.unread > 0 {
            format!(" ↓ {} · Ctrl+End ", plural(ch.unread as u64, "new", "new"))
        } else {
            " ↓ to the latest · Ctrl+End ".to_string()
        };
        let style = if ch.unread > 0 {
            Style::new().fg(t.on_accent).bg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.dim).bg(t.raised)
        };
        right = put_right(buf, area.right() - 2, area.y, &label, style, area.x + 2);
    }
    let names: Vec<&str> = ch.typing.iter().map(|(n, _)| n.as_str()).collect();
    let who = match names.as_slice() {
        [] => return,
        [one] => format!("{one} is typing"),
        [a, b] => format!("{a} and {b} are typing"),
        _ => "several people are typing".to_string(),
    };
    let dots = ".".repeat(1 + (app.frame / 4 % 3) as usize);
    let room = right.saturating_sub(area.x + 3) as usize;
    put(buf, area.x + 2, area.y, &clip(&format!("{who}{dots}"), room), Style::new().fg(t.dim).bg(t.bg).add_modifier(Modifier::ITALIC), room as u16);
}

fn compose_bar(buf: &mut Buffer, area: Rect, compose: &Compose, t: &Theme, icons: Icons) {
    fill(buf, area, Style::new().fg(t.dim).bg(t.raised));
    let right = put_right(buf, area.right() - 1, area.y, "Esc — cancel", Style::new().fg(t.faint).bg(t.raised), area.x + 4);
    let room = right.saturating_sub(area.x + 2) as usize;
    match compose {
        Compose::Reply { from, preview, .. } => {
            let x = put(buf, area.x + 1, area.y, "↪ ", Style::new().fg(t.accent).bg(t.raised), 2);
            let x = put(buf, x, area.y, &clip(from, room / 2), Style::new().fg(t.nick(from)).bg(t.raised).add_modifier(Modifier::BOLD), room as u16);
            let left = right.saturating_sub(x + 3) as usize;
            put(buf, x, area.y, &clip(&format!(": {preview}"), left), Style::new().fg(t.dim).bg(t.raised), left as u16);
        }
        Compose::Edit { .. } => {
            let x = put(buf, area.x + 1, area.y, &icons.spaced(Icon::Edit), Style::new().fg(t.warn).bg(t.raised), 3);
            put(buf, x, area.y, &clip("editing a message — Enter saves", room), Style::new().fg(t.dim).bg(t.raised), room as u16);
        }
    }
}

fn completion_popup(buf: &mut Buffer, area: Rect, bottom: u16, app: &App) {
    let t = &app.theme;
    let Some(comp) = &app.completion else { return };
    let label_w = comp.items.iter().map(|i| width(&i.label)).max().unwrap_or(0);
    let hint_w = comp.items.iter().map(|i| width(&i.hint)).max().unwrap_or(0);
    let w = ((label_w + if hint_w > 0 { hint_w + 3 } else { 0 } + 4) as u16).clamp(16, area.width.saturating_sub(4));
    let h = comp.items.len() as u16 + 2;
    if bottom < area.y + 2 + h {
        return;
    }
    let boxed = Rect::new(area.x + 1, bottom - h, w, h);
    fill(buf, boxed, Style::new().fg(t.text).bg(t.raised));
    frame(buf, boxed, Style::new().fg(t.line).bg(t.raised), None);
    for (i, item) in comp.items.iter().enumerate() {
        let y = boxed.y + 1 + i as u16;
        let selected = i == comp.sel;
        let bg = if selected { t.sel } else { t.raised };
        fill(buf, Rect::new(boxed.x + 1, y, w - 2, 1), Style::new().bg(bg));
        let mut style = Style::new().fg(if selected { t.text } else { t.dim }).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        let inner = w as usize - 4;
        let x = put(buf, boxed.x + 2, y, &clip(&item.label, inner), style, inner as u16);
        let room = (boxed.right() - 2).saturating_sub(x + 2) as usize;
        if !item.hint.is_empty() && room > 4 {
            put_right(buf, boxed.right() - 2, y, &clip(&item.hint, room), Style::new().fg(t.faint).bg(bg), x + 2);
        }
    }
}

// ------------------------------------------------------------------------ feed

/// Thumbnail lines: each cell is two pixels ("▀": the top is the glyph color, the bottom the background).
fn thumb_lines(thumb: &Thumb) -> Vec<Line<'static>> {
    (0..thumb.rows())
        .map(|row| {
            let spans = (0..thumb.w)
                .map(|x| {
                    let top = thumb.at(x, row * 2);
                    let bottom = thumb.at(x, row * 2 + 1);
                    Span::styled(
                        "▀",
                        Style::new().fg(Color::Rgb(top[0], top[1], top[2])).bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
                    )
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

/// Whether this message shows a thumbnail.
fn wants_preview(app: &App, msg: &Message) -> bool {
    app.settings.previews && msg.file.as_ref().is_some_and(|f| f.image && f.size <= AUTO_PREVIEW_LIMIT)
}

fn signature(app: &App, msg: &Message, header: bool, w: usize) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (app.ui.generation, w, header, msg.edited, msg.text.len(), &msg.style, app.settings.styles).hash(&mut h);
    for r in &msg.reactions {
        (&r.emoji, r.users.len(), r.users.iter().any(|u| u == app.my_name())).hash(&mut h);
    }
    if let Some(file) = &msg.file {
        let state = match app.thumbs.get(&file.id) {
            _ if !wants_preview(app, msg) => 0u8,
            None => 1,
            Some(ThumbState::Loading) => 2,
            Some(ThumbState::Ready(_)) => 3,
            Some(ThumbState::Failed) => 4,
        };
        state.hash(&mut h);
    }
    h.finish()
}

/// Lay one message out in lines `w` wide.
fn lay(app: &App, msg: &Message, header: bool, w: usize) -> Laid {
    let t = &app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (mut paint, mut body, mut widest) = (None, 0..0, 0);
    if msg.system {
        let markup = Markup { theme: t, me: "", base: Style::new().fg(t.faint).add_modifier(Modifier::ITALIC) };
        for (i, line) in message_lines(&msg.text, &markup, w.saturating_sub(2).max(1)).into_iter().enumerate() {
            let mut spans = vec![Span::styled(if i == 0 { "◆ " } else { "  " }, Style::new().fg(t.line))];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
        return Laid { sig: 0, lines, paint, body, widest };
    }
    if header {
        let time = local_time(msg.ts).format("%H:%M").to_string();
        lines.push(Line::from(vec![
            Span::styled(clip(&msg.from, w.saturating_sub(7).max(1)), Style::new().fg(t.nick(&msg.from)).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {time}"), Style::new().fg(t.faint)),
        ]));
    }
    if let Some(reply) = &msg.reply {
        let quote = clip(&reply.text, w.saturating_sub(5 + width(&reply.from)).max(4));
        lines.push(Line::from(vec![
            Span::styled("╭─ ", Style::new().fg(t.line)),
            Span::styled(clip(&reply.from, w / 3), Style::new().fg(t.nick(&reply.from))),
            Span::styled(format!(": {quote}"), Style::new().fg(t.faint)),
        ]));
    }
    if !msg.text.is_empty() {
        // the author's style, unless this reader turned styles off
        let style = msg.style.as_ref().filter(|_| app.settings.styles > 0);
        let shown = look(style, t, app.settings.styles == 2);
        let markup = Markup { theme: t, me: app.my_name(), base: shown.base };
        let mut text = message_lines(&msg.text, &markup, w);
        widest = text.iter().map(|l| l.width()).max().unwrap_or(0);
        if msg.edited.is_some() {
            const MARK: &str = " (edited)";
            match text.last_mut() {
                Some(last) if last.width() + width(MARK) <= w => last.spans.push(Span::styled(MARK, Style::new().fg(t.faint))),
                _ => text.push(Line::from(Span::styled(MARK.trim_start(), Style::new().fg(t.faint)))),
            }
        }
        body = lines.len()..lines.len() + text.len();
        paint = shown.paint;
        lines.extend(text);
    }
    if let Some(file) = &msg.file {
        let size = human_size(file.size);
        let room = w.saturating_sub(width(&size) + 6).max(4);
        lines.push(Line::from(vec![
            Span::styled(app.icons.spaced(if file.image { Icon::Picture } else { Icon::Attachment }), Style::new().fg(t.dim)),
            Span::styled(clip(&file.name, room), Style::new().fg(t.info).add_modifier(Modifier::UNDERLINED)),
            Span::styled(format!("  {size}"), Style::new().fg(t.faint)),
        ]));
        if wants_preview(app, msg) {
            match app.thumbs.get(&file.id) {
                Some(ThumbState::Ready(thumb)) if thumb.w as usize <= w => lines.extend(thumb_lines(thumb)),
                Some(ThumbState::Ready(_)) | Some(ThumbState::Failed) => {}
                Some(ThumbState::Loading) | None => {
                    lines.push(Line::from(Span::styled(format!("{}loading the preview…", app.icons.spaced(Icon::Loading)), Style::new().fg(t.faint))));
                }
            }
        }
    }
    if !msg.reactions.is_empty() {
        let mut spans = Vec::new();
        let mut used = 0;
        for r in &msg.reactions {
            let pill = format!(" {} {} ", r.emoji, r.users.len());
            if used + width(&pill) + 1 > w {
                spans.push(Span::styled("…", Style::new().fg(t.faint)));
                break;
            }
            used += width(&pill) + 1;
            let mine = r.users.iter().any(|u| u == app.my_name());
            let style = if mine {
                Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(t.dim).bg(t.raised)
            };
            spans.push(Span::styled(pill, style));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    Laid { sig: 0, lines, paint, body, widest }
}

fn pre_rows(ch: &ChannelState, i: usize, now: chrono::DateTime<chrono::Local>) -> Vec<Pre> {
    let msg = &ch.messages[i];
    let mut pre = Vec::new();
    if i == 0 {
        pre.push(Pre::Top(if ch.loading {
            "loading history…"
        } else if ch.more {
            "there is more above — PgUp"
        } else {
            "the channel starts here"
        }));
    }
    let new_day = i == 0 || !same_day(ch.messages[i - 1].ts, msg.ts);
    if new_day {
        pre.push(Pre::Blank);
        pre.push(Pre::Day(day_label(msg.ts, now)));
    }
    if ch.first_unread == Some(msg.id) {
        pre.push(Pre::New);
    }
    if ch.starts_group(i) {
        pre.push(Pre::Blank);
    }
    pre
}

fn messages(buf: &mut Buffer, area: Rect, app: &mut App) {
    let t = app.theme.clone();
    app.ui.chat_rows = area.height;
    app.ui.chat_total = 0;
    if area.height == 0 || area.width < 8 {
        return;
    }
    let Some(index) = app.channels.get(app.current).filter(|c| c.is_text()).map(|_| app.current) else {
        put_center(buf, area, area.y + area.height / 2, "no text channels", Style::new().fg(t.faint).bg(t.bg));
        return;
    };
    if app.channels[index].messages.is_empty() {
        let ch = &app.channels[index];
        let mid = area.y + area.height / 2;
        if !ch.loaded {
            let label = format!("{} loading #{}…", spinner(app.frame), ch.name);
            put_center(buf, area, mid, &label, Style::new().fg(t.dim).bg(t.bg));
        } else {
            put_center(buf, area, mid.saturating_sub(1), &format!("# {}", ch.name), Style::new().fg(t.text).bg(t.bg).add_modifier(Modifier::BOLD));
            put_center(buf, area, mid, "quiet here — write the first message", Style::new().fg(t.dim).bg(t.bg));
            put_center(buf, area, mid + 1, "Ctrl+O — send a file · F1 — everything else", Style::new().fg(t.faint).bg(t.bg));
        }
        return;
    }

    // two cells on the left for the selection/mention mark, two on the right for air and the scrollbar
    let w = area.width.saturating_sub(4) as usize;
    let now = chrono::Local::now();
    let count = app.channels[index].messages.len();

    // pass 1: the height of every block (the lines before plus the message)
    let mut starts = Vec::with_capacity(count);
    let mut heights = Vec::with_capacity(count);
    let mut total = 0usize;
    for i in 0..count {
        let (id, sig, header) = {
            let ch = &app.channels[index];
            let msg = &ch.messages[i];
            let header = ch.starts_group(i);
            (msg.id, signature(app, msg, header, w), header)
        };
        if app.ui.cache.get(&id).is_none_or(|laid| laid.sig != sig) {
            let laid = Laid { sig, ..lay(app, &app.channels[index].messages[i], header, w) };
            app.ui.cache.insert(id, laid);
        }
        let pre = pre_rows(&app.channels[index], i, now).len();
        let h = pre + app.ui.cache[&id].lines.len();
        starts.push(total);
        heights.push(h);
        total += h;
    }
    app.ui.chat_total = total;

    // scrolling: hold the anchor, show the selection, stay in range
    let rows = area.height as usize;
    let max_scroll = total.saturating_sub(rows);
    let ch = &mut app.channels[index];
    if ch.scroll > 0
        && let Some((id, offset)) = ch.anchor
        && let Ok(i) = ch.messages.binary_search_by_key(&id, |m| m.id)
    {
        let bottom = starts[i] + offset.min(heights[i].saturating_sub(1));
        ch.scroll = total.saturating_sub(bottom + 1);
    }
    if std::mem::take(&mut ch.reveal)
        && let Some(i) = ch.selected.and_then(|id| ch.messages.binary_search_by_key(&id, |m| m.id).ok())
    {
        let top = total.saturating_sub(rows + ch.scroll.min(max_scroll));
        let (block_top, block_bottom) = (starts[i], starts[i] + heights[i]);
        if block_top < top {
            ch.scroll = total.saturating_sub(block_top + rows);
        } else if block_bottom > top + rows {
            ch.scroll = total.saturating_sub(block_bottom);
        }
    }
    ch.scroll = ch.scroll.min(max_scroll);
    let top = total.saturating_sub(rows + ch.scroll);
    // a short feed has nowhere to scroll — keep it at the bottom, by the input
    let pad = rows.saturating_sub(total) as u16;
    ch.anchor = (ch.scroll > 0).then(|| {
        let bottom_row = top + rows - 1;
        let i = starts.partition_point(|s| *s <= bottom_row).saturating_sub(1);
        (ch.messages[i].id, bottom_row - starts[i])
    });

    // pass 2: draw only what is in view
    let selected = if app.focus == Focus::Messages { app.channels[index].selected } else { None };
    let me = app.my_name().to_string();
    let first = starts.partition_point(|s| *s <= top).saturating_sub(1);
    let mut wanted = Vec::new();
    let mut animating = false;
    for (i, start) in starts.iter().enumerate().skip(first) {
        if *start >= top + rows {
            break;
        }
        let ch = &app.channels[index];
        let msg = &ch.messages[i];
        let pre = pre_rows(ch, i, now);
        let laid = &app.ui.cache[&msg.id];
        if wants_preview(app, msg)
            && let Some(file) = &msg.file
            && !app.thumbs.contains_key(&file.id)
        {
            wanted.push(file.clone());
        }
        let is_selected = selected == Some(msg.id);
        let mentioned = !msg.system && msg.from != me && tovuer_proto::mentions(&msg.text, &me);
        let mut row = *start;
        for p in &pre {
            if row >= top && row < top + rows {
                let y = area.y + pad + (row - top) as u16;
                match p {
                    Pre::Blank => {}
                    Pre::Day(label) => separator(buf, area, y, label, t.faint, t.line, t.bg),
                    Pre::New => separator(buf, area, y, "new messages", t.err, t.err, t.bg),
                    Pre::Top(label) => put_center(buf, area, y, label, Style::new().fg(t.faint).bg(t.bg).add_modifier(Modifier::ITALIC)),
                }
            }
            row += 1;
        }
        for (li, line) in laid.lines.iter().enumerate() {
            if row >= top && row < top + rows {
                let y = area.y + pad + (row - top) as u16;
                if is_selected {
                    fill(buf, Rect::new(area.x, y, area.width - 1, 1), Style::new().bg(t.sel));
                    put(buf, area.x, y, "▌", Style::new().fg(t.accent).bg(t.sel), 1);
                } else if mentioned {
                    fill(buf, Rect::new(area.x, y, area.width - 1, 1), Style::new().bg(t.mention));
                    put(buf, area.x, y, "▌", Style::new().fg(t.warn).bg(t.mention), 1);
                }
                put_line(buf, area.x + 2, y, line, w as u16);
                if let Some(paint) = &laid.paint
                    && laid.body.contains(&li)
                {
                    let row = Rect::new(area.x + 2, y, (line.width() as u16).min(w as u16), 1);
                    paint_row(buf, row, paint, li - laid.body.start, laid.widest, app.frame);
                    animating |= paint.flow;
                }
                app.ui.hits.push((Rect::new(area.x, y, area.width - 1, 1), Hit::Message(msg.id)));
            }
            row += 1;
        }
    }
    app.ui.want_thumbs.extend(wanted);
    app.ui.animating |= animating;

    // a scrollbar only when there is something to scroll
    if total > rows {
        let x = area.right() - 1;
        let thumb_h = ((rows * rows) / total).clamp(1, rows);
        let thumb_y = (top * (rows - thumb_h)).checked_div(max_scroll).unwrap_or(0);
        for r in 0..rows {
            let inside = r >= thumb_y && r < thumb_y + thumb_h;
            put(buf, x, area.y + r as u16, if inside { "┃" } else { "│" }, Style::new().fg(if inside { t.dim } else { t.line }).bg(t.bg), 1);
        }
    }
}

/// A line with a centered label: `────── today ──────`.
fn separator(buf: &mut Buffer, area: Rect, y: u16, label: &str, text: Color, line: Color, bg: Color) {
    let inner = Rect::new(area.x + 2, y, area.width.saturating_sub(5), 1);
    hrule(buf, inner.x, y, inner.width, Style::new().fg(line).bg(bg));
    put_center(buf, inner, y, &format!("  {label}  "), Style::new().fg(text).bg(bg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_use_half_blocks() {
        let thumb = Thumb { w: 2, h: 4, px: vec![[1, 1, 1], [2, 2, 2], [3, 3, 3], [4, 4, 4], [5, 5, 5], [6, 6, 6], [7, 7, 7], [8, 8, 8]] };
        let lines = thumb_lines(&thumb);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans.len(), 2);
        let cell = &lines[1].spans[1];
        assert_eq!(cell.content, "▀");
        assert_eq!(cell.style.fg, Some(Color::Rgb(6, 6, 6)), "the top pixel is the glyph color");
        assert_eq!(cell.style.bg, Some(Color::Rgb(8, 8, 8)), "the bottom one is the background");
    }
}
