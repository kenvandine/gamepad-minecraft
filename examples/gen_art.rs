// gamepad-minecraft - a controller-first Minecraft Java launcher
//
// Copyright (C) 2026 Ken VanDine
// SPDX-License-Identifier: GPL-3.0-or-later

//! Generates the launcher's bundled artwork into `data/art/`:
//!
//! - `hero.png` - the Play tab's key art: an original Nether wastes
//!   scene - a player in enchanted, red-trimmed netherite armour on a
//!   netherrack plateau above a lava sea, lavafalls pouring through the
//!   haze - rendered by the small voxel ray tracer below (procedural
//!   16x16 block textures, flood-filled block light, light shafts
//!   through the cliffs, volumetric fog, bloom). The player wears the
//!   skin in `data/art/hero-skin.png`, mapped texel for texel.
//! - `logo.png` - the blocky stone "MINECRAFT" wordmark with "JAVA
//!   EDITION" underneath.
//! - `grass_block.png` - the isometric grass block icon shown beside the
//!   selected installation.
//!
//! Everything is drawn from code - no Mojang artwork, fonts or textures
//! are used - and the output is deterministic, so re-running this only
//! changes the PNGs when this file changes. The launcher just bundles
//! the results with `include_bytes!`; nothing here runs at startup.
//!
//!     cargo run --release --example gen_art            # full quality
//!     cargo run --release --example gen_art -- preview # quick, low-res
//!     cargo run --release --example gen_art -- closeup # just the player

use std::ops::{Add, Mul, Neg, Sub};
use std::path::Path;

use gamepad_minecraft::pixelart;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

// ─── Math ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, s: f32) -> V3 {
        v3(self.x * s, self.y * s, self.z * s)
    }
}
impl Mul<V3> for V3 {
    type Output = V3;
    fn mul(self, o: V3) -> V3 {
        v3(self.x * o.x, self.y * o.y, self.z * o.z)
    }
}
impl Neg for V3 {
    type Output = V3;
    fn neg(self) -> V3 {
        v3(-self.x, -self.y, -self.z)
    }
}

