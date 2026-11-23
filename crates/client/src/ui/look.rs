//! Message styles on screen: the author's color or gradient, kept readable
//! on the reader's theme, and gradients that flow along the text.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use tovuer_proto::{TextStyle, parse_color};

use super::theme::Theme;

/// How a styled text is drawn: the base style for the markup parser and,
/// for a gradient, how to recolor it afterwards.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub base: Style,
    pub paint: Option<Paint>,
}

/// A gradient over text drawn in the `marker` color.
#[derive(Debug, Clone, PartialEq)]
pub struct Paint {
    pub stops: Vec<[u8; 3]>,
    pub flow: bool,
    /// The plain text of the message is drawn in this color first; only
    /// those cells are recolored (links, mentions and code keep theirs).
    pub marker: Color,
}

/// One full run of a flowing gradient takes this many frames (80 ms each).
const FLOW_FRAMES: u64 = 32;
/// A flowing gradient repeats every this many cells at most.
const FLOW_SPAN: f32 = 24.0;

/// How `style` shows on theme `t`. `motion: false` — gradients stand still.
pub fn look(style: Option<&TextStyle>, t: &Theme, motion: bool) -> Look {
    let mut base = Style::new().fg(t.text);
    let Some(style) = style else { return Look { base, paint: None } };
    if style.code {
        base = base.fg(t.warn).bg(t.raised);
    }
    for (on, modifier) in [(style.bold, Modifier::BOLD), (style.italic, Modifier::ITALIC), (style.strike, Modifier::CROSSED_OUT)] {
        if on {
            base = base.add_modifier(modifier);
        }
    }
    let stops: Vec<[u8; 3]> = style.gradient.iter().filter_map(|c| parse_color(c)).map(|c| readable(t, c)).collect();
    if stops.len() >= 2 {
        let marker = rgb(stops[0]);
        return Look { base: base.fg(marker), paint: Some(Paint { stops, flow: style.flow && motion, marker }) };
    }
    if let Some(color) = style.color.as_deref().and_then(parse_color) {
        base = base.fg(rgb(readable(t, color)));
    }
    Look { base, paint: None }
}

fn rgb(c: [u8; 3]) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}

fn as_rgb(c: Color) -> Option<[u8; 3]> {
    match c {
        Color::Rgb(r, g, b) => Some([r, g, b]),
        _ => None,
    }
}

fn luminance(c: [u8; 3]) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn contrast(a: [u8; 3], b: [u8; 3]) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn mix(a: [u8; 3], b: [u8; 3], k: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * k).round() as u8)
}

/// Nudge a color picked by someone else toward the theme's text color until
/// it reads on the background (dark blue on a dark theme, white on a light one).
pub fn readable(t: &Theme, c: [u8; 3]) -> [u8; 3] {
    // the terminal's own or a transparent background: its color is unknown
    let Some(bg) = as_rgb(t.bg) else { return c };
    const ENOUGH: f32 = 3.0;
    if contrast(c, bg) >= ENOUGH {
        return c;
    }
    let toward = as_rgb(t.text).unwrap_or(if luminance(bg) < 0.5 { [255; 3] } else { [0; 3] });
    (1..=10).map(|k| mix(c, toward, k as f32 / 10.0)).find(|m| contrast(*m, bg) >= ENOUGH).unwrap_or(toward)
}

/// The gradient's color at `pos` (0..1). A flowing one wraps around to its
/// first color, so it can run forever without a seam.
pub fn sample(stops: &[[u8; 3]], pos: f32, cyclic: bool) -> [u8; 3] {
    match stops.len() {
        0 => [255; 3],
        1 => stops[0],
        n => {
            let segments = if cyclic { n } else { n - 1 };
            let pos = if cyclic { pos.rem_euclid(1.0) } else { pos.clamp(0.0, 1.0) };
            let at = pos * segments as f32;
            let i = (at.floor() as usize).min(segments - 1);
            mix(stops[i], stops[(i + 1) % n], at - i as f32)
        }
    }
}

