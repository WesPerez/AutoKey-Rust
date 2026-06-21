// Unified icon rendering for the tray, taskbar, and EXE.
//
// Draws a rounded background with a route mark, status dot, and an optional
// badge that shows the current config identifier (defaults to "0" for the
// default config, mirroring the original C# version's behavior).
//
// All icons share the same pixel pipeline so the tray, window title bar,
// and taskbar badge always stay visually consistent.

use std::io::Write;

pub const ICON_SIZE: usize = 256;
const ICO_SIZES: &[usize] = &[16, 32, 48, 64, 128, 256];

#[derive(Clone, Copy)]
struct IconMetrics {
    size: usize,
    scale: f32,
}

impl IconMetrics {
    fn new(size: usize) -> Self {
        assert!(size > 0, "icon size must be non-zero");
        Self {
            size,
            scale: size as f32 / 64.0,
        }
    }

    fn px(self, value: f32) -> f32 {
        value * self.scale
    }
}

#[derive(Clone, Copy)]
struct Rgba([u8; 4]);

impl Rgba {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }

    const fn with_alpha(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([r, g, b, a])
    }
}

/// Palette tuned to match the existing sky-blue theme with a clearer icon mark.
const BG_TOP: Rgba = Rgba::rgb(13, 42, 67);
const BG_BOTTOM: Rgba = Rgba::rgb(25, 126, 190);
const PANEL_GLOW: Rgba = Rgba::with_alpha(255, 255, 255, 34);
const ROUTE_SHADOW: Rgba = Rgba::with_alpha(8, 26, 42, 95);
const ROUTE_MAIN: Rgba = Rgba::rgb(242, 250, 255);
const ROUTE_ACCENT: Rgba = Rgba::rgb(94, 220, 255);
const WHITE: Rgba = Rgba::rgb(255, 255, 255);
const ACCENT_RUNNING: Rgba = Rgba::rgb(76, 175, 80);
const ACCENT_STOPPED: Rgba = Rgba::rgb(211, 47, 47);
const BADGE_TEXT: Rgba = Rgba::rgb(9, 55, 93);

/// Render a full RGBA buffer for the given state + config name.
/// The badge text is derived from the config name (default → "0").
pub fn render_icon_rgba(is_running: bool, config_name: &str) -> Vec<u8> {
    render_icon_rgba_at(ICON_SIZE, is_running, config_name)
}

/// Render a full RGBA buffer at the requested icon size.
pub fn render_icon_rgba_at(size: usize, is_running: bool, config_name: &str) -> Vec<u8> {
    render_icon_rgba_at_with_badge(size, is_running, config_name, true)
}

/// Render a full RGBA buffer without a config badge.
pub fn render_icon_rgba_unbadged(is_running: bool) -> Vec<u8> {
    render_icon_rgba_at_unbadged(ICON_SIZE, is_running)
}

/// Render a full RGBA buffer at the requested icon size without a config badge.
pub fn render_icon_rgba_at_unbadged(size: usize, is_running: bool) -> Vec<u8> {
    render_icon_rgba_at_with_badge(size, is_running, crate::config::DEFAULT_CONFIG_NAME, false)
}

/// Render a multi-size ICO containing the badge-bearing runtime icon.
#[allow(dead_code)]
pub fn render_icon_ico(is_running: bool, config_name: &str) -> Vec<u8> {
    let images: Vec<(usize, Vec<u8>)> = ICO_SIZES
        .iter()
        .copied()
        .map(|size| {
            let rgba = render_icon_rgba_at(size, is_running, config_name);
            (size, encode_bmp_icon_image(size, rgba))
        })
        .collect();

    encode_ico_images(images)
}

/// Render a multi-size ICO without a config badge.
pub fn render_icon_ico_unbadged(is_running: bool) -> Vec<u8> {
    let images: Vec<(usize, Vec<u8>)> = ICO_SIZES
        .iter()
        .copied()
        .map(|size| {
            let rgba = render_icon_rgba_at_unbadged(size, is_running);
            (size, encode_bmp_icon_image(size, rgba))
        })
        .collect();

    encode_ico_images(images)
}