impl V3 {
    fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: V3) -> V3 {
        v3(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    fn norm(self) -> V3 {
        self * (1.0 / self.dot(self).sqrt())
    }
    fn get(self, i: usize) -> f32 {
        [self.x, self.y, self.z][i]
    }
    fn max_c(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }
}

fn mix(a: V3, b: V3, t: f32) -> V3 {
    a * (1.0 - t) + b * t
}

/// sRGB hex -> linear-light colour.
fn rgb(hex: u32) -> V3 {
    let c = |s: u32| ((hex >> s) & 0xFF) as f32 / 255.0;
    let lin = |v: f32| v.powf(2.2);
    v3(lin(c(16)), lin(c(8)), lin(c(0)))
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

fn h3(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    hash(
        (x as u32).wrapping_mul(73_856_093)
            ^ (y as u32).wrapping_mul(19_349_663)
            ^ (z as u32).wrapping_mul(83_492_791)
            ^ seed.wrapping_mul(2_654_435_761),
    )
}

fn r3(x: i32, y: i32, z: i32, seed: u32) -> f32 {
    h3(x, y, z, seed) as f32 / u32::MAX as f32
}

/// Smooth 2D value noise in [0, 1].
fn noise2(x: f32, z: f32, seed: u32) -> f32 {
    let (xi, zi) = (x.floor() as i32, z.floor() as i32);
    let (fx, fz) = (x - x.floor(), z - z.floor());
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sz) = (s(fx), s(fz));
    let a = r3(xi, 0, zi, seed);
    let b = r3(xi + 1, 0, zi, seed);
    let c = r3(xi, 0, zi + 1, seed);
    let d = r3(xi + 1, 0, zi + 1, seed);
    (a * (1.0 - sx) + b * sx) * (1.0 - sz) + (c * (1.0 - sx) + d * sx) * sz
}

fn fbm2(x: f32, z: f32, seed: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    for o in 0..4 {
        sum += noise2(x * freq, z * freq, seed + o) * amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / 0.9375
}

// ─── Blocks & textures ──────────────────────────────────────────────

const AIR: u8 = 0;
const NETHERRACK: u8 = 1;
const QUARTZ_ORE: u8 = 2;
const LAVA: u8 = 3;
const BLACKSTONE: u8 = 4;
const MAGMA: u8 = 5;
const GLOWSTONE: u8 = 6;
// Everything from here on is a "cross" plant: two diagonal textured
// planes inside the cell rather than a full cube.
const FIRE: u8 = 7;

fn is_cross(b: u8) -> bool {
    b >= FIRE
}
fn is_opaque(b: u8) -> bool {
    b != AIR && !is_cross(b)
}

#[derive(Clone, Copy, PartialEq)]
enum Face {
    Top,
    Bottom,
    Side,
}

struct Texel {
    col: V3,
    emit: V3,
    alpha: bool,
}

fn solid(col: V3) -> Texel {
    Texel { col, emit: V3::default(), alpha: true }
}
fn glowing(col: V3, k: f32) -> Texel {
    Texel { col, emit: col * k, alpha: true }
}
const CLEAR: Texel = Texel { col: v3(0.0, 0.0, 0.0), emit: v3(0.0, 0.0, 0.0), alpha: false };

/// Netherrack, as the game draws it: one 16x16 tile repeated on every
/// face of every block (no per-block variation, so the block grid shows
/// on big surfaces). A fine mottle of muted maroons: small lumps, light
/// in the middle and each its own shade, set in staggered rows with dark
/// seams between them - a faint diamond lattice from a distance.
fn netherrack(tx: i32, ty: i32) -> Texel {
    const PALETTE: [u32; 7] = [0x3a1414, 0x4f1d1c, 0x632725, 0x763331, 0x8a403c, 0xa04f4a, 0xb2605a];
    // Distances to the nearest two lump centres (wrapping), and the
    // nearest one's shade.
    let (mut f1, mut f2, mut shade) = (f32::MAX, f32::MAX, 0.0);
    for gy in 0..4 {
        for gx in 0..4 {
            let cx = (gx * 4 + (gy % 2) * 2) as f32 + r3(gx, gy, 1, 66) * 2.0;
            let cy = (gy * 4) as f32 + r3(gx, gy, 2, 66) * 2.0;
            let wrap = |d: f32| d.abs().min(16.0 - d.abs());
            let (dx, dy) = (wrap(tx as f32 + 0.5 - cx), wrap(ty as f32 + 0.5 - cy));
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < f1 {
                (f2, f1, shade) = (f1, dist, r3(gx, gy, 3, 66));
            } else if dist < f2 {
                f2 = dist;
            }
        }
    }
    let seam = f2 - f1;
    let v = (seam / 2.0).min(1.0) * 0.6 + shade * 0.3 + (r3(tx, ty, 0, 67) - 0.5) * 0.7;
    let mut level = ((v * 4.6).round() as i32).clamp(0, 6);
    if seam < 0.45 {
        level = level.min(1);
    }
    solid(rgb(PALETTE[level as usize]))
}

/// Nether quartz ore: netherrack with crescent-shaped veins of milky
/// quartz, `W` bright, `w` lit, `q` shadowed.
fn quartz_ore(tx: i32, ty: i32) -> Texel {
    const VEINS: [&str; 16] = [
        "................",
        "....wWWw........",
        "..wWWwqqWw......",
        ".wWq.....qWw....",
        ".Wq.......qW....",
        ".q....wWw..q....",
        ".....wWqWw......",
        "....wWq..qW..wW.",
        "...qWq....q.wWq.",
        "....q......wWq..",
        "..wW......wWq...",
        ".wWWw.....qq....",
        ".qqWWw...wW.....",
        "...qqWw.wWq.....",
        ".....qq.qq......",
        "................",
    ];
    match VEINS[ty as usize].as_bytes()[tx as usize] {
        b'W' => solid(rgb(0xf4ecdc)),
        b'w' => solid(rgb(0xd2c4ac)),
        b'q' => solid(rgb(0x8a7c68)),
        _ => netherrack(tx, ty),
    }
}

/// Worley-ish distance to the nearest of a few jittered points in a
/// 16x16 tile, wrapping - the cellular blobs of the lava texture.
fn cells16(tx: i32, ty: i32, seed: u32) -> f32 {
    let mut best = f32::MAX;
    for i in 0..7 {
        let cx = r3(i, 0, 0, seed) * 16.0;
        let cy = r3(i, 1, 0, seed) * 16.0;
        let dx = ((tx as f32 + 0.5 - cx).abs()).min(16.0 - (tx as f32 + 0.5 - cx).abs());
        let dy = ((ty as f32 + 0.5 - cy).abs()).min(16.0 - (ty as f32 + 0.5 - cy).abs());
        best = best.min((dx * dx + dy * dy).sqrt());
    }
    best
}

fn texel(b: u8, face: Face, tx: i32, ty: i32, cell: (i32, i32, i32)) -> Texel {
    let s = h3(cell.0, cell.1, cell.2, b as u32);
    let r = r3(tx, ty, 1, s);
    let r2 = r3(tx, ty, 2, s);
    match b {
        NETHERRACK => netherrack(tx, ty),
        QUARTZ_ORE => quartz_ore(tx, ty),
        LAVA => {
            // Bright yellow-orange blobs threaded with darker veins, the
            // tile shifted per block so neighbours don't repeat.
            let seed = 11 + (h3(cell.0, 0, cell.2, 3) % 4);
            let d = cells16(tx, ty, seed);
            let t = (d / 5.0).min(1.0);
            let col = if t < 0.35 {
                mix(rgb(0xffe27a), rgb(0xffb43a), t / 0.35)
            } else {
                mix(rgb(0xff9a2a), rgb(0xc8400e), (t - 0.35) / 0.65)
            };
            glowing(col * (0.92 + r * 0.16), 3.4)
        }
        BLACKSTONE => {
            let mut col = mix(rgb(0x1c181e), rgb(0x3a3238), r3(tx / 2, ty, 4, s));
            if r2 > 0.95 {
                col = rgb(0x4e444c);
            }
            solid(col)
        }
        MAGMA => {
            let crack = r3(tx / 3, ty / 3, 5, s) < 0.45 && (tx % 3 == 0 || ty % 3 == 1);
            if crack {
                glowing(mix(rgb(0xff6a10), rgb(0xffb040), r), 2.6)
            } else {
                solid(mix(rgb(0x30100a), rgb(0x52200e), r))
            }
        }
        GLOWSTONE => {
            let col = if r > 0.75 {
                rgb(0xfff0b0)
            } else if r < 0.25 {
                rgb(0xb07a30)
            } else {
                rgb(0xf0c060)
            };
            glowing(col, 2.0)
        }
        FIRE => {
            let height = 5.0 + 10.0 * noise2(tx as f32 * 0.55, s as f32 * 0.001, 13);
            if (15 - ty) as f32 <= height {
                let t = (15 - ty) as f32 / 15.0;
                glowing(mix(rgb(0xffd070), rgb(0xff4a0a), t), 2.2)
            } else {
                CLEAR
            }
        }
        _ => {
            let _ = face;
            solid(rgb(0xff00ff))
        }
    }
}

/// Coloured block light each emitter seeds into the light volume.
fn emission(b: u8) -> V3 {
    match b {
        LAVA => v3(2.6, 0.62, 0.1),
        FIRE => v3(3.2, 1.5, 0.5),
        MAGMA => v3(0.9, 0.3, 0.08),
        GLOWSTONE => v3(2.0, 1.4, 0.6),
        _ => V3::default(),
    }
}

// ─── World ──────────────────────────────────────────────────────────

const W: i32 = 128;
const H: i32 = 64;
const D: i32 = 128;

struct World {
    cells: Vec<u8>,
    light: Vec<V3>,
    sun: Vec<f32>,
    ground: Vec<i32>,
}

fn idx(x: i32, y: i32, z: i32) -> usize {
    ((z * H + y) * W + x) as usize
}
fn in_bounds(x: i32, y: i32, z: i32) -> bool {
    (0..W).contains(&x) && (0..H).contains(&y) && (0..D).contains(&z)
}

impl World {
    fn get(&self, x: i32, y: i32, z: i32) -> u8 {
        if in_bounds(x, y, z) {
            self.cells[idx(x, y, z)]
        } else {
            AIR
        }
    }
    fn set(&mut self, x: i32, y: i32, z: i32, b: u8) {
        if in_bounds(x, y, z) {
            self.cells[idx(x, y, z)] = b;
        }
    }
    fn ground_at(&self, x: i32, z: i32) -> i32 {
        self.ground[(z.clamp(0, D - 1) * W + x.clamp(0, W - 1)) as usize]
    }
}

/// Top block of the plateau the hero stands on.
const PLATEAU: i32 = 19;
/// Surface of the lava sea below it.
const SEA: i32 = 8;
/// Where the hero character stands, on the plateau.
const PLAYER_POS: V3 = v3(64.3, PLATEAU as f32 + 1.0, 14.0);
/// Direction *towards* the light: the glow of the lava sea beyond the
/// far cliffs, high and behind the scene, so the haze is backlit and
/// shafts slant down towards the camera through gaps in the cliffs.
fn sun_dir() -> V3 {
    v3(0.12, 1.0, 0.75).norm()
}

/// Ridged noise in [0, 1]: sharp crests, like eroded netherrack.
fn ridged(x: f32, z: f32, seed: u32) -> f32 {
    1.0 - (fbm2(x, z, seed) * 2.0 - 1.0).abs()
}

/// The far cliff wall's front face (z) at a given x.
fn wall_z(x: i32) -> i32 {
    100 + (fbm2(x as f32 * 0.06, 0.5, 50) * 18.0) as i32
}

/// Lavafalls: (x, width, fall from y). The first three pour down the far
/// cliffs; the last is a thin stream falling out of the fog onto the
/// ridges mid-scene, like a leak in the ceiling far above.
const LAVAFALLS: [(i32, i32, i32); 4] = [(30, 4, 56), (82, 2, 60), (104, 3, 50), (71, 1, 63)];

fn build_world() -> World {
    let mut w = World {
        cells: vec![AIR; (W * H * D) as usize],
        light: Vec::new(),
        sun: Vec::new(),
        ground: vec![0; (W * D) as usize],
    };

    // Terrain: a lava sea with netherrack ridges rising out of it, a
    // broad flat plateau in the foreground for the hero, a lower shelf
    // with a lava pool beside it, and towering cliffs at the back.
    for z in 0..D {
        for x in 0..W {
            let (fx, fz) = (x as f32, z as f32);
            // Rolling lowland, mostly just under the lava.
            let mut g = 1.0 + fbm2(fx * 0.05, fz * 0.05, 1) * 7.0;
            // Ridges and islands, taller further back.
            let rise = ((fz - 30.0) / 60.0).clamp(0.0, 1.0);
            let ridge = ridged(fx * 0.035, fz * 0.045, 2);
            g += (ridge - 0.7).max(0.0) * (24.0 + rise * 24.0) * (0.6 + fbm2(fx * 0.1, fz * 0.1, 3) * 0.8);
            g = g.min(10.0 + rise * 10.0);

            // The plateau: a rounded rectangle with a ragged rim.
            let px = (fx - 64.0) / 10.0;
            let pz = (fz - 12.0) / 12.0;
            let rim = 1.0 + (fbm2(fx * 0.12, fz * 0.12, 4) - 0.5) * 0.35;
            let plat = (px.abs().powf(3.0) + pz.abs().powf(3.0)).powf(1.0 / 3.0);
            if plat < rim {
                g = PLATEAU as f32;
            } else if plat < rim + 0.18 {
                // Stepped, crumbling slopes just past the rim.
                let k = (plat - rim) / 0.18;
                g = g.max(PLATEAU as f32 - k * 11.0 - fbm2(fx * 0.4, fz * 0.4, 5) * 3.0);
            }
            // A lower shelf off to the right, holding a lava pool.
            let sx = (fx - 83.0) / 10.0;
            let sz = (fz - 24.0) / 9.0;
            let shelf = sx * sx + sz * sz;
            if shelf < 1.0 {
                g = g.max(PLATEAU as f32 - 3.0);
            }

            // The far cliffs.
            let wz = wall_z(x);
            if z >= wz {
                let gap = fbm2(fx * 0.09, 3.0, 51);
                let top = if gap > 0.62 { 24.0 } else { (H - 2) as f32 };
                g = g.max(top - (fbm2(fx * 0.3, fz * 0.3, 52) * 4.0));
            }

            let g = (g as i32).clamp(1, H - 2);
            w.ground[(z * W + x) as usize] = g;
            for y in 0..=g {
                let rocky = z >= wz && fbm2(fx * 0.2, y as f32 * 0.2 + fz * 0.1, 6) > 0.6;
                let b = if rocky {
                    BLACKSTONE
                } else if r3(x, y, z, 20) < 0.02 {
                    QUARTZ_ORE
                } else {
                    NETHERRACK
                };
                w.set(x, y, z, b);
            }
            // Lava fills every hollow up to the sea's surface.
            for y in g + 1..=SEA {
                w.set(x, y, z, LAVA);
            }
            if shelf < 0.45 && g == PLATEAU - 3 {
                w.set(x, g, z, LAVA);
            }
            if g > SEA && r3(x, g, z, 21) < 0.01 && plat > rim {
                w.set(x, g, z, MAGMA);
            }
        }
    }

    // A few patches of magma on the plateau, kept clear of the hero.
    for z in 2..36 {
        for x in 36..90 {
            let near_hero = (x - 64i32).abs() < 7 && (8..22).contains(&z);
            if w.ground_at(x, z) == PLATEAU && !near_hero && fbm2(x as f32 * 0.3, z as f32 * 0.3, 22) > 0.72 {
                w.set(x, PLATEAU, z, MAGMA);
            }
        }
    }

    // Lavafalls: a column of lava down the cliff face (or out of the
    // sky), pooling where it lands.
    for (i, &(lx, width, from)) in LAVAFALLS.iter().enumerate() {
        let thin = i == LAVAFALLS.len() - 1;
        for x in lx..lx + width {
            let face = if thin { 66 } else { wall_z(x) - 1 };
            for z in face - 1..=face {
                if thin && z != face {
                    continue;
                }
                for y in (SEA..=from).rev() {
                    if is_opaque(w.get(x, y, z)) && w.get(x, y, z) != LAVA {
                        // Splash: a small pool where the stream lands.
                        for dz in -2..=2 {
                            for dx in -2..=2 {
                                if dx * dx + dz * dz <= 5 && w.get(x + dx, y + 1, z + dz) == AIR {
                                    w.set(x + dx, y, z + dz, LAVA);
                                }
                            }
                        }
                        break;
                    }
                    w.set(x, y, z, LAVA);
                }
            }
        }
    }

    // Glowstone clusters clinging high on the cliffs.
    for x in 0..W {
        let wz = wall_z(x);
        for y in 40..H - 1 {
            if r3(x, y, 0, 23) < 0.025 && w.get(x, y, wz - 1) == AIR {
                w.set(x, y, wz - 1, GLOWSTONE);
                if r3(x, y, 1, 23) < 0.5 {
                    w.set(x, y - 1, wz - 1, GLOWSTONE);
                }
            }
        }
    }

    // Fires flickering on netherrack near the lava.
    for z in 0..D {
        for x in 0..W {
            let g = w.ground_at(x, z);
            if g <= SEA || w.get(x, g, z) == LAVA || w.get(x, g + 1, z) != AIR {
                continue;
            }
            let near_lava = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .any(|&(dx, dz)| w.get(x + dx, g, z + dz) == LAVA || w.get(x + dx, g - 1, z + dz) == LAVA);
            if near_lava && r3(x, g, z, 24) < 0.18 {
                w.set(x, g + 1, z, FIRE);
            }
        }
    }

    w.light = compute_block_light(&w);
    w.sun = compute_sun_visibility(&w);
    w
}


/// Minecraft-style flood fill of coloured light out of every emitter,
/// decaying per block and blocked by opaque cells - then sampled
/// trilinearly so the result is smooth rather than stepped.
fn compute_block_light(w: &World) -> Vec<V3> {
    let mut light = vec![V3::default(); w.cells.len()];
    for (i, &b) in w.cells.iter().enumerate() {
        light[i] = emission(b);
    }
    const DECAY: f32 = 0.82;
    for _ in 0..16 {
        let prev = light.clone();
        for z in 0..D {
            for y in 0..H {
                for x in 0..W {
                    let b = w.get(x, y, z);
                    if is_opaque(b) {
                        continue;
                    }
                    let mut best = prev[idx(x, y, z)];
                    for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
                        let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                        if !in_bounds(nx, ny, nz) {
                            continue;
                        }
                        let n = prev[idx(nx, ny, nz)] * DECAY;
                        best = v3(best.x.max(n.x), best.y.max(n.y), best.z.max(n.z));
                    }
                    light[idx(x, y, z)] = best;
                }
            }
        }
    }
    light
}

fn compute_sun_visibility(w: &World) -> Vec<f32> {
    let dir = sun_dir();
    let mut sun = vec![0.0; w.cells.len()];
    // One z-slice per chunk, spread across threads.
    std::thread::scope(|sc| {
        for (z, slice) in sun.chunks_mut((W * H) as usize).enumerate() {
            sc.spawn(move || {
                let z = z as i32;
                for y in 0..H {
                    for x in 0..W {
                        if is_opaque(w.get(x, y, z)) {
                            continue;
                        }
                        let o = v3(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                        if trace_world(w, o, dir, 200.0, true).is_none() {
                            slice[(y * W + x) as usize] = 1.0;
                        }
                    }
                }
            });
        }
    });
    sun
}

/// Trilinear sample of a per-cell field at cell centres.
fn sample_grid<T: Copy + Default + Add<Output = T> + Mul<f32, Output = T>>(field: &[T], p: V3) -> T {
    let q = p - v3(0.5, 0.5, 0.5);
    let (x0, y0, z0) = (q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32);
    let (fx, fy, fz) = (q.x - x0 as f32, q.y - y0 as f32, q.z - z0 as f32);
    let mut acc = T::default();
    for dz in 0..2 {
        for dy in 0..2 {
            for dx in 0..2 {
                let (x, y, z) = (x0 + dx, y0 + dy, z0 + dz);
                let wgt = if dx == 1 { fx } else { 1.0 - fx }
                    * if dy == 1 { fy } else { 1.0 - fy }
                    * if dz == 1 { fz } else { 1.0 - fz };
                let v = if in_bounds(x, y, z) { field[idx(x, y, z)] } else { T::default() };
                acc = acc + v * wgt;
            }
        }
    }
    acc
}

// ─── Ray tracing ────────────────────────────────────────────────────

struct Hit {
    t: f32,
    n: V3,
    col: V3,
    emit: V3,
    /// Plants/entities skip the cube-face ambient occlusion pass.
    flat: bool,
}

fn ray_box(o: V3, d: V3, lo: V3, hi: V3) -> Option<(f32, f32, usize)> {
    let mut t0 = f32::NEG_INFINITY;
    let mut t1 = f32::INFINITY;
    let mut axis = 0;
    for i in 0..3 {
        let inv = 1.0 / d.get(i);
        let mut a = (lo.get(i) - o.get(i)) * inv;
        let mut b = (hi.get(i) - o.get(i)) * inv;
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        if a > t0 {
            t0 = a;
            axis = i;
        }
        t1 = t1.min(b);
    }
    (t0 <= t1 && t1 > 0.0).then_some((t0, t1, axis))
}

/// Amanatides-Woo voxel walk. `shadow` skips fire (it shouldn't cast
/// shadows) and the texture lookups nobody will read.
fn trace_world(w: &World, o: V3, d: V3, tmax: f32, shadow: bool) -> Option<Hit> {
    let (t_enter, t_exit, enter_axis) =
        ray_box(o, d, v3(0.0, 0.0, 0.0), v3(W as f32, H as f32, D as f32))?;
    let mut t = t_enter.max(0.0);
    let p = o + d * (t + 1e-4);
    let mut cell = [p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32];
    let step = [d.x.signum() as i32, d.y.signum() as i32, d.z.signum() as i32];
    let mut t_max = [0.0f32; 3];
    let mut t_delta = [0.0f32; 3];
    for i in 0..3 {
        let di = d.get(i);
        t_delta[i] = if di != 0.0 { (1.0 / di).abs() } else { f32::INFINITY };
        let boundary = if di > 0.0 { cell[i] as f32 + 1.0 } else { cell[i] as f32 };
        t_max[i] = if di != 0.0 { (boundary - o.get(i)) / di } else { f32::INFINITY };
    }
    let mut axis = if t_enter > 0.0 { enter_axis } else { 1 };
    let limit = t_exit.min(tmax);
    while t < limit {
        let b = w.get(cell[0], cell[1], cell[2]);
        let t_next = t_max[0].min(t_max[1]).min(t_max[2]);
        if is_opaque(b) {
            if shadow {
                return Some(Hit { t, n: V3::default(), col: V3::default(), emit: V3::default(), flat: false });
            }
            let mut n = V3::default();
            let sgn = -(step[axis] as f32);
            match axis {
                0 => n.x = sgn,
                1 => n.y = sgn,
                _ => n.z = sgn,
            }
            let hp = o + d * t;
            let l = hp - v3(cell[0] as f32, cell[1] as f32, cell[2] as f32);
            let tc = |v: f32| ((v * 16.0) as i32).clamp(0, 15);
            let (face, tx, ty) = match axis {
                1 => (if n.y > 0.0 { Face::Top } else { Face::Bottom }, tc(l.x), tc(l.z)),
                0 => (Face::Side, tc(l.z), tc(1.0 - l.y)),
                _ => (Face::Side, tc(l.x), tc(1.0 - l.y)),
            };
            let tex = texel(b, face, tx, ty, (cell[0], cell[1], cell[2]));
            return Some(Hit { t, n, col: tex.col, emit: tex.emit, flat: false });
        }
        if is_cross(b) && !(shadow && b == FIRE) {
            if let Some(hit) = cross_hit(o, d, cell, b, t, t_next) {
                return Some(hit);
            }
        }
        axis = if t_max[0] < t_max[1] {
            if t_max[0] < t_max[2] { 0 } else { 2 }
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        t = t_max[axis];
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
    }
    None
}

/// Intersects the two diagonal planes of a plant cell, nearest first.
fn cross_hit(o: V3, d: V3, cell: [i32; 3], b: u8, t0: f32, t1: f32) -> Option<Hit> {
    let c = v3(cell[0] as f32 + 0.5, 0.0, cell[2] as f32 + 0.5);
    let mut best: Option<Hit> = None;
    for sign in [1.0f32, -1.0] {
        // Plane: (x - cx) - sign * (z - cz) = 0
        let denom = d.x - sign * d.z;
        if denom.abs() < 1e-6 {
            continue;
        }
        let t = -((o.x - c.x) - sign * (o.z - c.z)) / denom;
        if t < t0 - 1e-4 || t > t1 + 1e-4 || best.as_ref().is_some_and(|h| h.t < t) {
            continue;
        }
        let p = o + d * t;
        let along = (p.x - c.x + 0.5).clamp(0.0, 0.999);
        let ly = p.y - cell[1] as f32;
        let tx = (along * 16.0) as i32;
        let ty = ((1.0 - ly) * 16.0).clamp(0.0, 15.0) as i32;
        let tex = texel(b, Face::Side, tx, ty, (cell[0], cell[1], cell[2]));
        if tex.alpha {
            let n = v3(1.0, 0.0, -sign).norm();
            let n = if n.dot(d) > 0.0 { -n } else { n };
            best = Some(Hit { t, n: (n + v3(0.0, 0.6, 0.0)).norm(), col: tex.col, emit: tex.emit, flat: true });
        }
    }
    best
}

// ─── The hero character ─────────────────────────────────────────────

/// The hero's skin: a standard 64x64 player skin with slim (3px) arms,
/// read texel for texel, outer layers included.
struct Skin {
    rgba: Vec<[u8; 4]>,
}

impl Skin {
    fn load() -> Skin {
        let bytes = glib::Bytes::from_static(include_bytes!("../data/art/hero-skin.png"));
        let tex = gdk::Texture::from_bytes(&bytes).expect("hero-skin.png is not a PNG");
        assert!(tex.width() == 64 && tex.height() == 64, "hero-skin.png must be a 64x64 skin");
        let mut bgra = vec![0u8; 64 * 64 * 4];
        tex.download(&mut bgra, 64 * 4);
        // GDK hands back premultiplied BGRA.
        let rgba = bgra
            .chunks(4)
            .map(|p| {
                let a = p[3];
                let un = |c: u8| if a == 0 { 0 } else { (c as u32 * 255 / a as u32).min(255) as u8 };
                [un(p[2]), un(p[1]), un(p[0]), a]
            })
            .collect();
        Skin { rgba }
    }

    /// Linear colour of one skin texel, `None` where it's transparent.
    fn at(&self, x: i32, y: i32) -> Option<V3> {
        let [r, g, b, a] = self.rgba[(y.clamp(0, 63) * 64 + x.clamp(0, 63)) as usize];
        (a >= 128).then(|| rgb((r as u32) << 16 | (g as u32) << 8 | b as u32))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Piece {
    Helmet,
    Chestplate,
    Pauldron,
    Leggings,
    Boots,
}

#[derive(Clone, Copy, PartialEq)]
enum Tex {
    /// A box of the skin, by its UV origin in the 64x64 layout; the
    /// outer layer floats a quarter pixel proud of the inner one.
    Skin { u: i32, v: i32, outer: bool },
    /// Armour: shells further out still, as the game draws it, open
    /// where the skin should show through.
    Armour(Piece),
}

impl Tex {
    /// Shells with holes, whose inside faces can show through them.
    fn see_through(self) -> bool {
        matches!(self, Tex::Armour(_) | Tex::Skin { outer: true, .. })
    }
    fn glints(self) -> bool {
        matches!(self, Tex::Armour(_))
    }
}

struct Obb {
    c: V3,
    ax: [V3; 3],
    half: V3,
    tex: Tex,
    /// Texel grid. Outer layers and armour map their inflated boxes
    /// onto the grid of the part they cover.
    grid: [i32; 3],
}

struct Character {
    parts: Vec<Obb>,
    skin: Skin,
}

fn rot_y(v: V3, a: f32) -> V3 {
    v3(v.x * a.cos() + v.z * a.sin(), v.y, -v.x * a.sin() + v.z * a.cos())
}
fn rot_x(v: V3, a: f32) -> V3 {
    v3(v.x, v.y * a.cos() - v.z * a.sin(), v.y * a.sin() + v.z * a.cos())
}

/// A player mid-stride towards the camera, empty-handed, built from
/// the game's own model: sizes are in skin pixels (1/16 block) - 8x8x8
/// head, 8x12x4 body, 3x12x4 slim arms, 4x12x4 legs. The model faces
/// local -z, so its right side is +x.
fn build_character() -> Character {
    let yaw = 0.45;
    let px = 1.0 / 16.0;
    let mut parts = Vec::new();
    let head_turn = -0.25;
    let right_arm = 0.4; // arms swing against the legs
    let left_arm = -0.4;
    let legs = 0.34;
    // (centre, pivot, swing about X, extra yaw) for each bone.
    let head = (v3(0.0, 28.0, 0.0), v3(0.0, 24.0, 0.0), -0.06, head_turn);
    let body = (v3(0.0, 18.0, 0.0), v3(0.0, 18.0, 0.0), 0.0, 0.0);
    let r_arm = (v3(5.5, 18.0, 0.0), v3(5.5, 22.0, 0.0), right_arm, 0.0);
    let l_arm = (v3(-5.5, 18.0, 0.0), v3(-5.5, 22.0, 0.0), left_arm, 0.0);
    let r_leg = (v3(2.0, 6.0, 0.0), v3(2.0, 12.0, 0.0), -legs, 0.0);
    let l_leg = (v3(-2.0, 6.0, 0.0), v3(-2.0, 12.0, 0.0), legs, 0.0);
    let sk = |u, v| Tex::Skin { u, v, outer: false };
    let sk_out = |u, v| Tex::Skin { u, v, outer: true };
    let arm = v3(3.0, 12.0, 4.0);
    let leg = v3(4.0, 12.0, 4.0);
    let out = |s: V3, k: f32| s + v3(k, k, k);
    let shifted = |b: (V3, V3, f32, f32), dx: f32| (b.0 + v3(dx, 0.0, 0.0), b.1, b.2, b.3);
    // (texture, bone, size, texel grid)
    let defs = [
        (sk(0, 0), head, v3(8.0, 8.0, 8.0), [8, 8, 8]),
        (sk_out(32, 0), head, v3(9.0, 9.0, 9.0), [8, 8, 8]),
        (Tex::Armour(Piece::Helmet), head, v3(10.0, 10.0, 10.0), [8, 8, 8]),
        (sk(16, 16), body, v3(8.0, 12.0, 4.0), [8, 12, 4]),
        (sk_out(16, 32), body, v3(8.5, 12.5, 4.5), [8, 12, 4]),
        (Tex::Armour(Piece::Chestplate), body, v3(9.6, 13.6, 5.6), [8, 12, 4]),
        (sk(40, 16), r_arm, arm, [3, 12, 4]),
        (sk_out(40, 32), r_arm, out(arm, 0.5), [3, 12, 4]),
        (Tex::Armour(Piece::Pauldron), shifted(r_arm, 0.5), v3(5.6, 13.6, 5.6), [4, 12, 4]),
        (sk(32, 48), l_arm, arm, [3, 12, 4]),
        (sk_out(48, 48), l_arm, out(arm, 0.5), [3, 12, 4]),
        (Tex::Armour(Piece::Pauldron), shifted(l_arm, -0.5), v3(5.6, 13.6, 5.6), [4, 12, 4]),
        (sk(0, 16), r_leg, leg, [4, 12, 4]),
        (sk_out(0, 32), r_leg, out(leg, 0.5), [4, 12, 4]),
        (Tex::Armour(Piece::Leggings), r_leg, out(leg, 1.0), [4, 12, 4]),
        (Tex::Armour(Piece::Boots), r_leg, out(leg, 1.8), [4, 12, 4]),
        (sk(16, 48), l_leg, leg, [4, 12, 4]),
        (sk_out(0, 48), l_leg, out(leg, 0.5), [4, 12, 4]),
        (Tex::Armour(Piece::Leggings), l_leg, out(leg, 1.0), [4, 12, 4]),
        (Tex::Armour(Piece::Boots), l_leg, out(leg, 1.8), [4, 12, 4]),
    ];
    for (tex, (centre, pivot, swing, extra_yaw), size, grid) in defs {
        let limb = |v: V3| rot_y(rot_x(v, swing), extra_yaw);
        let model_c = pivot + rot_y(rot_x(centre - pivot, swing), extra_yaw);
        let world_c = PLAYER_POS + rot_y(model_c, yaw) * px;
        let tf = |v: V3| rot_y(limb(v), yaw);
        let ax = [tf(v3(1.0, 0.0, 0.0)), tf(v3(0.0, 1.0, 0.0)), tf(v3(0.0, 0.0, 1.0))];
        parts.push(Obb { c: world_c, ax, half: size * (0.5 * px), tex, grid });
    }
    Character { parts, skin: Skin::load() }
}

/// Which side of a box a ray hit, in the model's own terms.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Front,
    Back,
    Top,
    Bottom,
    /// The model's right (+x) and left sides.
    Right,
    Left,
}

/// The skin texel for a cell of a box whose net starts at (`u0`, `v0`),
/// using the game's layout: top and bottom above, then right side,
/// front, left side and back in a row. `c` counts cells from the box's
/// left (-x), bottom and front corner.
fn skin_texel(skin: &Skin, u0: i32, v0: i32, side: Side, c: [i32; 3], g: [i32; 3]) -> Option<V3> {
    let [w, h, d] = g;
    let row = v0 + d + (h - 1 - c[1]);
    let (x, y) = match side {
        Side::Front => (u0 + d + (w - 1 - c[0]), row),
        Side::Back => (u0 + 2 * d + w + c[0], row),
        Side::Right => (u0 + (d - 1 - c[2]), row),
        Side::Left => (u0 + d + w + c[2], row),
        Side::Top => (u0 + d + (w - 1 - c[0]), v0 + (d - 1 - c[2])),
        Side::Bottom => (u0 + d + w + (w - 1 - c[0]), v0 + c[2]),
    };
    skin.at(x, y)
}

/// Netherite armour with a redstone-red trim, painted from per-face
/// maps (rows top down, columns left to right as you face it; sides run
/// front to back): `.` plate, `h` raised plate, `d` seam, `r`/`R` trim
/// and its glinting edge, ` ` open. Plates get a lit top edge and a
/// shadowed bottom edge wherever they border an opening.
fn armour_texel(map: &[&str], u: i32, v: i32, n: f32) -> Option<V3> {
    let at = |u: i32, v: i32| -> u8 {
        if v < 0 || u < 0 {
            return b'.';
        }
        map.get(v as usize).and_then(|r| r.as_bytes().get(u as usize).copied()).unwrap_or(b' ')
    };
    let c = at(u, v);
    // Netherite: a dark, faintly purple-brown alloy in three tones.
    let plate = if n < 0.3 {
        rgb(0x2f282b)
    } else if n < 0.75 {
        rgb(0x3a3135)
    } else {
        rgb(0x463c40)
    };
    let bevel = if at(u, v - 1) == b' ' {
        1.35
    } else if at(u, v + 1) == b' ' || v as usize == map.len() - 1 {
        0.7
    } else {
        1.0
    };
    Some(match c {
        b' ' => return None,
        b'h' => mix(rgb(0x564b4f), rgb(0x62575b), n),
        b'd' => rgb(0x1f191c),
        b'r' => mix(rgb(0x8a1016), rgb(0xa8161c), n) * bevel,
        b'R' => mix(rgb(0xc42420), rgb(0xd8342a), n),
        _ => plate * bevel,
    })
}

fn armour_map(piece: Piece, side: Side) -> &'static [&'static str] {
    match (piece, side) {
        // Open at the face, with a nose guard down the middle.
        (Piece::Helmet, Side::Front) => {
            &["hhhhhhhh", "h..rr..h", ".  dd  .", ".  dd  .", ".      .", "r      r", ".      .", "d      d"]
        }
        (Piece::Helmet, Side::Back) => {
            &["hhhhhhhh", "........", "...rr...", "..r..r..", "........", "........", "d......d", "dddddddd"]
        }
        (Piece::Helmet, Side::Top) => {
            &["hhhhhhhh", "h......h", "h..rr..h", "h.r..r.h", "h.r..r.h", "h..rr..h", "h......h", "hhhhhhhh"]
        }
        (Piece::Helmet, Side::Bottom) => &[],
        (Piece::Helmet, _) => {
            &["hhhhhhhh", "........", "..r..r..", "...rr...", "........", "d......d", "........", "dddddddd"]
        }
        // Open at the collar so the shirt shows; a trim chevron across
        // the chest.
        (Piece::Chestplate, Side::Front) => &[
            "hhh  hhh", ".hd  dh.", "..hddh..", "r......r", ".r....r.", "..rRRr..", "...dd...", "d......d", "........",
            "..d..d..", "hhhhhhhh", "dddddddd",
        ],
        (Piece::Chestplate, Side::Back) => &[
            "hhhhhhhh", "h......h", "........", "..rRRr..", "........", "...dd...", "d......d", "........",
            "..d..d..", "........", "hhhhhhhh", "dddddddd",
        ],
        (Piece::Chestplate, Side::Top) => &["hhh  hhh", "h.h  h.h", "h.h  h.h", "hhhhhhhh"],
        (Piece::Chestplate, Side::Bottom) => &[],
        (Piece::Chestplate, _) => {
            &["hhhh", "....", "r..r", "....", "d..d", "....", "....", "....", "d..d", "....", "hhhh", "dddd"]
        }
        // Shoulder guards over the upper arm, then the coat sleeve.
        (Piece::Pauldron, Side::Top) => &["hhhh", "h..h", "h..h", "hhhh"],
        (Piece::Pauldron, Side::Bottom) => &[],
        (Piece::Pauldron, _) => &["hhhh", "h..h", "rRRr", "....", "d..d", "....", "....", "hhhh", "dddd"],
        // Leggings down to the knee...
        (Piece::Leggings, Side::Top | Side::Bottom) => &[],
        (Piece::Leggings, _) => &["hhhh", "....", "r..r", ".rr.", "....", "d..d", "....", "....", "dddd"],
        // ...then the boots, with a turned-down cuff.
        (Piece::Boots, Side::Top) => &[],
        (Piece::Boots, Side::Bottom) => &["dddd", "dddd", "dddd", "dddd"],
        (Piece::Boots, _) => &["", "", "", "", "", "", "", "", "hhhh", ".rr.", "....", "dddd"],
    }
}

/// The purple shimmer enchanted gear gets: diagonal bands sweeping
/// across the surface in world space.
fn enchant_glint(p: V3) -> V3 {
    let band = (0.5 + 0.5 * ((p.x - p.z) * 6.0 + p.y * 9.0).sin()).powf(6.0);
    let fine = 0.5 + 0.5 * ((p.x + p.z) * 23.0 - p.y * 11.0).sin();
    v3(0.30, 0.10, 0.75) * (0.012 + band * 0.045 + fine * band * 0.02)
}

fn part_texel(ch: &Character, tex: Tex, side: Side, c: [i32; 3], g: [i32; 3]) -> Option<V3> {
    let n = r3(c[0] * 7 + c[2], c[1], side as i32, 3);
    match tex {
        Tex::Skin { u, v, .. } => skin_texel(&ch.skin, u, v, side, c, g),
        Tex::Armour(piece) => {
            // As you face each side: left to right, top down.
            let (u, v) = match side {
                Side::Front => (g[0] - 1 - c[0], g[1] - 1 - c[1]),
                Side::Back => (c[0], g[1] - 1 - c[1]),
                Side::Right | Side::Left => (c[2], g[1] - 1 - c[1]),
                Side::Top | Side::Bottom => (c[0], c[2]),
            };
            armour_texel(armour_map(piece, side), u, v, n)
        }
    }
}

fn trace_character(ch: &Character, o: V3, d: V3, tmax: f32) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    for p in &ch.parts {
        let rel = o - p.c;
        let lo_ = v3(rel.dot(p.ax[0]), rel.dot(p.ax[1]), rel.dot(p.ax[2]));
        let ld = v3(d.dot(p.ax[0]), d.dot(p.ax[1]), d.dot(p.ax[2]));
        let Some((t0, t1, axis)) = ray_box(lo_, ld, -p.half, p.half) else { continue };
        if t0 <= 0.0 || t0 > tmax || best.as_ref().is_some_and(|b| b.t < t0) {
            continue;
        }
        // Shells have holes, so a ray through one can still hit the
        // far side from the inside; try entry, then exit.
        for (t, axis, inside) in [(t0, axis, false), (t1, exit_axis(lo_, ld, p.half, t1), true)] {
            if t > tmax || best.as_ref().is_some_and(|b| b.t < t) || (inside && !p.tex.see_through()) {
                continue;
            }
            let lp = lo_ + ld * t;
            // Which face: from outside, the one the ray enters through;
            // from inside, the one it's nearest.
            let sign = if inside { lp.get(axis).signum() } else { -ld.get(axis).signum() };
            let n = p.ax[axis] * if inside { -sign } else { sign };
            let g = p.grid;
            let cell = |i: usize| {
                let f = (lp.get(i) + p.half.get(i)) / (2.0 * p.half.get(i));
                ((f * g[i] as f32) as i32).clamp(0, g[i] - 1)
            };
            // Cells from the left (-x), bottom and front (-z) corner.
            let c = [cell(0), cell(1), cell(2)];
            let side = match axis {
                0 if sign > 0.0 => Side::Right,
                0 => Side::Left,
                1 if sign > 0.0 => Side::Top,
                1 => Side::Bottom,
                _ if sign < 0.0 => Side::Front,
                _ => Side::Back,
            };
            let Some(col) = part_texel(ch, p.tex, side, c, g) else {
                continue;
            };
            let col = if inside { col * 0.5 } else { col };
            let emit = if p.tex.glints() { enchant_glint(o + d * t) } else { V3::default() };
            best = Some(Hit { t, n, col, emit, flat: true });
            break;
        }
    }
    best
}

/// The axis whose slab a ray leaves a box through at `t`.
fn exit_axis(o: V3, d: V3, half: V3, t: f32) -> usize {
    let p = o + d * t;
    (0..3)
        .min_by(|&a, &b| {
            let ea = (p.get(a).abs() - half.get(a)).abs();
            let eb = (p.get(b).abs() - half.get(b)).abs();
            ea.total_cmp(&eb)
        })
        .unwrap()
}

// ─── Shading ────────────────────────────────────────────────────────

struct Scene {
    world: World,
    character: Character,
}

fn occluded(s: &Scene, p: V3, d: V3) -> bool {
    trace_world(&s.world, p, d, 200.0, true).is_some() || trace_character(&s.character, p, d, 200.0).is_some()
}

fn ambient_occlusion(w: &World, p: V3, n: V3) -> f32 {
    let q = p + n * 0.5;
    let (t1, t2) = if n.y.abs() > 0.5 {
        (v3(1.0, 0.0, 0.0), v3(0.0, 0.0, 1.0))
    } else {
        (v3(0.0, 1.0, 0.0), n.cross(v3(0.0, 1.0, 0.0)))
    };
    let mut occ = 0.0;
    for (a, b) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7, 0.7), (-0.7, 0.7), (0.7, -0.7), (-0.7, -0.7)] {
        let s = q + t1 * (a * 0.6) + t2 * (b * 0.6);
        if is_opaque(w.get(s.x.floor() as i32, s.y.floor() as i32, s.z.floor() as i32)) {
            occ += 1.0;
        }
    }
    1.0 - occ * 0.075
}

