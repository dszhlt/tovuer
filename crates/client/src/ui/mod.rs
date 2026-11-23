//! Drawing. The screen is rebuilt every frame from the [`App`] state; small
//! drawing primitives and the main screen layout live here too.

pub mod chat;
pub mod overlays;
pub mod sidebar;
pub mod text;
pub mod theme;

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use tovuer_proto::{ChannelKind, FileMeta};

use self::theme::Theme;
use crate::app::{App, Conn, Focus, Screen, Tone};

/// Where a mouse click landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Channel(usize),
    /// The "+" next to TEXT or VOICE.
    NewChannel(ChannelKind),
    Message(u64),
    Input,
}

/// What drawing remembers between frames and reports to the logic.
#[derive(Default)]
pub struct UiState {
    /// Height of the message area and number of lines in the feed — for scrolling.
    pub chat_rows: u16,
    pub chat_total: usize,
    /// Height of the list in the open window (for PgUp/PgDn).
    pub list_rows: u16,
    /// Laid-out messages: redone only when something changes.
    pub cache: HashMap<u64, chat::Laid>,
    generation: u64,
    pub hits: Vec<(Rect, Hit)>,
    /// Thumbnails missing while drawing — they need fetching.
    pub want_thumbs: Vec<FileMeta>,
    /// A picture the viewer wants in the terminal's own graphics, for this many cells.
    pub want_sharp: Option<(FileMeta, (u16, u16))>,
    /// A Sixel/iTerm2 picture is on screen: what was drawn over it last frame.
    /// Such a picture is not redrawn by itself, so when that changes the
    /// whole screen is repainted.
    pub fragile: Option<u64>,
    /// Repaint the whole screen after this frame.
    pub repaint: bool,
    /// A Sixel/iTerm2 picture was drawn in this frame.
    pub fragile_now: bool,
    /// The file picker's last folder.
    pub last_dir: Option<PathBuf>,
    /// The window is too narrow — the channel list is hidden.
    pub side_hidden: bool,
}

impl UiState {
    /// Drop the layout cache (the theme, width or channel list changed).
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cache.clear();
    }
}

// ------------------------------------------------------------------- primitives

/// Fill an area: erase whatever was drawn there and set the style (default
/// background and text color). Pop-ups are drawn over the finished screen,
/// so old characters really have to be erased.
pub fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    let area = area.intersection(buf.area);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(style);
            }
        }
    }
}

/// Write a string within `max` cells. Returns the x after the text.
pub fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, max: u16) -> u16 {
    if max == 0 || y >= buf.area.bottom() || x >= buf.area.right() {
        return x;
    }
    buf.set_stringn(x, y, s, max as usize, style).0
}

/// Write a string so that it ends at `right` (exclusive).
pub fn put_right(buf: &mut Buffer, right: u16, y: u16, s: &str, style: Style, min_x: u16) -> u16 {
    let w = text::width(s) as u16;
    let avail = right.saturating_sub(min_x);
    let shown = if w > avail { text::clip(s, avail as usize) } else { s.to_string() };
    let x = right.saturating_sub(text::width(&shown) as u16).max(min_x);
    put(buf, x, y, &shown, style, avail);
    x
}

pub fn put_center(buf: &mut Buffer, area: Rect, y: u16, s: &str, style: Style) {
    let shown = text::clip(s, area.width as usize);
    let x = area.x + (area.width.saturating_sub(text::width(&shown) as u16)) / 2;
    put(buf, x, y, &shown, style, area.width);
}

pub fn put_line(buf: &mut Buffer, x: u16, y: u16, line: &Line<'_>, max: u16) {
    if max > 0 && y < buf.area.bottom() && x < buf.area.right() {
        buf.set_line(x, y, line, max);
    }
}

pub fn hrule(buf: &mut Buffer, x: u16, y: u16, w: u16, style: Style) {
    put(buf, x, y, &"─".repeat(w as usize), style, w);
}

pub fn vrule(buf: &mut Buffer, x: u16, y: u16, h: u16, style: Style) {
    for row in y..y.saturating_add(h) {
        put(buf, x, row, "│", style, 1);
    }
}

/// A frame with rounded corners. `title` goes into the top line.
pub fn frame(buf: &mut Buffer, area: Rect, style: Style, title: Option<(&str, Style)>) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let (x0, y0, x1, y1) = (area.x, area.y, area.right() - 1, area.bottom() - 1);
    let bar = "─".repeat(area.width as usize - 2);
    put(buf, x0, y0, &format!("╭{bar}╮"), style, area.width);
    put(buf, x0, y1, &format!("╰{bar}╯"), style, area.width);
    for y in y0 + 1..y1 {
        put(buf, x0, y, "│", style, 1);
        put(buf, x1, y, "│", style, 1);
    }
    if let Some((title, title_style)) = title
        && area.width > 6
    {
        let shown = text::clip(title, area.width as usize - 6);
        put(buf, x0 + 2, y0, &format!(" {shown} "), title_style, area.width - 4);
    }
}