/// Render the embedded app/EXE icon without a config badge.
#[allow(dead_code)]
pub fn render_app_icon_rgba_at(size: usize) -> Vec<u8> {
    render_icon_rgba_at_unbadged(size, false)
}

/// Render a transparent taskbar overlay badge for pinned Windows taskbar icons.
pub fn render_taskbar_overlay_rgba_at(size: usize, is_running: bool, config_name: &str) -> Vec<u8> {
    let metrics = IconMetrics::new(size);
    let accent = if is_running {
        ACCENT_RUNNING
    } else {
        ACCENT_STOPPED
    };
    let badge = config_badge_text(config_name);
    let mut buf = vec![0u8; metrics.size * metrics.size * 4];
    draw_overlay_badge(&mut buf, metrics, &badge, accent);
    buf
}

fn render_icon_rgba_at_with_badge(
    size: usize,
    is_running: bool,
    config_name: &str,
    show_badge: bool,
) -> Vec<u8> {
    let metrics = IconMetrics::new(size);
    let badge = if show_badge {
        config_badge_text(config_name)
    } else {
        String::new()
    };
    let accent = if is_running {
        ACCENT_RUNNING
    } else {
        ACCENT_STOPPED
    };

    let mut buf = vec![0u8; metrics.size * metrics.size * 4];

    // 1) Rounded app tile.
    fill_rounded_rect_vertical_gradient(
        &mut buf,
        metrics,
        (
            metrics.px(2.0),
            metrics.px(2.0),
            metrics.px(62.0),
            metrics.px(62.0),
        ),
        metrics.px(14.0),
        (BG_TOP, BG_BOTTOM),
    );
    fill_rounded_rect(
        &mut buf,
        metrics,
        (
            metrics.px(8.0),
            metrics.px(8.0),
            metrics.px(56.0),
            metrics.px(56.0),
        ),
        metrics.px(10.0),
        PANEL_GLOW,
    );

    // 2) Dispatch route mark.
    stroke_line(
        &mut buf,
        metrics,
        (metrics.px(17.0), metrics.px(44.0)),
        (metrics.px(31.0), metrics.px(30.0)),
        metrics.px(8.0),
        ROUTE_SHADOW,
    );
    stroke_line(
        &mut buf,
        metrics,
        (metrics.px(31.0), metrics.px(30.0)),
        (metrics.px(45.0), metrics.px(30.0)),
        metrics.px(8.0),
        ROUTE_SHADOW,
    );
    stroke_line(
        &mut buf,
        metrics,
        (metrics.px(17.0), metrics.px(44.0)),
        (metrics.px(31.0), metrics.px(30.0)),
        metrics.px(5.4),
        ROUTE_MAIN,
    );
    stroke_line(
        &mut buf,
        metrics,
        (metrics.px(31.0), metrics.px(30.0)),
        (metrics.px(45.0), metrics.px(30.0)),
        metrics.px(5.4),
        ROUTE_MAIN,
    );
    stroke_line(
        &mut buf,
        metrics,
        (metrics.px(39.0), metrics.px(23.0)),
        (metrics.px(47.0), metrics.px(30.0)),
        metrics.px(4.4),
        ROUTE_ACCENT,
    );
    stroke_line(
        &mut buf,
        metrics,
        (metrics.px(39.0), metrics.px(37.0)),
        (metrics.px(47.0), metrics.px(30.0)),
        metrics.px(4.4),
        ROUTE_ACCENT,
    );

    // 3) Running/stopped state dot.
    fill_aa_circle(
        &mut buf,
        metrics,
        metrics.px(19.0),
        metrics.px(45.0),
        metrics.px(9.2),
        WHITE,
    );
    fill_aa_circle(
        &mut buf,
        metrics,
        metrics.px(19.0),
        metrics.px(45.0),
        metrics.px(6.8),
        accent,
    );

    // 4) Config badge in the top-right corner (skip if empty).
    if !badge.is_empty() {
        draw_badge(&mut buf, metrics, &badge, accent);
    }

    buf
}