const SUN_COL: V3 = v3(2.6, 0.78, 0.18);
const AMBIENT: V3 = v3(0.12, 0.075, 0.065);
/// Light on surfaces from the glow beyond: warm, but much less saturated
/// than the haze, so block textures keep their own colours.
const SURFACE_LIGHT: V3 = v3(2.3, 1.3, 0.85);
/// The haze's own glow, lit from every side by the lava sea.
const FOG_COL: V3 = v3(0.26, 0.045, 0.008);
const MAX_DIST: f32 = 180.0;

/// Henyey-Greenstein forward scattering towards the backlight.
fn fog_phase(d: V3) -> f32 {
    let g = 0.55;
    (1.0 - g * g) / (1.0 + g * g - 2.0 * g * d.dot(sun_dir())).powf(1.5) * 0.25
}

fn fog_inscatter(sun: f32, glow: V3, phase: f32) -> V3 {
    SUN_COL * (sun * phase * 0.6) + glow * 0.1 + FOG_COL
}

fn background(d: V3) -> V3 {
    // Past the edge of the world: nothing but haze, so it melts into
    // the fog as if it went on forever.
    fog_inscatter(1.0, V3::default(), fog_phase(d))
}

fn shade_ray(s: &Scene, o: V3, d: V3, jitter: f32) -> V3 {
    let hw = trace_world(&s.world, o, d, MAX_DIST, false);
    let t_world = hw.as_ref().map_or(MAX_DIST, |h| h.t);
    let hc = trace_character(&s.character, o, d, t_world);
    let is_char = hc.is_some();
    let hit = hc.or(hw);

    let (surface, t_end) = match &hit {
        None => (background(d), MAX_DIST),
        Some(h) => {
            let p = o + d * h.t;
            let lit_p = p + h.n * 0.02;
            let sd = sun_dir();
            let ndl = h.n.dot(sd).max(0.0);
            let direct = if ndl > 0.0 && !occluded(s, lit_p, sd) { SURFACE_LIGHT * (ndl * 0.15) } else { V3::default() };
            let block = sample_grid(&s.world.light, p + h.n * 0.5);
            let ao = if h.flat { 1.0 } else { ambient_occlusion(&s.world, p, h.n) };
            let mut light = direct + block * ao + AMBIENT * ((0.75 + 0.25 * h.n.y) * ao);
            if is_char {
                // A soft key from camera-left so the hero reads clearly,
                // and a hot rim from the lava glow behind them.
                let key = v3(-0.6, 0.5, -0.65).norm();
                let rim = v3(0.5, 0.2, 1.0).norm();
                light = light
                    + v3(1.0, 0.6, 0.4) * (h.n.dot(key).max(0.0) * 1.1)
                    + v3(2.2, 0.8, 0.2) * h.n.dot(rim).max(0.0)
                    + v3(0.12, 0.07, 0.07);
            }
            (h.col * light + h.emit, h.t)
        }
    };

    // Volumetric fog: thick orange haze, backlit, with shafts where the
    // cliffs let the light through and a glow around every emitter.
    let phase = fog_phase(d);
    let steps = ((t_end / 0.7) as usize).clamp(4, 200);
    let dt = t_end / steps as f32;
    let mut trans = 1.0;
    let mut scatter = V3::default();
    for i in 0..steps {
        let p = o + d * ((i as f32 + jitter) * dt);
        let density = 0.016 + 0.006 * (-(p.y - SEA as f32).max(0.0) / 6.0).exp();
        let sun = sample_grid(&s.world.sun, p);
        let glow = sample_grid(&s.world.light, p);
        let inscatter = fog_inscatter(sun, glow, phase);
        scatter = scatter + inscatter * (trans * density * dt);
        trans *= (-density * dt).exp();
    }
    surface * trans + scatter
}

