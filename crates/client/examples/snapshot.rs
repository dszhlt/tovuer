//! A snapshot of the interface without a network or a terminal.
//!
//! ```text
//! cargo run -p tovuer --example snapshot -- <scene> [theme] [width] [height] [file.html]
//! ```
//! Scenes: main, voice, select, login, palette, help, settings, style, users,
//! react, picker, viewer, typing, toast. Without a file name it prints the screen as text.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use tovuer::app::Launch;
use tovuer::{demo, ui};
use unicode_width::UnicodeWidthStr;

/// "Default terminal" colors for the snapshot.
const DEFAULT_FG: &str = "#d8dae4";
const DEFAULT_BG: &str = "#0c0d12";
const ANSI: [&str; 16] = [
    "#1c1e26", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#565f89", "#ff899d", "#b9f27c", "#ffc777",
    "#8db0ff", "#c7a9ff", "#a4daff", "#e6e8ef",
];

fn css(color: Color, default: &str) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) if (i as usize) < ANSI.len() => ANSI[i as usize].to_string(),
        _ => default.to_string(),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn html(buf: &Buffer, cursor: Option<(u16, u16)>) -> String {
    let area = buf.area;
    let mut out = String::from(
        "<!doctype html><meta charset=utf-8><style>\
         body{margin:0;background:#000}\
         .t{display:inline-block;font:15px/20px 'JetBrainsMono Nerd Font','JetBrains Mono','Noto Color Emoji',monospace;white-space:pre}\
         .r{height:20px;display:flex}\
         .r span{display:inline-block;height:20px;overflow:hidden;flex:none}\
         </style><div class=t>",
    );
    for y in 0..area.height {
        out.push_str("<div class=r>");
        let mut x = 0;
        while x < area.width {
            let cell = &buf[(x, y)];
            let symbol = cell.symbol();
            let w = UnicodeWidthStr::width(symbol).clamp(1, 2) as u16;
            let mut fg = css(cell.fg, DEFAULT_FG);
            let mut bg = css(cell.bg, DEFAULT_BG);
            if cursor == Some((x, y)) || cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut style = format!("width:{w}ch;color:{fg};background:{bg}");
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str(";font-weight:700");
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                style.push_str(";font-style:italic");
            }
            let mut deco = Vec::new();
            if cell.modifier.contains(Modifier::UNDERLINED) {
                deco.push("underline");
            }
            if cell.modifier.contains(Modifier::CROSSED_OUT) {
                deco.push("line-through");
            }
            if !deco.is_empty() {
                style.push_str(&format!(";text-decoration:{}", deco.join(" ")));
            }
            out.push_str(&format!("<span style=\"{style}\">{}</span>", escape(symbol)));
            x += w;
        }
        out.push_str("</div>");
    }
    out.push_str("</div>");
    out
}

fn plain(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut x = 0;
        let mut line = String::new();
        while x < area.width {
            let symbol = buf[(x, y)].symbol();
            line.push_str(symbol);
            x += UnicodeWidthStr::width(symbol).clamp(1, 2) as u16;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize, default: &str| args.get(i).cloned().unwrap_or_else(|| default.to_string());
    let scene = arg(0, "main");
    let theme = arg(1, "tovuer");
    let width: u16 = arg(2, "120").parse().unwrap_or(120);
    let height: u16 = arg(3, "34").parse().unwrap_or(34);

    let dir = std::env::temp_dir().join(format!("tovuer-snapshot-{}", std::process::id()));
    let launch = Launch { version: tovuer::VERSION.into(), room: "home".into(), ..Launch::default() };
    let (mut app, _sent, _tasks) = demo::blank(&dir, launch);
    app.settings.theme = theme.trim_end_matches("+clear").to_string();
    app.settings.transparent = theme.ends_with("+clear");
    app.refresh_theme();
    demo::scene(&mut app, &scene);

    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    // two passes: the first learns sizes (feed height and so on), the second draws clean
    ui::render(&mut buf, area, &mut app);
    buf.reset();
    let cursor = ui::render(&mut buf, area, &mut app);

    match args.get(4) {
        Some(path) => {
            std::fs::write(path, html(&buf, cursor)).expect("cannot write the file");
            eprintln!("{path}: {width}x{height}, scene {scene}, theme {theme}");
        }
        None => print!("{}", plain(&buf)),
    }
    let _ = std::fs::remove_dir_all(dir);
}
