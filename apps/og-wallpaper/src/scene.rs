//! The actual "Tokyo, half-traditional half-futuristic" line-art scene.
//! Everything is drawn as strokes (no fills) in the system's own theme
//! colors, procedurally against whatever canvas size it's handed — this
//! isn't pixel art, it's a generator, so it scales to any monitor.

use iced::widget::canvas::{Frame, LineCap, LineJoin, Path, Stroke};
use iced::{Color, Point};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

#[derive(Clone, Copy)]
struct RainDrop {
    x: f32,
    /// 0..1, this drop's position within its own fall loop at t=0.
    phase: f32,
    speed: f32,
    len: f32,
    alpha: f32,
}

#[derive(Clone, Copy)]
struct Billboard {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rate: f32,
    phase: f32,
}

#[derive(Clone, Copy)]
struct WavingSign {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    phase: f32,
}

pub struct Scene {
    rain: Vec<RainDrop>,
    billboards: Vec<Billboard>,
    signs: Vec<WavingSign>,
}

impl Scene {
    /// Built once at startup against the (first-seen) output size — the
    /// layout is a fraction-of-bounds generator, so it's fine if the
    /// actual canvas is a different size on other outputs; only the rain/
    /// billboard/sign *counts* and seeds are fixed here, not raw pixels.
    pub fn generate() -> Self {
        let mut rng = StdRng::seed_from_u64(0xC0FFEE);

        // Rain in a handful of patches (not a uniform curtain) — each
        // patch is a horizontal band of the screen with its own drops, so
        // the shower reads as scattered rather than a flat wash.
        let mut rain = Vec::new();
        let patches: &[(f32, f32)] = &[(0.03, 0.14), (0.22, 0.12), (0.60, 0.14), (0.82, 0.13)];
        for &(px, pw) in patches {
            for _ in 0..18 {
                rain.push(RainDrop {
                    x: px + rng.gen_range(0.0..pw),
                    phase: rng.gen_range(0.0..1.0),
                    speed: rng.gen_range(0.35..0.7),
                    len: rng.gen_range(0.015..0.035),
                    alpha: rng.gen_range(0.2..0.5),
                });
            }
        }

        let billboards = vec![
            Billboard { x: 0.63, y: 0.42, w: 0.05, h: 0.07, rate: 1.3, phase: 0.0 },
            Billboard { x: 0.74, y: 0.30, w: 0.04, h: 0.10, rate: 0.9, phase: 2.1 },
            Billboard { x: 0.87, y: 0.50, w: 0.045, h: 0.06, rate: 1.7, phase: 4.4 },
        ];

        let signs = vec![
            WavingSign { x: 0.56, y: 0.55, w: 0.05, h: 0.10, phase: 0.0 },
            WavingSign { x: 0.80, y: 0.60, w: 0.045, h: 0.09, phase: 1.7 },
            WavingSign { x: 0.10, y: 0.62, w: 0.035, h: 0.07, phase: 3.2 },
        ];

        Self { rain, billboards, signs }
    }

