use std::time::Instant;

use rand::Rng;

use crate::config::{EffectKind, FxConfig};

const STREAK_COUNT: usize = 7;
const GLOW_THICKNESS: f32 = 120.0;
const START_OFFSET: f32 = 150.0;
const GUST_COUNT: usize = 5;
const SPARKLE_COUNT: usize = 30;

struct Streak {
    x: f32,
    length: f32,
    speed: f32, // px/ms
    delay_ms: f32,
}

/// A "wind" gust travels left to right in a straight line, does one full
/// circular loop-the-loop partway across, then continues straight off the
/// right edge. Position is tracked as arc length `s` along that whole
/// path so a single `speed` covers all three legs at a constant pace.
struct Gust {
    base_y: f32,
    radius: f32,
    loop_center_x: f32,
    /// Arc length where the straight approach ends and the loop begins.
    l1: f32,
    /// Arc length of the loop itself (`2*pi*radius`).
    loop_len: f32,
    /// Total path length (approach + loop + exit) — `s` ranges `0..=total_len`.
    total_len: f32,
    trail_len: f32,
    speed: f32, // arc px/ms
    delay_ms: f32,
}

impl Gust {
    /// `None` once `s` is outside the path (before it starts / after the
    /// exit leg has run past the far edge).
    fn point_at(&self, s: f32) -> Option<(f32, f32)> {
        if s < 0.0 || s > self.total_len {
            return None;
        }
        if s < self.l1 {
            Some((-START_OFFSET + s, self.base_y))
        } else if s < self.l1 + self.loop_len {
            // Circle parameterized so it's tangent to the straight legs at
            // (loop_center_x, base_y): theta=0 enters at the bottom moving
            // right, sweeps up over the top, and returns to the same point
            // moving right again at theta=2*pi.
            let theta = (s - self.l1) / self.radius;
            let x = self.loop_center_x + self.radius * theta.sin();
            let y = self.base_y - self.radius + self.radius * theta.cos();
            Some((x, y))
        } else {
            let x = self.loop_center_x + (s - self.l1 - self.loop_len);
            Some((x, self.base_y))
        }
    }
}

struct Sparkle {
    x: f32,
    y: f32,
    size: f32,
    delay_ms: f32,
    lifetime_ms: f32,
}

pub struct Effect {
    start: Instant,
    duration_ms: f32,
    kind: EffectKind,
    color: (u8, u8, u8),
    streaks: Vec<Streak>,
    gusts: Vec<Gust>,
    sparkles: Vec<Sparkle>,
    /// Elapsed time (ms) at which every element of the active effect has
    /// fully finished (cleared the screen / faded out) — may run well past
    /// `duration_ms` for elements that got a long start delay, so
    /// completion can't just check against `duration_ms` directly or
    /// they'd freeze/cut off mid-animation.
    finish_ms: f32,
}

impl Effect {
    pub fn new(cfg: &FxConfig, width: u32, height: u32) -> Self {
        let mut rng = rand::thread_rng();
        let duration_ms = cfg.duration_ms.max(300) as f32;

        let streaks: Vec<Streak> = (0..STREAK_COUNT)
            .map(|_| Streak {
                x: rng.gen_range(0.0..width as f32),
                length: rng.gen_range(80.0..220.0),
                speed: (height as f32 + 300.0) / duration_ms * rng.gen_range(0.7..1.3),
                delay_ms: rng.gen_range(0.0..duration_ms * 0.4),
            })
            .collect();

        let gusts: Vec<Gust> = (0..GUST_COUNT)
            .map(|_| {
                let radius = rng.gen_range(50.0..120.0_f32).min(height as f32 * 0.35);
                let loop_center_x = width as f32 * rng.gen_range(0.35..0.65);
                let base_y = rng.gen_range((radius + 20.0)..(height as f32 - radius - 20.0).max(radius + 21.0));
                let l1 = loop_center_x + START_OFFSET;
                let loop_len = radius * std::f32::consts::TAU;
                let l3 = (width as f32 + START_OFFSET) - loop_center_x;
                let total_len = l1 + loop_len + l3;
                Gust {
                    base_y,
                    radius,
                    loop_center_x,
                    l1,
                    loop_len,
                    total_len,
                    trail_len: rng.gen_range(150.0..260.0),
                    speed: total_len / duration_ms * rng.gen_range(0.8..1.2),
                    delay_ms: rng.gen_range(0.0..duration_ms * 0.3),
                }
            })
            .collect();

        let sparkles: Vec<Sparkle> = (0..SPARKLE_COUNT)
            .map(|_| Sparkle {
                x: rng.gen_range(0.0..width as f32),
                y: rng.gen_range(0.0..height as f32),
                size: rng.gen_range(5.0..12.0),
                delay_ms: rng.gen_range(0.0..duration_ms.max(1.0)),
                lifetime_ms: rng.gen_range(150.0..400.0),
            })
            .collect();

        let finish_ms = match cfg.effect {
            // A streak's head must travel from -START_OFFSET to
            // height+length (fully clear of the bottom edge).
            EffectKind::Rain => streaks
                .iter()
                .map(|s| s.delay_ms + (height as f32 + s.length + START_OFFSET) / s.speed)
                .fold(duration_ms, f32::max),
            // A gust's trailing end must also finish the whole path
            // (approach + loop + exit) before it's done.
            EffectKind::Wind => gusts
                .iter()
                .map(|g| g.delay_ms + (g.total_len + g.trail_len) / g.speed)
                .fold(duration_ms, f32::max),
            EffectKind::Sparkle => sparkles
                .iter()
                .map(|s| s.delay_ms + s.lifetime_ms)
                .fold(duration_ms, f32::max),
            EffectKind::Glow => duration_ms,
        };

        Self {
            start: Instant::now(),
            duration_ms,
            kind: cfg.effect,
            color: cfg.color,
            streaks,
            gusts,
            sparkles,
            finish_ms,
        }
    }
}

