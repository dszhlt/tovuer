//! Color themes. All colors are 24-bit; the `terminal` theme uses the
//! terminal's own palette instead (picking up its theme and transparency).

use ratatui::style::Color;

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    /// The main background (the chat feed).
    pub bg: Color,
    /// Side panels and the status bar.
    pub panel: Color,
    /// Raised elements: the input, pop-ups, "pills".
    pub raised: Color,
    /// Dividers and frames.
    pub line: Color,
    pub text: Color,
    /// Secondary text.
    pub dim: Color,
    /// Faint text: times, hints.
    pub faint: Color,
    pub accent: Color,
    /// Text on the accent background.
    pub on_accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    pub info: Color,
    /// Background of the selected row.
    pub sel: Color,
    /// Background of a message that mentions you.
    pub mention: Color,
    /// Nick colors.
    pub nicks: [Color; 8],
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// A palette in one line: background, panel, raised, line, text, secondary,
/// faint, accent, text on accent, ok, warning, error, info, selection, mention.
const fn theme(name: &'static str, c: [u32; 15], nicks: [u32; 8]) -> Theme {
    Theme {
        name,
        bg: rgb(c[0]),
        panel: rgb(c[1]),
        raised: rgb(c[2]),
        line: rgb(c[3]),
        text: rgb(c[4]),
        dim: rgb(c[5]),
        faint: rgb(c[6]),
        accent: rgb(c[7]),
        on_accent: rgb(c[8]),
        ok: rgb(c[9]),
        warn: rgb(c[10]),
        err: rgb(c[11]),
        info: rgb(c[12]),
        sel: rgb(c[13]),
        mention: rgb(c[14]),
        nicks: [
            rgb(nicks[0]),
            rgb(nicks[1]),
            rgb(nicks[2]),
            rgb(nicks[3]),
            rgb(nicks[4]),
            rgb(nicks[5]),
            rgb(nicks[6]),
            rgb(nicks[7]),
        ],
    }
}

