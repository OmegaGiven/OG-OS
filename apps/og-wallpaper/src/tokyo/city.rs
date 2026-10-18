//! Procedural neo-Tokyo night skyline for one output size.
//!
//! Output (both full resolution):
//! - `color`: RGBA8 sRGB. RGB is the pixel's color; A is how much of it is
//!   emitted light (lit windows, neon, billboards). The shader modulates that
//!   part over time (flicker, windows switching off, billboard content).
//! - `data`: R32Uint per pixel, `kind | id << 3 | u << 16 | v << 24`. `kind`
//!   says what kind of light the pixel belongs to (see `Kind`), `id` picks its
//!   random behaviour, and `u,v` are billboard-local coordinates (0..255).
//!
//! Everything below `horizon` is bay water. The shader draws it as a
//! rippling reflection of the city, so here it's just a flat dark fill.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

#[derive(Clone, Copy)]
#[repr(u32)]
enum Kind {
    Window = 1,
    Sign = 2,
    Billboard = 3,
    Blinker = 4,
    Tower = 5,
    Star = 6,
}

fn pack(kind: Kind, id: u32, u: f32, v: f32) -> u32 {
    let q = |x: f32| (x.clamp(0.0, 1.0) * 255.0) as u32;
    kind as u32 | ((id & 0x1fff) << 3) | (q(u) << 16) | (q(v) << 24)
}

type Rgb = [f32; 3];

/// sRGB hex → linear RGB.
fn hex(c: u32) -> Rgb {
    let l = |x: u32| {
        let x = x as f32 / 255.0;
        if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
    };
    [l(c >> 16 & 0xff), l(c >> 8 & 0xff), l(c & 0xff)]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn scale(a: Rgb, k: f32) -> Rgb {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn to_srgb8(x: f32) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let s = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0 + 0.5) as u8
}

fn hash2(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xff_ffff) as f32 / 16_777_216.0
}

/// Smooth value noise.
fn vnoise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(fx), s(fy));
    let a = hash2(xi, yi, seed);
    let b = hash2(xi + 1, yi, seed);
    let c = hash2(xi, yi + 1, seed);
    let d = hash2(xi + 1, yi + 1, seed);
    a + (b - a) * sx + (c - a) * sy + (a - b - c + d) * sx * sy
}

fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    let mut v = 0.0;
    let mut amp = 0.5;
    let (mut x, mut y) = (x, y);
    for o in 0..5 {
        v += vnoise(x, y, seed + o) * amp;
        x *= 2.03;
        y *= 2.03;
        amp *= 0.5;
    }
    v
}

pub struct City {
    pub color: Vec<u8>,
    pub data: Vec<u32>,
    pub horizon: f32,
}

struct Canvas {
    w: usize,
    h: usize,
    col: Vec<Rgb>,
    emis: Vec<f32>,
    data: Vec<u32>,
    /// Emitted light to be blurred into bloom at the end.
    glow: Vec<Rgb>,
}

