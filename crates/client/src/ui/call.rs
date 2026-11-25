//! The call view: everyone in our voice channel as a tile instead of the
//! feed — their camera or screen where they show one, a badge otherwise.
//! A green frame marks who is talking.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use tovuer_proto::{User, VideoSource};

use super::icons::Icon;
use super::{Hit, Spot, SpotImage, cells_hash, fill, frame, put, put_center, put_right, spinner, text};
use crate::app::{App, Focus};
use crate::video::{VideoFrame, fit};

/// A frame scaled for one tile in blocks, kept while neither changes.
pub struct Scaled {
    seq: u32,
    size: (u32, u32),
    mirror: bool,
    pixels: image::RgbImage,
}

/// How much wider than tall (in cells) a tile is made: a cell is about
/// twice as tall as wide, and video is 4:3 or 16:9.
const TILE_ASPECT: u16 = 3;
/// The frame and the name line.
const TILE_CHROME: u16 = 3;

/// The best grid for `n` tiles in `area`: (columns, rows).
pub fn grid(n: usize, area: Rect) -> (u16, u16) {
    let n = n.max(1) as u16;
    let mut best = (1, n, 0u32);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let (w, h) = (area.width / cols, area.height / rows);
        // the video inside: as wide as the tile, or as its height allows
        let video_h = h.saturating_sub(TILE_CHROME);
        let video_w = w.saturating_sub(2).min(video_h * TILE_ASPECT);
        let size = video_w as u32 * (video_w as u32 / TILE_ASPECT as u32);
        if size > best.2 {
            best = (cols, rows, size);
        }
    }
    (best.0, best.1)
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App) {
    let members = app.call_members();
    if members.is_empty() || area.width < 10 || area.height < 4 {
        return;
    }
    let frames = &app.call.frames;
    app.ui.video.retain(|id, _| frames.contains_key(id));
    let picking = app.focus == Focus::Messages;
    let area = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
    let spotlit = app.call.focus.and_then(|id| members.iter().position(|u| u.id == id));
    if let Some(i) = spotlit {
        // one big, the others named on a line below
        let strip = u16::from(members.len() > 1 && area.height > 10);
        tile(buf, Rect::new(area.x, area.y, area.width, area.height - strip), app, &members[i], picking && app.call.sel == i);
        if strip == 1 {
            let others: Vec<&str> = members.iter().filter(|u| u.id != members[i].id).map(|u| u.name.as_str()).collect();
            let line = format!("also here: {} · Enter — everyone", others.join(" · "));
            let style = Style::new().fg(app.theme.faint).bg(app.theme.bg);
            put_center(buf, area, area.bottom() - 1, &line, style);
        }
        return;
    }

    let (cols, rows) = grid(members.len(), area);
    let (cell_w, cell_h) = (area.width / cols, area.height / rows);
    // tiles keep a video-like shape; the grid sits in the middle
    let tile_w = cell_w.min((cell_h.saturating_sub(TILE_CHROME)) * TILE_ASPECT + 2).max(cell_w.min(12));
    let tile_h = cell_h.min((tile_w.saturating_sub(2)) / TILE_ASPECT + TILE_CHROME).max(cell_h.min(TILE_CHROME + 1));
    let top = area.y + (area.height - tile_h * rows) / 2;
    for (i, user) in members.iter().enumerate() {
        let (row, col) = (i as u16 / cols, i as u16 % cols);
        // the last row may be short: centre it
        let in_row = (members.len() as u16 - row * cols).min(cols);
        let left = area.x + (area.width - tile_w * in_row) / 2;
        let r = Rect::new(left + col * tile_w, top + row * tile_h, tile_w, tile_h);
        tile(buf, r, app, user, picking && app.call.sel == i);
    }
}

