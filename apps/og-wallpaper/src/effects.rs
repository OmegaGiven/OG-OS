//! Renders one region's effect, live, on top of the theme's base image.
//! Every effect is generated fresh per-frame from `t` (no per-effect
//! mutable state to manage). Region shape can be a Rect, a hand-traced
//! Polygon, or a flood-fill Mask (see og_wallpaper_core::manifest) —
//! particle effects (Rain/Twinkle) are bounded to the exact shape via
//! rejection-sampling (point-in-polygon / mask lookup); Flash/Wave only
//! support Rect and Polygon exactly (Polygon gets a real fill/outline via
//! Path) and fall back to a Mask's bounding box, since neither has a
//! cheap way to fill/follow an arbitrary flood-fill outline the way a
//! Polygon's ordered points do.

use iced::widget::canvas::{Frame, LineCap, Path, Stroke};
use iced::{Color, Point, Rectangle};
use og_wallpaper_core::manifest::{Effect, RegionShape};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// A decoded flood-fill mask, loaded once at theme load — `data[y*w+x]`
/// is nonzero where the click-and-flood-fill selected that pixel.
pub struct MaskBuf {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl MaskBuf {
    /// `fx`/`fy` are 0..1 within the mask's own bounding box (not the
    /// whole image).
    fn contains_fraction(&self, fx: f32, fy: f32) -> bool {
        if self.w == 0 || self.h == 0 {
            return false;
        }
        let x = ((fx * self.w as f32) as i64).clamp(0, self.w as i64 - 1) as u32;
        let y = ((fy * self.h as f32) as i64).clamp(0, self.h as i64 - 1) as u32;
        self.data[(y * self.w + x) as usize] != 0
    }
}

pub struct RuntimeRegion {
    pub shape: RegionShape,
    pub mask: Option<MaskBuf>,
    pub effect: Effect,
    pub color_override: Option<String>,
}

/// `image_rect` is where the base image actually landed on the canvas
/// (after cover-fit scale + centering offset) — shapes are authored in
/// the *image's* own fraction space, not the raw canvas, so they stay
/// pinned to the artwork regardless of output resolution/aspect ratio.
pub fn draw_region_in(frame: &mut Frame, region: &RuntimeRegion, image_rect: Rectangle, t: f32, default_color: Color) {
    let to_canvas = |fx: f32, fy: f32| Point::new(image_rect.x + fx * image_rect.width, image_rect.y + fy * image_rect.height);

    let b = region.shape.bounds();
    let bounds = Rectangle {
        x: image_rect.x + b.x * image_rect.width,
        y: image_rect.y + b.y * image_rect.height,
        width: b.w * image_rect.width,
        height: b.h * image_rect.height,
    };
    let color = region
        .color_override
        .as_deref()
        .map(|hex| og_wallpaper_core::filters::hex_to_rgba(hex, 255))
        .map(|c| Color::from_rgba8(c.0[0], c.0[1], c.0[2], default_color.a))
        .unwrap_or(default_color);

    // A point (in the shape's own image-fraction bounding box, 0..1) is
    // "inside" for particle rejection-sampling purposes.
    let inside = |fx_in_bbox: f32, fy_in_bbox: f32| -> bool {
        match &region.shape {
            RegionShape::Rect(_) => true,
            RegionShape::Mask { .. } => region.mask.as_ref().is_some_and(|m| m.contains_fraction(fx_in_bbox, fy_in_bbox)),
            RegionShape::Polygon { points } => {
                let fx = b.x + fx_in_bbox * b.w;
                let fy = b.y + fy_in_bbox * b.h;
                og_wallpaper_core::filters::point_in_polygon(fx, fy, points)
            }
        }
    };

    match &region.effect {
        Effect::Rain { density, speed } => draw_rain(frame, bounds, inside, *density, *speed, t, color),
        Effect::Twinkle { count, rate } => draw_twinkle(frame, bounds, inside, *count, *rate, t, color),
        Effect::Flash { rate } => draw_flash(frame, &region.shape, bounds, to_canvas, *rate, t, color),
        Effect::Wave { amplitude, speed } => draw_wave(frame, bounds, *amplitude, *speed, t, color),
    }
}


/// A region's own RNG is reseeded from its bounds each frame — cheap
/// (dozens of particles, not thousands) and means no per-region state
/// needs to be threaded through the render loop just to keep drop
/// positions stable frame to frame; the seed alone does that.
fn region_rng(bounds: Rectangle) -> StdRng {
    let seed = (bounds.x as u64) << 32 | (bounds.y as u64) << 16 | (bounds.width as u64);
    StdRng::seed_from_u64(seed ^ 0x5EED)
}

fn draw_rain(frame: &mut Frame, bounds: Rectangle, inside: impl Fn(f32, f32) -> bool, density: u32, speed: f32, t: f32, color: Color) {
    let mut rng = region_rng(bounds);
    let mut placed = 0;
    for _attempt in 0..density * 4 {
        if placed >= density {
            break;
        }
        let fx = rng.gen_range(0.0..1.0);
        let fy = rng.gen_range(0.0..1.0);
        if !inside(fx, fy) {
            continue;
        }
        placed += 1;
        let x = bounds.x + fx * bounds.width;
        let phase = rng.gen_range(0.0..1.0);
        let alpha = rng.gen_range(0.2..0.55);
        let len = bounds.height.max(1.0) * rng.gen_range(0.04..0.09);
        let cycle = ((t * speed + phase) % 1.0 + 1.0) % 1.0;
        let y = bounds.y + cycle * (bounds.height + len) - len;
        let path = Path::line(Point::new(x - len * 0.2, y), Point::new(x + len * 0.2, y + len));
        let stroke = Stroke {
            style: Color { a: alpha * color.a, ..color }.into(),
            width: 1.0,
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        frame.stroke(&path, stroke);
    }
}

fn draw_twinkle(frame: &mut Frame, bounds: Rectangle, inside: impl Fn(f32, f32) -> bool, count: u32, rate: f32, t: f32, color: Color) {
    let mut rng = region_rng(bounds);
    let mut placed = 0;
    for _attempt in 0..count * 4 {
        if placed >= count {
            break;
        }
        let fx = rng.gen_range(0.0..1.0);
        let fy = rng.gen_range(0.0..1.0);
        if !inside(fx, fy) {
            continue;
        }
        placed += 1;
        let x = bounds.x + fx * bounds.width;
        let y = bounds.y + fy * bounds.height;
        let phase = rng.gen_range(0.0..std::f32::consts::TAU);
        let pulse = 0.5 + 0.5 * (t * rate + phase).sin();
        let alpha = (0.15 + 0.85 * pulse.powf(2.0)) * color.a;
        let r = 1.0 + 1.5 * pulse;
        frame.fill(&Path::circle(Point::new(x, y), r), Color { a: alpha, ..color });
    }
}

fn draw_flash(
    frame: &mut Frame,
    shape: &RegionShape,
    bounds: Rectangle,
    to_canvas: impl Fn(f32, f32) -> Point,
    rate: f32,
    t: f32,
    color: Color,
) {
    let pulse = 0.5 + 0.5 * (t * rate).sin();
    let alpha = (0.12 + 0.6 * pulse.powf(1.6)) * color.a;
    let fill_color = Color { a: alpha, ..color };
    let stroke = Stroke { style: Color { a: alpha.min(color.a), ..color }.into(), width: 1.2, ..Stroke::default() };

    match shape {
        RegionShape::Polygon { points } if points.len() >= 3 => {
            let path = Path::new(|p| {
                p.move_to(to_canvas(points[0].0, points[0].1));
                for &(fx, fy) in &points[1..] {
                    p.line_to(to_canvas(fx, fy));
                }
                p.close();
            });
            frame.fill(&path, fill_color);
            frame.stroke(&path, stroke);
        }
        // Rect, Mask (bounding-box fallback — see module doc), or a
        // degenerate <3-point Polygon.
        _ => {
            frame.fill_rectangle(Point::new(bounds.x, bounds.y), iced::Size::new(bounds.width, bounds.height), fill_color);
            frame.stroke(
                &Path::rectangle(Point::new(bounds.x, bounds.y), iced::Size::new(bounds.width, bounds.height)),
                stroke,
            );
        }
    }
}

/// Approximation, not a true pixel warp of the underlying image (the
/// canvas API here has no cheap way to displace image content per-strip
/// without pre-slicing it) — draws a wavy line near the bottom of the
/// shape's bounding box, same technique the original hand-drawn scene
/// used for its banners. Good enough for "this sign is waving in the
/// wind"; real per-pixel cloth warp is a possible follow-up, not this
/// pass. Same for Polygon/Mask shapes — uses the bounding box, not the
/// exact outline (Flash uses the exact outline for Polygon; a wave
/// following an arbitrary traced edge is a bigger follow-up).
fn draw_wave(frame: &mut Frame, bounds: Rectangle, amplitude: f32, speed: f32, t: f32, color: Color) {
    let segments = 10;
    let base_y = bounds.y + bounds.height * 0.85;
    let amp = bounds.width.min(bounds.height) * amplitude;
    let path = Path::new(|p| {
        for i in 0..=segments {
            let f = i as f32 / segments as f32;
            let x = bounds.x + bounds.width * f;
            let y = base_y + (t * speed + f * 6.0).sin() * amp * (0.3 + 0.7 * f);
            if i == 0 {
                p.move_to(Point::new(x, y));
            } else {
                p.line_to(Point::new(x, y));
            }
        }
    });
    frame.stroke(&path, Stroke { style: color.into(), width: 1.6, line_cap: LineCap::Round, ..Stroke::default() });
}