impl Canvas {
    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        (x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h).then(|| y as usize * self.w + x as usize)
    }

    /// Axis-aligned rectangle, [x0, x1) × [y0, y1).
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgb, emis: f32, data: u32) {
        let (xa, xb) = (x0.round().max(0.0) as usize, (x1.round().max(0.0) as usize).min(self.w));
        let (ya, yb) = (y0.round().max(0.0) as usize, (y1.round().max(0.0) as usize).min(self.h));
        for y in ya..yb {
            for x in xa..xb {
                let i = y * self.w + x;
                self.col[i] = c;
                self.emis[i] = emis;
                self.data[i] = data;
                if emis > 0.0 {
                    self.glow[i] = scale(c, emis);
                }
            }
        }
    }

    /// Billboard: emissive white mask whose content the shader paints.
    fn billboard(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, id: u32) {
        let (xa, xb) = (x0.round().max(0.0) as usize, (x1.round().max(0.0) as usize).min(self.w));
        let (ya, yb) = (y0.round().max(0.0) as usize, (y1.round().max(0.0) as usize).min(self.h));
        for y in ya..yb {
            for x in xa..xb {
                let i = y * self.w + x;
                let u = (x as f32 - x0) / (x1 - x0);
                let v = (y as f32 - y0) / (y1 - y0);
                self.col[i] = [1.0, 1.0, 1.0];
                self.emis[i] = 1.0;
                self.data[i] = pack(Kind::Billboard, id, u, v);
                // Bloom approximated with an average billboard tint.
                self.glow[i] = [0.35, 0.18, 0.45];
            }
        }
    }

    /// Anti-aliased capsule from p to q, `width` wide.
    fn line(&mut self, p: (f32, f32), q: (f32, f32), width: f32, c: Rgb, emis: f32, data: u32) {
        self.line_tapered(p, q, width, width, c, emis, data);
    }

    fn line_tapered(&mut self, p: (f32, f32), q: (f32, f32), w0: f32, w1: f32, c: Rgb, emis: f32, data: u32) {
        let r = w0.max(w1) * 0.5 + 1.0;
        let (x0, x1) = ((p.0.min(q.0) - r).floor() as i32, (p.0.max(q.0) + r).ceil() as i32);
        let (y0, y1) = ((p.1.min(q.1) - r).floor() as i32, (p.1.max(q.1) + r).ceil() as i32);
        let (dx, dy) = (q.0 - p.0, q.1 - p.1);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let Some(i) = self.idx(x, y) else { continue };
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = (((cx - p.0) * dx + (cy - p.1) * dy) / len2).clamp(0.0, 1.0);
                let (ex, ey) = (cx - (p.0 + dx * t), cy - (p.1 + dy * t));
                let d = (ex * ex + ey * ey).sqrt();
                let half = (w0 + (w1 - w0) * t) * 0.5;
                let a = (half + 0.5 - d).clamp(0.0, 1.0);
                if a > 0.0 {
                    self.col[i] = mix(self.col[i], c, a);
                    self.emis[i] = self.emis[i] + (emis - self.emis[i]) * a;
                    if a > 0.5 {
                        self.data[i] = data;
                    }
                    if emis > 0.0 {
                        self.glow[i] = mix(self.glow[i], scale(c, emis), a);
                    }
                }
            }
        }
    }

    fn disc(&mut self, cx: f32, cy: f32, r: f32, c: Rgb, alpha: f32, emis: f32, data: u32) {
        let (x0, x1) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (y0, y1) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let Some(i) = self.idx(x, y) else { continue };
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0) * alpha;
                if a > 0.0 {
                    self.col[i] = mix(self.col[i], c, a);
                    self.emis[i] = self.emis[i] + (emis - self.emis[i]) * a;
                    if a > 0.5 && data != 0 {
                        self.data[i] = data;
                    }
                    if emis > 0.0 {
                        self.glow[i] = mix(self.glow[i], scale(c, emis), a);
                    }
                }
            }
        }
    }
}

/// Separable box blur, `passes` times (≈ gaussian), in place.
fn blur(buf: &mut [Rgb], w: usize, h: usize, r: usize, passes: usize) {
    let mut tmp = vec![[0.0f32; 3]; buf.len()];
    let norm = 1.0 / (2 * r + 1) as f32;
    for _ in 0..passes {
        for y in 0..h {
            let row = &buf[y * w..(y + 1) * w];
            let mut acc = [0.0f32; 3];
            for k in 0..=r.min(w - 1) {
                for c in 0..3 {
                    acc[c] += row[k][c];
                }
            }
            for x in 0..w {
                tmp[y * w + x] = scale(acc, norm);
                if x + r + 1 < w {
                    for c in 0..3 {
                        acc[c] += row[x + r + 1][c];
                    }
                }
                if x >= r {
                    for c in 0..3 {
                        acc[c] -= row[x - r][c];
                    }
                }
            }
        }
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for k in 0..=r.min(h - 1) {
                for c in 0..3 {
                    acc[c] += tmp[k * w + x][c];
                }
            }
            for y in 0..h {
                buf[y * w + x] = scale(acc, norm);
                if y + r + 1 < h {
                    for c in 0..3 {
                        acc[c] += tmp[(y + r + 1) * w + x][c];
                    }
                }
                if y >= r {
                    for c in 0..3 {
                        acc[c] -= tmp[(y - r) * w + x][c];
                    }
                }
            }
        }
    }
}

