//! Color themes for the TUI.
//!
//! [`Theme`] is the seam the renderer draws with: every color it exposes has
//! been checked by [`crate::color`] against the theme's own background, so
//! `ui.rs` never has to think about contrast itself.
//!
//! Two kinds of theme exist:
//!
//! - *baked* themes ship a full palette ([`crate::color`] lifts any slot
//!   that would be too dim against the background);
//! - `terminal` (the default) asks the terminal for its own palette via
//!   OSC 10/11/4 and derives the rest.
//!
//! Selection lives in three places, all funnelling through [`Theme::of`]:
//! the `--theme` CLI flag, `theme = "..."` in the config file, and the
//! `Shift+T` theme picker inside the TUI.

use crate::color::{self, INK};
use anyhow::{Result, anyhow};
use ratatui::style::Color;
use std::io::Write;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// WCAG floors used when normalizing a palette: AAA for body text, AA for
/// accents, labels and the other semantic colors.
const TEXT_MIN: f64 = 7.0;
const BODY_MIN: f64 = 4.5;

/// Minimum contrast between the background and a selection shade; below
/// this a selected row would be invisible.
const SHADE_MIN: f64 = 1.18;

/// One selectable theme. `Terminal` follows whatever palette the terminal
/// reports for itself; the rest are baked-in palettes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeKind {
    /// Follow the terminal's own palette (queried at startup).
    #[default]
    Terminal,
    /// pg-studio's built-in dark palette; also the fallback everywhere.
    Default,
    CatppuccinMocha,
    CatppuccinLatte,
    CatppuccinFrappe,
    CatppuccinMacchiato,
    TokyoNight,
    TokyoNightStorm,
    TokyoNightLight,
    Dracula,
    GruvboxDark,
    GruvboxLight,
    Nord,
    SolarizedDark,
    SolarizedLight,
    OneDark,
    GithubDark,
    GithubLight,
    RosePine,
    RosePineMoon,
    RosePineDawn,
    EverforestDark,
    EverforestLight,
    Kanagawa,
    Monokai,
}

/// Authored palette slots, in table order: background, text, muted, dim,
/// accent, success, error, warn, info.
type HexPalette = [u32; 9];

/// Every theme in cycle order, with its id, label and authored palette.
/// This table is the single source of truth for ordering and naming;
/// [`ThemeKind::ALL`] mirrors it and the tests keep the two in sync.
struct Entry {
    kind: ThemeKind,
    id: &'static str,
    label: &'static str,
    hex: HexPalette,
}

/// Authored palette slots before contrast normalization.
#[derive(Debug, Clone, Copy)]
struct Palette {
    bg: Color,
    text: Color,
    muted: Color,
    dim: Color,
    accent: Color,
    success: Color,
    error: Color,
    warn: Color,
    info: Color,
}

impl Palette {
    /// Expands nine `0xRRGGBB` literals into concrete colors.
    fn from_hex([bg, text, muted, dim, accent, success, error, warn, info]: HexPalette) -> Self {
        let hex = |v: u32| {
            Color::Rgb(
                ((v >> 16) & 0xff) as u8,
                ((v >> 8) & 0xff) as u8,
                (v & 0xff) as u8,
            )
        };
        Self {
            bg: hex(bg),
            text: hex(text),
            muted: hex(muted),
            dim: hex(dim),
            accent: hex(accent),
            success: hex(success),
            error: hex(error),
            warn: hex(warn),
            info: hex(info),
        }
    }
}

impl ThemeKind {
    /// Every theme in cycle order, `terminal` first so an unconfigured run
    /// keeps following the terminal's palette.
    pub const ALL: [ThemeKind; 25] = [
        Self::Terminal,
        Self::Default,
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::CatppuccinFrappe,
        Self::CatppuccinMacchiato,
        Self::TokyoNight,
        Self::TokyoNightStorm,
        Self::TokyoNightLight,
        Self::Dracula,
        Self::GruvboxDark,
        Self::GruvboxLight,
        Self::Nord,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::OneDark,
        Self::GithubDark,
        Self::GithubLight,
        Self::RosePine,
        Self::RosePineMoon,
        Self::RosePineDawn,
        Self::EverforestDark,
        Self::EverforestLight,
        Self::Kanagawa,
        Self::Monokai,
    ];

