//! The login screen, the joining screen and the pop-up windows.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::icons::{Icon, Icons};
use super::text::{clip, clip_left, width, wrap};
use super::theme::{Theme, blend};
use super::{centered, fill, frame, meter, put, put_center, put_line, put_right, spinner, tone_color};
use crate::app::commands::COMMANDS;
use crate::app::emoji::QUICK;
use crate::app::input::Input;
use crate::app::overlay::{Confirm, Overlay, Palette, Peek, Picker, Prompt, Setting, SettingsView, Viewer};
use crate::app::style::{StyleRow, StyleView, color_name, gradient_name};
use crate::app::{App, Conn, LoginField};
use crate::settings::human_size;
use crate::transfer::Thumb;

const LOGO: [&str; 3] = ["╔╦╗ ╔═╗ ╦  ╦ ╦ ╦ ╔═╗ ╦═╗", " ║  ║ ║ ╚╗╔╝ ║ ║ ║╣  ╠╦╝", " ╩  ╚═╝  ╚╝  ╚═╝ ╚═╝ ╩╚═"];
const TAGLINE: &str = "voice and chat right in the terminal";

/// The logo, shading from the accent color to "info".
fn logo(buf: &mut Buffer, area: Rect, y: u16, t: &Theme) {
    let w = LOGO[0].chars().count() as u16;
    let x0 = area.x + area.width.saturating_sub(w) / 2;
    for (row, line) in LOGO.iter().enumerate() {
        for (i, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let color = blend(t.accent, t.info, i as f32 / (w - 1) as f32);
            let mut s = [0u8; 4];
            put(buf, x0 + i as u16, y + row as u16, ch.encode_utf8(&mut s), Style::new().fg(color).bg(t.bg).add_modifier(Modifier::BOLD), 1);
        }
    }
}

/// One field of the login form.
struct Field<'a> {
    label: &'a str,
    input: &'a Input,
    active: bool,
    /// Show dots instead of characters.
    mask: bool,
    placeholder: &'a str,
}

/// A framed form field with a label. Returns the cursor position if the field is active.
fn field(buf: &mut Buffer, area: Rect, f: Field<'_>, t: &Theme) -> Option<(u16, u16)> {
    let Field { label, input, active, mask, placeholder } = f;
    let border = Style::new().fg(if active { t.accent } else { t.line }).bg(t.bg);
    let title = Style::new().fg(if active { t.accent } else { t.dim }).bg(t.bg);
    frame(buf, area, border, Some((label, title)));
    let inner = area.width.saturating_sub(4) as usize;
    let layout = input.layout(usize::MAX / 4, mask);
    let line = &layout.lines[0];
    // a long value: show its tail so the cursor stays in the field
    let cursor = layout.cursor.0 as usize;
    let skip = cursor.saturating_sub(inner.saturating_sub(1));
    let shown: String = {
        let mut out = String::new();
        let mut used = 0;
        let mut passed = 0;
        for g in unicode_segmentation::UnicodeSegmentation::graphemes(line.as_str(), true) {
            let gw = width(g);
            if passed < skip {
                passed += gw;
                continue;
            }
            if used + gw > inner {
                break;
            }
            out.push_str(g);
            used += gw;
        }
        out
    };
    if input.is_empty() {
        put(buf, area.x + 2, area.y + 1, &clip(placeholder, inner), Style::new().fg(t.faint).bg(t.bg), inner as u16);
    } else {
        put(buf, area.x + 2, area.y + 1, &shown, Style::new().fg(t.text).bg(t.bg), inner as u16);
    }
    active.then(|| (area.x + 2 + (cursor - skip) as u16, area.y + 1))
}

pub fn login(buf: &mut Buffer, area: Rect, app: &App) -> Option<(u16, u16)> {
    let t = &app.theme;
    let form = &app.login;
    let mut fields: Vec<(LoginField, &str, &Input, bool, &str)> = vec![(LoginField::Nick, "Nick", &form.nick, false, "what to call you")];
    if !app.launch.friend {
        fields.push((LoginField::Room, "Room", &form.room, false, "same name as your friends, e.g. our-place"));
    }
    if form.ask_password {
        fields.push((LoginField::Password, "Room password", &form.password, true, "optional — keeps strangers out"));
    }
    let w = area.width.saturating_sub(4).min(58);
    let compact = area.height < 22;
    let logo_h: u16 = if compact { 2 } else { 6 };
    let h = logo_h + fields.len() as u16 * 3 + 4;
    let boxed = centered(area, w, h);
    let mut y = boxed.y;

    if compact {
        put_center(buf, boxed, y, "◆ tovuer", Style::new().fg(t.accent).bg(t.bg).add_modifier(Modifier::BOLD));
        y += 2;
    } else {
        logo(buf, boxed, y, t);
        put_center(buf, boxed, y + 4, TAGLINE, Style::new().fg(t.dim).bg(t.bg));
        y += 6;
    }

    let mut cursor = None;
    for (which, label, input, mask, placeholder) in fields {
        let active = form.field == which;
        let at = field(buf, Rect::new(boxed.x, y, w, 3), Field { label, input, active, mask, placeholder }, t);
        cursor = cursor.or(at);
        y += 3;
    }

    if let Some(error) = &form.error {
        let lines = wrap(&[Span::raw(error.clone())], w as usize - 2);
        for line in lines.iter().take(2) {
            put(buf, boxed.x + 1, y, "✕ ", Style::new().fg(t.err).bg(t.bg), 2);
            put_line(buf, boxed.x + 3, y, &line.clone().style(Style::new().fg(t.err).bg(t.bg)), w - 3);
            y += 1;
        }
    } else {
        y += 1;
    }
    let hint = if form.offer_update {
        "F5 — update the client · Enter — join · Esc — quit"
    } else if app.launch.friend && !form.ask_password {
        "Enter — join · Esc — quit"
    } else {
        "Enter — join · Tab — next field · Esc — quit"
    };
    put_center(buf, boxed, y + 1, hint, Style::new().fg(t.faint).bg(t.bg));
    if let Some((got, total)) = app.updating {
        let label = format!("{} downloading the update{}", spinner(app.frame), super::percent(got, total));
        put_center(buf, boxed, y + 2, &label, Style::new().fg(t.info).bg(t.bg));
    }
    put_right(buf, area.right() - 1, area.bottom() - 1, &format!("tovuer {}", app.launch.version), Style::new().fg(t.faint).bg(t.bg), area.x);
    cursor
}