fn tile(buf: &mut Buffer, r: Rect, app: &mut App, user: &User, selected: bool) {
    let t = app.theme.clone();
    if r.width < 4 || r.height < 3 {
        return;
    }
    let speaking = app.speaking.contains(&user.id) && !user.muted;
    let border = if speaking {
        Style::new().fg(t.ok).add_modifier(Modifier::BOLD)
    } else if selected {
        Style::new().fg(t.accent)
    } else {
        Style::new().fg(t.line)
    };
    // the frame's lines run through the middle of its cells: outside them
    // is the page, inside the tile
    frame(buf, r, border.bg(t.bg), None);
    app.ui.hits.push((r, Hit::Tile(user.id)));
    let inner = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
    fill(buf, inner, Style::new().fg(t.dim).bg(t.raised));
    let screen = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(1));
    match app.call.frames.get(&user.id).cloned() {
        Some(frame) if screen.height > 0 => video(buf, screen, app, user.id, &frame),
        _ => badge(buf, screen, app, user, speaking),
    }
    label(buf, Rect::new(inner.x, inner.bottom() - 1, inner.width, 1), app, user, speaking);
}

/// The name line at the bottom of a tile.
fn label(buf: &mut Buffer, r: Rect, app: &App, user: &User, speaking: bool) {
    let t = &app.theme;
    let bg = t.raised;
    let mut right = r.right();
    let mut icons: Vec<(Icon, Color)> = Vec::new();
    match user.video {
        Some(VideoSource::Camera) => icons.push((Icon::Camera, t.dim)),
        Some(VideoSource::Screen) => icons.push((Icon::Screen, t.dim)),
        None => {}
    }
    if user.deafened {
        icons.push((Icon::Deafened, t.err));
    } else if user.muted {
        icons.push((Icon::Muted, t.err));
    }
    for (icon, color) in icons {
        if right < r.x + 8 {
            break;
        }
        right = put_right(buf, right, r.y, app.icons.get(icon), Style::new().fg(color).bg(bg), r.x + 6) - 1;
    }
    let me = user.id == app.my_id();
    let name = if me { format!("{} (you)", user.name) } else { user.name.clone() };
    let dot = if speaking { "◉" } else { "●" };
    put(buf, r.x, r.y, dot, Style::new().fg(if speaking { t.ok } else { t.nick(&user.name) }).bg(bg), 1);
    let room = right.saturating_sub(r.x + 2 + 1);
    let style = Style::new().fg(if speaking { t.text } else { t.dim }).bg(bg).add_modifier(if speaking { Modifier::BOLD } else { Modifier::empty() });
    put(buf, r.x + 2, r.y, &text::clip(&name, room as usize), style, room);
}

/// No video: the first letter of the nick in a ring, like an avatar.
fn badge(buf: &mut Buffer, r: Rect, app: &App, user: &User, speaking: bool) {
    let t = &app.theme;
    if r.height == 0 || r.width < 3 {
        return;
    }
    let color = if speaking { t.ok } else { t.nick(&user.name) };
    let initial: String = user.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let mid = r.y + r.height / 2;
    if user.video.is_some() {
        // the video is on its way
        let what = if user.video == Some(VideoSource::Screen) { "screen" } else { "camera" };
        put_center(buf, r, mid, &format!("{} {what}…", spinner(app.frame)), Style::new().fg(t.faint).bg(t.raised));
        return;
    }
    if r.height >= 3 && r.width >= 7 {
        let ring = Style::new().fg(color).bg(t.raised);
        let x = r.x + (r.width - 5) / 2;
        put(buf, x, mid - 1, "╭───╮", ring, 5);
        put(buf, x, mid, "│", ring, 1);
        put(buf, x + 2, mid, &initial, ring.add_modifier(Modifier::BOLD), 1);
        put(buf, x + 4, mid, "│", ring, 1);
        put(buf, x, mid + 1, "╰───╯", ring, 5);
    } else {
        put_center(buf, r, mid, &initial, Style::new().fg(color).bg(t.raised).add_modifier(Modifier::BOLD));
    }
}