fn encode_ico_images(images: Vec<(usize, Vec<u8>)>) -> Vec<u8> {
    let mut ico = Vec::new();
    ico.write_all(&0u16.to_le_bytes()).unwrap();
    ico.write_all(&1u16.to_le_bytes()).unwrap();
    ico.write_all(&(images.len() as u16).to_le_bytes()).unwrap();

    let mut offset = 6 + images.len() * 16;
    for (size, data) in &images {
        ico.push(if *size >= 256 { 0 } else { *size as u8 });
        ico.push(if *size >= 256 { 0 } else { *size as u8 });
        ico.push(0u8);
        ico.push(0u8);
        ico.write_all(&1u16.to_le_bytes()).unwrap();
        ico.write_all(&32u16.to_le_bytes()).unwrap();
        ico.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
        ico.write_all(&(offset as u32).to_le_bytes()).unwrap();
        offset += data.len();
    }

    for (_, data) in images {
        ico.write_all(&data).unwrap();
    }

    ico
}

fn encode_bmp_icon_image(size: usize, mut rgba: Vec<u8>) -> Vec<u8> {
    for chunk in rgba.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let bmp_header_size = 40u32;
    let and_mask_row_size = size.div_ceil(32) * 4;
    let and_mask_size = (and_mask_row_size * size) as u32;
    let pixel_data_size = (size * size * 4) as u32;
    let mut data = Vec::with_capacity((bmp_header_size + pixel_data_size + and_mask_size) as usize);

    data.write_all(&bmp_header_size.to_le_bytes()).unwrap();
    data.write_all(&(size as i32).to_le_bytes()).unwrap();
    data.write_all(&((size as i32) * 2).to_le_bytes()).unwrap();
    data.write_all(&1u16.to_le_bytes()).unwrap();
    data.write_all(&32u16.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&pixel_data_size.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();
    data.write_all(&0u32.to_le_bytes()).unwrap();

    for y in (0..size).rev() {
        let row_start = y * size * 4;
        data.write_all(&rgba[row_start..row_start + size * 4])
            .unwrap();
    }

    let and_mask = vec![0u8; and_mask_size as usize];
    data.write_all(&and_mask).unwrap();
    data
}

/// Derive a short badge label from the config name.
/// Mirrors the C# `GetConfigBadgeText`: default → "0", otherwise up to 2
/// leading alphanumeric characters.
pub fn config_badge_text(config_name: &str) -> String {
    let trimmed = config_name.trim();
    if trimmed.eq_ignore_ascii_case(crate::config::DEFAULT_CONFIG_NAME) || trimmed.is_empty() {
        return "0".to_owned();
    }

    let digits: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(2)
        .collect();
    if !digits.is_empty() {
        return digits;
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

fn fill_rounded_rect(
    buf: &mut [u8],
    metrics: IconMetrics,
    rect: (f32, f32, f32, f32),
    r: f32,
    color: Rgba,
) {
    let (x0, y0, x1, y1) = rect;
    for y in 0..metrics.size {
        for x in 0..metrics.size {
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
                blend(buf, metrics, x, y, color, cov);
            }
        }
    }
}

fn fill_rounded_rect_vertical_gradient(
    buf: &mut [u8],
    metrics: IconMetrics,
    rect: (f32, f32, f32, f32),
    r: f32,
    colors: (Rgba, Rgba),
) {
    let (x0, y0, x1, y1) = rect;
    let (top, bottom) = colors;

    for y in 0..metrics.size {
        for x in 0..metrics.size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = (px - (x0 + r)).max((x1 - r) - px).max(0.0);
            let dy = (py - (y0 + r)).max((y1 - r) - py).max(0.0);
            let outside = if px < x0 + r || px > x1 - r || py < y0 + r || py > y1 - r {
                ((dx * dx + dy * dy) - r * r).max(0.0).sqrt()
            } else {
                0.0
            };
            let cov = (1.0 - outside).clamp(0.0, 1.0);
            if cov > 0.0 {
                let t = ((py - y0) / (y1 - y0)).clamp(0.0, 1.0);
                blend(buf, metrics, x, y, mix(top, bottom, t), cov);
            }
        }
    }
}

fn stroke_line(
    buf: &mut [u8],
    metrics: IconMetrics,
    from: (f32, f32),
    to: (f32, f32),
    width: f32,
    color: Rgba,
) {
    let (x0, y0) = from;
    let (x1, y1) = to;
    let radius = width / 2.0;
    let min_x = (x0.min(x1) - radius - 1.0).floor().max(0.0) as usize;
    let max_x = (x0.max(x1) + radius + 1.0)
        .ceil()
        .min((metrics.size - 1) as f32) as usize;
    let min_y = (y0.min(y1) - radius - 1.0).floor().max(0.0) as usize;
    let max_y = (y0.max(y1) + radius + 1.0)
        .ceil()
        .min((metrics.size - 1) as f32) as usize;
    let vx = x1 - x0;
    let vy = y1 - y0;
    let len2 = vx * vx + vy * vy;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let t = if len2 <= f32::EPSILON {
                0.0
            } else {
                (((px - x0) * vx + (py - y0) * vy) / len2).clamp(0.0, 1.0)
            };
            let cx = x0 + vx * t;
            let cy = y0 + vy * t;
            let dx = px - cx;
            let dy = py - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let cov = (radius - dist + 0.75).clamp(0.0, 1.0);
            if cov > 0.0 {
                blend(buf, metrics, x, y, color, cov);
            }
        }
    }
}