/// A 0..1 level bar: a thin line filled from the left.
pub fn meter(buf: &mut Buffer, x: u16, y: u16, w: u16, level: f32, fill_style: Style, track: Style) {
    let level = level.clamp(0.0, 1.0);
    let halves = (level * w as f32 * 2.0).round() as u16;
    for i in 0..w {
        let (symbol, style) = if halves >= (i + 1) * 2 {
            ("━", fill_style)
        } else if halves == i * 2 + 1 {
            ("╸", fill_style)
        } else {
            ("─", track)
        };
        put(buf, x + i, y, symbol, style, 1);
    }
}

/// " 42%" for transfer progress; empty while the size is unknown.
pub fn percent(done: u64, total: u64) -> String {
    (done * 100).checked_div(total).map(|p| format!(" {}%", p.min(100))).unwrap_or_default()
}

/// A spinner frame.
pub fn spinner(frame: u64) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(frame % FRAMES.len() as u64) as usize]
}

/// Dim everything under a pop-up (except pictures — they must not be grayed).
pub fn dim(buf: &mut Buffer, area: Rect, theme: &Theme) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y))
                && cell.symbol() != "▀"
            {
                cell.set_fg(theme.faint);
                cell.modifier = Modifier::empty();
                if cell.bg != theme.bg && cell.bg != theme.panel && cell.bg != Color::Reset {
                    cell.set_bg(theme.raised);
                }
            }
        }
    }
}

pub fn tone_color(theme: &Theme, tone: Tone) -> Color {
    match tone {
        Tone::Info => theme.info,
        Tone::Ok => theme.ok,
        Tone::Warn => theme.warn,
        Tone::Error => theme.err,
    }
}

/// A `w`×`h` rectangle centered in `area`.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

// ---------------------------------------------------------------------- frame

/// Draw the whole screen.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let cursor = render(f.buffer_mut(), area, app);
    if let Some(position) = cursor {
        f.set_cursor_position(position);
    }
}

/// Draw the screen into a buffer; returns where to show the cursor.
pub fn render(buf: &mut Buffer, area: Rect, app: &mut App) -> Option<(u16, u16)> {
    let t = app.theme.clone();
    fill(buf, area, Style::new().fg(t.text).bg(t.bg));
    app.ui.hits.clear();
    app.ui.fragile_now = false;
    if area.width < 40 || area.height < 12 {
        let style = Style::new().fg(t.dim);
        put_center(buf, area, area.y + area.height / 2, "the window is too small", style);
        return None;
    }
    let mut cursor = match app.screen {
        Screen::Login => overlays::login(buf, area, app),
        Screen::Main if app.me.is_none() => {
            overlays::splash(buf, area, app);
            None
        }
        Screen::Main => main_screen(buf, area, app),
    };
    if app.overlay.is_some() {
        dim(buf, area, &t);
        cursor = overlays::draw(buf, area, app);
    }
    toasts(buf, area, app);
    watch_fragile(app);
    cursor
}

/// A Sixel or iTerm2 picture lives only in the terminal: the cells under it
/// are not redrawn, so a toast that disappears from over it would stay as a
/// ghost. When what lies on top of such a picture changes, repaint everything.
fn watch_fragile(app: &mut App) {
    use std::hash::{Hash, Hasher};
    let covers = app.ui.fragile_now.then(|| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for toast in &app.toasts {
            toast.text.hash(&mut hasher);
        }
        hasher.finish()
    });
    if let (Some(now), Some(before)) = (covers, app.ui.fragile)
        && now != before
    {
        app.ui.repaint = true;
    }
    app.ui.fragile = covers;
}

/// Side panel widths for the window width: (channels, members).
pub fn side_widths(width: u16, members_on: bool) -> (u16, u16) {
    let side = match width {
        w if w >= 110 => 28,
        w if w >= 80 => 24,
        w if w >= 62 => 20,
        _ => 0,
    };
    let members = match width {
        w if !members_on || w < 100 => 0,
        w if w >= 130 => 24,
        _ => 20,
    };
    (side, members)
}

