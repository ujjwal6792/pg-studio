//! Color math behind theme resolution: WCAG relative luminance, contrast
//! ratios, and the nudges that keep text legible on any terminal palette.
//!
//! This is the one place contrast is decided. Theme resolution
//! (`crate::theme`) runs every color through these functions, so rendering
//! code can style text without re-checking readability.

use ratatui::style::Color;

/// Pure white: the lightest target a contrast nudge can move towards.
pub const WHITE: Color = Color::Rgb(0xff, 0xff, 0xff);

/// Near-black: the darkest target a contrast nudge can move towards. Softer
/// than pure black so lifted text does not look crushed on dark themes.
pub const INK: Color = Color::Rgb(0x0a, 0x0a, 0x0a);

/// Number of blend steps an `ensure_contrast` search may take.
const STEPS: u16 = 24;

/// xterm's default rendering of the named ANSI colors. Lets luminance and
/// blending reason about a palette color as well as an RGB one.
fn named_rgb(color: Color) -> Option<(u8, u8, u8)> {
    Some(match color {
        Color::Black => (0x00, 0x00, 0x00),
        Color::Red => (0xcd, 0x00, 0x00),
        Color::Green => (0x00, 0xcd, 0x00),
        Color::Yellow => (0xcd, 0xcd, 0x00),
        Color::Blue => (0x00, 0x00, 0xee),
        Color::Magenta => (0xcd, 0x00, 0xcd),
        Color::Cyan => (0x00, 0xcd, 0xcd),
        Color::Gray => (0xe5, 0xe5, 0xe5),
        Color::DarkGray => (0x7f, 0x7f, 0x7f),
        Color::LightRed => (0xff, 0x00, 0x00),
        Color::LightGreen => (0x00, 0xff, 0x00),
        Color::LightYellow => (0xff, 0xff, 0x00),
        Color::LightBlue => (0x5c, 0x5c, 0xff),
        Color::LightMagenta => (0xff, 0x00, 0xff),
        Color::LightCyan => (0x00, 0xff, 0xff),
        Color::White => (0xff, 0xff, 0xff),
        _ => return None,
    })
}

/// RGB triple for a color we can reason about; `None` for palette slots and
/// `Reset`, whose real value only the terminal knows.
fn to_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        other => named_rgb(other),
    }
}