fn fill_aa_circle(
    buf: &mut [u8],
    metrics: IconMetrics,
    cx: f32,
    cy: f32,
    radius: f32,
    color: Rgba,
) {
    let r2 = radius * radius;
    for y in 0..metrics.size {
        for x in 0..metrics.size {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist2 = dx * dx + dy * dy;
            if dist2 <= r2 {
                // Edge coverage based on distance to the radius
                let dist = dist2.sqrt();
                let cov = (radius - dist + 0.5).clamp(0.0, 1.0);
                blend(buf, metrics, x, y, color, cov);
            }
        }
    }
}

/// Draw a small filled badge disc anchored at the top-right of the icon, with
/// the config number rendered on top. The disc is white with a colored ring
/// matching the running/stopped accent so the number stays readable against
/// any background.
fn draw_badge(buf: &mut [u8], metrics: IconMetrics, text: &str, accent: Rgba) {
    let cx = metrics.px(47.7);
    let cy = metrics.px(16.1);

    // 1) Colored accent ring (sits behind the white disc).
    fill_aa_circle(buf, metrics, cx, cy, metrics.px(16.0), accent);
    // 2) White disc on top, leaving a readable colored ring visible.
    fill_aa_circle(buf, metrics, cx, cy, metrics.px(13.4), WHITE);

    let glyph = badge_glyph(text);
    if glyph.width == 0 || glyph.height == 0 {
        return;
    }

    let single = glyph.source_chars <= 1;
    let (target_w, target_h) = (
        metrics.px(if single { 17.0 } else { 25.0 }),
        metrics.px(if single { 22.5 } else { 18.5 }),
    );

    let out_w = target_w.round().max(glyph.width as f32) as i32;
    let out_h = target_h.round().max(glyph.height as f32) as i32;
    draw_scaled_glyph(buf, metrics, &glyph, (cx, cy), (out_w, out_h), BADGE_TEXT);
}

fn draw_overlay_badge(buf: &mut [u8], metrics: IconMetrics, text: &str, accent: Rgba) {
    let cx = metrics.px(32.0);
    let cy = metrics.px(32.0);

    fill_aa_circle(buf, metrics, cx, cy, metrics.px(31.0), accent);
    fill_aa_circle(buf, metrics, cx, cy, metrics.px(26.8), WHITE);

    let glyph = badge_glyph(text);
    if glyph.width == 0 || glyph.height == 0 {
        return;
    }

    let single = glyph.source_chars <= 1;
    let out_w = metrics
        .px(if single { 26.0 } else { 42.0 })
        .round()
        .max(glyph.width as f32) as i32;
    let out_h = metrics
        .px(if single { 34.0 } else { 30.0 })
        .round()
        .max(glyph.height as f32) as i32;
    draw_scaled_glyph(buf, metrics, &glyph, (cx, cy), (out_w, out_h), BADGE_TEXT);
}

