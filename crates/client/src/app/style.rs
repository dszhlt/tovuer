//! Your message style: one color or a moving gradient, and markup for the
//! whole text. It travels with every message you send, so everyone sees it.

use crossterm::event::{KeyCode, KeyEvent};
use tovuer_proto::{MAX_GRADIENT_STOPS, TextStyle, color_hex, parse_color};

use super::input::Input;
use super::overlay::{Overlay, Prompt, PromptAction};
use super::{App, Tone};

/// Ready-made colors: bright enough for dark themes (light themes get them
/// darkened on screen as needed).
pub const COLORS: &[(&str, &str)] = &[
    ("red", "#ff5f6d"),
    ("orange", "#ff9f43"),
    ("gold", "#ffd166"),
    ("lime", "#a3e635"),
    ("green", "#2ed573"),
    ("teal", "#1dd1a1"),
    ("sky", "#48dbfb"),
    ("blue", "#54a0ff"),
    ("violet", "#a78bfa"),
    ("pink", "#ff6b9d"),
    ("white", "#f1f2f6"),
];

/// Ready-made gradients.
pub const GRADIENTS: &[(&str, &[&str])] = &[
    ("sunset", &["#ff512f", "#f09819", "#dd2476"]),
    ("ocean", &["#2bc0e4", "#3a7bd5", "#00d2ff"]),
    ("aurora", &["#00f260", "#0575e6", "#a855f7"]),
    ("candy", &["#ff9a9e", "#fecfef", "#a18cd1"]),
    ("fire", &["#f12711", "#f5af19", "#ffe259"]),
    ("ice", &["#e0f7ff", "#74ebd5", "#9face6"]),
    ("neon", &["#f72585", "#7209b7", "#4cc9f0"]),
    ("rainbow", &["#ff3b3b", "#ffd93b", "#3bff7a", "#3b8bff"]),
];

/// Rows of the style window, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleRow {
    Color,
    Gradient,
    Flow,
    Bold,
    Italic,
    Strike,
    Code,
    Reset,
}

impl StyleRow {
    pub const ALL: [StyleRow; 8] =
        [StyleRow::Color, StyleRow::Gradient, StyleRow::Flow, StyleRow::Bold, StyleRow::Italic, StyleRow::Strike, StyleRow::Code, StyleRow::Reset];

    pub fn label(self) -> &'static str {
        match self {
            StyleRow::Color => "Color",
            StyleRow::Gradient => "Gradient",
            StyleRow::Flow => "Gradient flows",
            StyleRow::Bold => "Bold",
            StyleRow::Italic => "Italic",
            StyleRow::Strike => "Strikethrough",
            StyleRow::Code => "Code look",
            StyleRow::Reset => "Back to plain",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            StyleRow::Color => "← → a ready color · Enter — your own (#ff8800)",
            StyleRow::Gradient => "← → a ready gradient · Enter — your own (2–4 colors); it wins over the color",
            StyleRow::Flow => "the gradient keeps running along the text, for everyone",
            StyleRow::Bold | StyleRow::Italic | StyleRow::Strike => "for the whole text of every message (**bold**, *italic*, ~~strike~~ still work inside)",
            StyleRow::Code => "every message looks like inline code",
            StyleRow::Reset => "Enter — no color, no gradient, no markup",
        }
    }
}

#[derive(Debug, Default)]
pub struct StyleView {
    pub sel: usize,
}

/// The color as a preset name, a hex value or "default".
pub fn color_name(style: &TextStyle) -> String {
    match style.color.as_deref().and_then(parse_color) {
        None => "default".into(),
        Some(c) => COLORS.iter().find(|(_, hex)| parse_color(hex) == Some(c)).map_or_else(|| color_hex(c), |(name, _)| name.to_string()),
    }
}

/// The gradient as a preset name, its colors, or "off".
pub fn gradient_name(style: &TextStyle) -> String {
    if style.gradient.len() < 2 {
        return "off".into();
    }
    let mine: Vec<Option<[u8; 3]>> = style.gradient.iter().map(|c| parse_color(c)).collect();
    GRADIENTS
        .iter()
        .find(|(_, stops)| stops.iter().map(|c| parse_color(c)).collect::<Vec<_>>() == mine)
        .map_or_else(|| style.gradient.join(" "), |(name, _)| name.to_string())
}

/// One line about a style: "sunset gradient, flowing, bold".
pub fn describe(style: &TextStyle) -> String {
    let Some(style) = style.clean() else { return "plain".into() };
    let mut parts = Vec::new();
    if !style.gradient.is_empty() {
        parts.push(format!("{} gradient", gradient_name(&style)));
        if style.flow {
            parts.push("flowing".into());
        }
    } else if style.color.is_some() {
        parts.push(color_name(&style));
    }
    for (on, name) in [(style.bold, "bold"), (style.italic, "italic"), (style.strike, "strike"), (style.code, "code")] {
        if on {
            parts.push(name.into());
        }
    }
    parts.join(", ")
}