const NEON: [u32; 6] = [0xff2d95, 0x00e5ff, 0xb24bff, 0xff7800, 0x39ff9f, 0xffd23f];

fn neon(rng: &mut StdRng) -> Rgb {
    // Pink and cyan dominate, like the real thing.
    let i = [0, 0, 0, 1, 1, 1, 2, 2, 3, 4, 5][rng.gen_range(0..11)];
    hex(NEON[i])
}

struct Layer {
    min_h: f32,
    max_h: f32,
    min_w: f32,
    max_w: f32,
    gap: (f32, f32),
    wall: Rgb,
    fog: f32,
    window: (f32, f32, f32, f32), // w, h, pitch x, pitch y
    lit: f64,
}

pub fn generate(w: u32, h: u32) -> City {
    let (wf, hf) = (w as f32, h as f32);
    let (wu, hu) = (w as usize, h as usize);
    let mut rng = StdRng::seed_from_u64(0x70_4b_10 ^ ((w as u64) << 20) ^ h as u64);
    let horizon = (hf * 0.76).round();
    let n = wu * hu;
    let mut cv = Canvas { w: wu, h: hu, col: vec![[0.0; 3]; n], emis: vec![0.0; n], data: vec![0; n], glow: vec![[0.0; 3]; n] };

    // --- sky ---------------------------------------------------------------
    let sky_top = hex(0x04050e);
    let sky_mid = hex(0x120a2c);
    let glow_c = hex(0x4a1250);
    let fog = hex(0x3a1a4f);
    let moon = (wf * if wf / hf > 2.5 { 0.82 } else { 0.78 }, hf * 0.17, hf * 0.032);
    for y in 0..horizon as usize {
        let t = y as f32 / horizon;
        let base = if t < 0.55 { mix(sky_top, sky_mid, t / 0.55) } else { mix(sky_mid, glow_c, ((t - 0.55) / 0.45).powf(1.6)) };
        for x in 0..wu {
            // Soft clouds, lit pink-violet from the city below.
            let c = fbm(x as f32 / 420.0, y as f32 / 140.0, 7);
            let cloud = ((c - 0.48) * 3.2).clamp(0.0, 1.0) * (0.25 + 0.75 * t);
            let lit = mix(hex(0x2a1640), hex(0x7a2f6e), t * t);
            cv.col[y * wu + x] = mix(base, lit, cloud * 0.55);
        }
    }
    // Stars, only where the sky is clear and dark.
    for _ in 0..(wf * hf / 3500.0) as usize {
        let (x, y) = (rng.gen_range(0.0..wf), rng.gen_range(0.0..horizon * 0.55));
        let i = y as usize * wu + x as usize;
        let c = fbm(x / 420.0, y / 140.0, 7);
        if c < 0.5 {
            let b = rng.gen_range(0.25..0.8);
            cv.col[i] = mix(cv.col[i], [b, b, b * 1.1], 0.9);
            cv.emis[i] = 1.0;
            cv.data[i] = pack(Kind::Star, rng.gen_range(0..8192), 0.0, 0.0);
        }
    }
    // Moon with a soft halo.
    {
        let halo = hex(0x8a68b0);
        let r_out = moon.2 * 7.0;
        let (x0, x1) = ((moon.0 - r_out).max(0.0) as usize, ((moon.0 + r_out) as usize).min(wu));
        let (y0, y1) = ((moon.1 - r_out).max(0.0) as usize, ((moon.1 + r_out) as usize).min(hu));
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 - moon.0).powi(2) + (y as f32 - moon.1).powi(2)).sqrt() / moon.2;
                let k = 0.16 * (-(d - 1.0).max(0.0) / 1.6).exp();
                let i = y * wu + x;
                cv.col[i] = [cv.col[i][0] + halo[0] * k, cv.col[i][1] + halo[1] * k, cv.col[i][2] + halo[2] * k];
            }
        }
    }
    cv.disc(moon.0, moon.1, moon.2, hex(0xf3e9ff), 1.0, 0.0, 0);
    // Faint maria.
    cv.disc(moon.0 + moon.2 * 0.28, moon.1 - moon.2 * 0.12, moon.2 * 0.32, hex(0xdcd0ea), 0.35, 0.0, 0);
    cv.disc(moon.0 - moon.2 * 0.3, moon.1 + moon.2 * 0.25, moon.2 * 0.22, hex(0xe2d8ee), 0.3, 0.0, 0);

    // --- skyline layers (far → near) ---------------------------------------
    let ultrawide = wf / hf > 2.5;
    let tower_x = wf * if ultrawide { 0.24 } else { 0.30 };
    let spire_x = if ultrawide { Some(wf * 0.66) } else { None };
    let layers = [
        Layer { min_h: 0.10, max_h: 0.30, min_w: 26.0, max_w: 80.0, gap: (0.0, 6.0), wall: hex(0x1a1030), fog: 0.55, window: (2.0, 2.0, 6.0, 7.0), lit: 0.05 },
        Layer { min_h: 0.16, max_h: 0.46, min_w: 40.0, max_w: 120.0, gap: (2.0, 10.0), wall: hex(0x0d0b1f), fog: 0.25, window: (3.0, 4.0, 7.0, 9.0), lit: 0.11 },
        Layer { min_h: 0.22, max_h: 0.62, min_w: 80.0, max_w: 210.0, gap: (8.0, 40.0), wall: hex(0x07060f), fog: 0.06, window: (4.0, 6.0, 10.0, 13.0), lit: 0.10 },
    ];
    let warm = [hex(0xffd9a0), hex(0xffe9c8), hex(0xffc47a)];
    let cool = [hex(0xa8d8ff), hex(0xd0f0ff)];
    let mut light_id: u32 = 1;
    for (li, layer) in layers.iter().enumerate() {
        let mut x = -rng.gen_range(0.0..40.0);
        while x < wf {
            let bw = rng.gen_range(layer.min_w..layer.max_w);
            let mut bh = horizon * rng.gen_range(layer.min_h..layer.max_h);
            // Keep the landmarks visible: near buildings stay low around them.
            if li == 2 {
                let near = |cx: f32| (x + bw * 0.5 - cx).abs() < hf * 0.16;
                if near(tower_x) || spire_x.is_some_and(near) {
                    bh = bh.min(horizon * 0.22);
                }
            }
            let top = horizon - bh;
            let wall = mix(layer.wall, fog, layer.fog);
            cv.rect(x, top, x + bw, horizon, wall, 0.0, 0);
            // Setback crown on some towers.
            if bh > horizon * 0.3 && rng.gen_bool(0.35) {
                let cw = bw * rng.gen_range(0.4..0.7);
                let ch = bh * rng.gen_range(0.06..0.14);
                let cx = x + (bw - cw) * 0.5;
                cv.rect(cx, top - ch, cx + cw, top, wall, 0.0, 0);
                if rng.gen_bool(0.6) {
                    let ax = cx + cw * 0.5;
                    let ah = rng.gen_range(20.0..70.0);
                    cv.line((ax, top - ch), (ax, top - ch - ah), 1.5, wall, 0.0, 0);
                    let id = pack(Kind::Blinker, light_id, 0.0, 0.0);
                    light_id += 1;
                    cv.disc(ax, top - ch - ah, 2.2, hex(0xff2a2a), 1.0, 1.0, id);
                }
            }
            // Windows.
            let (ww, wh, px, py) = layer.window;
            let dim = 1.0 - layer.fog * 0.7;
            let mut yy = top + py;
            // Offices light up in floor-wide bands; between them, scattered.
            let busy_floor = |rng: &mut StdRng| rng.gen_bool(0.12);
            while yy + wh < horizon - 4.0 {
                let floor_lit = busy_floor(&mut rng);
                let mut xx = x + px * 0.6;
                while xx + ww < x + bw - px * 0.4 {
                    if rng.gen_bool(if floor_lit { 0.85 } else { layer.lit }) {
                        let c = if rng.gen_bool(0.75) { warm[rng.gen_range(0..3)] } else { cool[rng.gen_range(0..2)] };
                        let id = pack(Kind::Window, light_id, 0.0, 0.0);
                        light_id += 1;
                        cv.rect(xx, yy, xx + ww, yy + wh, scale(c, dim * 0.8), 1.0, id);
                        // Windows barely bloom; neon does.
                        for gy in yy as usize..(yy + wh) as usize {
                            for gx in xx.max(0.0) as usize..((xx + ww) as usize).min(wu) {
                                if gy < hu {
                                    cv.glow[gy * wu + gx] = scale(cv.glow[gy * wu + gx], 0.15);
                                }
                            }
                        }
                    } else if li > 0 {
                        cv.rect(xx, yy, xx + ww, yy + wh, mix(wall, hex(0x241d3a), 0.5), 0.0, 0);
                    }
                    xx += px;
                }
                yy += py;
            }
            // Neon: vertical signs and billboards on the near and mid layers.
            if li >= 1 && bh > horizon * 0.18 && rng.gen_bool(if li == 2 { 0.55 } else { 0.22 }) {
                let sw = if li == 2 { rng.gen_range(22.0..34.0) } else { rng.gen_range(12.0..18.0) };
                let sh = (bh * rng.gen_range(0.25..0.55)).min(if li == 2 { 300.0 } else { 150.0 });
                let sx = if rng.gen_bool(0.5) { x - sw * 0.6 } else { x + bw - sw * 0.4 };
                let sy = top + bh * rng.gen_range(0.08..0.3);
                let c = neon(&mut rng);
                let id = pack(Kind::Sign, light_id, 0.0, 0.0);
                light_id += 1;
                cv.rect(sx, sy, sx + sw, sy + sh, scale(c, 0.05), 0.0, 0);
                // Glyph-like strokes stacked down the sign.
                let cell = sw - 6.0;
                let mut gy = sy + 4.0;
                while gy + cell < sy + sh - 3.0 {
                    let g = cell / 4.0;
                    for _ in 0..rng.gen_range(3..6) {
                        let (a, b) = (rng.gen_range(0..5) as f32, rng.gen_range(0..5) as f32);
                        let (p, q) = if rng.gen_bool(0.5) {
                            ((sx + 3.0 + a * g, gy + b * g * 0.5), (sx + 3.0 + a * g, gy + (b * 0.5 + rng.gen_range(1..3) as f32) * g))
                        } else {
                            ((sx + 3.0, gy + a * g), (sx + 3.0 + rng.gen_range(2..5) as f32 * g, gy + a * g))
                        };
                        cv.line(p, q, if li == 2 { 2.2 } else { 1.4 }, c, 1.0, id);
                    }
                    gy += cell + 4.0;
                }
                // Neon outline.
                for (p, q) in [((sx, sy), (sx + sw, sy)), ((sx + sw, sy), (sx + sw, sy + sh)), ((sx + sw, sy + sh), (sx, sy + sh)), ((sx, sy + sh), (sx, sy))] {
                    cv.line(p, q, 1.5, c, 1.0, id);
                }
            }
            if li == 2 && bh > horizon * 0.3 && rng.gen_bool(0.3) {
                let bw2 = (bw * rng.gen_range(0.6..0.9)).min(260.0);
                let bh2 = bw2 * rng.gen_range(0.45..0.65);
                let bx = x + (bw - bw2) * 0.5;
                let by = top + bh * rng.gen_range(0.05..0.2);
                cv.rect(bx - 4.0, by - 4.0, bx + bw2 + 4.0, by + bh2 + 4.0, hex(0x0a0a12), 0.0, 0);
                cv.billboard(bx, by, bx + bw2, by + bh2, light_id);
                light_id += 1;
            }
            // Rooftop neon edge on some near buildings.
            if li == 2 && rng.gen_bool(0.25) {
                let c = neon(&mut rng);
                let id = pack(Kind::Sign, light_id, 0.0, 0.0);
                light_id += 1;
                cv.line((x + 2.0, top + 1.0), (x + bw - 2.0, top + 1.0), 2.0, c, 1.0, id);
            }
            x += bw + rng.gen_range(layer.gap.0..layer.gap.1);
        }

        // Landmarks sit between the mid and near layers.
        if li == 1 {
            draw_tower(&mut cv, tower_x, horizon, horizon * 0.66, hf, &mut light_id);
            if let Some(sx) = spire_x {
                draw_spire(&mut cv, sx, horizon, horizon * 0.74, hf, &mut light_id);
            }
        }
    }

    // Mist where the city meets the water.
    for y in 0..horizon as usize {
        let k = (-(horizon - y as f32) / (hf * 0.05)).exp() * 0.28;
        for x in 0..wu {
            let i = y * wu + x;
            cv.col[i] = mix(cv.col[i], fog, k * (1.0 - cv.emis[i] * 0.6));
        }
    }

    // Waterfront: a dark quay with a row of lamps.
    cv.rect(0.0, horizon - 5.0, wf, horizon + 3.0, hex(0x05040a), 0.0, 0);
    let mut lx = rng.gen_range(10.0..40.0);
    while lx < wf {
        let id = pack(Kind::Window, light_id, 0.0, 0.0);
        light_id += 1;
        cv.disc(lx, horizon - 6.0, 1.8, hex(0xffcf8a), 1.0, 1.0, id);
        lx += rng.gen_range(28.0..60.0);
    }

    // Water: flat dark fill; the shader reflects the city into it.
    cv.rect(0.0, horizon + 3.0, wf, hf, hex(0x03040a), 0.0, 0);

    // --- bloom ---------------------------------------------------------------
    let mut wide = cv.glow.clone();
    blur(&mut cv.glow, wu, hu, 5, 3);
    blur(&mut wide, wu, hu, 22, 3);
    for i in 0..n {
        let g = cv.glow[i];
        let wg = wide[i];
        let add = [g[0] * 0.7 + wg[0] * 0.35, g[1] * 0.7 + wg[1] * 0.35, g[2] * 0.7 + wg[2] * 0.35];
        if (i / wu) as f32 >= horizon + 3.0 {
            continue; // water gets its glow from the reflection
        }
        let c = cv.col[i];
        cv.col[i] = [c[0] + add[0], c[1] + add[1], c[2] + add[2]];
    }
    drop(wide);

    // --- sakura branches framing the top corners -----------------------------
    let reach = hf * 0.36;
    branch(&mut cv, &mut rng, (-20.0, hf * 0.02), 0.32, reach, 26.0, 0);
    if ultrawide {
        branch(&mut cv, &mut rng, (wf + 20.0, -10.0), std::f32::consts::PI - 0.42, reach * 0.85, 22.0, 0);
    }

    // --- pack ---------------------------------------------------------------
    let mut color = Vec::with_capacity(n * 4);
    for i in 0..n {
        let c = cv.col[i];
        color.extend_from_slice(&[to_srgb8(c[0]), to_srgb8(c[1]), to_srgb8(c[2]), (cv.emis[i].clamp(0.0, 1.0) * 255.0) as u8]);
    }
    City { color, data: cv.data, horizon }
}