/// A frame in blocks, and real pixels on top at the end of the frame
/// where the terminal has graphics. A screen is drawn in sextants — six
/// dots a cell, so its text can be made out; a camera in half-blocks —
/// two dots a cell, each in its true color.
fn video(buf: &mut Buffer, r: Rect, app: &mut App, id: u32, frame: &VideoFrame) {
    let mirror = id == app.my_id() && frame.source == VideoSource::Camera;
    let fine = frame.source == VideoSource::Screen && fine_blocks();
    // the picture's cells, then the dots in them
    let (w, h) = fit(frame.image.width(), frame.image.height(), r.width as u32, r.height as u32 * 2);
    let (cols, rows) = (w as u16, h.div_ceil(2) as u16);
    let size = if fine { (cols as u32 * 2, rows as u32 * 3) } else { (w, h) };
    let fresh = app.ui.video.get(&id).is_some_and(|s| s.seq == frame.seq && s.size == size && s.mirror == mirror);
    if !fresh {
        let mut pixels = image::imageops::resize(&frame.image, size.0, size.1, image::imageops::FilterType::Triangle);
        if mirror {
            image::imageops::flip_horizontal_in_place(&mut pixels);
        }
        app.ui.video.insert(id, Scaled { seq: frame.seq, size, mirror, pixels });
    }
    let Some(scaled) = app.ui.video.get(&id) else { return };
    let x0 = r.x + (r.width - cols) / 2;
    let y0 = r.y + (r.height - rows) / 2;
    let backdrop = match app.theme.raised {
        Color::Rgb(r, g, b) => [r, g, b],
        _ => [24, 24, 28],
    };
    let at = |x: u32, y: u32| scaled.pixels.get_pixel_checked(x, y).map_or(backdrop, |p| p.0);
    let rgb = |c: [u8; 3]| Color::Rgb(c[0], c[1], c[2]);
    for row in 0..rows {
        for x in 0..cols {
            let Some(cell) = buf.cell_mut((x0 + x, y0 + row)) else { continue };
            let (px, py) = (x as u32, row as u32);
            if fine {
                let dots = [0, 1, 2].map(|dy| [0, 1].map(|dx| at(px * 2 + dx, py * 3 + dy)));
                let (symbol, fg, bg) = sextant([dots[0][0], dots[0][1], dots[1][0], dots[1][1], dots[2][0], dots[2][1]]);
                cell.set_char(symbol).set_fg(rgb(fg)).set_bg(rgb(bg));
            } else {
                cell.set_symbol("▀").set_fg(rgb(at(px, py * 2))).set_bg(rgb(at(px, py * 2 + 1)));
            }
        }
    }

    if app.sharp_pictures() {
        let cells = (r.width, r.height);
        let tile = app.call.sharp.get(&id);
        let ready = tile.and_then(|t| t.ready.clone()).filter(|(_, s)| s.size() == cells);
        let stale = ready.as_ref().is_none_or(|(seq, _)| *seq != frame.seq);
        if stale && !tile.is_some_and(|t| t.failed) {
            app.ui.want_sharp_video.push((id, cells));
        }
        if let Some((_, sharp)) = ready {
            app.ui.spots.push(Spot { image: SpotImage::Ready(sharp), rect: r, cells: cells_hash(buf, r) });
        }
    }
}