/// Set the color from a preset name, a hex value, or `off`. A color only
/// shows without a gradient, so the gradient goes.
pub fn set_color(style: &mut TextStyle, arg: &str) -> Result<(), String> {
    let arg = arg.trim().to_lowercase();
    if matches!(arg.as_str(), "off" | "default" | "none" | "") {
        style.color = None;
        return Ok(());
    }
    let hex = COLORS.iter().find(|(name, _)| *name == arg).map(|(_, hex)| *hex).unwrap_or(arg.as_str());
    let color = parse_color(hex).ok_or_else(|| format!("“{arg}” is not a color — a name ({}) or #rrggbb", names(COLORS.iter().map(|c| c.0))))?;
    style.color = Some(color_hex(color));
    style.gradient.clear();
    Ok(())
}

/// Set the gradient from a preset name, 2–4 colors, or `off`.
pub fn set_gradient(style: &mut TextStyle, arg: &str) -> Result<(), String> {
    let arg = arg.trim().to_lowercase();
    if matches!(arg.as_str(), "off" | "none" | "") {
        style.gradient.clear();
        return Ok(());
    }
    if let Some((_, stops)) = GRADIENTS.iter().find(|(name, _)| *name == arg) {
        style.gradient = stops.iter().map(|s| s.to_string()).collect();
        return Ok(());
    }
    let words: Vec<&str> = arg.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    let mut stops = Vec::new();
    for word in &words {
        let hex = COLORS.iter().find(|(name, _)| name == word).map(|(_, hex)| *hex).unwrap_or(word);
        match parse_color(hex) {
            Some(c) => stops.push(color_hex(c)),
            None => return Err(format!("“{word}” is not a color — a gradient is a name ({}) or 2–4 colors like #ff0080 #7928ca", names(GRADIENTS.iter().map(|g| g.0)))),
        }
    }
    if !(2..=MAX_GRADIENT_STOPS).contains(&stops.len()) {
        return Err(format!("a gradient needs 2–{MAX_GRADIENT_STOPS} colors"));
    }
    style.gradient = stops;
    Ok(())
}

fn names<'a>(list: impl Iterator<Item = &'a str>) -> String {
    list.collect::<Vec<_>>().join(", ")
}

/// Step through "default" and the ready colors (`delta` = ±1).
fn cycle_color(style: &mut TextStyle, delta: isize) {
    let choices: Vec<Option<&str>> = std::iter::once(None).chain(COLORS.iter().map(|(_, hex)| Some(*hex))).collect();
    let now = style.color.as_deref().and_then(parse_color);
    let at = choices.iter().position(|c| c.and_then(parse_color) == now).unwrap_or(0) as isize;
    let next = choices[(at + delta).rem_euclid(choices.len() as isize) as usize];
    style.color = next.map(str::to_string);
    style.gradient.clear();
}

/// Step through "off" and the ready gradients.
fn cycle_gradient(style: &mut TextStyle, delta: isize) {
    let now = gradient_name(style);
    let mut choices = vec!["off"];
    choices.extend(GRADIENTS.iter().map(|g| g.0));
    let at = choices.iter().position(|c| *c == now).unwrap_or(0) as isize;
    let next = choices[(at + delta).rem_euclid(choices.len() as isize) as usize];
    let _ = set_gradient(style, next);
}

impl App {
    pub fn open_style(&mut self) {
        self.overlay = Some(Overlay::Style(StyleView::default()));
        self.dirty = true;
    }