/// Tokyo Tower-style lattice: tapering legs, X bracing, two lit decks and an
/// antenna with an aviation light.
fn draw_tower(cv: &mut Canvas, x: f32, base: f32, height: f32, hf: f32, light_id: &mut u32) {
    let orange = hex(0xff4d1a);
    let white = hex(0xfff1e6);
    let hw0 = hf * 0.075;
    // (height fraction, half width)
    let levels = [(0.0, hw0), (0.18, hw0 * 0.62), (0.36, hw0 * 0.34), (0.55, hw0 * 0.2), (0.74, hw0 * 0.11), (0.86, 3.0)];
    let id = pack(Kind::Tower, *light_id, 0.0, 0.0);
    *light_id += 1;
    let y_of = |f: f32| base - height * f;
    for k in 0..levels.len() - 1 {
        let (f0, w0) = levels[k];
        let (f1, w1) = levels[k + 1];
        let c = if k % 2 == 0 { orange } else { white };
        for s in [-1.0, 1.0] {
            cv.line((x + s * w0, y_of(f0)), (x + s * w1, y_of(f1)), 2.4, c, 0.85, id);
        }
        // X bracing in a few panels per level.
        let panels = 3;
        for p in 0..panels {
            let ta = p as f32 / panels as f32;
            let tb = (p + 1) as f32 / panels as f32;
            let fa = f0 + (f1 - f0) * ta;
            let fb = f0 + (f1 - f0) * tb;
            let wa = w0 + (w1 - w0) * ta;
            let wb = w0 + (w1 - w0) * tb;
            cv.line((x - wa, y_of(fa)), (x + wb, y_of(fb)), 1.1, c, 0.7, id);
            cv.line((x + wa, y_of(fa)), (x - wb, y_of(fb)), 1.1, c, 0.7, id);
        }
    }
    // Observation decks.
    for (f, half) in [(0.36, hw0 * 0.45), (0.62, hw0 * 0.18)] {
        let y = y_of(f);
        cv.rect(x - half, y - 7.0, x + half, y + 2.0, hex(0x1a0f14), 0.0, 0);
        cv.rect(x - half + 3.0, y - 5.0, x + half - 3.0, y - 1.0, hex(0xffd8a8), 1.0, id);
    }
    // Antenna and its light.
    let top = y_of(0.86);
    cv.line((x, top), (x, base - height), 1.6, white, 0.6, id);
    let bid = pack(Kind::Blinker, *light_id, 0.0, 0.0);
    *light_id += 1;
    cv.disc(x, base - height, 2.6, hex(0xff2a2a), 1.0, 1.0, bid);
}