/// A watched screen in a red frame: who, what, where on top, the way out below.
fn live_panel(buf: &mut Buffer, panel: Rect, app: &mut App, user: &User, full: bool) {
    let t = app.theme.clone();
    if panel.width < 12 || panel.height < 4 {
        return;
    }
    frame(buf, panel, Style::new().fg(t.err).bg(t.bg), None);
    let x = put(buf, panel.x + 2, panel.y, " ● LIVE ", Style::new().fg(t.on_accent).bg(t.err).add_modifier(Modifier::BOLD), panel.width.saturating_sub(4));
    let place = user.voice.clone().unwrap_or_default();
    let title = format!(" {} · screen · {place} ", user.name);
    let room = panel.right().saturating_sub(x + 2);
    put(buf, x, panel.y, &text::clip(&title, room as usize), Style::new().fg(t.text).bg(t.bg).add_modifier(Modifier::BOLD), room);
    let hint = match (full, app.live().len() > 1) {
        (true, _) => " Esc — back to the chat · Alt+W — next / stop ",
        (false, true) => " Alt+F — full window · Alt+W — next · /watch off ",
        (false, false) => " Alt+F — full window · Alt+W — stop watching ",
    };
    // on the bottom line, if it fits there
    if text::width(hint) as u16 + 4 < panel.width {
        put_right(buf, panel.right() - 2, panel.bottom() - 1, hint, Style::new().fg(t.faint).bg(t.bg), panel.x + 2);
    }
    let inner = Rect::new(panel.x + 1, panel.y + 1, panel.width - 2, panel.height - 2);
    fill(buf, inner, Style::new().fg(t.dim).bg(t.raised));
    match app.call.frames.get(&user.id).cloned() {
        Some(picture) => video(buf, inner, app, user.id, &picture),
        None => {
            let wait = format!("{} connecting to {}'s screen…", spinner(app.frame), user.name);
            put_center(buf, inner, inner.y + inner.height / 2, &wait, Style::new().fg(t.faint).bg(t.raised));
        }
    }
    // a click makes it big, or back
    app.ui.hits.push((inner, Hit::Stream));
}

/// The watched screen over the whole window (Alt+F).
pub fn stream_full(buf: &mut Buffer, area: Rect, app: &mut App) {
    if let Some(user) = app.watched().cloned() {
        live_panel(buf, area, app, &user, true);
    }
}

/// Whether this terminal gets screens in sextants (`TOVUER_BLOCKS=half` or
/// `sextant` decides; Windows consoles may lack the characters).
fn fine_blocks() -> bool {
    static FINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FINE.get_or_init(|| match std::env::var("TOVUER_BLOCKS").as_deref() {
        Ok("half") => false,
        Ok("sextant") => true,
        _ => !cfg!(windows),
    })
}

/// Six dots (left to right, top to bottom) as one cell: a sextant
/// character and its two colors — the dots split into the two groups
/// that differ most.
pub fn sextant(dots: [[u8; 3]; 6]) -> (char, [u8; 3], [u8; 3]) {
    let luma = |p: &[u8; 3]| p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114;
    let average = |on: u8, want: bool| {
        let (mut sum, mut n) = ([0u32; 3], 0u32);
        for (i, d) in dots.iter().enumerate() {
            if (on >> i & 1 == 1) == want {
                for c in 0..3 {
                    sum[c] += d[c] as u32;
                }
                n += 1;
            }
        }
        sum.map(|v| (v / n.max(1)) as u8)
    };
    let lumas = dots.each_ref().map(luma);
    let (lo, hi) = (*lumas.iter().min().unwrap_or(&0), *lumas.iter().max().unwrap_or(&0));
    if hi - lo < 12_000 {
        let all = average(0, false);
        return ('█', all, all);
    }
    // split by brightness, then once more by which of the two colors is nearer
    let mid = (lo + hi) / 2;
    let mut on = lumas.iter().enumerate().fold(0u8, |bits, (i, l)| bits | (u8::from(*l > mid) << i));
    let (fg, bg) = (average(on, true), average(on, false));
    let far = |a: &[u8; 3], b: &[u8; 3]| (0..3).map(|c| (a[c] as i32 - b[c] as i32).pow(2)).sum::<i32>();
    let regrouped = dots.iter().enumerate().fold(0u8, |bits, (i, d)| bits | (u8::from(far(d, &fg) < far(d, &bg)) << i));
    if regrouped != 0 && regrouped != 63 {
        on = regrouped;
    }
    (sextant_char(on), average(on, true), average(on, false))
}

/// The character with these dots lit (bit 0 — top left … bit 5 — bottom right).
fn sextant_char(on: u8) -> char {
    match on & 63 {
        0 | 63 => '█',
        21 => '▌',
        42 => '▐',
        v => char::from_u32(0x1FB00 + v as u32 - 1 - u32::from(v > 21) - u32::from(v > 42)).unwrap_or('█'),
    }
}