/// Until the room is joined: what is going on and what can be done.
pub fn splash(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    let boxed = centered(area, area.width.saturating_sub(4).min(66), 12);
    logo(buf, boxed, boxed.y, t);
    let y = boxed.y + 5;
    match &app.conn {
        Conn::Waiting { reason, until } => {
            let left = until.saturating_duration_since(std::time::Instant::now()).as_secs() + 1;
            put_center(buf, boxed, y, "could not open the room", Style::new().fg(t.err).bg(t.bg).add_modifier(Modifier::BOLD));
            for (i, line) in wrap(&[Span::raw(reason.clone())], boxed.width as usize).iter().take(2).enumerate() {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                put_center(buf, boxed, y + 1 + i as u16, &text, Style::new().fg(t.dim).bg(t.bg));
            }
            put_center(buf, boxed, y + 4, &format!("next try in {left} s"), Style::new().fg(t.faint).bg(t.bg));
        }
        Conn::Connecting { attempt, server } => {
            let label = format!("{} looking for the room…", spinner(app.frame));
            put_center(buf, boxed, y, &label, Style::new().fg(t.text).bg(t.bg).add_modifier(Modifier::BOLD));
            put_center(buf, boxed, y + 1, &clip(server, boxed.width as usize), Style::new().fg(t.dim).bg(t.bg));
            if *attempt > 1 {
                put_center(buf, boxed, y + 2, &format!("attempt {attempt}"), Style::new().fg(t.faint).bg(t.bg));
            }
        }
        _ => put_center(buf, boxed, y, "finding the other members…", Style::new().fg(t.dim).bg(t.bg)),
    }
    let change = if app.launch.friend { "change the nick" } else { "change the room or nick" };
    put_center(buf, boxed, boxed.bottom() - 1, &format!("Enter — try now · Esc — {change} · Ctrl+Q — quit"), Style::new().fg(t.faint).bg(t.bg));
}

// ------------------------------------------------------------------------ windows

/// Draw a raised window with a frame and a title; return its inside.
fn window(buf: &mut Buffer, area: Rect, title: &str, t: &Theme) -> Rect {
    fill(buf, area, Style::new().fg(t.text).bg(t.raised));
    frame(buf, area, Style::new().fg(t.accent).bg(t.raised), Some((title, Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD))));
    Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), area.height.saturating_sub(2))
}