    /// Config-file / `--theme` spelling.
    pub fn id(&self) -> &'static str {
        self.entry().id
    }

    /// Human label for logs and the help screen.
    pub fn label(&self) -> &'static str {
        self.entry().label
    }

    /// Next theme in the cycle list, wrapping around.
    pub fn next(&self) -> Self {
        let idx = Self::ALL.iter().position(|k| k == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    fn entry(&self) -> &'static Entry {
        THEMES
            .iter()
            .find(|t| t.kind == *self)
            .unwrap_or(&THEMES[0])
    }
}

impl FromStr for ThemeKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let normalized = normalize_name(s);
        let kind = THEMES
            .iter()
            .find(|t| t.id == normalized)
            .map(|t| t.kind)
            .or_else(|| alias(&normalized));
        match kind {
            Some(kind) => Ok(kind),
            None => Err(anyhow!(
                "unknown theme '{s}' (available: {})",
                THEMES.iter().map(|t| t.id).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

/// Lower-cases and folds separators so `Gruvbox_Dark`, `tokyo night` and
/// `catppuccin-mocha` all resolve.
fn normalize_name(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            '_' | ' ' => '-',
            other => other,
        })
        .collect()
}

/// Friendly shortcuts for the spellings people actually type.
fn alias(name: &str) -> Option<ThemeKind> {
    Some(match name {
        "catppuccin" => ThemeKind::CatppuccinMocha,
        "gruvbox" => ThemeKind::GruvboxDark,
        "solarized" => ThemeKind::SolarizedDark,
        "github" => ThemeKind::GithubDark,
        "tokyonight" => ThemeKind::TokyoNight,
        "onedark" => ThemeKind::OneDark,
        "everforest" => ThemeKind::EverforestDark,
        _ => return None,
    })
}

/// Every theme in cycle order. `terminal` has no baked palette: its colors
/// come from the terminal at runtime.
const THEMES: &[Entry] = &[
    Entry {
        kind: ThemeKind::Terminal,
        id: "terminal",
        label: "Terminal palette",
        hex: [0; 9],
    },
    Entry {
        kind: ThemeKind::Default,
        id: "default",
        label: "pg-studio Default",
        hex: [
            0x1e1e1e, 0xe5e5e5, 0x7f849c, 0x7f849c, 0x00cdcd, 0x00cd00, 0xcd0000, 0xcdcd00,
            0xcd00cd,
        ],
    },
    Entry {
        kind: ThemeKind::CatppuccinMocha,
        id: "catppuccin-mocha",
        label: "Catppuccin Mocha",
        hex: [
            0x1e1e2e, 0xcdd6f4, 0xa6adc8, 0x6c7086, 0xcba6f7, 0xa6e3a1, 0xf38ba8, 0xf9e2af,
            0x74c7ec,
        ],
    },
    Entry {
        kind: ThemeKind::CatppuccinLatte,
        id: "catppuccin-latte",
        label: "Catppuccin Latte",
        hex: [
            0xeff1f5, 0x4c4f69, 0x6c6f85, 0x9ca0b0, 0x8839ef, 0x40a02b, 0xd20f39, 0xdf8e1d,
            0x209fb5,
        ],
    },
    Entry {
        kind: ThemeKind::CatppuccinFrappe,
        id: "catppuccin-frappe",
        label: "Catppuccin Frappé",
        hex: [
            0x303446, 0xc6d0f5, 0xa5adce, 0x737994, 0xca9ee6, 0xa6d189, 0xe78284, 0xe5c890,
            0x85c1dc,
        ],
    },
    Entry {
        kind: ThemeKind::CatppuccinMacchiato,
        id: "catppuccin-macchiato",
        label: "Catppuccin Macchiato",
        hex: [
            0x24273a, 0xcad3f5, 0xa5adcb, 0x6e738d, 0xc6a0f6, 0xa6da95, 0xed8796, 0xeed49f,
            0x7dc4e4,
        ],
    },
    Entry {
        kind: ThemeKind::TokyoNight,
        id: "tokyo-night",
        label: "Tokyo Night",
        hex: [
            0x1a1b26, 0xc0caf5, 0x9aa5ce, 0x565f89, 0x7aa2f7, 0x9ece6a, 0xf7768e, 0xe0af68,
            0x7dcfff,
        ],
    },
    Entry {
        kind: ThemeKind::TokyoNightStorm,
        id: "tokyo-night-storm",
        label: "Tokyo Night Storm",
        hex: [
            0x24283b, 0xc0caf5, 0x9aa5ce, 0x565f89, 0x7aa2f7, 0x9ece6a, 0xf7768e, 0xe0af68,
            0x7dcfff,
        ],
    },
    Entry {
        kind: ThemeKind::TokyoNightLight,
        id: "tokyo-night-light",
        label: "Tokyo Night Light",
        hex: [
            0xd5d6db, 0x343b58, 0x5f6790, 0x8a8a8a, 0x34548a, 0x385f0d, 0x8c4351, 0x8f5e15,
            0x0f4b6e,
        ],
    },
    Entry {
        kind: ThemeKind::Dracula,
        id: "dracula",
        label: "Dracula",
        hex: [
            0x282a36, 0xf8f8f2, 0x8b95b8, 0x6272a4, 0xbd93f9, 0x50fa7b, 0xff5555, 0xf1fa8c,
            0x8be9fd,
        ],
    },
    Entry {
        kind: ThemeKind::GruvboxDark,
        id: "gruvbox-dark",
        label: "Gruvbox Dark",
        hex: [
            0x282828, 0xebdbb2, 0xa89984, 0x928374, 0xfe8019, 0xb8bb26, 0xfb4934, 0xfabd2d,
            0x8ec07c,
        ],
    },
    Entry {
        kind: ThemeKind::GruvboxLight,
        id: "gruvbox-light",
        label: "Gruvbox Light",
        hex: [
            0xfbf1c7, 0x3c3836, 0x7c6f64, 0x928374, 0xaf3a03, 0x79740e, 0xcc241d, 0xb57614,
            0x427b58,
        ],
    },
    Entry {
        kind: ThemeKind::Nord,
        id: "nord",
        label: "Nord",
        hex: [
            0x2e3440, 0xeceff4, 0xaab6c8, 0x616e88, 0x88c0d0, 0xa3be8c, 0xbf616a, 0xebcb8b,
            0x81a1c1,
        ],
    },
    Entry {
        kind: ThemeKind::SolarizedDark,
        id: "solarized-dark",
        label: "Solarized Dark",
        hex: [
            0x002b36, 0x93a1a1, 0x839496, 0x657b83, 0x268bd2, 0x859900, 0xdc322f, 0xb58900,
            0x2aa198,
        ],
    },
    Entry {
        kind: ThemeKind::SolarizedLight,
        id: "solarized-light",
        label: "Solarized Light",
        hex: [
            0xfdf6e3, 0x586e75, 0x839496, 0x93a1a1, 0x268bd2, 0x859900, 0xdc322f, 0xb58900,
            0x2aa198,
        ],
    },
    Entry {
        kind: ThemeKind::OneDark,
        id: "one-dark",
        label: "One Dark",
        hex: [
            0x282c34, 0xabb2bf, 0x9ba3b2, 0x5c6370, 0x61afef, 0x98c379, 0xe06c75, 0xe5c07b,
            0x56b6c2,
        ],
    },
    Entry {
        kind: ThemeKind::GithubDark,
        id: "github-dark",
        label: "GitHub Dark",
        hex: [
            0x0d1117, 0xc9d1d9, 0x8b949e, 0x6e7681, 0x58a6ff, 0x3fb950, 0xf85149, 0xd29922,
            0xbc8cff,
        ],
    },
    Entry {
        kind: ThemeKind::GithubLight,
        id: "github-light",
        label: "GitHub Light",
        hex: [
            0xffffff, 0x24292f, 0x57606a, 0x6e7781, 0x0969da, 0x1a7f37, 0xcf222e, 0x9a6700,
            0x8250df,
        ],
    },
    Entry {
        kind: ThemeKind::RosePine,
        id: "rose-pine",
        label: "Rosé Pine",
        hex: [
            0x191724, 0xe0def4, 0x908caa, 0x6e6a86, 0xc4a7e7, 0x9ccfd8, 0xeb6f92, 0xf6c177,
            0xebbcba,
        ],
    },
    Entry {
        kind: ThemeKind::RosePineMoon,
        id: "rose-pine-moon",
        label: "Rosé Pine Moon",
        hex: [
            0x232136, 0xe0def4, 0x908caa, 0x6e6a86, 0xc4a7e7, 0x9ccfd8, 0xeb6f92, 0xf6c177,
            0xea9a97,
        ],
    },
    Entry {
        kind: ThemeKind::RosePineDawn,
        id: "rose-pine-dawn",
        label: "Rosé Pine Dawn",
        hex: [
            0xfaf4ed, 0x575279, 0x797593, 0xa3a1b5, 0x907aa9, 0x286983, 0xb4637a, 0xea9d34,
            0x56949f,
        ],
    },
    Entry {
        kind: ThemeKind::EverforestDark,
        id: "everforest-dark",
        label: "Everforest Dark",
        hex: [
            0x2b3339, 0xd3c6aa, 0x9da9a0, 0x859289, 0x83c092, 0xa7c080, 0xe67e80, 0xdbbc7f,
            0x7fbbb3,
        ],
    },
    Entry {
        kind: ThemeKind::EverforestLight,
        id: "everforest-light",
        label: "Everforest Light",
        hex: [
            0xfdf6e3, 0x5c6a72, 0x6e7b73, 0x939f91, 0x3a94c5, 0x8da101, 0xf85552, 0xdfa000,
            0x35a77c,
        ],
    },
    Entry {
        kind: ThemeKind::Kanagawa,
        id: "kanagawa",
        label: "Kanagawa",
        hex: [
            0x1f1f28, 0xdcd7ba, 0xc8c093, 0x727169, 0x7e9cd8, 0x98bb6c, 0xe46876, 0xe6c384,
            0x7aa89f,
        ],
    },
    Entry {
        kind: ThemeKind::Monokai,
        id: "monokai",
        label: "Monokai",
        hex: [
            0x272822, 0xf8f8f2, 0xb5b5a8, 0x75715e, 0x66d9ef, 0xa6e22e, 0xf92672, 0xe6db74,
            0xae81ff,
        ],
    },
];

/// A resolved theme: every color is guaranteed readable against `bg`.
/// `ui.rs` reads these fields directly and never adjusts contrast itself.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Background the palette was resolved against (usually the terminal's).
    pub bg: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub accent: Color,
    pub success: Color,
    pub error: Color,
    pub warn: Color,
    pub info: Color,
    /// Background of the highlighted (focused, selected) row.
    pub highlight_bg: Color,
    /// Background of a selected row whose pane does not have focus.
    pub selected_bg: Color,
    /// Readable foreground on top of `highlight_bg` and `selected_bg`.
    pub on_highlight: Color,
    /// Dimmed backdrop painted behind popups.
    pub scrim: Color,
}