fn draw_scaled_glyph(
    buf: &mut [u8],
    metrics: IconMetrics,
    glyph: &StampedGlyph,
    center: (f32, f32),
    size: (i32, i32),
    color: Rgba,
) {
    let (cx, cy) = center;
    let (out_w, out_h) = size;
    let ox = cx.round() as i32 - out_w / 2;
    let oy = cy.round() as i32 - out_h / 2;

    for out_y in 0..out_h {
        for out_x in 0..out_w {
            let gx = (out_x as usize * glyph.width) / out_w as usize;
            let gy = (out_y as usize * glyph.height) / out_h as usize;
            if glyph.pixel(gx, gy) {
                let px = ox + out_x;
                let py = oy + out_y;
                if (0..metrics.size as i32).contains(&px) && (0..metrics.size as i32).contains(&py)
                {
                    blend(buf, metrics, px as usize, py as usize, color, 1.0);
                }
            }
        }
    }
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let mut out = [0u8; 4];
    for (index, channel) in out.iter_mut().enumerate() {
        let value = a.0[index] as f32 + (b.0[index] as f32 - a.0[index] as f32) * t;
        *channel = value.round().clamp(0.0, 255.0) as u8;
    }
    Rgba(out)
}

fn blend(buf: &mut [u8], metrics: IconMetrics, x: usize, y: usize, color: Rgba, coverage: f32) {
    if coverage <= 0.0 {
        return;
    }
    let off = (y * metrics.size + x) * 4;
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
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
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

fn badge_glyph(text: &str) -> StampedGlyph {
    draw_alphanumeric_chars(text)
}

fn draw_alphanumeric_chars(text: &str) -> StampedGlyph {
    let mut chars = text.chars().take(2);
    let a = chars.next();
    let b = chars.next();
    match (a, b) {
        (Some(ca), Some(cb)) => {
            let ga = glyph_for(ca).unwrap_or_else(|| glyph_for('0').unwrap());
            let gb = glyph_for(cb).unwrap_or_else(|| glyph_for('0').unwrap());
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
    source_chars: usize,
    pixels: Vec<bool>,
}

impl StampedGlyph {
    fn blank() -> Self {
        Self {
            width: 0,
            height: 0,
            source_chars: 0,
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
            source_chars: 1,
            pixels,
        }
    }

    fn two(a: &Glyph, b: &Glyph) -> Self {
        let cell = 5;
        let gap = 1;
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
            source_chars: 2,
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
        assert_eq!(config_badge_text("config1"), "1");
        assert_eq!(config_badge_text("config12"), "12");
        assert_eq!(config_badge_text("1"), "1");
    }

    #[test]
    fn renders_without_panic_for_various_states() {
        for running in [false, true] {
            for name in ["默认", "abc", "config1", "1", "Z"] {
                let rgba = render_icon_rgba(running, name);
                assert_eq!(rgba.len(), ICON_SIZE * ICON_SIZE * 4);
                // Not fully transparent
                assert!(rgba.chunks_exact(4).any(|px| px[3] > 0));
            }
        }
    }

    #[test]
    fn renders_native_icon_sizes_without_panic() {
        for size in [16, 32, 48, 64, 128, 256] {
            let rgba = render_icon_rgba_at(size, false, "config12");
            assert_eq!(rgba.len(), size * size * 4);
            assert!(rgba.chunks_exact(4).any(|px| px[3] > 0));
        }
    }

    #[test]
    fn renders_taskbar_overlay_with_badge_pixels() {
        let rgba = render_taskbar_overlay_rgba_at(64, true, "config12");
        assert_eq!(rgba.len(), 64 * 64 * 4);
        assert!(rgba.chunks_exact(4).any(|px| px[3] > 0));
        assert!(rgba.chunks_exact(4).any(|px| {
            px[0] == BADGE_TEXT.0[0]
                && px[1] == BADGE_TEXT.0[1]
                && px[2] == BADGE_TEXT.0[2]
                && px[3] > 0
        }));
    }
}