// ─── Image pipeline ─────────────────────────────────────────────────

struct Hdr {
    w: usize,
    h: usize,
    px: Vec<V3>,
}

impl Hdr {
    fn at(&self, x: usize, y: usize) -> V3 {
        self.px[y * self.w + x]
    }
}

/// `closeup` frames just the character, for checking the skin and pose.
fn render_hero(width: usize, height: usize, ss: usize, closeup: bool) -> Hdr {
    let scene = Scene { world: build_world(), character: build_character() };
    let (eye, pitch, fov): (V3, f32, f32) = if closeup {
        (PLAYER_POS + v3(0.0, 1.25, -4.8), -0.02, 50.0)
    } else {
        (PLAYER_POS + v3(-0.5, 5.0, -10.0), -0.33, 70.0)
    };
    let fwd = v3(0.0, pitch.sin(), pitch.cos()).norm();
    let right = fwd.cross(v3(0.0, 1.0, 0.0)).norm() * -1.0;
    let up = right.cross(fwd) * -1.0;
    let tan_h = (fov.to_radians() / 2.0).tan();
    let tan_v = tan_h * height as f32 / width as f32;

    let mut px = vec![V3::default(); width * height];
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get());
    let rows_per = height.div_ceil(threads);
    std::thread::scope(|sc| {
        for (chunk_i, chunk) in px.chunks_mut(rows_per * width).enumerate() {
            let scene = &scene;
            sc.spawn(move || {
                for (i, out) in chunk.iter_mut().enumerate() {
                    let x = i % width;
                    let y = chunk_i * rows_per + i / width;
                    let mut acc = V3::default();
                    for sy in 0..ss {
                        for sx in 0..ss {
                            let u = ((x as f32 + (sx as f32 + 0.5) / ss as f32) / width as f32) * 2.0 - 1.0;
                            let v = ((y as f32 + (sy as f32 + 0.5) / ss as f32) / height as f32) * 2.0 - 1.0;
                            let d = (fwd + right * (u * tan_h) + up * (-v * tan_v)).norm();
                            let jitter = r3(x as i32, y as i32, (sy * ss + sx) as i32, 77);
                            acc = acc + shade_ray(scene, eye, d, jitter);
                        }
                    }
                    *out = acc * (1.0 / (ss * ss) as f32);
                }
            });
        }
    });
    Hdr { w: width, h: height, px }
}

