// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Small in-process pixel-art renderers: a blocky bitmap font (for the
//! PLAY button and the logo's "JAVA EDITION" line) and per-account
//! pixel-face avatars. Same philosophy as `qr.rs` - no font files, no
//! image crate, just an RGBA buffer that `main.rs` wraps in a
//! `gdk::MemoryTexture`. GTK-free so `examples/gen_art.rs` can reuse it.

/// A straight (non-premultiplied) RGBA8 image.
#[derive(Debug, Clone)]
pub struct Image {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            rgba: vec![0; (width * height * 4) as usize],
            width,
            height,
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: [u8; 4]) {
        for py in y.max(0)..(y + h as i32).min(self.height as i32) {
            for px in x.max(0)..(x + w as i32).min(self.width as i32) {
                let idx = ((py as u32 * self.width + px as u32) * 4) as usize;
                self.rgba[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }
}

// ─── Bitmap font ────────────────────────────────────────────────────
// 7 rows tall, variable width, drawn in the spirit of Minecraft's own
// UI font (but not copied from it). Lowercase input is upper-cased.
const GLYPH_HEIGHT: u32 = 7;

fn glyph(c: char) -> &'static [&'static str; 7] {
    match c.to_ascii_uppercase() {
        'A' => &[".###.", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
        'B' => &["####.", "#...#", "#...#", "####.", "#...#", "#...#", "####."],
        'C' => &[".###.", "#...#", "#....", "#....", "#....", "#...#", ".###."],
        'D' => &["####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####."],
        'E' => &["#####", "#....", "#....", "####.", "#....", "#....", "#####"],
        'F' => &["#####", "#....", "#....", "####.", "#....", "#....", "#...."],
        'G' => &[".####", "#....", "#....", "#..##", "#...#", "#...#", ".###."],
        'H' => &["#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#"],
        'I' => &["###", ".#.", ".#.", ".#.", ".#.", ".#.", "###"],
        'J' => &["....#", "....#", "....#", "....#", "#...#", "#...#", ".###."],
        'K' => &["#...#", "#..#.", "#.#..", "##...", "#.#..", "#..#.", "#...#"],
        'L' => &["#....", "#....", "#....", "#....", "#....", "#....", "#####"],
        'M' => &["#...#", "##.##", "#.#.#", "#.#.#", "#...#", "#...#", "#...#"],
        'N' => &["#...#", "##..#", "#.#.#", "#..##", "#...#", "#...#", "#...#"],
        'O' => &[".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
        'P' => &["####.", "#...#", "#...#", "####.", "#....", "#....", "#...."],
        'Q' => &[".###.", "#...#", "#...#", "#...#", "#.#.#", "#..#.", ".##.#"],
        'R' => &["####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#"],
        'S' => &[".####", "#....", "#....", ".###.", "....#", "....#", "####."],
        'T' => &["#####", "..#..", "..#..", "..#..", "..#..", "..#..", "..#.."],
        'U' => &["#...#", "#...#", "#...#", "#...#", "#...#", "#...#", ".###."],
        'V' => &["#...#", "#...#", "#...#", "#...#", ".#.#.", ".#.#.", "..#.."],
        'W' => &["#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#"],
        'X' => &["#...#", ".#.#.", "..#..", "..#..", "..#..", ".#.#.", "#...#"],
        'Y' => &["#...#", ".#.#.", "..#..", "..#..", "..#..", "..#..", "..#.."],
        'Z' => &["#####", "....#", "...#.", "..#..", ".#...", "#....", "#####"],
        '0' => &[".###.", "#...#", "#..##", "#.#.#", "##..#", "#...#", ".###."],
        '1' => &["..#..", ".##..", "..#..", "..#..", "..#..", "..#..", "#####"],
        '2' => &[".###.", "#...#", "....#", "..##.", ".#...", "#....", "#####"],
        '3' => &[".###.", "#...#", "....#", "..##.", "....#", "#...#", ".###."],
        '4' => &["...#.", "..##.", ".#.#.", "#..#.", "#####", "...#.", "...#."],
        '5' => &["#####", "#....", "####.", "....#", "....#", "#...#", ".###."],
        '6' => &["..##.", ".#...", "#....", "####.", "#...#", "#...#", ".###."],
        '7' => &["#####", "....#", "...#.", "..#..", "..#..", "..#..", "..#.."],
        '8' => &[".###.", "#...#", "#...#", ".###.", "#...#", "#...#", ".###."],
        '9' => &[".###.", "#...#", "#...#", ".####", "....#", "...#.", ".##.."],
        '.' => &[".", ".", ".", ".", ".", ".", "#"],
        ':' => &[".", "#", "#", ".", ".", "#", "#"],
        '-' => &["....", "....", "....", "####", "....", "....", "...."],
        '!' => &["#", "#", "#", "#", "#", ".", "#"],
        '\'' => &["#", "#", ".", ".", ".", ".", "."],
        _ => &["...", "...", "...", "...", "...", "...", "..."], // space/unknown
    }
}

/// One empty column between glyphs, in font cells.
const GLYPH_SPACING: u32 = 1;

/// Width of `text` in font cells (multiply by the render scale for px).
pub fn text_width_cells(text: &str) -> u32 {
    let total: u32 = text
        .chars()
        .map(|c| glyph(c)[0].len() as u32 + GLYPH_SPACING)
        .sum();
    total.saturating_sub(GLYPH_SPACING)
}

/// Calls `plot(x, y)` for every lit font cell of `text`, in cell units.
pub fn for_each_text_cell(text: &str, mut plot: impl FnMut(u32, u32)) {
    let mut pen = 0u32;
    for c in text.chars() {
        let rows = glyph(c);
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                if ch == b'#' {
                    plot(pen + x as u32, y as u32);
                }
            }
        }
        pen += rows[0].len() as u32 + GLYPH_SPACING;
    }
}

/// Renders `text` with `scale` px per font cell and a one-cell drop
/// shadow down-right, the way Minecraft draws its own UI labels.
pub fn render_text(text: &str, scale: u32, color: [u8; 4], shadow: [u8; 4]) -> Image {
    let w = (text_width_cells(text) + 1) * scale;
    let h = (GLYPH_HEIGHT + 1) * scale;
    let mut img = Image::new(w.max(1), h);
    let s = scale as i32;
    for_each_text_cell(text, |x, y| {
        img.fill_rect((x as i32 + 1) * s, (y as i32 + 1) * s, scale, scale, shadow);
    });
    for_each_text_cell(text, |x, y| {
        img.fill_rect(x as i32 * s, y as i32 * s, scale, scale, color);
    });
    img
}

// ─── Avatars ────────────────────────────────────────────────────────
// No skin download (that would need the network, and the launcher has
// to work offline) - instead every account gets a stable, distinct
// 8x8 blocky face derived from a hash of its UUID.

fn hash32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

fn shade(c: [u8; 3], f: f32) -> [u8; 4] {
    let s = |v: u8| ((v as f32 * f).round().clamp(0.0, 255.0)) as u8;
    [s(c[0]), s(c[1]), s(c[2]), 255]
}

/// Renders an 8x8 pixel face at `px_per_cell` px per pixel, seeded by
/// `seed` (the account UUID), on a pastel background tile.
pub fn render_avatar(seed: &str, px_per_cell: u32) -> Image {
    let mut h = seed
        .bytes()
        .fold(0x811c_9dc5u32, |acc, b| (acc ^ b as u32).wrapping_mul(0x0100_0193));
    let mut next = || {
        h = hash32(h.wrapping_add(0x9e37_79b9));
        h
    };

    const SKIN: [[u8; 3]; 5] = [
        [0xF2, 0xC4, 0x9B],
        [0xE0, 0xA8, 0x7A],
        [0xC6, 0x8A, 0x5E],
        [0x9A, 0x63, 0x3E],
        [0x6E, 0x45, 0x2B],
    ];
    const HAIR: [[u8; 3]; 6] = [
        [0x3B, 0x24, 0x16],
        [0x5E, 0x3A, 0x1F],
        [0x1E, 0x1A, 0x1A],
        [0xC9, 0x8A, 0x2E],
        [0xA8, 0x3A, 0x1E],
        [0xD8, 0xC8, 0xA0],
    ];
    const EYES: [[u8; 3]; 4] = [
        [0x2E, 0x4C, 0xA8],
        [0x3E, 0x7A, 0x3A],
        [0x5A, 0x3A, 0x22],
        [0x6A, 0x4A, 0x9A],
    ];
    const BG: [[u8; 3]; 5] = [
        [0xC9, 0xB6, 0xF2],
        [0xA9, 0xDC, 0xF0],
        [0xBF, 0xE8, 0xB0],
        [0xF5, 0xD0, 0xA0],
        [0xF2, 0xB8, 0xC8],
    ];
    let skin = SKIN[next() as usize % SKIN.len()];
    let hair = HAIR[next() as usize % HAIR.len()];
    let eyes = EYES[next() as usize % EYES.len()];
    let bg = BG[next() as usize % BG.len()];
    let fringe = next() % 3;

    // The face sits inside a 12x12 tile so the circular crop applied in
    // CSS doesn't cut its corners off.
    let tile = 12u32;
    let mut img = Image::new(tile * px_per_cell, tile * px_per_cell);
    img.fill_rect(0, 0, img.width, img.height, shade(bg, 1.0));
    for y in 0..8u32 {
        for x in 0..8u32 {
            let jitter = 0.92 + (next() % 17) as f32 / 100.0;
            let is_hair = y < 2
                || (y == 2 && (x == 0 || x == 7 || (fringe == 1 && x < 3) || (fringe == 2 && x > 4)))
                || (y == 3 && (x == 0 || x == 7) && fringe != 0);
            let color = if is_hair {
                shade(hair, jitter)
            } else if y == 4 && (x == 1 || x == 6) {
                [0xFF, 0xFF, 0xFF, 0xFF]
            } else if y == 4 && (x == 2 || x == 5) {
                shade(eyes, 1.0)
            } else if y == 5 && (x == 3 || x == 4) {
                shade(skin, 0.82)
            } else if y == 6 && (2..6).contains(&x) {
                shade(skin, 0.68)
            } else {
                shade(skin, jitter)
            };
            img.fill_rect(
                ((x + 2) * px_per_cell) as i32,
                ((y + 2) * px_per_cell) as i32,
                px_per_cell,
                px_per_cell,
                color,
            );
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_buffer_matches_dimensions() {
        let img = render_text("PLAY", 3, [255; 4], [0, 0, 0, 255]);
        assert_eq!(img.width, (text_width_cells("PLAY") + 1) * 3);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
    }

    #[test]
    fn lowercase_renders_like_uppercase() {
        let a = render_text("play", 2, [255; 4], [0, 0, 0, 255]);
        let b = render_text("PLAY", 2, [255; 4], [0, 0, 0, 255]);
        assert_eq!(a.rgba, b.rgba);
    }

    #[test]
    fn avatar_is_deterministic_per_seed() {
        assert_eq!(render_avatar("abc", 2).rgba, render_avatar("abc", 2).rgba);
        assert_ne!(render_avatar("abc", 2).rgba, render_avatar("xyz", 2).rgba);
    }
}