impl Default for Theme {
    /// The built-in dark palette. Never queries the terminal, so this is
    /// safe on code paths that never render (e.g. `pg-studio remove`).
    fn default() -> Self {
        Self::of(ThemeKind::Default)
    }
}

impl Theme {
    /// The theme for `kind`: `terminal` is queried from the environment,
    /// the rest resolve their baked palette.
    pub fn of(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Terminal => Self::from_terminal(),
            _ => Self::resolve(Palette::from_hex(kind.entry().hex)),
        }
    }

    /// Queries the terminal for its own palette (OSC 10/11/4) and normalizes
    /// it through [`Theme::resolve`]. Falls back to the built-in default
    /// theme when the terminal stays silent (e.g. macOS Terminal.app).
    pub fn from_terminal() -> Self {
        query_terminal_replies()
            .as_deref()
            .and_then(parse_terminal_palette)
            .map(Self::resolve)
            .unwrap_or_else(|| Self::of(ThemeKind::Default))
    }

    /// Normalizes an authored palette: every foreground color is lifted until
    /// it clears its WCAG floor against `bg`, and the selection/popup surfaces
    /// are derived from the background so rows and dialogs stay readable too.
    fn resolve(p: Palette) -> Self {
        let bg = p.bg;
        let text = color::ensure_contrast(p.text, bg, TEXT_MIN);
        let muted = color::ensure_contrast(p.muted, bg, BODY_MIN);
        let dim = color::ensure_contrast(p.dim, bg, BODY_MIN);
        let accent = color::ensure_contrast(p.accent, bg, BODY_MIN);
        let success = color::ensure_contrast(p.success, bg, BODY_MIN);
        let error = color::ensure_contrast(p.error, bg, BODY_MIN);
        let warn = color::ensure_contrast(p.warn, bg, BODY_MIN);
        let info = color::ensure_contrast(p.info, bg, BODY_MIN);

        let highlight_bg = raised(bg, accent, 0.22);
        let selected_bg = raised(bg, accent, 0.12);
        // Legible on whichever of the two highlight shades is behind it.
        let on_highlight =
            color::ensure_contrast(color::readable_fg(highlight_bg), selected_bg, BODY_MIN);
        // Dimming always travels away from the background, so a popup keeps
        // its lift on both dark and light terminals.
        let scrim = color::blend(bg, INK, if color::is_light(bg) { 0.12 } else { 0.30 });

        Self {
            bg,
            text,
            muted,
            dim,
            accent,
            success,
            error,
            warn,
            info,
            highlight_bg,
            selected_bg,
            on_highlight,
            scrim,
        }
    }
}