/// Floating embers, drawn into the HDR buffer so bloom picks them up.
fn add_embers(img: &mut Hdr) {
    let scale = img.w as f32 / 1920.0;
    for i in 0..90 {
        let rx = r3(i, 0, 0, 501);
        let ry = r3(i, 1, 0, 501);
        // Bias towards the middle band and lower half, like drifting
        // sparks rising off the forest floor.
        let x = (0.5 + (rx - 0.5) * (0.6 + 0.4 * r3(i, 2, 0, 501))) * img.w as f32;
        let y = (0.3 + ry * 0.55) * img.h as f32;
        let size = ((3.0 + r3(i, 3, 0, 501) * 5.0) * scale).max(2.0) as i32;
        let heat = 0.8 + r3(i, 4, 0, 501) * 1.6;
        let col = mix(v3(1.0, 0.22, 0.04), v3(1.0, 0.55, 0.15), r3(i, 5, 0, 501)) * heat;
        for dy in 0..size {
            for dx in 0..size {
                let (px, py) = (x as i32 + dx, y as i32 + dy);
                if px >= 0 && py >= 0 && (px as usize) < img.w && (py as usize) < img.h {
                    let k = py as usize * img.w + px as usize;
                    img.px[k] = img.px[k] + col;
                }
            }
        }
    }
}