fn main_screen(buf: &mut Buffer, area: Rect, app: &mut App) -> Option<(u16, u16)> {
    let t = app.theme.clone();
    let body = Rect::new(area.x, area.y, area.width, area.height - 1);
    let (side_w, mem_w) = side_widths(area.width, app.settings.members);

    app.ui.side_hidden = side_w == 0;
    let side = Rect::new(body.x, body.y, side_w, body.height);
    let members = Rect::new(body.right() - mem_w, body.y, mem_w, body.height);
    let chat_area = Rect::new(side.right(), body.y, body.width - side_w - mem_w, body.height);

    if side_w > 0 {
        sidebar::channels(buf, side, app);
    }
    if mem_w > 0 {
        sidebar::members(buf, members, app);
    }
    let cursor = chat::draw(buf, chat_area, app);
    // on flat themes the panels share a background — separate them with lines
    if t.flat() {
        let rule = Style::new().fg(t.line);
        if side_w > 0 {
            vrule(buf, side.right() - 1, body.y, body.height, rule);
        }
        if mem_w > 0 {
            vrule(buf, members.x, body.y, body.height, rule);
        }
    }
    status_bar(buf, Rect::new(area.x, area.bottom() - 1, area.width, 1), app);
    cursor
}

fn status_bar(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    fill(buf, area, Style::new().fg(t.dim).bg(t.panel));
    let (label, hints) = match app.focus {
        Focus::Input if app.ui.side_hidden => ("INPUT", "Ctrl+K channels · F1 help"),
        Focus::Input => ("INPUT", "Enter send · Tab channels · Ctrl+K commands · Ctrl+O file · F1 help"),
        Focus::Sidebar if app.is_admin() => ("CHANNELS", "↑↓ pick · Enter open/join · n new text · v new voice · Del delete · t topic · Esc"),
        Focus::Sidebar => ("CHANNELS", "↑↓ pick · Enter open/join · n new text · v new voice · t topic · Esc to input"),
        Focus::Messages => ("SELECT", "↑↓ · r reply · e edit · d delete · a react · Enter open · o open outside · s save · y copy"),
    };
    let chip = Style::new().fg(t.on_accent).bg(t.accent).add_modifier(Modifier::BOLD);
    let mut x = put(buf, area.x, area.y, &format!(" {label} "), chip, area.width);

    // on the right: the room state; to its left, what runs in the background
    let (dot, state, color) = match &app.conn {
        Conn::Online => {
            let ping = app.latency.map(|d| format!(" · {} ms", d.as_millis())).unwrap_or_default();
            ("●", format!("online{ping}"), t.ok)
        }
        Conn::Connecting { .. } => (spinner(app.frame), "joining…".to_string(), t.warn),
        Conn::Waiting { until, .. } => {
            let left = until.saturating_duration_since(std::time::Instant::now()).as_secs() + 1;
            ("✕", format!("offline · retry in {left} s"), t.err)
        }
        Conn::Offline => ("○", "offline".to_string(), t.faint),
    };
    let right_text = format!("{dot} {state} ");
    let mut right = put_right(buf, area.right(), area.y, &right_text, Style::new().fg(color).bg(t.panel), x);

    let mut background: Vec<(String, Color)> = Vec::new();
    if let Some((got, total)) = app.updating {
        background.push((format!("↻ updating{}", percent(got, total)), t.info));
    } else if let Some(v) = &app.update_available {
        background.push((format!("↑ version {v} is out: /update"), t.info));
    }
    for tr in app.transfers.iter().take(2) {
        let arrow = if tr.upload { "↑" } else { "↓" };
        background.push((format!("{arrow} {}{}", text::clip(&tr.name, 18), percent(tr.done, tr.total)), t.info));
    }
    for (label, color) in background {
        let w = text::width(&label) as u16 + 3;
        if right.saturating_sub(w) <= x + 12 {
            break;
        }
        right = put_right(buf, right - 2, area.y, &label, Style::new().fg(color).bg(t.panel), x);
    }

    x += 1;
    let room = right.saturating_sub(x + 1);
    put(buf, x, area.y, &text::clip(hints, room as usize), Style::new().fg(t.faint).bg(t.panel), room);
}

/// Toasts in the top right corner.
fn toasts(buf: &mut Buffer, area: Rect, app: &App) {
    let t = &app.theme;
    let max_w = (area.width as usize * 2 / 3).clamp(20, 64);
    let mut y = area.y + 1;
    for toast in &app.toasts {
        let color = tone_color(t, toast.tone);
        let lines = text::wrap(&[ratatui::text::Span::raw(toast.text.clone())], max_w - 4);
        let w = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 4;
        let h = lines.len() as u16;
        if y + h >= area.bottom().saturating_sub(1) {
            break;
        }
        let x = area.right().saturating_sub(w + 1);
        let boxed = Rect::new(x, y, w, h);
        fill(buf, boxed, Style::new().fg(t.text).bg(t.raised));
        for (i, line) in lines.iter().enumerate() {
            put(buf, x, y + i as u16, "▌", Style::new().fg(color).bg(t.raised), 1);
            put_line(buf, x + 2, y + i as u16, line, w - 3);
        }
        y += h;
    }
}