/// WCAG relative luminance: 0.0 for black, 1.0 for white. A terminal palette
/// slot or `Reset` reports middle gray so comparisons stay well-defined.
pub fn luminance(color: Color) -> f64 {
    let Some((r, g, b)) = to_rgb(color) else {
        return 0.5;
    };
    let linear = |channel: u8| {
        let c = channel as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// WCAG contrast ratio between two colors: 1.0 when identical, 21.0 for
/// black on white. The usual legibility floors are 4.5 for body text and
/// 3.0 for large text and borders.
pub fn contrast_ratio(a: Color, b: Color) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether the color reads as light. Picks the direction contrast nudges
/// push: away from the background's own lightness.
pub fn is_light(color: Color) -> bool {
    luminance(color) > 0.35
}

/// Mixes two colors: `t = 0` keeps `base`, `t = 1` returns `top`. Named ANSI
/// colors blend via their default rendering; `Reset` and palette slots (whose
/// value is unknown) fall back to `top`.
pub fn blend(base: Color, top: Color, t: f32) -> Color {
    match (to_rgb(base), to_rgb(top)) {
        (Some((ar, ag, ab)), Some((tr, tg, tb))) => {
            let mix = |a: u8, b: u8| (a as f32 * (1.0 - t) + b as f32 * t).round() as u8;
            Color::Rgb(mix(ar, tr), mix(ag, tg), mix(ab, tb))
        }
        _ => top,
    }
}

/// White or near-black, whichever contrasts more with `bg`.
pub fn readable_fg(bg: Color) -> Color {
    if contrast_ratio(WHITE, bg) >= contrast_ratio(INK, bg) {
        WHITE
    } else {
        INK
    }
}

/// Pushes `fg` towards white or near-black (whichever suits `bg`) until the
/// pair reaches `min` contrast. Hue survives; only lightness moves.
pub fn ensure_contrast(fg: Color, bg: Color, min: f64) -> Color {
    if contrast_ratio(fg, bg) >= min {
        return fg;
    }
    let target = if is_light(bg) { INK } else { WHITE };
    let mut best = fg;
    for step in 1..=STEPS {
        let candidate = blend(fg, target, step as f32 / STEPS as f32);
        if contrast_ratio(candidate, bg) >= min {
            return candidate;
        }
        best = candidate;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const DARK: Color = Color::Rgb(0x1a, 0x1b, 0x26);
    const LIGHT: Color = Color::Rgb(0xef, 0xf1, 0xf5);

    #[test]
    fn luminance_spans_black_to_white() {
        assert!(luminance(Color::Black) < 0.001);
        assert!(luminance(Color::White) > 0.999);
    }

    #[test]
    fn black_on_white_is_the_maximum_contrast() {
        let ratio = contrast_ratio(Color::Black, Color::White);
        assert!((ratio - 21.0).abs() < 0.01, "{ratio}");
        assert_eq!(contrast_ratio(DARK, DARK), 1.0);
    }

    #[test]
    fn blend_moves_from_base_to_top() {
        assert_eq!(blend(Color::Black, Color::White, 0.0), Color::Rgb(0, 0, 0));
        assert_eq!(
            blend(Color::Black, Color::White, 1.0),
            Color::Rgb(255, 255, 255)
        );
        assert_eq!(
            blend(Color::Black, Color::White, 0.5),
            Color::Rgb(128, 128, 128)
        );
    }

    #[test]
    fn named_ansi_colors_measure_like_their_default_rendering() {
        assert!(luminance(Color::Black) < 0.001);
        assert!(luminance(Color::White) > 0.999);
        assert_eq!(
            blend(Color::Rgb(0, 0, 0), Color::Cyan, 1.0),
            Color::Rgb(0, 0xcd, 0xcd)
        );
        // `Reset` and indexed palette slots have no known value: blends fall
        // back to the top color rather than guessing.
        assert_eq!(blend(Color::Black, Color::Reset, 0.5), Color::Reset);
    }

    #[test]
    fn readable_fg_picks_the_extreme_that_contrasts() {
        assert_eq!(readable_fg(Color::Black), WHITE);
        assert_eq!(readable_fg(Color::White), INK);
        assert!(contrast_ratio(readable_fg(DARK), DARK) > 10.0);
        assert!(contrast_ratio(readable_fg(LIGHT), LIGHT) > 10.0);
    }

    #[test]
    fn ensure_contrast_lifts_dim_colors_to_the_floor() {
        // Tokyo Night's comment gray sits around 2.7:1 on its background.
        let dim = Color::Rgb(0x56, 0x5f, 0x89);
        let fixed = ensure_contrast(dim, DARK, 4.5);
        assert!(contrast_ratio(dim, DARK) < 4.5);
        assert!(contrast_ratio(fixed, DARK) >= 4.5);
    }

    #[test]
    fn ensure_contrast_leaves_readable_colors_untouched() {
        assert_eq!(ensure_contrast(WHITE, DARK, 4.5), WHITE);
        assert_eq!(ensure_contrast(INK, LIGHT, 4.5), INK);
    }

    #[test]
    fn ensure_contrast_keeps_the_hue() {
        // A lifted red stays red: the dominant channel keeps leading.
        let Color::Rgb(r, g, b) = ensure_contrast(Color::Rgb(0xcc, 0x00, 0x00), DARK, 4.5) else {
            panic!("expected an rgb color");
        };
        assert!(r > g && r > b, "expected red-dominant, got {r}/{g}/{b}");
    }

    #[test]
    fn ensure_contrast_works_on_light_backgrounds() {
        // Solarized's #b58900 yellow is roughly 2.6:1 on its light background.
        let fixed = ensure_contrast(Color::Rgb(0xb5, 0x89, 0x00), LIGHT, 4.5);
        assert!(contrast_ratio(fixed, LIGHT) >= 4.5);
    }
}