/// Futuristic spire: slim tapering needle with glowing rings.
fn draw_spire(cv: &mut Canvas, x: f32, base: f32, height: f32, hf: f32, light_id: &mut u32) {
    let body = hex(0x0b0a18);
    let hw = hf * 0.03;
    let id = pack(Kind::Tower, *light_id, 0.0, 0.0);
    *light_id += 1;
    let steps = 60;
    for s in 0..steps {
        let f0 = s as f32 / steps as f32;
        let f1 = (s + 1) as f32 / steps as f32;
        let w0 = hw * (1.0 - f0).powf(0.7) + 1.0;
        cv.rect(x - w0, base - height * f1, x + w0, base - height * f0 + 1.0, body, 0.0, 0);
    }
    // Light seams up both edges and rings.
    let cyan = hex(0x00e5ff);
    for s in [-1.0, 1.0] {
        cv.line_tapered((x + s * hw, base), (x + s * 1.0, base - height), 1.6, 0.8, cyan, 0.9, id);
    }
    for (f, rw) in [(0.3f32, 1.5f32), (0.5, 1.2), (0.68, 0.9), (0.82, 0.6)] {
        let w = hw * (1.0 - f).powf(0.7) * rw + 6.0;
        let y = base - height * f;
        cv.line((x - w, y), (x + w, y), 2.2, hex(0xff2d95), 1.0, id);
    }
    let bid = pack(Kind::Blinker, *light_id, 0.0, 0.0);
    *light_id += 1;
    cv.disc(x, base - height, 2.6, hex(0xff2a2a), 1.0, 1.0, bid);
}

