//! Side panels: channels (with who is in each voice channel) and the voice
//! console on the left, who is online on the right.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::icons::Icon;
use super::{Hit, fill, meter, put, put_right, text};
use crate::app::model::clock;
use crate::app::{App, Focus};
use tovuer_proto::ChannelKind;

/// A row of the channel list before drawing.
enum Row {
    Blank,
    Section(&'static str),
    /// Index of the channel in `app.channels`.
    Channel(usize),
    /// A member of a voice channel (user id).
    Member(u32),
}

const STEPS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A tiny three-bar voice level meter.
fn level_bars(level: f32) -> String {
    let level = (level * 4.0).clamp(0.0, 1.0);
    // three bars with different weights look livelier than one
    [0.7f32, 1.0, 0.85]
        .iter()
        .map(|k| STEPS[((level * k * 7.0).round() as usize).min(7)])
        .collect()
}

/// "Talking" with no known level (they are in another channel, their sound
/// does not reach us): draw a running wave to show they are on air.
fn wave_bars(frame: u64, seed: u32) -> String {
    const WAVE: [usize; 8] = [2, 4, 6, 4, 3, 5, 2, 4];
    let phase = (frame / 2) as usize + seed as usize;
    (0..3).map(|i| STEPS[WAVE[(phase + i * 3) % WAVE.len()]]).collect()
}

pub fn channels(buf: &mut Buffer, area: Rect, app: &mut App) {
    let t = app.theme.clone();
    let base = Style::new().fg(t.dim).bg(t.panel);
    fill(buf, area, base);
    // the last column stays empty: flat themes draw a divider there
    let w = area.width.saturating_sub(1);
    let x = area.x;

    // header: the program and the room
    put(buf, x + 1, area.y, "◆ tovuer", Style::new().fg(t.accent).bg(t.panel).add_modifier(Modifier::BOLD), w - 1);
    let online = format!("{} online", app.users.len());
    let server = format!("{} · {online}", app.server_name);
    put(buf, x + 1, area.y + 1, &text::clip(&server, w as usize - 1), Style::new().fg(t.faint).bg(t.panel), w - 1);

    // bottom: the voice console (if we are in a channel) and the "me" row
    let dock_h = if app.voice.is_some() { 4 } else { 0 };
    let me_y = area.bottom() - 1;
    let dock_y = me_y - dock_h;
    let list = Rect::new(x, area.y + 3, w, dock_y.saturating_sub(area.y + 3));

    let mut rows: Vec<Row> = vec![Row::Section("TEXT")];
    rows.extend(app.channels.iter().enumerate().filter(|(_, c)| c.is_text()).map(|(i, _)| Row::Channel(i)));
    rows.push(Row::Blank);
    rows.push(Row::Section("VOICE"));
    for (i, ch) in app.channels.iter().enumerate().filter(|(_, c)| !c.is_text()) {
        rows.push(Row::Channel(i));
        rows.extend(app.voice_members(&ch.name).iter().map(|u| Row::Member(u.id)));
    }

    // if the list does not fit, keep the row under the cursor in view
    let selected_row = rows.iter().position(|r| matches!(r, Row::Channel(i) if *i == app.side_sel)).unwrap_or(0);
    let visible = list.height as usize;
    let offset = if rows.len() <= visible { 0 } else { selected_row.saturating_sub(visible / 2).min(rows.len() - visible) };

    let my_id = app.my_id();
    for (line, row) in rows.iter().skip(offset).take(visible).enumerate() {
        let y = list.y + line as u16;
        match row {
            Row::Blank => {}
            Row::Section(title) => {
                put(buf, x + 1, y, title, Style::new().fg(t.faint).bg(t.panel).add_modifier(Modifier::BOLD), w - 1);
                // "+" makes a channel of this kind (a click, or n / v in the list)
                let (kind, key) = if *title == "TEXT" { (ChannelKind::Text, "n") } else { (ChannelKind::Voice, "v") };
                let hint = if app.focus == Focus::Sidebar { format!("{key} +") } else { "+".to_string() };
                let left = put_right(buf, x + w - 1, y, &hint, Style::new().fg(t.faint).bg(t.panel), x + 8);
                app.ui.hits.push((Rect::new(left.saturating_sub(1), y, x + w - left + 1, 1), Hit::NewChannel(kind)));
            }
            Row::Channel(i) => {
                let ch = &app.channels[*i];
                let cursor = app.focus == Focus::Sidebar && app.side_sel == *i;
                let open = ch.is_text() && *i == app.current;
                let joined = app.voice.as_ref().is_some_and(|v| !ch.is_text() && v.channel == ch.name);
                let bg = if cursor { t.sel } else { t.panel };
                let mut style = Style::new().bg(bg).fg(if ch.unread > 0 || open || joined { t.text } else { t.dim });
                if open || joined || ch.unread > 0 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if joined {
                    style = style.fg(t.ok);
                }
                fill(buf, Rect::new(x, y, w, 1), Style::new().bg(bg));
                app.ui.hits.push((Rect::new(x, y, w, 1), Hit::Channel(*i)));
                if open || joined {
                    put(buf, x, y, "▎", Style::new().fg(if joined { t.ok } else { t.accent }).bg(bg), 1);
                }
                let icon = if ch.is_text() { "# ".to_string() } else { app.icons.spaced(Icon::Voice) };
                let mut right = x + w;
                if ch.mentions > 0 {
                    let pill = format!(" @{} ", ch.mentions);
                    right = put_right(buf, right - 1, y, &pill, Style::new().fg(t.on_accent).bg(t.err).add_modifier(Modifier::BOLD), x + 4);
                } else if ch.unread > 0 {
                    let pill = format!(" {} ", ch.unread.min(999));
                    right = put_right(buf, right - 1, y, &pill, Style::new().fg(t.on_accent).bg(t.accent).add_modifier(Modifier::BOLD), x + 4);
                } else if !ch.is_text() {
                    let inside = app.voice_members(&ch.name).len();
                    if inside > 0 {
                        right = put_right(buf, right - 1, y, &inside.to_string(), Style::new().fg(t.faint).bg(bg), x + 4);
                    }
                }
                let room = right.saturating_sub(x + 2 + 1) as usize;
                let nx = put(buf, x + 2, y, &icon, style, 3);
                put(buf, nx, y, &text::clip(&ch.name, room.saturating_sub(text::width(&icon))), style, room as u16);
            }
            Row::Member(id) => {
                let Some(user) = app.user(*id) else { continue };
                let speaking = app.speaking.contains(id) && !user.muted;
                let color = t.nick(&user.name);
                let name_style = if speaking {
                    Style::new().fg(t.text).bg(t.panel).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(t.dim).bg(t.panel)
                };
                let dot = if speaking { "◉" } else { "●" };
                put(buf, x + 5, y, dot, Style::new().fg(if speaking { t.ok } else { color }).bg(t.panel), 1);
                // on the right: microphone/sound state or the live voice level
                let tail = if user.deafened {
                    Some((app.icons.get(Icon::Deafened).to_string(), t.err))
                } else if user.muted {
                    Some((app.icons.get(Icon::Muted).to_string(), t.err))
                } else if speaking {
                    let level = if *id == my_id { app.engine.controls().mic_level() } else { app.engine.peer_level(*id) };
                    let bars = if level > 0.002 { level_bars(level) } else { wave_bars(app.frame, *id) };
                    Some((bars, t.ok))
                } else if *id != my_id && app.settings.is_muted(&user.name) {
                    Some(("✕".to_string(), t.warn))
                } else {
                    None
                };
                let mut right = x + w;
                if let Some((tail, tail_color)) = tail {
                    right = put_right(buf, right - 1, y, &tail, Style::new().fg(tail_color).bg(t.panel), x + 8);
                }
                let room = right.saturating_sub(x + 7 + 1) as usize;
                put(buf, x + 7, y, &text::clip(&user.name, room), name_style, room as u16);
            }
        }
    }

    if app.voice.is_some() {
        voice_dock(buf, Rect::new(x, dock_y, w, dock_h), app);
    }

    // the "me" row
    let me = app.my_name().to_string();
    fill(buf, Rect::new(x, me_y, w, 1), Style::new().bg(t.raised));
    put(buf, x + 1, me_y, "●", Style::new().fg(t.nick(&me)).bg(t.raised), 1);
    let mut right = x + w;
    if app.is_admin() {
        right = put_right(buf, right - 1, me_y, app.icons.get(Icon::Admin), Style::new().fg(t.warn).bg(t.raised), x + 4);
    }
    let room = right.saturating_sub(x + 3 + 1) as usize;
    put(buf, x + 3, me_y, &text::clip(&me, room), Style::new().fg(t.text).bg(t.raised).add_modifier(Modifier::BOLD), room as u16);
}

/// The voice console: which channel, for how long, the microphone level.
fn voice_dock(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    let Some(session) = &app.voice else { return };
    let bg = t.raised;
    fill(buf, area, Style::new().fg(t.dim).bg(bg));
    let (x, w) = (area.x, area.width);

    // line 1: the channel and the time in it
    let time = clock(session.since.elapsed());
    let right = put_right(buf, x + w - 1, area.y, &time, Style::new().fg(t.faint).bg(bg), x + 6);
    let room = right.saturating_sub(x + 4 + 1) as usize;
    put(buf, x + 1, area.y, app.icons.get(Icon::Voice), Style::new().fg(t.ok).bg(bg), 2);
    put(buf, x + 4, area.y, &text::clip(&session.channel, room), Style::new().fg(t.ok).bg(bg).add_modifier(Modifier::BOLD), room as u16);

    // line 2: the microphone level and state
    let no_mic = session.info.as_ref().is_some_and(|i| i.mic.is_none());
    let (label, color): (&str, Color) = if session.starting {
        ("AUDIO…", t.warn)
    } else if app.deafened() {
        ("DEAF", t.err)
    } else if app.muted() {
        ("MUTED", t.err)
    } else if no_mic {
        ("NO MIC", t.warn)
    } else {
        ("ON AIR", t.ok)
    };
    let label_x = put_right(buf, x + w - 1, area.y + 1, label, Style::new().fg(color).bg(bg).add_modifier(Modifier::BOLD), x + 6);
    put(buf, x + 1, area.y + 1, app.icons.get(Icon::Mic), Style::new().fg(color).bg(bg), 2);
    let bar_w = label_x.saturating_sub(x + 4 + 1);
    // the meter works while muted too: you can see the microphone hears at all
    let level = (app.engine.controls().mic_level() * 4.0).min(1.0);
    let fill_color = if app.muted() || app.deafened() { t.faint } else { t.level(level) };
    meter(buf, x + 4, area.y + 1, bar_w, level, Style::new().fg(fill_color).bg(bg), Style::new().fg(t.line).bg(bg));

    // line 3: the device or a hint
    let device = session
        .info
        .as_ref()
        .and_then(|i| i.mic.clone())
        .unwrap_or_else(|| if session.starting { "opening devices…".into() } else { "no microphone".into() });
    put(buf, x + 1, area.y + 2, &text::clip(&device, w as usize - 2), Style::new().fg(t.faint).bg(bg), w - 2);
    let hint = if w >= 30 { "M mic · Alt+D sound · /v leave" } else { "M mic · Alt+D sound" };
    put(buf, x + 1, area.y + 3, &text::clip(hint, w as usize - 2), Style::new().fg(t.faint).bg(bg), w - 2);
}

pub fn members(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    fill(buf, area, Style::new().fg(t.dim).bg(t.panel));
    // the first column is for the divider on flat themes
    let (x, w) = (area.x + 1, area.width.saturating_sub(1));
    let title = format!("ONLINE — {}", app.users.len());
    put(buf, x + 1, area.y, &title, Style::new().fg(t.faint).bg(t.panel).add_modifier(Modifier::BOLD), w - 1);

    let my_id = app.my_id();
    for (i, user) in app.users.iter().enumerate() {
        let y = area.y + 2 + i as u16;
        if y >= area.bottom() {
            break;
        }
        if y == area.bottom() - 1 && i + 1 < app.users.len() {
            let more = format!("… and {} more", app.users.len() - i);
            put(buf, x + 1, y, &more, Style::new().fg(t.faint).bg(t.panel), w - 1);
            break;
        }
        let speaking = app.speaking.contains(&user.id) && !user.muted;
        put(buf, x + 1, y, if speaking { "◉" } else { "●" }, Style::new().fg(if speaking { t.ok } else { t.nick(&user.name) }).bg(t.panel), 1);
        let icon = if user.deafened {
            Some((app.icons.get(Icon::Deafened), t.err))
        } else if user.muted && user.voice.is_some() {
            Some((app.icons.get(Icon::Muted), t.err))
        } else if user.voice.is_some() {
            Some((app.icons.get(Icon::Voice), t.dim))
        } else if user.admin {
            Some((app.icons.get(Icon::Admin), t.warn))
        } else {
            None
        };
        let mut right = x + w;
        if let Some((icon, color)) = icon {
            right = put_right(buf, right - 1, y, icon, Style::new().fg(color).bg(t.panel), x + 4);
        }
        let mut style = Style::new().fg(if user.id == my_id { t.text } else { t.dim }).bg(t.panel);
        if user.id == my_id {
            style = style.add_modifier(Modifier::BOLD);
        }
        let room = right.saturating_sub(x + 3 + 1) as usize;
        put(buf, x + 3, y, &text::clip(&user.name, room), style, room as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_moves_and_differs_between_people() {
        assert_ne!(wave_bars(0, 1), wave_bars(2, 1), "the wave moves with time");
        assert_ne!(wave_bars(0, 1), wave_bars(0, 2), "different people are in different phases");
        assert_eq!(wave_bars(7, 3).chars().count(), 3);
    }

    #[test]
    fn level_bars_grow_with_level() {
        assert_eq!(level_bars(0.0), "▁▁▁");
        assert_eq!(level_bars(1.0), "▆█▇");
        let quiet: u32 = level_bars(0.03).chars().map(|c| c as u32).sum();
        let loud: u32 = level_bars(0.2).chars().map(|c| c as u32).sum();
        assert!(loud > quiet);
        assert_eq!(level_bars(0.1).chars().count(), 3);
    }
}