/// Key hints in the bottom line of a window frame.
fn footer(buf: &mut Buffer, area: Rect, hint: &str, t: &Theme) {
    if area.width > 8 {
        let shown = clip(hint, area.width as usize - 6);
        put_right(buf, area.right() - 2, area.bottom() - 1, &format!(" {shown} "), Style::new().fg(t.faint).bg(t.raised), area.x + 2);
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App) -> Option<(u16, u16)> {
    let mut overlay = app.overlay.take()?;
    let cursor = match &mut overlay {
        Overlay::Help { scroll } => {
            help(buf, area, app, scroll);
            None
        }
        Overlay::Log { scroll } => {
            log(buf, area, app, scroll);
            None
        }
        Overlay::Palette(palette) => Some(palette_window(buf, area, app, palette)),
        Overlay::Settings(view) => {
            settings(buf, area, app, view);
            None
        }
        Overlay::Style(view) => {
            style_window(buf, area, app, view);
            None
        }
        Overlay::Picker(picker) => {
            picker_window(buf, area, app, picker);
            None
        }
        Overlay::Viewer(viewer) => {
            viewer_window(buf, area, app, viewer);
            None
        }
        Overlay::Users { sel } => {
            users(buf, area, app, *sel);
            None
        }
        Overlay::React { sel, .. } => {
            react(buf, area, app, *sel);
            None
        }
        Overlay::Confirm(confirm) => {
            confirm_window(buf, area, app, confirm);
            None
        }
        Overlay::Prompt(prompt) => Some(prompt_window(buf, area, app, prompt)),
    };
    app.overlay = Some(overlay);
    cursor
}

/// A scrollable list of lines inside a window; clamps `scroll` so it stays in range.
fn scrolled(buf: &mut Buffer, inner: Rect, lines: &[Line<'static>], scroll: &mut usize, from_bottom: bool, t: &Theme) {
    let rows = inner.height as usize;
    let max = lines.len().saturating_sub(rows);
    *scroll = (*scroll).min(max);
    let top = if from_bottom { max - *scroll } else { *scroll };
    for (i, line) in lines.iter().skip(top).take(rows).enumerate() {
        put_line(buf, inner.x, inner.y + i as u16, line, inner.width);
    }
    if lines.len() > rows && rows > 0 {
        let thumb = (rows * rows / lines.len()).clamp(1, rows);
        let pos = (top * (rows - thumb)).checked_div(max).unwrap_or(0);
        for r in 0..rows {
            let inside = r >= pos && r < pos + thumb;
            put(buf, inner.right() + 1, inner.y + r as u16, if inside { "┃" } else { "│" }, Style::new().fg(if inside { t.accent } else { t.line }).bg(t.raised), 1);
        }
    }
}

fn help_lines(t: &Theme, w: usize) -> Vec<Line<'static>> {
    let head = |s: &str| Line::from(Span::styled(s.to_string(), Style::new().fg(t.accent).add_modifier(Modifier::BOLD)));
    let key_w = 22usize.min(w / 2);
    let row = |keys: &str, what: &str| {
        let pad = key_w.saturating_sub(width(keys));
        Line::from(vec![
            Span::styled(format!("  {keys}{}", " ".repeat(pad)), Style::new().fg(t.text).add_modifier(Modifier::BOLD)),
            Span::styled(clip(what, w.saturating_sub(key_w + 3)), Style::new().fg(t.dim)),
        ])
    };
    let mut lines = vec![
        head("GENERAL"),
        row("Ctrl+K", "palette: find and run any action"),
        row("Tab / Shift+Tab", "panels: input → channels → messages"),
        row("Alt+↑ / Alt+↓", "next channel (also Ctrl+P / Ctrl+N)"),
        row("Alt+1…9", "text channel by number"),
        row("PgUp / PgDn", "scroll the feed · Ctrl+End — to the latest"),
        row("Ctrl+O", "send a file (or drop a file into the window)"),
        row("F2 · F3 · F4", "settings · peer volumes · your message style"),
        row("Ctrl+L", "system log"),
        row("Ctrl+Q", "quit"),
        Line::default(),
        head("INPUT"),
        row("Enter", "send · Alt+Enter — new line"),
        row("↑ on an empty line", "edit your last message"),
        row("Tab", "complete a /command, @nick, :emoji:"),
        row("Ctrl+↑ / Ctrl+↓", "input history"),
        Line::default(),
        head("CHANNELS  (Tab from the input)"),
        row("n · v", "create a text · voice channel (or click + next to TEXT / VOICE)"),
        row("Del", "delete the channel under the cursor (room owner)"),
        row("t", "the text channel's topic"),
        Line::default(),
        head("VOICE"),
        row("Enter on a channel", "join or leave a voice channel"),
        row("Alt+M", "microphone (or just M on an empty line)"),
        row("Alt+D", "sound: hear no one and stay silent"),
        Line::default(),
        head("MESSAGES  (Shift+Tab from the input)"),
        row("↑ ↓", "pick a message"),
        row("r · e · d", "reply · edit · delete"),
        row("a", "add a reaction"),
        row("Enter · s", "open a file or picture · save it"),
        row("o", "open in your system's app (a photo in full quality)"),
        row("y", "copy the text"),
        Line::default(),
        head("MARKUP"),
        row("**bold** *italic*", "~~strike~~  `code`  > quote  ```block```"),
        row("@nick · @all", "mention someone or everyone"),
        Line::default(),
        head("COMMANDS"),
    ];
    for c in COMMANDS {
        let name = format!("/{} {}", c.names[0], c.args);
        lines.push(row(name.trim_end(), c.help));
    }
    lines
}

fn help(buf: &mut Buffer, area: Rect, app: &mut App, scroll: &mut usize) {
    let t = app.theme.clone();
    let boxed = centered(area, area.width.saturating_sub(4).min(86), area.height.saturating_sub(2));
    let inner = window(buf, boxed, "Help", &t);
    app.ui.list_rows = inner.height;
    let lines = help_lines(&t, inner.width as usize);
    scrolled(buf, inner, &lines, scroll, false, &t);
    footer(buf, boxed, "↑↓ PgUp PgDn — scroll · Esc — close", &t);
}

fn log(buf: &mut Buffer, area: Rect, app: &mut App, scroll: &mut usize) {
    let t = app.theme.clone();
    let boxed = centered(area, area.width.saturating_sub(4).min(110), area.height.saturating_sub(2));
    let inner = window(buf, boxed, "System log", &t);
    app.ui.list_rows = inner.height;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &app.log {
        let stamp = entry.at.format("%H:%M:%S ").to_string();
        let body_w = (inner.width as usize).saturating_sub(width(&stamp)).max(8);
        let color = match entry.tone {
            crate::app::Tone::Info => t.dim,
            tone => tone_color(&t, tone),
        };
        for (i, line) in wrap(&[Span::styled(entry.text.clone(), Style::new().fg(color))], body_w).into_iter().enumerate() {
            let prefix = if i == 0 { stamp.clone() } else { " ".repeat(width(&stamp)) };
            let mut spans = vec![Span::styled(prefix, Style::new().fg(t.faint))];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }
    scrolled(buf, inner, &lines, scroll, true, &t);
    footer(buf, boxed, "↑↓ PgUp PgDn — scroll · Esc — close", &t);
}

fn palette_window(buf: &mut Buffer, area: Rect, app: &mut App, palette: &mut Palette) -> (u16, u16) {
    let t = app.theme.clone();
    let rows = palette.actions.len().min(area.height.saturating_sub(8) as usize).max(3) as u16;
    let boxed = centered(area, area.width.saturating_sub(4).min(64), rows + 4);
    // a little above center: the eye does not jump while filtering
    let boxed = Rect::new(boxed.x, boxed.y.saturating_sub((area.height / 8).min(boxed.y.saturating_sub(area.y))), boxed.width, boxed.height);
    let inner = window(buf, boxed, "Commands", &t);
    app.ui.list_rows = rows;

    let layout = palette.query.layout(usize::MAX / 4, false);
    put(buf, inner.x, inner.y, "› ", Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD), 2);
    if palette.query.is_empty() {
        put(buf, inner.x + 2, inner.y, "what to do? start typing…", Style::new().fg(t.faint).bg(t.raised), inner.width - 2);
    } else {
        put(buf, inner.x + 2, inner.y, &layout.lines[0], Style::new().fg(t.text).bg(t.raised), inner.width - 2);
    }
    super::hrule(buf, inner.x, inner.y + 1, inner.width, Style::new().fg(t.line).bg(t.raised));

    let list_y = inner.y + 2;
    let visible = rows as usize;
    let top = palette.sel.saturating_sub(visible - 1).min(palette.shown.len().saturating_sub(visible));
    if palette.shown.is_empty() {
        put(buf, inner.x, list_y, "nothing found", Style::new().fg(t.faint).bg(t.raised), inner.width);
    }
    for (row, index) in palette.shown.iter().skip(top).take(visible).enumerate() {
        let action = &palette.actions[*index];
        let y = list_y + row as u16;
        let selected = top + row == palette.sel;
        let bg = if selected { t.sel } else { t.raised };
        fill(buf, Rect::new(boxed.x + 1, y, boxed.width - 2, 1), Style::new().bg(bg));
        let mut style = Style::new().fg(if selected { t.text } else { t.dim }).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        put(buf, inner.x, y, action.icon, Style::new().fg(t.accent).bg(bg), 2);
        let right = if action.hint.is_empty() {
            inner.right()
        } else {
            put_right(buf, inner.right(), y, &action.hint, Style::new().fg(t.faint).bg(bg), inner.x + 10)
        };
        let room = right.saturating_sub(inner.x + 3 + 1) as usize;
        put(buf, inner.x + 3, y, &clip(&action.label, room), style, room as u16);
    }
    footer(buf, boxed, "↑↓ — pick · Enter — run · Esc — close", &t);
    (inner.x + 2 + layout.cursor.0.min(inner.width.saturating_sub(3)), inner.y)
}

fn settings(buf: &mut Buffer, area: Rect, app: &mut App, view: &mut SettingsView) {
    let t = app.theme.clone();
    let sections = Setting::ALL.iter().filter(|s| s.section().is_some()).count() as u16;
    let wanted = Setting::ALL.len() as u16 + sections * 2 + 4;
    let boxed = centered(area, area.width.saturating_sub(4).min(74), wanted.min(area.height.saturating_sub(2)));
    let inner = window(buf, boxed, "Settings", &t);
    view.sel = view.sel.min(Setting::ALL.len() - 1);

    // rows: section headings and the settings themselves
    let mut rows: Vec<(Option<&'static str>, Option<usize>)> = Vec::new();
    for (i, setting) in Setting::ALL.iter().enumerate() {
        if let Some(section) = setting.section() {
            if i > 0 {
                rows.push((None, None));
            }
            rows.push((Some(section), None));
        }
        rows.push((None, Some(i)));
    }
    let list_h = inner.height.saturating_sub(3) as usize;
    app.ui.list_rows = list_h as u16;
    let selected_row = rows.iter().position(|(_, s)| *s == Some(view.sel)).unwrap_or(0);
    let top = if rows.len() <= list_h { 0 } else { selected_row.saturating_sub(list_h / 2).min(rows.len() - list_h) };

    for (line, (section, setting)) in rows.iter().skip(top).take(list_h).enumerate() {
        let y = inner.y + line as u16;
        if let Some(section) = section {
            put(buf, inner.x, y, section, Style::new().fg(t.faint).bg(t.raised).add_modifier(Modifier::BOLD), inner.width);
            continue;
        }
        let Some(i) = setting else { continue };
        let which = Setting::ALL[*i];
        let selected = *i == view.sel;
        let bg = if selected { t.sel } else { t.raised };
        fill(buf, Rect::new(boxed.x + 1, y, boxed.width - 2, 1), Style::new().bg(bg));
        let mut label = Style::new().fg(if selected { t.text } else { t.dim }).bg(bg);
        if selected {
            label = label.add_modifier(Modifier::BOLD);
        }
        let loading = matches!(which, Setting::Mic | Setting::Spk) && view.mics.is_none();
        let value = app.setting_value(which);
        let value_color = match value.as_str() {
            "on" => t.ok,
            "off" => t.faint,
            _ => t.accent,
        };
        let shown = clip(&value, (inner.width as usize / 2).max(8));
        let shown = if selected { format!("‹ {shown} ›") } else { format!("  {shown}  ") };
        let right = put_right(buf, inner.right(), y, &shown, Style::new().fg(value_color).bg(bg), inner.x + 8);
        let room = right.saturating_sub(inner.x + 2 + 1) as usize;
        let text = if loading { format!("{} {}", which.label(), spinner(app.frame)) } else { which.label().to_string() };
        put(buf, inner.x + 1, y, &clip(&text, room), label, room as u16);
    }

    // bottom of the window: what the selected row means and the live mic level
    let which = Setting::ALL[view.sel];
    let help_y = inner.bottom() - 2;
    super::hrule(buf, inner.x, help_y - 1, inner.width, Style::new().fg(t.line).bg(t.raised));
    put(buf, inner.x, help_y, &clip(which.help(), inner.width as usize), Style::new().fg(t.dim).bg(t.raised), inner.width);
    if app.engine.running() {
        let level = (app.engine.controls().mic_level() * 4.0).min(1.0);
        let x = put(buf, inner.x, help_y + 1, &app.icons.spaced(Icon::Mic), Style::new().fg(t.dim).bg(t.raised), 3);
        let bar_w = inner.width.saturating_sub(4).min(30);
        meter(buf, x, help_y + 1, bar_w, level, Style::new().fg(t.level(level)).bg(t.raised), Style::new().fg(t.line).bg(t.raised));
        let state = if app.engine.monitor() { "test: speak and hear yourself" } else { "microphone now" };
        put(buf, x + bar_w + 2, help_y + 1, state, Style::new().fg(t.faint).bg(t.raised), inner.right().saturating_sub(x + bar_w + 2));
    } else {
        put(buf, inner.x, help_y + 1, "the microphone level shows in a voice channel and during the mic test", Style::new().fg(t.faint).bg(t.raised), inner.width);
    }
    footer(buf, boxed, "↑↓ — pick · ←→ / Enter — change · Esc — close", &t);
}

/// What the style window shows as an example.
const STYLE_SAMPLE: &str = "hey! this is how my messages look now — with a link https://example.com and `code` too";

fn style_window(buf: &mut Buffer, area: Rect, app: &mut App, view: &mut StyleView) {
    let t = app.theme.clone();
    let rows = StyleRow::ALL.len() as u16;
    let boxed = centered(area, area.width.saturating_sub(4).min(72), (rows + 8).min(area.height.saturating_sub(2)));
    let inner = window(buf, boxed, "Your message style", &t);
    view.sel = view.sel.min(StyleRow::ALL.len() - 1);
    let style = app.settings.style.clean();

    // the preview: your next message, the way everyone will see it
    let me = app.my_name().to_string();
    let nick = if me.is_empty() { "you".to_string() } else { me };
    let x = put(buf, inner.x + 1, inner.y, &nick, Style::new().fg(t.nick(&nick)).bg(t.raised).add_modifier(Modifier::BOLD), inner.width);
    put(buf, x + 2, inner.y, "now", Style::new().fg(t.faint).bg(t.raised), 3);
    let shown = super::look::look(style.as_ref(), &t, true);
    let markup = super::text::Markup { theme: &t, me: "", base: shown.base.bg(shown.base.bg.unwrap_or(t.raised)) };
    let text_w = inner.width.saturating_sub(2);
    let lines = super::text::message_lines(STYLE_SAMPLE, &markup, text_w as usize);
    let widest = lines.iter().map(|l| l.width()).max().unwrap_or(0);
    for (i, line) in lines.iter().take(2).enumerate() {
        let y = inner.y + 1 + i as u16;
        put_line(buf, inner.x + 1, y, line, text_w);
        if let Some(paint) = &shown.paint {
            super::look::paint_row(buf, Rect::new(inner.x + 1, y, (line.width() as u16).min(text_w), 1), paint, i, widest, app.frame);
            app.ui.animating |= paint.flow;
        }
    }
    let list_y = inner.y + 4;
    super::hrule(buf, inner.x, list_y - 1, inner.width, Style::new().fg(t.line).bg(t.raised));

    let s = &app.settings.style;
    let has_gradient = s.gradient.len() >= 2;
    for (i, row) in StyleRow::ALL.iter().enumerate() {
        let y = list_y + i as u16;
        if y + 3 > inner.bottom() {
            break;
        }
        let selected = i == view.sel;
        let bg = if selected { t.sel } else { t.raised };
        fill(buf, Rect::new(boxed.x + 1, y, boxed.width - 2, 1), Style::new().bg(bg));
        // the markup rows show their own effect
        let mut label = Style::new().fg(if selected { t.text } else { t.dim }).bg(bg);
        label = match row {
            StyleRow::Bold => label.add_modifier(Modifier::BOLD),
            StyleRow::Italic => label.add_modifier(Modifier::ITALIC),
            StyleRow::Strike => label.add_modifier(Modifier::CROSSED_OUT),
            StyleRow::Code => label.fg(t.warn),
            _ if selected => label.add_modifier(Modifier::BOLD),
            _ => label,
        };
        put(buf, inner.x + 1, y, row.label(), label, inner.width / 2);

        let on = |b: bool| if b { ("on", t.ok) } else { ("off", t.faint) };
        let (value, color) = match row {
            StyleRow::Color if has_gradient && s.color.is_some() => (format!("{} · hidden", color_name(s)), t.faint),
            StyleRow::Color if has_gradient => ("default".to_string(), t.faint),
            StyleRow::Color => (color_name(s), t.accent),
            StyleRow::Gradient => (gradient_name(s), if has_gradient { t.accent } else { t.faint }),
            StyleRow::Flow if !has_gradient => ("needs a gradient".to_string(), t.faint),
            StyleRow::Flow => (on(s.flow).0.to_string(), on(s.flow).1),
            StyleRow::Bold => (on(s.bold).0.to_string(), on(s.bold).1),
            StyleRow::Italic => (on(s.italic).0.to_string(), on(s.italic).1),
            StyleRow::Strike => (on(s.strike).0.to_string(), on(s.strike).1),
            StyleRow::Code => (on(s.code).0.to_string(), on(s.code).1),
            StyleRow::Reset => ("Enter".to_string(), t.faint),
        };
        let value = clip(&value, (inner.width as usize / 2).saturating_sub(10).max(6));
        let value = if selected && *row != StyleRow::Reset { format!("‹ {value} ›") } else { format!("  {value}  ") };
        let right = put_right(buf, inner.right(), y, &value, Style::new().fg(color).bg(bg), inner.x + 8);
        // a swatch of the color or the gradient in front of the value
        let swatch: Vec<[u8; 3]> = match row {
            StyleRow::Color if !has_gradient => s.color.as_deref().and_then(tovuer_proto::parse_color).map(|c| vec![super::look::readable(&t, c); 4]).unwrap_or_default(),
            StyleRow::Gradient if has_gradient => {
                let stops: Vec<[u8; 3]> = s.gradient.iter().filter_map(|c| tovuer_proto::parse_color(c)).map(|c| super::look::readable(&t, c)).collect();
                (0..8).map(|k| super::look::sample(&stops, k as f32 / 7.0, false)).collect()
            }
            _ => Vec::new(),
        };
        let start = right.saturating_sub(swatch.len() as u16 + 1);
        for (k, c) in swatch.iter().enumerate() {
            put(buf, start + k as u16, y, "█", Style::new().fg(Color::Rgb(c[0], c[1], c[2])).bg(bg), 1);
        }
    }

    let row = StyleRow::ALL[view.sel];
    let help_y = inner.bottom() - 1;
    super::hrule(buf, inner.x, help_y - 1, inner.width, Style::new().fg(t.line).bg(t.raised));
    put(buf, inner.x, help_y, &clip(row.help(), inner.width as usize), Style::new().fg(t.dim).bg(t.raised), inner.width);
    footer(buf, boxed, "↑↓ — pick · ←→ — change · Enter — your own · Esc", &t);
}

/// Draw a picture from half blocks, centered in `area`.
fn paint_thumb(buf: &mut Buffer, area: Rect, thumb: &Thumb) {
    let x0 = area.x + area.width.saturating_sub(thumb.w) / 2;
    let y0 = area.y + area.height.saturating_sub(thumb.rows()) / 2;
    for row in 0..thumb.rows().min(area.height) {
        for x in 0..thumb.w.min(area.width) {
            let (top, bottom) = (thumb.at(x, row * 2), thumb.at(x, row * 2 + 1));
            let style = Style::new().fg(Color::Rgb(top[0], top[1], top[2])).bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
            put(buf, x0 + x, y0 + row, "▀", style, 1);
        }
    }
}

fn file_icon(icons: Icons, name: &str, dir: bool) -> &'static str {
    icons.get(if dir {
        Icon::Folder
    } else if tovuer_proto::is_image_name(name) {
        Icon::Picture
    } else {
        Icon::File
    })
}

fn picker_window(buf: &mut Buffer, area: Rect, app: &mut App, picker: &mut Picker) {
    let t = app.theme.clone();
    let boxed = centered(area, area.width.saturating_sub(4).min(120), area.height.saturating_sub(2));
    let inner = window(buf, boxed, "Send a file", &t);
    let channel = app.channel_name().to_string();

    // header: the path and the filter
    put(buf, inner.x, inner.y, &clip_left(&picker.dir.display().to_string(), inner.width as usize), Style::new().fg(t.dim).bg(t.raised), inner.width);
    let filter_y = inner.y + 1;
    if let Some(error) = &picker.error {
        put(buf, inner.x, filter_y, &clip(&format!("✕ {error}"), inner.width as usize), Style::new().fg(t.err).bg(t.raised), inner.width);
    } else if picker.filter.is_empty() {
        let hint = if picker.hidden { "type to filter · hidden files shown" } else { "type to filter" };
        put(buf, inner.x, filter_y, hint, Style::new().fg(t.faint).bg(t.raised), inner.width);
    } else {
        let x = put(buf, inner.x, filter_y, &app.icons.spaced(Icon::Search), Style::new().fg(t.dim).bg(t.raised), 3);
        put(buf, x, filter_y, &picker.filter, Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD), inner.width - 3);
    }

    // on the right — a preview of the picture under the cursor (if the window is wide enough)
    let preview_w = if inner.width >= 84 { 42 } else { 0 };
    let list = Rect::new(inner.x, inner.y + 3, inner.width - preview_w, inner.height.saturating_sub(3));
    super::hrule(buf, inner.x, inner.y + 2, inner.width, Style::new().fg(t.line).bg(t.raised));
    app.ui.list_rows = list.height;

    let rows = list.height as usize;
    let top = picker.sel.saturating_sub(rows.saturating_sub(1)).min(picker.shown.len().saturating_sub(rows));
    if picker.shown.is_empty() {
        let empty = if picker.filter.is_empty() { "nothing here" } else { "nothing matches the filter" };
        put(buf, list.x, list.y, empty, Style::new().fg(t.faint).bg(t.raised), list.width);
    }
    for row in 0..rows {
        let Some(entry) = picker.entry(top + row) else { break };
        let y = list.y + row as u16;
        let selected = top + row == picker.sel;
        let bg = if selected { t.sel } else { t.raised };
        fill(buf, Rect::new(list.x - 1, y, list.width + 1, 1), Style::new().bg(bg));
        let color = if entry.dir { t.info } else if selected { t.text } else { t.dim };
        let mut style = Style::new().fg(color).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        put(buf, list.x, y, file_icon(app.icons, &entry.name, entry.dir), Style::new().fg(color).bg(bg), 2);
        let meta = if entry.dir {
            String::new()
        } else {
            let date = entry
                .modified
                .map(|m| chrono::DateTime::<chrono::Local>::from(m).format("%d.%m.%y").to_string())
                .unwrap_or_default();
            format!("{:>9}  {date}", human_size(entry.size))
        };
        let too_big = !entry.dir && entry.size > app.max_file;
        let right = put_right(buf, list.right() - 1, y, &meta, Style::new().fg(if too_big { t.err } else { t.faint }).bg(bg), list.x + 8);
        let room = right.saturating_sub(list.x + 3 + 1) as usize;
        let name = if entry.dir { format!("{}/", entry.name) } else { entry.name.clone() };
        put(buf, list.x + 3, y, &clip(&name, room), style, room as u16);
    }

    if preview_w > 0 {
        let pane = Rect::new(list.right() + 1, list.y, preview_w - 1, list.height);
        super::vrule(buf, list.right(), list.y, list.height, Style::new().fg(t.line).bg(t.raised));
        match &picker.peek {
            Some((_, Peek::Ready(thumb))) => paint_thumb(buf, pane, thumb),
            Some((_, Peek::Loading)) => put_center(buf, pane, pane.y + pane.height / 2, &format!("{} preview…", spinner(app.frame)), Style::new().fg(t.faint).bg(t.raised)),
            Some((_, Peek::Failed)) => put_center(buf, pane, pane.y + pane.height / 2, "no preview", Style::new().fg(t.faint).bg(t.raised)),
            None => {
                let mid = pane.y + pane.height / 2;
                match picker.current() {
                    Some(entry) if !entry.dir => {
                        put_center(buf, pane, mid.saturating_sub(1), &clip(&entry.name, pane.width as usize), Style::new().fg(t.text).bg(t.raised).add_modifier(Modifier::BOLD));
                        put_center(buf, pane, mid, &human_size(entry.size), Style::new().fg(t.dim).bg(t.raised));
                        if entry.size > app.max_file {
                            put_center(buf, pane, mid + 1, &format!("over the {} limit", human_size(app.max_file)), Style::new().fg(t.err).bg(t.raised));
                        } else {
                            put_center(buf, pane, mid + 1, &format!("Enter — send to #{channel}"), Style::new().fg(t.faint).bg(t.raised));
                        }
                    }
                    Some(_) => put_center(buf, pane, mid, "Enter — open the folder", Style::new().fg(t.faint).bg(t.raised)),
                    None => {}
                }
            }
        }
    }
    footer(buf, boxed, "Enter — open/send · ← — up · . — hidden · ~ — home · Esc", &t);
}