    /// Keys in the style window; `false` closes it.
    pub(super) fn style_key(&mut self, view: &mut StyleView, key: KeyEvent) -> bool {
        let last = StyleRow::ALL.len() - 1;
        view.sel = view.sel.min(last);
        let row = StyleRow::ALL[view.sel];
        let delta = match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::F(4) => return false,
            KeyCode::Up | KeyCode::Char('k') => {
                view.sel = view.sel.saturating_sub(1);
                return true;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                view.sel = (view.sel + 1).min(last);
                return true;
            }
            KeyCode::Left | KeyCode::Char('h') => -1,
            KeyCode::Right | KeyCode::Char('l') => 1,
            KeyCode::Enter | KeyCode::Char(' ') => 0,
            _ => return true,
        };
        let style = &mut self.settings.style;
        match (row, delta) {
            (StyleRow::Color, 0) => return self.ask_custom(PromptAction::Color, view.sel),
            (StyleRow::Gradient, 0) => return self.ask_custom(PromptAction::Gradient, view.sel),
            (StyleRow::Color, d) => cycle_color(style, d),
            (StyleRow::Gradient, d) => cycle_gradient(style, d),
            (StyleRow::Flow, _) => style.flow = !style.flow,
            (StyleRow::Bold, _) => style.bold = !style.bold,
            (StyleRow::Italic, _) => style.italic = !style.italic,
            (StyleRow::Strike, _) => style.strike = !style.strike,
            (StyleRow::Code, _) => style.code = !style.code,
            (StyleRow::Reset, 0) => *style = TextStyle { flow: true, ..TextStyle::default() },
            (StyleRow::Reset, _) => {}
        }
        self.save_settings();
        true
    }

    /// A prompt for your own color or gradient; the style window comes back after it.
    fn ask_custom(&mut self, action: PromptAction, sel: usize) -> bool {
        let (title, hint, now) = match action {
            PromptAction::Color => ("Your color", "#rrggbb, or a name: red, sky, violet…", self.settings.style.color.clone().unwrap_or_default()),
            _ => ("Your gradient", "2–4 colors: #ff0080 #7928ca #00c2ff — or a name: sunset, neon…", self.settings.style.gradient.join(" ")),
        };
        let mut input = Input::new(80);
        input.set(&now);
        self.overlay = Some(Overlay::Prompt(Prompt { title: title.into(), hint: hint.into(), input, action }));
        self.ui.style_sel = sel;
        // the prompt replaces the style window
        false
    }

    /// The answer to the custom color / gradient prompt. `false` — not valid, ask again.
    pub(super) fn apply_custom(&mut self, action: &PromptAction, value: &str) -> bool {
        let style = &mut self.settings.style;
        let result = match action {
            PromptAction::Color => set_color(style, value),
            _ => set_gradient(style, value),
        };
        if let Err(e) = result {
            self.warn(e);
            return false;
        }
        self.save_settings();
        self.overlay = Some(Overlay::Style(StyleView { sel: self.ui.style_sel }));
        true
    }

    /// `/color` and `/gradient`.
    pub(super) fn style_command(&mut self, gradient: bool, arg: &str) {
        if arg.is_empty() {
            self.open_style();
            return;
        }
        let style = &mut self.settings.style;
        let result = if gradient { set_gradient(style, arg) } else { set_color(style, arg) };
        match result {
            Ok(()) => {
                self.save_settings();
                let now = describe(&self.settings.style);
                self.toast(Tone::Ok, format!("your messages: {now}"));
            }
            Err(e) => self.warn(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_gradients_by_name_or_by_hand() {
        let mut style = TextStyle::default();
        set_color(&mut style, "Sky").unwrap();
        assert_eq!((style.color.as_deref(), color_name(&style).as_str()), (Some("#48dbfb"), "sky"));
        set_color(&mut style, "#FA0").unwrap();
        assert_eq!(color_name(&style), "#ffaa00");
        assert!(set_color(&mut style, "blurple").is_err());

        set_gradient(&mut style, "sunset").unwrap();
        assert_eq!(gradient_name(&style), "sunset");
        assert_eq!(style.color.as_deref(), Some("#ffaa00"), "the color waits under the gradient");
        set_gradient(&mut style, "#000 , pink #fff").unwrap();
        assert_eq!(style.gradient, vec!["#000000", "#ff6b9d", "#ffffff"]);
        assert!(set_gradient(&mut style, "#000").is_err(), "one color is not a gradient");
        assert!(set_gradient(&mut style, "#000 #111 #222 #333 #444").is_err());
        // picking a color drops the gradient, so the color shows
        set_color(&mut style, "red").unwrap();
        assert!(style.gradient.is_empty());
    }

    #[test]
    fn styles_read_in_one_line() {
        assert_eq!(describe(&TextStyle::default()), "plain");
        let mut style = TextStyle { flow: true, bold: true, ..TextStyle::default() };
        set_gradient(&mut style, "neon").unwrap();
        assert_eq!(describe(&style), "neon gradient, flowing, bold");
        set_color(&mut style, "teal").unwrap();
        style.italic = true;
        assert_eq!(describe(&style), "teal, bold, italic");
    }

    #[test]
    fn cycling_walks_the_presets_and_wraps() {
        let mut style = TextStyle::default();
        cycle_color(&mut style, 1);
        assert_eq!(color_name(&style), "red");
        cycle_color(&mut style, -1);
        assert_eq!(color_name(&style), "default");
        cycle_color(&mut style, -1);
        assert_eq!(color_name(&style), "white");
        cycle_gradient(&mut style, 1);
        assert_eq!(gradient_name(&style), "sunset");
        cycle_gradient(&mut style, -2);
        assert_eq!(gradient_name(&style), "rainbow");
    }
}