fn blur(src: &[V3], w: usize, h: usize, radius: i32) -> Vec<V3> {
    let sigma = radius as f32 / 2.5;
    let kernel: Vec<f32> = (-radius..=radius).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = kernel.iter().sum();
    let pass = |src: &[V3], horizontal: bool| -> Vec<V3> {
        let mut out = vec![V3::default(); w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = V3::default();
                for (k, wgt) in kernel.iter().enumerate() {
                    let o = k as i32 - radius;
                    let (sx, sy) = if horizontal {
                        ((x as i32 + o).clamp(0, w as i32 - 1) as usize, y)
                    } else {
                        (x, (y as i32 + o).clamp(0, h as i32 - 1) as usize)
                    };
                    acc = acc + src[sy * w + sx] * *wgt;
                }
                out[y * w + x] = acc * (1.0 / sum);
            }
        }
        out
    };
    pass(&pass(src, true), false)
}

fn aces(x: f32) -> f32 {
    ((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14)).clamp(0.0, 1.0)
}

fn finish_hero(img: &Hdr) -> Vec<u8> {
    // Bloom from a quarter-resolution bright pass.
    let (bw, bh) = (img.w / 4, img.h / 4);
    let mut bright = vec![V3::default(); bw * bh];
    for y in 0..bh {
        for x in 0..bw {
            let mut acc = V3::default();
            for dy in 0..4 {
                for dx in 0..4 {
                    let c = img.at(x * 4 + dx, y * 4 + dy);
                    let excess = (c.max_c() - 0.9).max(0.0);
                    acc = acc + c * (excess / c.max_c().max(1e-4));
                }
            }
            bright[y * bw + x] = acc * (1.0 / 16.0);
        }
    }
    let wide = blur(&bright, bw, bh, (bw / 40).max(2) as i32);
    let tight = blur(&bright, bw, bh, (bw / 160).max(1) as i32);

    let mut out = Vec::with_capacity(img.w * img.h * 3);
    for y in 0..img.h {
        for x in 0..img.w {
            let (bx, by) = ((x / 4).min(bw - 1), (y / 4).min(bh - 1));
            let mut c = img.at(x, y) + wide[by * bw + bx] * 0.5 + tight[by * bw + bx] * 0.35;
            let (u, v) = (x as f32 / img.w as f32 - 0.5, y as f32 / img.h as f32 - 0.5);
            let vignette = 1.0 - (u * u * 1.1 + v * v * 1.3).min(1.0) * 0.75;
            // A touch more saturation for the punchy key-art look.
            let luma = c.dot(v3(0.2126, 0.7152, 0.0722));
            c = mix(v3(luma, luma, luma), c, 1.05) * (1.1 * vignette);
            let dither = (r3(x as i32, y as i32, 0, 909) - 0.5) / 255.0;
            for ch in [c.x, c.y, c.z] {
                let srgb = aces(ch).powf(1.0 / 2.2) + dither;
                out.push((srgb * 255.0).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    out
}

// ─── Logo ───────────────────────────────────────────────────────────

/// Chunky two-cell-stroke letters for the wordmark (original glyphs,
/// in the spirit of - not traced from - the game's own logo).
fn logo_glyph(c: char) -> &'static [&'static str; 8] {
    match c {
        'M' => &["##.....##", "###...###", "####.####", "##.###.##", "##..#..##", "##.....##", "##.....##", "##.....##"],
        'I' => &["######", "..##..", "..##..", "..##..", "..##..", "..##..", "..##..", "######"],
        'N' => &["##....##", "###...##", "####..##", "##.##.##", "##..####", "##...###", "##....##", "##....##"],
        'E' => &["#######", "##.....", "##.....", "######.", "##.....", "##.....", "##.....", "#######"],
        'C' => &[".######", "##....#", "##.....", "##.....", "##.....", "##.....", "##....#", ".######"],
        'R' => &["#######.", "##....##", "##....##", "#######.", "##.##...", "##..##..", "##...##.", "##....##"],
        'A' => &["..####..", ".##..##.", "##....##", "##....##", "########", "##....##", "##....##", "##....##"],
        'F' => &["#######", "##.....", "##.....", "######.", "##.....", "##.....", "##.....", "##....."],
        'T' => &["########", "...##...", "...##...", "...##...", "...##...", "...##...", "...##...", "...##..."],
        _ => &["", "", "", "", "", "", "", ""],
    }
}

struct Mask {
    w: i32,
    h: i32,
    on: Vec<bool>,
}

impl Mask {
    fn new(w: i32, h: i32) -> Self {
        Self { w, h, on: vec![false; (w * h) as usize] }
    }
    fn get(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.w && y < self.h && self.on[(y * self.w + x) as usize]
    }
    fn set_rect(&mut self, x: i32, y: i32, s: i32) {
        for py in y..y + s {
            for px in x..x + s {
                if px >= 0 && py >= 0 && px < self.w && py < self.h {
                    self.on[(py * self.w + px) as usize] = true;
                }
            }
        }
    }
    fn dilate(&self, r: i32) -> Mask {
        let mut out = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                if self.get(x, y) {
                    out.set_rect(x - r, y - r, 2 * r + 1);
                }
            }
        }
        out
    }
    fn shifted_union(&self, dx: i32, dy_max: i32) -> Mask {
        let mut out = Mask::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                if (0..=dy_max).any(|dy| self.get(x - dx * dy / dy_max.max(1), y - dy)) {
                    out.on[(y * self.w + x) as usize] = true;
                }
            }
        }
        out
    }
}