fn viewer_window(buf: &mut Buffer, area: Rect, app: &mut App, viewer: &mut Viewer) {
    let t = app.theme.clone();
    let boxed = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
    let mut title = format!("{} · {}", viewer.file.name, human_size(viewer.file.size));
    if let Some(pic) = &viewer.pic {
        title.push_str(&format!(" · {}×{}", pic.full.0, pic.full.1));
    }
    let inner = window(buf, boxed, &title, &t);
    let mid = inner.y + inner.height / 2;
    if let Some(error) = &viewer.error {
        put_center(buf, inner, mid, &format!("✕ {error}"), Style::new().fg(t.err).bg(t.raised));
    } else if let Some(pic) = &viewer.pic {
        let cells = (inner.width, inner.height);
        let mut drawn = false;
        if app.sharp_pictures() {
            // real pixels through the terminal's graphics; encoding takes a
            // moment, until then (and if it fails) the blocks below stand in
            match &viewer.sharp {
                Some((for_cells, sharp)) if *for_cells == cells => {
                    let (w, h) = sharp.size();
                    let (w, h) = (w.min(inner.width), h.min(inner.height));
                    let at = Rect::new(inner.x + (inner.width - w) / 2, inner.y + (inner.height - h) / 2, w, h);
                    ratatui_image::Image::new(&sharp.proto).render(at, buf);
                    app.ui.fragile_now |= sharp.fragile();
                    drawn = true;
                }
                _ if viewer.sharp_asked != Some(cells) => {
                    viewer.sharp_asked = Some(cells);
                    app.ui.want_sharp = Some((viewer.file.clone(), cells));
                }
                _ => {}
            }
        }
        if !drawn {
            // fit to the window once and keep it until the window size changes
            let stale = viewer.fitted.as_ref().is_none_or(|(w, h, _)| (*w, *h) != cells);
            if stale {
                viewer.fitted = Some((inner.width, inner.height, pic.fit(inner.width, inner.height)));
            }
            if let Some((_, _, thumb)) = &viewer.fitted {
                paint_thumb(buf, inner, thumb);
            }
        }
    } else {
        put_center(buf, inner, mid, &format!("{} loading the picture…", spinner(app.frame)), Style::new().fg(t.dim).bg(t.raised));
    }
    let images = app.images();
    let position = images
        .iter()
        .position(|f| f.id == viewer.file.id)
        .filter(|_| images.len() > 1)
        .map(|i| format!("{} of {} · ←→ — flip · ", i + 1, images.len()))
        .unwrap_or_default();
    footer(buf, boxed, &format!("{position}o — open the original · s — save · Esc — close"), &t);
}