/// Draws one frame into `canvas` (tightly packed BGRA8888-little-endian /
/// ARGB8888 premultiplied, `width*height*4` bytes). Returns `true` once the
/// effect has fully finished (caller should stop calling `render`).
pub fn render(effect: &Effect, width: u32, height: u32, canvas: &mut [u8]) -> bool {
    let elapsed_ms = effect.start.elapsed().as_secs_f32() * 1000.0;

    match effect.kind {
        EffectKind::Glow => {
            draw_glow(effect, elapsed_ms, width, height, canvas);
        }
        EffectKind::Rain => {
            for streak in &effect.streaks {
                draw_streak(streak, effect.color, elapsed_ms, width, height, canvas);
            }
        }
        EffectKind::Wind => {
            for gust in &effect.gusts {
                draw_gust(gust, effect.color, elapsed_ms, width, height, canvas);
            }
        }
        EffectKind::Sparkle => {
            for sparkle in &effect.sparkles {
                draw_sparkle(sparkle, effect.color, elapsed_ms, width, height, canvas);
            }
        }
    }

    elapsed_ms >= effect.finish_ms
}

fn draw_streak(streak: &Streak, color: (u8, u8, u8), elapsed_ms: f32, width: u32, height: u32, canvas: &mut [u8]) {
    let t = elapsed_ms - streak.delay_ms;
    if t < 0.0 {
        return;
    }
    let head_y = t * streak.speed - START_OFFSET;

    let x = streak.x as i32;
    if x < 0 || x >= width as i32 {
        return;
    }

    let y_start = (head_y - streak.length).max(0.0) as i32;
    let y_end = (head_y.min(height as f32)) as i32;
    for y in y_start.max(0)..y_end.min(height as i32) {
        let dist_from_head = head_y - y as f32;
        let fade = (1.0 - (dist_from_head / streak.length)).clamp(0.0, 1.0);
        let alpha = (fade * 200.0) as u8;
        blend_pixel(canvas, width, height, x, y, color, alpha);
        // A faint two-pixel-wide streak reads better than a hairline.
        blend_pixel(canvas, width, height, x + 1, y, color, alpha / 2);
    }
}

/// A gust is sampled as a short trail of points behind its head along the
/// approach-line → loop → exit-line path, one sample per unit of arc
/// length so the trail reads as a continuous streak through the loop
/// instead of gaps between sparse dots.
fn draw_gust(gust: &Gust, color: (u8, u8, u8), elapsed_ms: f32, width: u32, height: u32, canvas: &mut [u8]) {
    let t = elapsed_ms - gust.delay_ms;
    if t < 0.0 {
        return;
    }
    let head_s = t * gust.speed;

    let steps = gust.trail_len as i32;
    for i in 0..steps {
        let s = head_s - i as f32;
        let Some((x, y)) = gust.point_at(s) else { continue };
        if x < 0.0 || x >= width as f32 {
            continue;
        }
        let frac = 1.0 - i as f32 / gust.trail_len; // 0 = tail, 1 = head
        let alpha = (frac.clamp(0.0, 1.0) * 220.0) as u8;
        blend_pixel(canvas, width, height, x as i32, y as i32, color, alpha);
        blend_pixel(canvas, width, height, x as i32, y as i32 + 1, color, alpha / 2);
        blend_pixel(canvas, width, height, x as i32, y as i32 - 1, color, alpha / 2);
    }
}