const THEMES: &[Theme] = &[
    theme(
        "tovuer",
        [
            0x0f1117, 0x0b0d12, 0x1a1d27, 0x2a2f40, 0xe6e8ef, 0x9aa0b4, 0x5c6274, 0x8b7cf6, 0x0f1117, 0x4ade80,
            0xfbbf24, 0xf87171, 0x38bdf8, 0x232740, 0x2c2545,
        ],
        [0xf472b6, 0xfb923c, 0xfacc15, 0x4ade80, 0x2dd4bf, 0x38bdf8, 0x818cf8, 0xc084fc],
    ),
    theme(
        "tokyo-night",
        [
            0x1a1b26, 0x16161e, 0x24283b, 0x2f3549, 0xc0caf5, 0xa9b1d6, 0x565f89, 0x7aa2f7, 0x1a1b26, 0x9ece6a,
            0xe0af68, 0xf7768e, 0x7dcfff, 0x283457, 0x2d2a4a,
        ],
        [0xf7768e, 0xff9e64, 0xe0af68, 0x9ece6a, 0x73daca, 0x7dcfff, 0x7aa2f7, 0xbb9af7],
    ),
    theme(
        "catppuccin",
        [
            0x1e1e2e, 0x181825, 0x313244, 0x45475a, 0xcdd6f4, 0xa6adc8, 0x6c7086, 0xcba6f7, 0x1e1e2e, 0xa6e3a1,
            0xf9e2af, 0xf38ba8, 0x89dceb, 0x3a3c51, 0x3b3050,
        ],
        [0xf38ba8, 0xfab387, 0xf9e2af, 0xa6e3a1, 0x94e2d5, 0x89dceb, 0x89b4fa, 0xcba6f7],
    ),
    theme(
        "dracula",
        [
            0x282a36, 0x21222c, 0x343746, 0x44475a, 0xf8f8f2, 0xbfc2d4, 0x6272a4, 0xbd93f9, 0x282a36, 0x50fa7b,
            0xf1fa8c, 0xff5555, 0x8be9fd, 0x44475a, 0x44355a,
        ],
        [0xff79c6, 0xffb86c, 0xf1fa8c, 0x50fa7b, 0x8be9fd, 0x9aedfe, 0xbd93f9, 0xff92df],
    ),
    theme(
        "gruvbox",
        [
            0x282828, 0x1d2021, 0x3c3836, 0x504945, 0xebdbb2, 0xbdae93, 0x7c6f64, 0xfe8019, 0x1d2021, 0xb8bb26,
            0xfabd2f, 0xfb4934, 0x83a598, 0x504945, 0x4a3a28,
        ],
        [0xfb4934, 0xfe8019, 0xfabd2f, 0xb8bb26, 0x8ec07c, 0x83a598, 0xd3869b, 0xd65d0e],
    ),
    theme(
        "nord",
        [
            0x2e3440, 0x272c36, 0x3b4252, 0x434c5e, 0xeceff4, 0xc5cddb, 0x6f7d97, 0x88c0d0, 0x2e3440, 0xa3be8c,
            0xebcb8b, 0xbf616a, 0x81a1c1, 0x434c5e, 0x3f4a63,
        ],
        [0xbf616a, 0xd08770, 0xebcb8b, 0xa3be8c, 0x8fbcbb, 0x88c0d0, 0x81a1c1, 0xb48ead],
    ),
    theme(
        "rose-pine",
        [
            0x191724, 0x1f1d2e, 0x26233a, 0x403d52, 0xe0def4, 0x908caa, 0x6e6a86, 0xc4a7e7, 0x191724, 0x9ccfd8,
            0xf6c177, 0xeb6f92, 0x3e8fb0, 0x2a273f, 0x33284a,
        ],
        [0xeb6f92, 0xf6c177, 0xebbcba, 0x9ccfd8, 0x3e8fb0, 0xc4a7e7, 0xea9a97, 0x56949f],
    ),
    theme(
        "everforest",
        [
            0x2d353b, 0x272e33, 0x343f44, 0x475258, 0xd3c6aa, 0x9da9a0, 0x7a8478, 0xa7c080, 0x2d353b, 0xa7c080,
            0xdbbc7f, 0xe67e80, 0x7fbbb3, 0x3d484d, 0x45443c,
        ],
        [0xe67e80, 0xe69875, 0xdbbc7f, 0xa7c080, 0x83c092, 0x7fbbb3, 0xd699b6, 0xbdc3af],
    ),
    theme(
        "ocean",
        [
            0x0b1a26, 0x08141d, 0x122a3b, 0x1c3b50, 0xd6eef8, 0x8fb3c4, 0x4f7385, 0x22d3ee, 0x08141d, 0x34d399,
            0xfbbf24, 0xfb7185, 0x60a5fa, 0x15354a, 0x123a4a,
        ],
        [0xfb7185, 0xfdba74, 0xfde047, 0x6ee7b7, 0x5eead4, 0x67e8f9, 0x93c5fd, 0xc4b5fd],
    ),
    theme(
        "sunset",
        [
            0x1f1418, 0x190f13, 0x2e1d23, 0x44282f, 0xf5e6dc, 0xc9a99b, 0x8a6a62, 0xff8a5c, 0x1f1418, 0xa3d977,
            0xffd166, 0xef476f, 0xf4a8c4, 0x3a2229, 0x47262c,
        ],
        [0xef476f, 0xff8a5c, 0xffd166, 0xa3d977, 0x7fd1b9, 0xf4a8c4, 0xc9a0dc, 0xffb3a1],
    ),
    theme(
        "matrix",
        [
            0x020a04, 0x010703, 0x06170b, 0x0d3018, 0x6dfc8a, 0x2fbf55, 0x1a7a36, 0x00ff66, 0x020a04, 0x00ff66,
            0xc6ff00, 0xff3b3b, 0x39ffb0, 0x0a2412, 0x13300f,
        ],
        [0x00ff66, 0x7dff9b, 0x39ffb0, 0xc6ff00, 0x00e5a0, 0xaaffc3, 0x5cff5c, 0x00ffaa],
    ),
    theme(
        "mono",
        [
            0x0e0e0e, 0x0a0a0a, 0x1a1a1a, 0x2e2e2e, 0xe8e8e8, 0xa0a0a0, 0x626262, 0xffffff, 0x000000, 0xf0f0f0,
            0xc8c8c8, 0xffffff, 0xd8d8d8, 0x262626, 0x303030,
        ],
        [0xffffff, 0xd9d9d9, 0xbfbfbf, 0xf2f2f2, 0xcccccc, 0xe6e6e6, 0xb3b3b3, 0xfafafa],
    ),
    theme(
        "paper",
        [
            0xfafafa, 0xf0f0f3, 0xe6e6eb, 0xd0d0d8, 0x1f2330, 0x5a6072, 0x9096a6, 0x6d4aff, 0xffffff, 0x16a34a,
            0xb45309, 0xdc2626, 0x0284c7, 0xe0e0f5, 0xf3e8ff,
        ],
        [0xbe185d, 0xc2410c, 0xa16207, 0x15803d, 0x0f766e, 0x0369a1, 0x4338ca, 0x7e22ce],
    ),
];