/// Accent-tinted background shade for selected rows: strong enough to stand
/// out from `bg`, weak enough to keep text readable on top.
fn raised(bg: Color, accent: Color, t: f32) -> Color {
    let mut t = t;
    let mut shade = color::blend(bg, accent, t);
    while color::contrast_ratio(shade, bg) < SHADE_MIN && t < 0.6 {
        t += 0.06;
        shade = color::blend(bg, accent, t);
    }
    shade
}

/// Resolves the theme named in the config file for a new TUI session.
/// `None` follows the terminal's own palette; an unknown name falls back to
/// the built-in default and yields a message worth logging.
pub fn resolve(configured: Option<&str>) -> (Theme, Option<String>) {
    match configured {
        None => (Theme::of(ThemeKind::Terminal), None),
        Some(name) => match name.parse::<ThemeKind>() {
            Ok(kind) => (Theme::of(kind), None),
            Err(e) => (Theme::of(ThemeKind::Default), Some(format!("{e:#}"))),
        },
    }
}

/// Queries the terminal for its color theme via OSC 10/11 (foreground,
/// background) and OSC 4 (palette colors 1-8). Returns `None` when the
/// terminal does not answer.
fn query_terminal_replies() -> Option<String> {
    let mut stdout = std::io::stdout().lock();
    let mut queries = String::new();
    queries.push_str("\x1b]10;?\x1b\\");
    queries.push_str("\x1b]11;?\x1b\\");
    for n in 1..=8 {
        queries.push_str(&format!("\x1b]4;{n};?\x1b\\"));
    }
    if stdout.write_all(queries.as_bytes()).is_err() || stdout.flush().is_err() {
        return None;
    }

    let mut buf: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = remaining.as_millis().min(100) as libc::c_int;
        let mut fds = [libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        }];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if rc <= 0 {
            continue;
        }
        let mut chunk = [0u8; 512];
        let n = unsafe { libc::read(0, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
        if n > 0 {
            buf.extend_from_slice(&chunk[..n as usize]);
        }
    }

    if buf.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Turns the terminal's OSC replies into an authored palette: whatever the
/// terminal reports wins, the rest falls back to the built-in default theme.
/// Muted/dim are mixed from the reported foreground and background instead
/// of one fixed gray, so light terminals get dark grays and dark terminals
/// light ones (the old pinned `#7F849C` was unreadable at both ends).
fn parse_terminal_palette(text: &str) -> Option<Palette> {
    let bytes = text.as_bytes();
    let mut fg: Option<Color> = None;
    let mut bg: Option<Color> = None;
    let mut palette: [Option<Color>; 8] = [None; 8];
    let mut any = false;

    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b']') {
            let start = i + 2;
            let mut end = start;
            let mut terminated = false;
            while end < bytes.len() {
                if bytes[end] == 0x07 {
                    terminated = true;
                    break;
                }
                if bytes[end] == 0x1b && bytes.get(end + 1) == Some(&b'\\') {
                    end += 2;
                    terminated = true;
                    break;
                }
                end += 1;
            }
            if terminated {
                let payload = String::from_utf8_lossy(&bytes[start..end]);
                if let Some(parsed) = parse_osc_payload(&payload) {
                    match parsed.0 {
                        10 => fg = Some(parsed.1),
                        11 => bg = Some(parsed.1),
                        1..=8 => palette[parsed.0 - 1] = Some(parsed.1),
                        _ => {}
                    }
                    any = true;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }

    let mut p = Palette::from_hex(ThemeKind::Default.entry().hex);
    if let Some(c) = fg {
        p.text = c;
    }
    if let Some(c) = palette[6] {
        p.text = c;
    }
    if let Some(c) = bg {
        p.bg = c;
    }
    if let Some(c) = palette[0] {
        p.error = c;
    }
    if let Some(c) = palette[1] {
        p.success = c;
    }
    if let Some(c) = palette[2] {
        p.warn = c;
    }
    if let Some(c) = palette[4] {
        p.info = c;
    }
    if let Some(c) = palette[5] {
        p.accent = c;
    }
    p.muted = color::blend(p.text, p.bg, 0.42);
    p.dim = color::blend(p.text, p.bg, 0.58);

    any.then_some(p)
}

/// Parses an OSC payload like `4;6;rgb:ffff/ffff/ffff`, `10;rgb:...` or
/// `11;rgb:...` into (index, color). Index 10 = foreground, 11 = background.
fn parse_osc_payload(payload: &str) -> Option<(usize, Color)> {
    let (key, color_part) = if let Some(rest) = payload.strip_prefix("4;") {
        let (num, rest) = rest.split_once(';')?;
        (num.parse::<usize>().ok()?, rest)
    } else if let Some(rest) = payload.strip_prefix("10;") {
        (10, rest)
    } else {
        let rest = payload.strip_prefix("11;")?;
        (11, rest)
    };

    let rgb = color_part.strip_prefix("rgb:")?;
    let comps: Vec<&str> = rgb.split('/').collect();
    if comps.len() != 3 {
        return None;
    }
    let r = parse_component(comps[0])?;
    let g = parse_component(comps[1])?;
    let b = parse_component(comps[2])?;
    Some((key, Color::Rgb(r, g, b)))
}

fn parse_component(s: &str) -> Option<u8> {
    let digits: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if digits.is_empty() {
        return None;
    }
    let d = if digits.len() == 1 {
        digits.repeat(2)
    } else {
        digits[..2].to_string()
    };
    u8::from_str_radix(&d, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(kind: ThemeKind) -> Theme {
        Theme::resolve(Palette::from_hex(kind.entry().hex))
    }

    #[test]
    fn every_listed_theme_resolves_to_a_readable_palette() {
        for kind in ThemeKind::ALL {
            if kind == ThemeKind::Terminal {
                continue; // ambient: no baked palette to check
            }
            let t = resolved(kind);
            let bg = t.bg;
            assert!(
                color::contrast_ratio(t.text, bg) >= TEXT_MIN,
                "{kind:?}: body text below {TEXT_MIN}:1"
            );
            for (name, c) in [
                ("muted", t.muted),
                ("dim", t.dim),
                ("accent", t.accent),
                ("success", t.success),
                ("error", t.error),
                ("warn", t.warn),
                ("info", t.info),
            ] {
                assert!(
                    color::contrast_ratio(c, bg) >= BODY_MIN,
                    "{kind:?}: {name} below {BODY_MIN}:1"
                );
            }
            assert!(
                color::contrast_ratio(t.highlight_bg, bg) >= SHADE_MIN,
                "{kind:?}: highlight row invisible against the background"
            );
            assert!(
                color::contrast_ratio(t.on_highlight, t.highlight_bg) >= BODY_MIN,
                "{kind:?}: text on the highlight row below {BODY_MIN}:1"
            );
            assert!(
                color::contrast_ratio(t.on_highlight, t.selected_bg) >= BODY_MIN,
                "{kind:?}: text on the unfocused selection below {BODY_MIN}:1"
            );
        }
    }

    #[test]
    fn table_and_cycle_list_cover_every_variant_exactly_once() {
        assert_eq!(THEMES.len(), ThemeKind::ALL.len());
        for kind in ThemeKind::ALL {
            let listed = THEMES.iter().filter(|t| t.kind == kind).count();
            assert_eq!(listed, 1, "{kind:?} listed {listed} times");
        }
    }

    #[test]
    fn ids_round_trip_through_from_str() {
        for kind in ThemeKind::ALL {
            assert_eq!(kind.id().parse::<ThemeKind>().unwrap(), kind);
            assert!(!kind.label().is_empty());
        }
    }

    #[test]
    fn from_str_accepts_aliases_and_loose_separators() {
        assert_eq!(
            "gruvbox".parse::<ThemeKind>().unwrap(),
            ThemeKind::GruvboxDark
        );
        assert_eq!(
            "Tokyo_Night".parse::<ThemeKind>().unwrap(),
            ThemeKind::TokyoNight
        );
        assert_eq!(
            "catppuccin mocha".parse::<ThemeKind>().unwrap(),
            ThemeKind::CatppuccinMocha
        );
    }

    #[test]
    fn unknown_theme_lists_the_valid_names() {
        let err = "nope".parse::<ThemeKind>().unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(err.contains("catppuccin-mocha"), "{err}");
        assert!(err.contains("terminal"), "{err}");
    }

    #[test]
    fn next_cycles_through_every_theme_and_wraps() {
        let mut seen = Vec::new();
        let mut kind = ThemeKind::ALL[0];
        loop {
            seen.push(kind);
            kind = kind.next();
            if kind == ThemeKind::ALL[0] {
                break;
            }
        }
        assert_eq!(seen.len(), ThemeKind::ALL.len());
    }

    #[test]
    fn resolve_falls_back_with_a_warning_on_unknown_names() {
        let (theme, warning) = resolve(Some("nope"));
        assert_eq!(theme.bg, resolved(ThemeKind::Default).bg);
        assert!(warning.is_some_and(|w| w.contains("nope")));

        let (theme, warning) = resolve(Some("dracula"));
        assert_eq!(theme.bg, resolved(ThemeKind::Dracula).bg);
        assert!(warning.is_none());
    }

    #[test]
    fn parses_kitty_style_replies() {
        let text = "\x1b]10;rgb:dddd/dddd/dddd\x1b\\\x1b]11;rgb:1111/1111/1111\x1b\\\x1b]4;6;rgb:22cc/44aa/66ff\x1b\\\x1b]4;1;rgb:cc0000/0000/0000\x07";
        let theme = Theme::resolve(parse_terminal_palette(text).expect("palette parsed"));
        assert_eq!(theme.text, Color::Rgb(0xdd, 0xdd, 0xdd));
        assert_eq!(theme.bg, Color::Rgb(0x11, 0x11, 0x11));
        // The terminal's own accent was too dim to read, so it got lifted.
        assert!(color::contrast_ratio(theme.accent, theme.bg) >= BODY_MIN);
        assert!(color::contrast_ratio(theme.error, theme.bg) >= BODY_MIN);
    }

    #[test]
    fn parses_short_hex() {
        let text = "\x1b]4;2;rgb:0/ff/0\x1b\\";
        let palette = parse_terminal_palette(text).expect("palette parsed");
        assert_eq!(palette.success, Color::Rgb(0x00, 0xff, 0x00));
    }

    #[test]
    fn light_terminal_replies_stay_readable() {
        // A light terminal: dark foreground on a light background. The old
        // code forced white text here, which was invisible.
        let text = "\x1b]10;rgb:2424/2929/2f2f\x1b\\\x1b]11;rgb:efff/f1f1/f5f5\x1b\\";
        let theme = Theme::resolve(parse_terminal_palette(text).expect("palette parsed"));
        assert_eq!(theme.bg, Color::Rgb(0xef, 0xf1, 0xf5));
        assert!(color::is_light(theme.bg));
        assert!(color::contrast_ratio(theme.text, theme.bg) >= TEXT_MIN);
        // Muted/dim follow the terminal instead of a pinned mid-gray.
        assert!(color::contrast_ratio(theme.muted, theme.bg) >= BODY_MIN);
        assert!(color::contrast_ratio(theme.dim, theme.bg) >= BODY_MIN);
        // The scrim dims away from the background on light themes too.
        assert!(color::contrast_ratio(theme.scrim, theme.bg) > 1.0);
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(parse_terminal_palette("no osc here").is_none());
    }
}