/// Where a flowing gradient is at this animation frame (0..1).
pub fn phase(frame: u64) -> f32 {
    (frame % FLOW_FRAMES) as f32 / FLOW_FRAMES as f32
}

/// Recolor one drawn row of styled text, `row` (one line high).
/// `line` — the row's number within the text (a flowing gradient shifts a
/// little per row, like a wave); `widest` — the widest row, a still gradient
/// stretches over it once.
pub fn paint_row(buf: &mut Buffer, row: Rect, paint: &Paint, line: usize, widest: usize, frame: u64) {
    for col in 0..row.width {
        let Some(cell) = buf.cell_mut((row.x + col, row.y)) else { break };
        if cell.fg != paint.marker {
            continue;
        }
        let pos = if paint.flow {
            col as f32 / (widest as f32).clamp(8.0, FLOW_SPAN) + line as f32 * 0.08 - phase(frame)
        } else {
            (col as f32 / (widest.max(2) - 1) as f32).min(1.0)
        };
        cell.set_fg(rgb(sample(&paint.stops, pos, paint.flow)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        Theme::by_name("tovuer")
    }

    #[test]
    fn unreadable_colors_are_nudged_toward_the_text() {
        let t = dark();
        let navy = [10, 10, 60];
        let fixed = readable(&t, navy);
        assert_ne!(fixed, navy);
        assert!(contrast(fixed, as_rgb(t.bg).unwrap()) >= 3.0);
        let bright = [255, 209, 102];
        assert_eq!(readable(&t, bright), bright, "a readable color stays as picked");
        // on the terminal's own background nobody knows the color: left alone
        assert_eq!(readable(&Theme::by_name("terminal"), navy), navy);
    }

    #[test]
    fn gradients_blend_and_wrap() {
        let stops = [[255, 0, 0], [0, 0, 255]];
        assert_eq!(sample(&stops, 0.0, false), [255, 0, 0]);
        assert_eq!(sample(&stops, 1.0, false), [0, 0, 255]);
        assert_eq!(sample(&stops, 0.5, false), [128, 0, 128]);
        // cyclic: the middle is the second color, the end comes back to the first
        assert_eq!(sample(&stops, 0.5, true), [0, 0, 255]);
        assert!(sample(&stops, 0.999, true)[0] >= 250, "almost back to the first color");
    }

    #[test]
    fn looks_follow_the_style() {
        let t = dark();
        assert_eq!(look(None, &t, true), Look { base: Style::new().fg(t.text), paint: None });
        let bold_code = TextStyle { bold: true, code: true, ..TextStyle::default() };
        let l = look(Some(&bold_code), &t, true);
        assert!(l.base.add_modifier.contains(Modifier::BOLD) && l.base.bg == Some(t.raised));
        let flowing = TextStyle { gradient: vec!["#ff0000".into(), "#00ff00".into()], flow: true, ..TextStyle::default() };
        assert!(look(Some(&flowing), &t, true).paint.unwrap().flow);
        assert!(!look(Some(&flowing), &t, false).paint.unwrap().flow, "the reader can stop the motion");
    }

    #[test]
    fn a_flowing_row_changes_from_frame_to_frame() {
        let t = dark();
        let style = TextStyle { gradient: vec!["#ff0000".into(), "#0000ff".into()], flow: true, ..TextStyle::default() };
        let Look { base, paint } = look(Some(&style), &t, true);
        let paint = paint.unwrap();
        let draw = |frame| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
            buf.set_string(0, 0, "a gradient that runs", base);
            buf.set_string(18, 0, "!!", Style::new().fg(t.info));
            paint_row(&mut buf, Rect::new(0, 0, 20, 1), &paint, 0, 20, frame);
            buf
        };
        let (a, b) = (draw(0), draw(5));
        assert_ne!(a[(0, 0)].fg, a[(10, 0)].fg, "colors change along the text");
        assert_ne!(a[(3, 0)].fg, b[(3, 0)].fg, "and with time");
        assert_eq!(a[(18, 0)].fg, t.info, "text in another color (a link) keeps it");
    }
}
