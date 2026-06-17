//! Unified icon rendering for the tray, taskbar, and EXE.
//!
//! Draws a rounded background with a status-colored center circle and an
//! optional badge in the top-right corner that shows the current config
//! identifier (defaults to "0" for the default config, mirroring the
//! original C# version's behavior).
//!
//! All icons share the same pixel pipeline so the tray, window title bar,
//! and taskbar badge always stay visually consistent.

const SIZE: usize = 32;

#[derive(Clone, Copy)]
struct Rgba([u8; 4]);

impl Rgba {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }
}

/// Palette tuned to match the existing sky-blue theme.
const BG: Rgba = Rgba::rgb(21, 101, 192);
const WHITE: Rgba = Rgba::rgb(255, 255, 255);
const ACCENT_RUNNING: Rgba = Rgba::rgb(76, 175, 80);
const ACCENT_STOPPED: Rgba = Rgba::rgb(211, 47, 47);
const BADGE_TEXT: Rgba = Rgba::rgb(21, 101, 192);

/// Render a full 32x32 RGBA buffer for the given state + config name.
/// The badge text is derived from the config name (default → "0").
pub fn render_icon_rgba(is_running: bool, config_name: &str) -> Vec<u8> {
    let badge = config_badge_text(config_name);
    let accent = if is_running {
        ACCENT_RUNNING
    } else {
        ACCENT_STOPPED
    };

    let mut buf = vec![0u8; SIZE * SIZE * 4];

    // 1) Rounded-rectangle background.
    fill_rounded_rect(&mut buf, 1.0, 1.0, 30.0, 30.0, 8.0, BG);

    // 2) Status center circle with a thin white ring (anti-aliased edges).
    fill_aa_circle(&mut buf, 16.0, 16.0, 9.5, WHITE);
    fill_aa_circle(&mut buf, 16.0, 16.0, 7.5, accent);

    // 3) Config badge in the top-right corner (skip if empty).
    if !badge.is_empty() {
        draw_badge(&mut buf, &badge);
    }

    buf
}

/// Derive a short badge label from the config name.
/// Mirrors the C# `GetConfigBadgeText`: default → "0", otherwise up to 2
/// leading alphanumeric characters.
pub fn config_badge_text(config_name: &str) -> String {
    let trimmed = config_name.trim();
    if trimmed.eq_ignore_ascii_case(crate::config::DEFAULT_CONFIG_NAME) || trimmed.is_empty() {
        return "0".to_owned();
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(2)
        .collect();
    if cleaned.is_empty() {
        trimmed.chars().take(1).collect()
    } else {
        cleaned.to_ascii_uppercase()
    }
}

fn fill_rounded_rect(buf: &mut [u8], x0: f32, y0: f32, x1: f32, y1: f32, r: f32, color: Rgba) {
    for y in 0..SIZE {
        for x in 0..SIZE {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Distance to the nearest edge of the rounded rect (0 inside, >0 outside)
            let dx = (px - (x0 + r)).max((x1 - r) - px).max(0.0);
            let dy = (py - (y0 + r)).max((y1 - r) - py).max(0.0);
            let outside = if px < x0 + r || px > x1 - r || py < y0 + r || py > y1 - r {
                ((dx * dx + dy * dy) - r * r).max(0.0).sqrt()
            } else {
                0.0
            };
            // 1px anti-aliased edge
            let cov = (1.0 - outside).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend(buf, x, y, color, cov);
            }
        }
    }
}

fn fill_aa_circle(buf: &mut [u8], cx: f32, cy: f32, radius: f32, color: Rgba) {
    let r2 = radius * radius;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist2 = dx * dx + dy * dy;
            if dist2 <= r2 {
                // Edge coverage based on distance to the radius
                let dist = dist2.sqrt();
                let cov = (radius - dist + 0.5).clamp(0.0, 1.0);
                blend(buf, x, y, color, cov);
            }
        }
    }
}

/// Draw a small filled badge circle with the config text, anchored at the
/// top-right of the icon. Slightly overlaps the corner so it reads clearly.
fn draw_badge(buf: &mut [u8], text: &str) {
    let cx = 23.0f32;
    let cy = 9.0f32;

    let single = text.chars().count() <= 1;
    let glyph = if single {
        draw_digit(text.chars().next().unwrap_or('0'))
    } else {
        draw_two_chars(text)
    };

    // 2× upscale with 3×3 supersampling for anti-aliased edges.
    let scale = 2;
    let ss = 3;
    let ss_total = (ss * ss) as f32;
    let src_w = glyph.width;
    let src_h = glyph.height;
    let out_w = src_w * scale;
    let out_h = src_h * scale;
    let ox = cx as i32 - out_w as i32 / 2;
    let oy = cy as i32 - out_h as i32 / 2;

    for out_y in 0..out_h {
        for out_x in 0..out_w {
            let mut coverage = 0.0f32;
            for sy in 0..ss {
                for sx in 0..ss {
                    let src_x =
                        (out_x as f32 + (sx as f32 + 0.5) / ss as f32) / scale as f32 - 0.5;
                    let src_y =
                        (out_y as f32 + (sy as f32 + 0.5) / ss as f32) / scale as f32 - 0.5;
                    let gx = src_x.round() as i32;
                    let gy = src_y.round() as i32;
                    if gx >= 0
                        && gx < src_w as i32
                        && gy >= 0
                        && gy < src_h as i32
                        && glyph.pixel(gx as usize, gy as usize)
                    {
                        coverage += 1.0;
                    }
                }
            }
            coverage /= ss_total;
            if coverage > 0.0 {
                let px = ox + out_x as i32;
                let py = oy + out_y as i32;
                if (0..SIZE as i32).contains(&px) && (0..SIZE as i32).contains(&py) {
                    blend(buf, px as usize, py as usize, BADGE_TEXT, coverage);
                }
            }
        }
    }
}