fn users(buf: &mut Buffer, area: Rect, app: &mut App, sel: usize) {
    let t = app.theme.clone();
    let others: Vec<(String, bool)> = app.others().iter().map(|u| (u.name.clone(), u.voice.is_some())).collect();
    let rows = others.len().clamp(1, area.height.saturating_sub(8) as usize) as u16;
    let boxed = centered(area, area.width.saturating_sub(4).min(62), rows + 5);
    let inner = window(buf, boxed, "Peer volumes", &t);
    put(buf, inner.x, inner.y, "only for you — everyone else hears them as usual", Style::new().fg(t.faint).bg(t.raised), inner.width);
    app.ui.list_rows = rows;
    if others.is_empty() {
        put(buf, inner.x, inner.y + 2, "nobody else is online yet", Style::new().fg(t.dim).bg(t.raised), inner.width);
    }
    let sel = sel.min(others.len().saturating_sub(1));
    let top = sel.saturating_sub(rows as usize - 1);
    for (row, (name, in_voice)) in others.iter().skip(top).take(rows as usize).enumerate() {
        let y = inner.y + 2 + row as u16;
        let selected = top + row == sel;
        let bg = if selected { t.sel } else { t.raised };
        fill(buf, Rect::new(boxed.x + 1, y, boxed.width - 2, 1), Style::new().bg(bg));
        let muted = app.settings.is_muted(name);
        let volume = app.settings.volumes.get(name).copied().unwrap_or(100);
        put(buf, inner.x, y, "●", Style::new().fg(t.nick(name)).bg(bg), 1);
        let name_w = (inner.width as usize / 3).max(8);
        let mut style = Style::new().fg(if muted { t.faint } else { t.text }).bg(bg);
        if selected {
            style = style.add_modifier(Modifier::BOLD);
        }
        put(buf, inner.x + 2, y, &clip(name, name_w), style, name_w as u16);
        let label = if muted { "muted".to_string() } else { format!("{volume}%") };
        let right = put_right(buf, inner.right(), y, &label, Style::new().fg(if muted { t.err } else { t.dim }).bg(bg), inner.x + 10);
        let bar_x = inner.x + 3 + name_w as u16;
        let bar_w = right.saturating_sub(bar_x + 2);
        let color = if muted { t.faint } else if *in_voice { t.accent } else { t.dim };
        meter(buf, bar_x, y, bar_w, volume as f32 / 200.0, Style::new().fg(color).bg(bg), Style::new().fg(t.line).bg(bg));
    }
    footer(buf, boxed, "←→ — volume · Enter — mute · 0 — reset · Esc", &t);
}