/// Paints one line of blocky lettering: textured stone faces, a darker
/// extrusion below, and a thick black outline around both.
#[allow(clippy::too_many_arguments)]
fn paint_lettering(
    img: &mut pixelart::Image,
    cells: &[(i32, i32)],
    x0: i32,
    y0: i32,
    cell: i32,
    depth: i32,
    outline: i32,
    face_lo: u32,
    face_hi: u32,
    side: u32,
    seed: u32,
) {
    let mut face = Mask::new(img.width as i32, img.height as i32);
    for &(cx, cy) in cells {
        face.set_rect(x0 + cx * cell, y0 + cy * cell, cell);
    }
    let body = face.shifted_union(0, depth);
    let ring = body.dilate(outline);
    let texel = (cell / 4).max(1);
    let to_rgb = |hex: u32, k: f32| {
        let c = |s: u32| (((hex >> s) & 0xFF) as f32 * k).clamp(0.0, 255.0) as u8;
        [c(16), c(8), c(0), 255]
    };
    for y in 0..img.height as i32 {
        for x in 0..img.width as i32 {
            let color = if face.get(x, y) {
                let r = r3(x / texel, y / texel, 0, seed);
                // Bevel: lit top edge, shaded bottom edge of each stroke.
                let top_edge = !face.get(x, y - texel);
                let bottom_edge = !face.get(x, y + texel);
                let base = if r > 0.5 { face_hi } else { face_lo };
                let k = if top_edge { 1.18 } else if bottom_edge { 0.78 } else { 0.92 + r * 0.16 };
                to_rgb(base, k)
            } else if body.get(x, y) {
                let r = r3(x / texel, y / texel, 1, seed);
                to_rgb(side, 0.85 + r * 0.3)
            } else if ring.get(x, y) {
                [0x12, 0x10, 0x12, 255]
            } else {
                continue;
            };
            let i = ((y as u32 * img.width + x as u32) * 4) as usize;
            img.rgba[i..i + 4].copy_from_slice(&color);
        }
    }
}

