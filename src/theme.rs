use ratatui::style::Color;
use std::io::Write;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub success: Color,
    pub error: Color,
    pub warn: Color,
    pub info: Color,
    pub highlight_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            text: Color::White,
            muted: Color::Rgb(0x7F, 0x84, 0x9C),
            dim: Color::Rgb(0x7F, 0x84, 0x9C),
            success: Color::Green,
            error: Color::Red,
            warn: Color::Yellow,
            info: Color::Magenta,
            highlight_bg: Color::Rgb(40, 44, 44),
        }
    }
}

fn blend(base: Color, top: Color, t: f32) -> Color {
    match (base, top) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(tr, tg, tb)) => {
            let mix = |a: u8, b: u8| (a as f32 * (1.0 - t) + b as f32 * t).round() as u8;
            Color::Rgb(mix(ar, tr), mix(ag, tg), mix(ab, tb))
        }
        _ => top,
    }
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

/// Queries the terminal for its color theme via OSC 10/11 (foreground,
/// background) and OSC 4 (palette colors 1-8). Returns None if the terminal
/// does not respond (e.g. macOS Terminal.app).
pub fn query_terminal_theme() -> Option<Theme> {
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
    parse_theme_replies(&String::from_utf8_lossy(&buf))
}

fn parse_theme_replies(text: &str) -> Option<Theme> {
    let bytes = text.as_bytes();
    let mut fg: Option<Color> = None;
    let mut bg: Option<Color> = None;
    let mut palette: [Option<Color>; 8] = [None; 8];

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
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }

    let mut theme = Theme::default();
    let mut any = false;

    if let Some(c) = fg {
        theme.text = c;
        any = true;
    } else if let Some(c) = palette[6] {
        theme.text = c;
        any = true;
    }
    if let Some(c) = palette[5] {
        theme.accent = c;
        any = true;
    }
    if let Some(c) = palette[0] {
        theme.error = c;
        any = true;
    }
    if let Some(c) = palette[1] {
        theme.success = c;
        any = true;
    }
    if let Some(c) = palette[2] {
        theme.warn = c;
        any = true;
    }
    if let Some(c) = palette[4] {
        theme.info = c;
        any = true;
    }
    // Muted/dim stay fixed at a readable medium gray regardless of the
    // terminal palette, which can be too dark to read against some themes.
    theme.muted = Color::Rgb(0x7F, 0x84, 0x9C);
    theme.dim = Color::Rgb(0x7F, 0x84, 0x9C);
    theme.highlight_bg = match bg {
        Some(b) => blend(b, theme.accent, 0.22),
        None => blend(Color::Rgb(30, 30, 30), theme.accent, 0.22),
    };

    if any { Some(theme) } else { None }
}

/// Parses an OSC payload like `4;6;rgb:ffff/ffff/ffff`, `10;rgb:...` or
/// `11;rgb:...` into (index, color). Index 10 = foreground, 11 = background.
fn parse_osc_payload(payload: &str) -> Option<(usize, Color)> {
    let (key, color_part) = if let Some(rest) = payload.strip_prefix("4;") {
        let (num, rest) = rest.split_once(';')?;
        (num.parse::<usize>().ok()?, rest)
    } else if let Some(rest) = payload.strip_prefix("10;") {
        (10, rest)
    } else if let Some(rest) = payload.strip_prefix("11;") {
        (11, rest)
    } else {
        return None;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kitty_style_replies() {
        let text = "\x1b]10;rgb:dddd/dddd/dddd\x1b\\\x1b]11;rgb:1111/1111/1111\x1b\\\x1b]4;6;rgb:22cc/44aa/66ff\x1b\\\x1b]4;1;rgb:cc0000/0000/0000\x07";
        let theme = parse_theme_replies(text).expect("theme parsed");
        assert_eq!(theme.text, Color::Rgb(0xdd, 0xdd, 0xdd));
        assert_eq!(theme.accent, Color::Rgb(0x22, 0x44, 0x66));
        assert_eq!(theme.error, Color::Rgb(0xcc, 0x00, 0x00));
    }

    #[test]
    fn parses_short_hex() {
        let text = "\x1b]4;2;rgb:0/ff/0\x1b\\";
        let theme = parse_theme_replies(text).expect("theme parsed");
        assert_eq!(theme.success, Color::Rgb(0x00, 0xff, 0x00));
    }

    #[test]
    fn returns_none_for_garbage() {
        assert!(parse_theme_replies("no osc here").is_none());
    }
}