fn blend(buf: &mut [u8], x: usize, y: usize, color: Rgba, coverage: f32) {
    if coverage <= 0.0 {
        return;
    }
    let off = (y * SIZE + x) * 4;
    if off + 3 >= buf.len() {
        return;
    }
    let a = color.0[3] as f32 * coverage / 255.0;
    let inv = 1.0 - a;
    for c in 0..3 {
        let src = color.0[c] as f32 * a;
        let dst = buf[off + c] as f32 * (buf[off + 3] as f32 / 255.0) * inv;
        buf[off + c] = (src + dst).min(255.0) as u8;
    }
    let new_alpha = (a + (buf[off + 3] as f32 / 255.0) * inv) * 255.0;
    buf[off + 3] = new_alpha.min(255.0) as u8;
}

// ── Minimal 5x7 bitmap font for digits 0-9 and A-Z ──────────────────────
// Each glyph is 5 wide × 7 tall. Only the characters we can actually produce
// as a badge are needed (alphanumeric). Compact and dependency-free.

struct Glyph {
    bits: [u8; 7],
}

impl Glyph {
    fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= 5 || y >= 7 {
            return false;
        }
        (self.bits[y] >> (4 - x)) & 1 != 0
    }
}

const fn glyph_for(c: char) -> Option<Glyph> {
    let bits = match c {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        _ => return None,
    };
    Some(Glyph { bits })
}

fn draw_digit(c: char) -> StampedGlyph {
    let g = glyph_for(c).or_else(|| glyph_for('0')).unwrap();
    StampedGlyph::from_glyph(&g)
}

fn draw_two_chars(text: &str) -> StampedGlyph {
    let mut chars = text.chars();
    let a = chars.next();
    let b = chars.next();
    match (a, b) {
        (Some(ca), Some(cb)) => {
            let ga =
                glyph_for(ca).unwrap_or_else(|| glyph_for('?').or_else(|| glyph_for('0')).unwrap());
            let gb =
                glyph_for(cb).unwrap_or_else(|| glyph_for('?').or_else(|| glyph_for('0')).unwrap());
            StampedGlyph::two(&ga, &gb)
        }
        (Some(ca), None) => {
            let g = glyph_for(ca).or_else(|| glyph_for('0')).unwrap();
            StampedGlyph::from_glyph(&g)
        }
        _ => StampedGlyph::blank(),
    }
}

/// A bitmap we can sample pixel-by-pixel, with an explicit width/height so
/// we can compose 1 or 2 glyphs side by side.
struct StampedGlyph {
    width: usize,
    height: usize,
    pixels: Vec<bool>,
}

impl StampedGlyph {
    fn blank() -> Self {
        Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        }
    }

    fn from_glyph(g: &Glyph) -> Self {
        let width = 5;
        let height = 7;
        let mut pixels = vec![false; width * height];
        for y in 0..height {
            for x in 0..width {
                pixels[y * width + x] = g.pixel(x, y);
            }
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    fn two(a: &Glyph, b: &Glyph) -> Self {
        // Shrink each glyph to 4-wide (drop rightmost column) and place with
        // 1px gap so the pair fits inside the small badge circle.
        let cell = 4;
        let gap = 0;
        let width = cell * 2 + gap;
        let height = 7;
        let mut pixels = vec![false; width * height];
        let stamp = |pixels: &mut [bool], gx0: usize, g: &Glyph| {
            for y in 0..7 {
                for x in 0..cell {
                    pixels[y * width + (gx0 + x)] = g.pixel(x, y);
                }
            }
        };
        stamp(&mut pixels, 0, a);
        stamp(&mut pixels, cell + gap, b);
        Self {
            width,
            height,
            pixels,
        }
    }

    fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        self.pixels[y * self.width + x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_badge_is_zero() {
        assert_eq!(config_badge_text("默认"), "0");
        assert_eq!(config_badge_text(""), "0");
        assert_eq!(config_badge_text("  默认  "), "0");
    }

    #[test]
    fn named_config_badge_is_alphanumeric() {
        assert_eq!(config_badge_text("abc"), "AB");
        assert_eq!(config_badge_text("config1"), "CO");
        assert_eq!(config_badge_text("1"), "1");
    }

    #[test]
    fn renders_without_panic_for_various_states() {
        for running in [false, true] {
            for name in ["默认", "abc", "config1", "1", "Z"] {
                let rgba = render_icon_rgba(running, name);
                assert_eq!(rgba.len(), SIZE * SIZE * 4);
                // Not fully transparent
                assert!(rgba.chunks_exact(4).any(|px| px[3] > 0));
            }
        }
    }
}