/// Rendered at the size the launcher displays it (~600px wide):
/// `gtk::Picture` draws at the image's natural size.
fn render_logo() -> pixelart::Image {
    let cell = 8;
    let word = "MINECRAFT";
    let mut cells = Vec::new();
    let mut pen = 0;
    for ch in word.chars() {
        let g = logo_glyph(ch);
        for (y, row) in g.iter().enumerate() {
            for (x, b) in row.bytes().enumerate() {
                if b == b'#' {
                    cells.push((pen + x as i32, y as i32));
                }
            }
        }
        pen += g[0].len() as i32 + 1;
    }
    let word_w = (pen - 1) * cell;
    let margin = 6;
    let depth = cell + cell / 2;

    let ed_cell = 5;
    let edition = "JAVA EDITION";
    let ed_w = pixelart::text_width_cells(edition) as i32 * ed_cell;
    let ed_y = margin + 8 * cell + depth - ed_cell * 2;
    let height = ed_y + 7 * ed_cell + ed_cell + margin + 2;
    let width = word_w + margin * 2;
    let mut img = pixelart::Image::new(width as u32, height as u32);

    paint_lettering(&mut img, &cells, margin, margin, cell, depth, 3, 0xa8a4a0, 0xc4c0bb, 0x585452, 1);

    let mut ed_cells = Vec::new();
    pixelart::for_each_text_cell(edition, |x, y| ed_cells.push((x as i32, y as i32)));
    paint_lettering(
        &mut img,
        &ed_cells,
        (width - ed_w) / 2,
        ed_y,
        ed_cell,
        ed_cell,
        2,
        0xe8e4dc,
        0xf6f2ea,
        0x6c6660,
        2,
    );
    img
}

// ─── Grass block icon ───────────────────────────────────────────────

fn render_grass_block(n: i32) -> pixelart::Image {
    let hgt = (n as f32 * 1.12) as i32;
    let (w, h) = (2 * n, n + hgt);
    let mut img = pixelart::Image::new(w as u32, h as u32);
    let grass = |tx: i32, ty: i32| {
        let r = r3(tx, ty, 0, 600);
        if r > 0.85 {
            0x7cc53e
        } else if r < 0.2 {
            0x4a8a28
        } else {
            0x5d9c34
        }
    };
    let dirt = |tx: i32, ty: i32| {
        let r = r3(tx, ty, 1, 600);
        if r > 0.8 {
            0x9a6e48
        } else if r < 0.25 {
            0x6b4a2c
        } else {
            0x86603c
        }
    };
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let nf = n as f32;
            let ap = ((fx - nf) / nf + 2.0 * fy / nf) / 2.0;
            let bp = (2.0 * fy / nf - (fx - nf) / nf) / 2.0;
            let (hex, k) = if (0.0..1.0).contains(&ap) && (0.0..1.0).contains(&bp) {
                (grass((ap * 16.0) as i32, (bp * 16.0) as i32), 1.0)
            } else {
                let left = fx < nf;
                let a = if left { fx / nf } else { (fx - nf) / nf };
                let top = if left { nf / 2.0 + a * nf / 2.0 } else { nf - a * nf / 2.0 };
                let hv = (fy - top) / hgt as f32;
                if !(0.0..1.0).contains(&hv) {
                    continue;
                }
                let (tx, ty) = ((a * 16.0) as i32 + if left { 0 } else { 16 }, (hv * 16.0) as i32);
                let fringe = 3 + (h3(tx, 0, 0, 601) % 3) as i32;
                let hex = if ty < fringe { grass(tx, ty) } else { dirt(tx, ty) };
                (hex, if left { 0.62 } else { 0.8 })
            };
            let c = |s: u32| (((hex >> s) & 0xFF) as f32 * k) as u8;
            let i = ((y * w + x) * 4) as usize;
            img.rgba[i..i + 4].copy_from_slice(&[c(16), c(8), c(0), 255]);
        }
    }
    img
}

// ─── Output ─────────────────────────────────────────────────────────

fn save_png(path: &Path, w: u32, h: u32, data: Vec<u8>, has_alpha: bool) {
    let (format, bpp) = if has_alpha {
        (gdk::MemoryFormat::R8g8b8a8, 4)
    } else {
        (gdk::MemoryFormat::R8g8b8, 3)
    };
    let tex = gdk::MemoryTexture::new(w as i32, h as i32, format, &glib::Bytes::from_owned(data), (w * bpp) as usize);
    tex.save_to_png(path).expect("failed to write PNG");
    println!("wrote {}", path.display());
}

fn main() {
    let preview = std::env::args().any(|a| a == "preview");
    let closeup = std::env::args().any(|a| a == "closeup");
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/art");
    std::fs::create_dir_all(&out_dir).unwrap();

    let logo = render_logo();
    save_png(&out_dir.join("logo.png"), logo.width, logo.height, logo.rgba, true);
    let grass = render_grass_block(48);
    save_png(&out_dir.join("grass_block.png"), grass.width, grass.height, grass.rgba, true);

    let (w, h, ss) = if preview || closeup { (960, 540, 2) } else { (1920, 1080, 3) };
    let started = std::time::Instant::now();
    let mut hdr = render_hero(w, h, ss, closeup);
    if !closeup {
        add_embers(&mut hdr);
    }
    let rgb_bytes = finish_hero(&hdr);
    let name = if closeup {
        "hero-closeup.png"
    } else if preview {
        "hero-preview.png"
    } else {
        "hero.png"
    };
    let path = if preview || closeup {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(name)
    } else {
        out_dir.join(name)
    };
    save_png(&path, w as u32, h as u32, rgb_bytes, false);
    println!("hero rendered in {:.1}s", started.elapsed().as_secs_f32());
}