fn react(buf: &mut Buffer, area: Rect, app: &mut App, sel: usize) {
    let t = app.theme.clone();
    let cols = crate::app::overlay::REACT_COLS;
    let grid_rows = QUICK.len().div_ceil(cols) as u16;
    let boxed = centered(area, cols as u16 * 5 + 4, grid_rows * 2 + 3);
    let inner = window(buf, boxed, "Reaction", &t);
    for (i, emoji) in QUICK.iter().enumerate() {
        let (col, row) = ((i % cols) as u16, (i / cols) as u16);
        let (x, y) = (inner.x + col * 5, inner.y + row * 2);
        let selected = i == sel % QUICK.len();
        let bg = if selected { t.accent } else { t.raised };
        fill(buf, Rect::new(x, y, 4, 1), Style::new().bg(bg));
        put(buf, x + 1, y, emoji, Style::new().bg(bg), 2);
    }
    footer(buf, boxed, "←→↑↓ · Enter", &t);
}

fn confirm_window(buf: &mut Buffer, area: Rect, app: &mut App, confirm: &Confirm) {
    let t = app.theme.clone();
    let w = area.width.saturating_sub(4).min(60);
    let lines = wrap(&[Span::styled(confirm.text.clone(), Style::new().fg(t.dim))], w as usize - 4);
    let boxed = centered(area, w, lines.len().min(4) as u16 + 5);
    let inner = window(buf, boxed, &confirm.title, &t);
    for (i, line) in lines.iter().take(4).enumerate() {
        put_line(buf, inner.x, inner.y + i as u16, line, inner.width);
    }
    let y = inner.bottom() - 1;
    let x = put(buf, inner.x, y, " Enter — yes ", Style::new().fg(t.on_accent).bg(t.err).add_modifier(Modifier::BOLD), inner.width);
    put(buf, x + 2, y, " Esc — cancel ", Style::new().fg(t.text).bg(t.sel), inner.right().saturating_sub(x + 2));
}