fn draw_sparkle(sparkle: &Sparkle, color: (u8, u8, u8), elapsed_ms: f32, width: u32, height: u32, canvas: &mut [u8]) {
    let t = elapsed_ms - sparkle.delay_ms;
    if t < 0.0 || t > sparkle.lifetime_ms {
        return;
    }
    // Quick fade in, quick fade out — a twinkle, not a fixed-brightness dot.
    let half = sparkle.lifetime_ms / 2.0;
    let envelope = if t < half { t / half } else { (sparkle.lifetime_ms - t) / half };
    let alpha = (envelope.clamp(0.0, 1.0) * 255.0) as u8;
    if alpha == 0 {
        return;
    }

    let cx = sparkle.x as i32;
    let cy = sparkle.y as i32;
    let r = sparkle.size as i32;

    // A small "+" — a filled circle at this size looks like a blob, a plus
    // reads clearly as a sparkle/star even at only a few pixels across.
    blend_pixel(canvas, width, height, cx, cy, color, alpha);
    for d in 1..=r {
        let falloff = (alpha as f32 * (1.0 - d as f32 / (r as f32 + 1.0))) as u8;
        blend_pixel(canvas, width, height, cx + d, cy, color, falloff);
        blend_pixel(canvas, width, height, cx - d, cy, color, falloff);
        blend_pixel(canvas, width, height, cx, cy + d, color, falloff);
        blend_pixel(canvas, width, height, cx, cy - d, color, falloff);
    }
}

fn draw_glow(effect: &Effect, elapsed_ms: f32, width: u32, height: u32, canvas: &mut [u8]) {
    let fade_in = 200.0_f32;
    let fade_out = 500.0_f32;
    let envelope = if elapsed_ms < fade_in {
        elapsed_ms / fade_in
    } else if elapsed_ms > effect.duration_ms - fade_out {
        ((effect.duration_ms - elapsed_ms) / fade_out).max(0.0)
    } else {
        1.0
    };
    if envelope <= 0.0 {
        return;
    }

    let max_alpha = 90.0 * envelope;
    let color = effect.color;
    let w = width as f32;
    let h = height as f32;
    let thickness = (GLOW_THICKNESS as u32).min(width / 2).min(height / 2);

    let mut plot = |x: u32, y: u32| {
        let dist_edge = (x as f32).min(w - 1.0 - x as f32).min(y as f32).min(h - 1.0 - y as f32);
        let falloff = (1.0 - dist_edge / GLOW_THICKNESS).clamp(0.0, 1.0);
        let alpha = (falloff * falloff * max_alpha) as u8;
        if alpha > 0 {
            blend_pixel(canvas, width, height, x as i32, y as i32, color, alpha);
        }
    };

    // Only the border bands can be within GLOW_THICKNESS of an edge —
    // skipping the (much larger) interior keeps this cheap enough to run
    // every frame at desktop resolutions.
    for y in 0..thickness {
        for x in 0..width {
            plot(x, y);
            plot(x, height - 1 - y);
        }
    }
    for y in thickness..(height - thickness) {
        for x in 0..thickness {
            plot(x, y);
            plot(width - 1 - x, y);
        }
    }
}

/// Additive-blends a premultiplied-alpha pixel into the ARGB8888 canvas.
fn blend_pixel(canvas: &mut [u8], width: u32, height: u32, x: i32, y: i32, rgb: (u8, u8, u8), alpha: u8) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 || alpha == 0 {
        return;
    }
    let idx = (y as usize * width as usize + x as usize) * 4;
    let Some(px) = canvas.get_mut(idx..idx + 4) else { return };

    let a = alpha as u32;
    let pr = (rgb.0 as u32 * a) / 255;
    let pg = (rgb.1 as u32 * a) / 255;
    let pb = (rgb.2 as u32 * a) / 255;

    // Existing content is already premultiplied; a simple additive combine
    // (clamped) is enough for a handful of translucent overlapping shapes
    // and avoids needing to read back un-premultiplied values.
    px[0] = px[0].saturating_add(pb as u8);
    px[1] = px[1].saturating_add(pg as u8);
    px[2] = px[2].saturating_add(pr as u8);
    px[3] = px[3].saturating_add(alpha);
}

impl og_wayland::overlay::Effect for Effect {
    fn render(&self, width: u32, height: u32, canvas: &mut [u8]) -> bool {
        render(self, width, height, canvas)
    }
}