/// Recursive sakura branch (silhouette) with blossom clusters.
/// `angle` is in screen space (y down). Branches keep within ±0.6 rad of
/// their root's heading and stay above `floor` (so they frame the corner
/// instead of drooping over the skyline).
fn branch(cv: &mut Canvas, rng: &mut StdRng, from: (f32, f32), angle: f32, len: f32, width: f32, depth: u32) {
    let floor = cv.h as f32 * 0.27;
    branch_inner(cv, rng, from, angle, angle, len, width, depth, floor);
}

#[allow(clippy::too_many_arguments)]
fn branch_inner(cv: &mut Canvas, rng: &mut StdRng, from: (f32, f32), heading: f32, angle: f32, len: f32, width: f32, depth: u32, floor: f32) {
    let bark = hex(0x2a1420);
    let segs = 4;
    let mut p = from;
    let mut a = angle;
    let seg_len = len / segs as f32;
    for s in 0..segs {
        a = heading + (a + rng.gen_range(-0.18..0.18) - heading).clamp(-0.6, 0.6);
        let q = (p.0 + a.cos() * seg_len, p.1 + a.sin() * seg_len);
        if q.1 > floor {
            break; // the twig just ends rather than running flat along the limit
        }
        let w0 = width * (1.0 - s as f32 / segs as f32 * 0.35);
        let w1 = width * (1.0 - (s + 1) as f32 / segs as f32 * 0.35);
        cv.line_tapered(p, q, w0, w1, bark, 0.0, 0);
        if depth < 4 && s >= 1 && rng.gen_bool(0.55) {
            let side = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
            let (turn, scale) = (rng.gen_range(0.35..0.8), rng.gen_range(0.45..0.65));
            branch_inner(cv, rng, q, heading, a + side * turn, len * scale, w1 * 0.6, depth + 1, floor);
        }
        if depth >= 1 {
            blossoms(cv, rng, q, (width * 1.4).clamp(8.0, 20.0));
        }
        p = q;
    }
    if depth < 4 {
        let turn = rng.gen_range(-0.4..0.4);
        branch_inner(cv, rng, p, heading, a + turn, len * 0.55, width * 0.55, depth + 1, floor);
    }
}