fn prompt_window(buf: &mut Buffer, area: Rect, app: &mut App, prompt: &Prompt) -> (u16, u16) {
    let t = app.theme.clone();
    let boxed = centered(area, area.width.saturating_sub(4).min(60), 6);
    let inner = window(buf, boxed, &prompt.title, &t);
    put(buf, inner.x, inner.y, &clip(&prompt.hint, inner.width as usize), Style::new().fg(t.faint).bg(t.raised), inner.width);
    let layout = prompt.input.layout(usize::MAX / 4, false);
    let line = &layout.lines[0];
    let room = inner.width.saturating_sub(3) as usize;
    // show a long value from its tail — that is where the cursor is
    let shown = if width(line) > room { clip_left(line, room) } else { line.clone() };
    put(buf, inner.x, inner.y + 2, "› ", Style::new().fg(t.accent).bg(t.raised).add_modifier(Modifier::BOLD), 2);
    put(buf, inner.x + 2, inner.y + 2, &shown, Style::new().fg(t.text).bg(t.raised), room as u16);
    footer(buf, boxed, "Enter — done · Esc — cancel", &t);
    let cursor_x = if width(line) > room { width(&shown) } else { layout.cursor.0 as usize };
    (inner.x + 2 + cursor_x as u16, inner.y + 2)
}