    pub fn draw(&self, frame: &mut Frame, t: f32, colors: &og_theme::AppColors) {
        let w = frame.width();
        let h = frame.height();
        let ground_y = h * 0.80;

        let structure = Stroke {
            style: colors.dim_text.into(),
            width: 1.6,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        let structure_soft =
            Stroke { style: Color { a: colors.dim_text.a * 0.5, ..colors.dim_text }.into(), width: 1.0, ..structure };
        // Ground line spanning the whole width.
        frame.stroke(&Path::line(Point::new(0.0, ground_y), Point::new(w, ground_y)), structure);

        draw_traditional_side(frame, w, ground_y, &structure, &structure_soft);
        draw_futuristic_side(frame, w, ground_y, t, &structure, &structure_soft, colors);

        for sign in &self.signs {
            draw_waving_sign(frame, sign, w, h, t, &structure);
        }

        for bb in &self.billboards {
            draw_billboard(frame, bb, w, h, t, colors);
        }

        for drop in &self.rain {
            draw_rain_drop(frame, drop, w, h, t, colors.accent);
        }
    }
}

// ── Traditional (left) half ─────────────────────────────────────────────

fn draw_traditional_side(frame: &mut Frame, w: f32, ground_y: f32, structure: &Stroke, soft: &Stroke) {
    // Spaced out with real gaps between each silhouette — these used to
    // overlap (torii tangled into the first low roof) and read as one
    // illegible blob.
    draw_low_roof(frame, w * 0.02, ground_y, w * 0.08, w * 0.05, structure);
    draw_torii(frame, w * 0.16, ground_y, w * 0.07, structure);
    draw_pagoda(frame, w * 0.30, ground_y, w * 0.10, structure, soft);
    draw_low_roof(frame, w * 0.42, ground_y, w * 0.07, w * 0.04, structure);
}

fn draw_torii(frame: &mut Frame, cx: f32, ground_y: f32, scale: f32, stroke: &Stroke) {
    let post_h = scale * 1.3;
    let half_w = scale * 0.42;
    let path = Path::new(|p| {
        // Two posts.
        p.move_to(Point::new(cx - half_w, ground_y));
        p.line_to(Point::new(cx - half_w, ground_y - post_h));
        p.move_to(Point::new(cx + half_w, ground_y));
        p.line_to(Point::new(cx + half_w, ground_y - post_h));
        // Lower crossbar.
        let mid_y = ground_y - post_h * 0.75;
        p.move_to(Point::new(cx - half_w * 0.9, mid_y));
        p.line_to(Point::new(cx + half_w * 0.9, mid_y));
        // Top beam (kasagi), slightly wider with upturned ends.
        let top_y = ground_y - post_h;
        let top_w = half_w * 1.25;
        p.move_to(Point::new(cx - top_w, top_y + scale * 0.12));
        p.line_to(Point::new(cx - top_w * 0.85, top_y));
        p.line_to(Point::new(cx + top_w * 0.85, top_y));
        p.line_to(Point::new(cx + top_w, top_y + scale * 0.12));
        // Second beam just under it.
        let top2_y = top_y + scale * 0.22;
        p.move_to(Point::new(cx - half_w * 1.05, top2_y));
        p.line_to(Point::new(cx + half_w * 1.05, top2_y));
    });
    frame.stroke(&path, *stroke);
}

fn draw_pagoda(frame: &mut Frame, cx: f32, ground_y: f32, scale: f32, stroke: &Stroke, soft: &Stroke) {
    let tiers = 3;
    let tier_h = scale * 0.32;
    let mut half_w = scale * 0.55;
    for i in 0..tiers {
        let top = ground_y - tier_h * (i as f32 + 1.0) - scale * 0.06 * i as f32;
        let bottom = top + tier_h;
        let eave_lift = tier_h * 0.28;
        let roof = Path::new(|p| {
            // Body walls.
            p.move_to(Point::new(cx - half_w * 0.6, bottom));
            p.line_to(Point::new(cx - half_w * 0.6, top + tier_h * 0.35));
            p.move_to(Point::new(cx + half_w * 0.6, bottom));
            p.line_to(Point::new(cx + half_w * 0.6, top + tier_h * 0.35));
            // Upturned roof (eaves flick up at the corners).
            p.move_to(Point::new(cx - half_w, top + eave_lift));
            p.line_to(Point::new(cx - half_w * 0.55, top));
            p.line_to(Point::new(cx, top - tier_h * 0.18));
            p.line_to(Point::new(cx + half_w * 0.55, top));
            p.line_to(Point::new(cx + half_w, top + eave_lift));
        });
        frame.stroke(&roof, *stroke);
        half_w *= 0.78;
    }
    // Spire on top.
    let top = ground_y - tier_h * tiers as f32 - scale * 0.06 * (tiers - 1) as f32 - tier_h * 0.18;
    frame.stroke(&Path::line(Point::new(cx, top), Point::new(cx, top - scale * 0.35)), *soft);
}

fn draw_low_roof(frame: &mut Frame, x: f32, ground_y: f32, width: f32, height: f32, stroke: &Stroke) {
    let path = Path::new(|p| {
        p.move_to(Point::new(x, ground_y));
        p.line_to(Point::new(x, ground_y - height * 0.5));
        p.line_to(Point::new(x + width * 0.5, ground_y - height));
        p.line_to(Point::new(x + width, ground_y - height * 0.5));
        p.line_to(Point::new(x + width, ground_y));
    });
    frame.stroke(&path, *stroke);
}

// ── Futuristic (right) half ─────────────────────────────────────────────

fn draw_futuristic_side(
    frame: &mut Frame,
    w: f32,
    ground_y: f32,
    t: f32,
    structure: &Stroke,
    soft: &Stroke,
    colors: &og_theme::AppColors,
) {
    let buildings: &[(f32, f32, f32)] = &[
        // (x fraction, width fraction, height fraction of ground_y)
        (0.55, 0.07, 0.30),
        (0.62, 0.10, 0.52),
        (0.72, 0.06, 0.38),
        (0.78, 0.12, 0.62),
        (0.90, 0.08, 0.44),
    ];
    for &(bx, bw, bh) in buildings {
        draw_skyscraper(frame, w * bx, w * bw, ground_y * bh, ground_y, structure, soft);
    }

    // Tallest tower, with a blinking antenna light.
    let tower_x = w * 0.795;
    let tower_h = ground_y * 0.68;
    draw_antenna(frame, tower_x, ground_y - tower_h, t, colors);
}

fn draw_skyscraper(
    frame: &mut Frame,
    x: f32,
    width: f32,
    height: f32,
    ground_y: f32,
    stroke: &Stroke,
    soft: &Stroke,
) {
    let top = ground_y - height;
    let outline = Path::new(|p| {
        p.move_to(Point::new(x, ground_y));
        p.line_to(Point::new(x, top));
        p.line_to(Point::new(x + width, top));
        p.line_to(Point::new(x + width, ground_y));
    });
    frame.stroke(&outline, *stroke);

    // Sparse window grid — dashes, not a full grid, to keep it linework
    // rather than a filled block.
    let rows = ((height / (width.max(1.0) * 0.5)).round().max(3.0) as i32).min(10);
    let cols = 3;
    for row in 0..rows {
        let y = top + height * (row as f32 + 0.5) / rows as f32;
        for col in 0..cols {
            let cx = x + width * (col as f32 + 0.5) / cols as f32;
            let dash = Path::line(
                Point::new(cx - width * 0.08, y),
                Point::new(cx + width * 0.08, y),
            );
            frame.stroke(&dash, *soft);
        }
    }
}

fn draw_antenna(frame: &mut Frame, x: f32, top_y: f32, t: f32, colors: &og_theme::AppColors) {
    let mast_top = top_y - 40.0;
    frame.stroke(
        &Path::line(Point::new(x, top_y), Point::new(x, mast_top)),
        Stroke { style: colors.dim_text.into(), width: 1.2, ..Stroke::default() },
    );
    let blink = 0.5 + 0.5 * (t * 2.2).sin();
    let dot = Path::circle(Point::new(x, mast_top), 2.5);
    frame.fill(&dot, Color { a: (0.3 + 0.7 * blink) * colors.accent.a, ..colors.accent });
}

// ── Animated dressing ────────────────────────────────────────────────────

fn draw_waving_sign(frame: &mut Frame, sign: &WavingSign, w: f32, h: f32, t: f32, structure: &Stroke) {
    let x = w * sign.x;
    let y = h * sign.y;
    let sw = w * sign.w;
    let sh = h * sign.h;
    // Vertical banner hanging from a short horizontal pole, its trailing
    // (right) edge perturbed by a sine wave along its height to read as
    // cloth moving in wind — the pole and hanging edge stay put.
    let pole = Path::line(Point::new(x - sw * 0.5, y), Point::new(x + sw * 0.15, y));
    frame.stroke(&pole, *structure);

    let segments = 8;
    let amplitude = sw * 0.22;
    let banner = Path::new(|p| {
        p.move_to(Point::new(x, y));
        for i in 1..=segments {
            let f = i as f32 / segments as f32;
            let wave = (t * 2.4 + sign.phase + f * 5.0).sin() * amplitude * f;
            p.line_to(Point::new(x + wave, y + sh * f));
        }
        for i in (0..segments).rev() {
            let f = i as f32 / segments as f32;
            let wave = (t * 2.4 + sign.phase + f * 5.0).sin() * amplitude * f;
            p.line_to(Point::new(x + wave - sw * 0.28, y + sh * f));
        }
        p.close();
    });
    frame.stroke(&banner, *structure);
}

fn draw_billboard(frame: &mut Frame, bb: &Billboard, w: f32, h: f32, t: f32, colors: &og_theme::AppColors) {
    let x = w * bb.x;
    let y = h * bb.y;
    let bw = w * bb.w;
    let bh = h * bb.h;
    let pulse = 0.5 + 0.5 * (t * bb.rate + bb.phase).sin();
    let alpha = (0.15 + 0.85 * pulse.powf(1.6)) * colors.accent.a;
    let stroke = Stroke { style: Color { a: alpha, ..colors.accent }.into(), width: 1.4, ..Stroke::default() };

    let frame_path = Path::new(|p| {
        p.move_to(Point::new(x, y));
        p.line_to(Point::new(x + bw, y));
        p.line_to(Point::new(x + bw, y + bh));
        p.line_to(Point::new(x, y + bh));
        p.close();
    });
    frame.stroke(&frame_path, stroke);

    // Abstract chevron "logo" inside, same pulse.
    let chevron = Path::new(|p| {
        p.move_to(Point::new(x + bw * 0.2, y + bh * 0.7));
        p.line_to(Point::new(x + bw * 0.5, y + bh * 0.3));
        p.line_to(Point::new(x + bw * 0.8, y + bh * 0.7));
    });
    frame.stroke(&chevron, stroke);
}

fn draw_rain_drop(frame: &mut Frame, drop: &RainDrop, w: f32, h: f32, t: f32, accent: Color) {
    let cycle = ((t * drop.speed + drop.phase) % 1.0 + 1.0) % 1.0;
    let y = cycle * (h * 1.1) - h * 0.05;
    let x = w * drop.x;
    let len = h * drop.len;
    // Slight left lean for a wind-blown look.
    let path = Path::line(Point::new(x - len * 0.25, y), Point::new(x + len * 0.25, y + len));
    let stroke = Stroke {
        style: Color { a: drop.alpha * accent.a, ..accent }.into(),
        width: 1.0,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    frame.stroke(&path, stroke);
}