/// Above the feed: the screen we watch, or a line saying who is live.
/// Returns what is left for the feed.
pub fn stream(buf: &mut Buffer, area: Rect, app: &mut App) -> Rect {
    let t = app.theme.clone();
    if area.width < 20 || area.height < 8 {
        return area;
    }
    if let Some(user) = app.watched().cloned() {
        let picture = app.call.frames.get(&user.id).cloned();
        // as tall as the picture wants at this width, leaving the feed some room
        let (pw, ph) = picture.as_ref().map_or((16, 9), |f| (f.image.width(), f.image.height()));
        let inner_w = area.width.saturating_sub(4) as u32;
        let want = (inner_w * ph).div_ceil(pw.max(1) * 2) as u16 + 2;
        let h = want.min(area.height * 2 / 3).min(area.height.saturating_sub(5)).max(5);
        let panel = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), h);
        live_panel(buf, panel, app, &user, false);
        return Rect::new(area.x, panel.bottom(), area.width, area.height - h);
    }
    let live = app.live();
    if live.is_empty() {
        return area;
    }
    let names: Vec<&str> = live.iter().map(|u| u.name.as_str()).collect();
    let what = match names.as_slice() {
        [one] => format!("{one} is sharing the screen in {}", live[0].voice.as_deref().unwrap_or("voice")),
        many => format!("{} share their screens", many.join(", ")),
    };
    let row = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    fill(buf, row, Style::new().bg(t.raised));
    let x = put(buf, row.x + 1, row.y, " ● LIVE ", Style::new().fg(t.on_accent).bg(t.err).add_modifier(Modifier::BOLD), row.width);
    let right = put_right(buf, row.right() - 1, row.y, "Alt+W — watch", Style::new().fg(t.faint).bg(t.raised), x + 2);
    put(buf, x + 1, row.y, &text::clip(&what, right.saturating_sub(x + 2) as usize), Style::new().fg(t.text).bg(t.raised), right.saturating_sub(x + 2));
    app.ui.hits.push((row, Hit::Live));
    Rect::new(area.x, area.y + 1, area.width, area.height - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sextants_are_the_unicode_ones() {
        assert_eq!(sextant_char(1), '\u{1FB00}', "top left");
        assert_eq!(sextant_char(20), '\u{1FB13}', "middle left and bottom left");
        assert_eq!(sextant_char(22), '\u{1FB14}', "after the left half, which is ▌");
        assert_eq!(sextant_char(62), '\u{1FB3B}', "all but the top left");
        assert_eq!((sextant_char(21), sextant_char(42), sextant_char(63)), ('▌', '▐', '█'));
        for v in 1..63u8 {
            assert_eq!(crate::ui::text::width(&sextant_char(v).to_string()), 1, "{v} takes one cell");
        }
    }

    #[test]
    fn a_cell_of_text_keeps_its_shape_and_colors() {
        let (ink, paper) = ([230, 230, 230], [20, 20, 30]);
        // a vertical stroke on the left: the left column lit
        let (c, fg, bg) = sextant([ink, paper, ink, paper, ink, paper]);
        assert_eq!((c, fg, bg), ('▌', ink, paper));
        let (c, _, _) = sextant([ink, ink, paper, paper, paper, paper]);
        assert_eq!(c, '\u{1FB02}', "the top row");
        let (c, fg, bg) = sextant([paper; 6]);
        assert_eq!((c, fg, bg), ('█', paper, paper), "one color: a full cell");
    }

    #[test]
    fn tiles_share_the_room_sensibly() {
        let wide = Rect::new(0, 0, 120, 30);
        assert_eq!(grid(1, wide), (1, 1));
        assert_eq!(grid(2, wide), (2, 1), "two side by side in a wide window");
        assert_eq!(grid(4, wide), (2, 2));
        let tall = Rect::new(0, 0, 50, 40);
        assert_eq!(grid(2, tall), (1, 2), "one above the other in a tall one");
        let (cols, rows) = grid(7, wide);
        assert!(cols * rows >= 7);
    }
}