fn blossoms(cv: &mut Canvas, rng: &mut StdRng, at: (f32, f32), spread: f32) {
    let pinks = [hex(0xffc4d8), hex(0xffa8c8), hex(0xffe0ec), hex(0xf5b0cc)];
    for _ in 0..rng.gen_range(7..15) {
        let (cx, cy) = (at.0 + rng.gen_range(-spread..spread), at.1 + rng.gen_range(-spread..spread) * 0.8);
        let r = rng.gen_range(2.2..4.2);
        let c = scale(pinks[rng.gen_range(0..pinks.len())], rng.gen_range(0.6..0.95));
        let rot = rng.gen_range(0.0..std::f32::consts::TAU);
        // Five petals around a darker centre.
        for k in 0..5 {
            let ang = rot + k as f32 * std::f32::consts::TAU / 5.0;
            cv.disc(cx + ang.cos() * r * 0.9, cy + ang.sin() * r * 0.9, r * 0.75, c, 0.85, 0.0, 0);
        }
        cv.disc(cx, cy, r * 0.35, hex(0xd9568a), 0.8, 0.0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_both_output_sizes() {
        for (w, h) in [(2560, 1440), (5120, 1440)] {
            let c = generate(w, h);
            assert_eq!(c.color.len(), (w * h * 4) as usize);
            assert_eq!(c.data.len(), (w * h) as usize);
            let lights = c.data.iter().filter(|d| **d & 7 == Kind::Window as u32).count();
            assert!(lights > 1000, "too few lit window pixels: {lights}");
            assert!(c.data.iter().any(|d| *d & 7 == Kind::Billboard as u32));
        }
    }
}