/// A theme on the terminal's palette: the 16 standard colors and its own background.
const TERMINAL: Theme = Theme {
    name: "terminal",
    bg: Color::Reset,
    panel: Color::Reset,
    raised: Color::Indexed(0),
    line: Color::Indexed(8),
    text: Color::Reset,
    dim: Color::Indexed(7),
    faint: Color::Indexed(8),
    accent: Color::Indexed(13),
    on_accent: Color::Indexed(0),
    ok: Color::Indexed(10),
    warn: Color::Indexed(11),
    err: Color::Indexed(9),
    info: Color::Indexed(12),
    sel: Color::Indexed(8),
    mention: Color::Indexed(0),
    nicks: [
        Color::Indexed(9),
        Color::Indexed(11),
        Color::Indexed(10),
        Color::Indexed(14),
        Color::Indexed(12),
        Color::Indexed(13),
        Color::Indexed(3),
        Color::Indexed(6),
    ],
};

impl Theme {
    /// A theme by name. An unknown name gives the signature theme.
    pub fn by_name(name: &str) -> Theme {
        if name == TERMINAL.name {
            return TERMINAL;
        }
        THEMES.iter().find(|t| t.name == name).unwrap_or(&THEMES[0]).clone()
    }

    pub fn exists(name: &str) -> bool {
        name == TERMINAL.name || THEMES.iter().any(|t| t.name == name)
    }

    /// Names of all themes in display order.
    pub fn names() -> Vec<&'static str> {
        THEMES.iter().map(|t| t.name).chain([TERMINAL.name]).collect()
    }

    /// The next one, wrapping around.
    pub fn next_after(name: &str) -> &'static str {
        let names = Self::names();
        let at = names.iter().position(|n| *n == name).unwrap_or(names.len() - 1);
        names[(at + 1) % names.len()]
    }

    /// Do not paint the main background and panels — the terminal's shows through.
    pub fn transparent(mut self) -> Theme {
        self.bg = Color::Reset;
        self.panel = Color::Reset;
        self
    }

    /// Panels blend into the background — so lines are needed between them.
    pub fn flat(&self) -> bool {
        self.bg == self.panel
    }

    /// A stable nick color: the same person is always the same color.
    pub fn nick(&self, name: &str) -> Color {
        let mut hash: u32 = 2166136261;
        for b in name.to_lowercase().bytes() {
            hash = (hash ^ b as u32).wrapping_mul(16777619);
        }
        self.nicks[(hash % self.nicks.len() as u32) as usize]
    }

    /// A color on the quiet → loud scale for level meters.
    pub fn level(&self, level: f32) -> Color {
        if level > 0.85 {
            self.err
        } else if level > 0.6 {
            self.warn
        } else {
            self.ok
        }
    }
}

/// A smooth blend of two colors (for the logo). Terminal palette colors
/// cannot be mixed — then it is just the first one.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_resolve() {
        for name in Theme::names() {
            assert!(Theme::exists(name), "{name}");
            assert_eq!(Theme::by_name(name).name, name);
        }
        assert_eq!(Theme::by_name("no such theme").name, "tovuer");
    }

    #[test]
    fn names_are_unique_and_cycle() {
        let names = Theme::names();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
        let mut seen = vec![names[0]];
        let mut cur = names[0];
        for _ in 1..names.len() {
            cur = Theme::next_after(cur);
            seen.push(cur);
        }
        assert_eq!(seen, names);
        assert_eq!(Theme::next_after(cur), names[0]);
        assert_eq!(Theme::next_after("garbage"), names[0]);
    }

    #[test]
    fn nick_colors_are_stable_and_case_insensitive() {
        let t = Theme::by_name("tovuer");
        assert_eq!(t.nick("Dana"), t.nick("dana"));
        let distinct: std::collections::HashSet<String> =
            ["dana", "bob", "kate", "oleg", "masha", "x", "yy", "zzz"].iter().map(|n| format!("{:?}", t.nick(n))).collect();
        assert!(distinct.len() >= 4, "the colors do not collapse into one");
    }

    #[test]
    fn transparency_and_flatness() {
        let solid = Theme::by_name("nord");
        assert!(!solid.flat());
        let clear = solid.transparent();
        assert_eq!((clear.bg, clear.panel), (Color::Reset, Color::Reset));
        assert!(clear.flat());
        assert!(Theme::by_name("terminal").flat());
    }

    #[test]
    fn blend_endpoints() {
        let (a, b) = (Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 50));
        assert_eq!(blend(a, b, 0.0), a);
        assert_eq!(blend(a, b, 1.0), b);
        assert_eq!(blend(a, b, 0.5), Color::Rgb(100, 50, 25));
        assert_eq!(blend(Color::Reset, b, 0.5), Color::Reset);
    }
}
