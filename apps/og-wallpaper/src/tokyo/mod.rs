//! "tokyo" procedural theme: a neo-futuristic Tokyo night across a bay —
//! rain, neon, a rippling reflection, and sakura petals blowing across on
//! gusty wind (pushed aside by the cursor). See city.rs for the generated
//! skyline and the .frag/.vert passes (drawn in `draw`) for everything that
//! moves.

mod city;

use std::time::Instant;

use crate::direct::gl::{self, Blend, Board, Frame, Generated, Program, Renderer, Tex, TexFormat, Uniforms, U_LEN};

/// Petal cell sizes per depth layer (must match tokyo.wgsl).
const PETAL_CELLS: [f64; 3] = [110.0, 190.0, 340.0];
/// Rain fall speeds per layer, px/s.
const RAIN_SPEED: [f64; 3] = [900.0, 1250.0, 1700.0];

/// Wind and rain integrated over time in f64 and wrapped, so positions stay
/// continuous (no jitter, no jumps) however long the session runs.
#[derive(Clone)]
pub struct Clock {
    last: Option<Instant>,
    t: f64,
    petal: [(f64, f64); 3],
    rain: [f64; 3],
    gust: f32,
}

impl Default for Clock {
    fn default() -> Self {
        Self { last: None, t: 0.0, petal: [(0.0, 0.0); 3], rain: [0.0; 3], gust: 0.0 }
    }
}

impl Clock {
    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = self.last.map_or(0.0, |l| now.duration_since(l).as_secs_f64()).min(0.25);
        self.last = Some(now);
        self.t += dt;
        // Gusts: a slow swell with occasional strong bursts.
        let t = self.t;
        let burst = ((t * 0.13).sin() * 0.5 + 0.5).powi(6) + ((t * 0.071 + 2.0).sin() * 0.5 + 0.5).powi(8);
        let gust = (0.35 + 0.25 * (t * 0.31).sin() + 1.6 * burst).max(0.1);
        self.gust = ((gust - 0.1) / 1.8).clamp(0.0, 1.0) as f32;
        for (i, p) in self.petal.iter_mut().enumerate() {
            let depth = [0.55, 0.8, 1.15][i];
            // Blown left→right, drifting down, faster when gusting.
            p.0 += 70.0 * depth * gust * dt;
            p.1 += 22.0 * depth * (0.6 + 0.4 * gust) * dt;
            let wrap = PETAL_CELLS[i] * 1024.0;
            p.0 %= wrap;
            p.1 %= wrap;
        }
        for (i, r) in self.rain.iter_mut().enumerate() {
            *r = (*r + RAIN_SPEED[i] * dt) % 1.0e6;
        }
    }

    /// Rain slant follows the gust (dx per dy).
    fn slant(&self) -> f32 {
        0.08 + 0.12 * self.gust
    }
}

/// The scene's settings handed to board generation, plus the wind clock.
#[derive(Clone, Default)]
pub struct Tokyo {
    clock: Clock,
}

pub struct Info {
    horizon: f32,
}

/// The city's lights change slowly: its cache is re-rendered this often.
const CITY_FPS: f32 = 15.0;
/// Per petal layer, far to near: (cell size, petal radius, color scale,
/// alpha scale). Cell sizes must match PETAL_CELLS.
const PETALS: [(f32, f32, f32, f32); 3] = [(110.0, 4.5, 0.55, 0.75), (190.0, 7.5, 0.8, 0.9), (340.0, 13.0, 1.0, 1.0)];

pub struct Programs {
    city: Program,
    base: Program,
    rain: Program,
    petal: Program,
}

impl gl::Scene for Tokyo {
    type Out = ();
    type Info = Info;
    type Gl = Programs;

    fn init_gl(r: &Renderer) -> Result<Programs, String> {
        Ok(Programs {
            city: r.program(gl::FULLSCREEN_VERT, include_str!("city.frag"), &["color_tex", "data_tex"])?,
            base: r.program(gl::FULLSCREEN_VERT, include_str!("base.frag"), &["cache"])?,
            rain: r.program(include_str!("rain.vert"), include_str!("rain.frag"), &["cache"])?,
            petal: r.program(include_str!("petal.vert"), include_str!("petal.frag"), &[])?,
        })
    }

    fn generate(&self, w: u32, h: u32) -> Generated<Info> {
        let c = city::generate(w, h);
        Generated {
            textures: vec![
                Tex { w, h, format: TexFormat::Srgba8, bytes: c.color },
                Tex { w, h, format: TexFormat::R32Ui, bytes: bytemuck::cast_slice(&c.data).to_vec() },
            ],
            info: Info { horizon: c.horizon },
        }
    }

    fn tick(&mut self) {
        self.clock.tick();
    }

    /// Per frame: the city cache (only ~15x a second), one fullscreen pass
    /// for city + water, then rain and petals as instanced sprites, so the
    /// per-pixel work is a texture read almost everywhere.
    fn draw(&self, r: &Renderer, g: &Programs, board: &mut Board<Info>, _: &mut (), f: &Frame) {
        let (w, h) = (f.w as u32, f.h as u32);
        let horizon = board.info.horizon;
        let c = &self.clock;
        let p = c.petal.map(|(x, y)| (x as f32, y as f32));
        let slant = c.slant();
        let mut u: Uniforms = [[0.0; 4]; U_LEN];
        u[0] = f.u0();
        u[1] = [horizon, c.gust, slant, 0.0];
        u[2] = f.mouse.map_or([0.0; 4], |(x, y)| [x, y, 1.0, 0.0]);
        u[3] = [p[0].0, p[0].1, p[1].0, p[1].1];
        u[4] = [p[2].0, p[2].1, 0.0, 0.0];
        u[5] = [c.rain[0] as f32, c.rain[1] as f32, c.rain[2] as f32, 0.0];

        if board.targets.is_empty() {
            let cache_h = (horizon.ceil() as u32 + 4).min(h);
            board.targets.push(r.target(w, cache_h));
        }
        let stale = board.stamp.map_or(true, |s| f.t < s || f.t - s >= 1.0 / CITY_FPS);
        r.blend(Blend::Off);
        if stale {
            let cache = &board.targets[0];
            r.bind_target(Some(cache.fb), cache.w, cache.h);
            r.use_program(&g.city, &u, &board.textures);
            r.fullscreen();
            board.stamp = Some(f.t);
            r.bind_target(f.target, w, h);
        }
        let cache = [board.targets[0].tex];
        r.use_program(&g.base, &u, &cache);
        r.fullscreen();

        r.blend(Blend::Add);
        for l in 0..3 {
            let lf = l as f32;
            let spacing = 16.0 + lf * 12.0;
            let len = 26.0 + lf * 22.0;
            // Columns are in x' = x - y * slant, which spans [-h * slant, w].
            let c0 = (-f.h * slant / spacing).floor() - 1.0;
            let c1 = (f.w / spacing).floor() + 1.0;
            let slots = (f.h / (len * 3.0)).ceil() + 2.0;
            u[6] = [lf, c0, slots, 0.0];
            r.use_program(&g.rain, &u, &cache);
            r.quads(((c1 - c0 + 1.0) * slots) as i32);
        }

        r.blend(Blend::Alpha);
        for (l, (cs, pr, k, m)) in PETALS.into_iter().enumerate() {
            let nx = (f.w / cs).ceil() + 5.0;
            let ny = (f.h / cs).ceil() + 5.0;
            u[6] = [l as f32, cs, pr, nx];
            u[7] = [0.0, 0.0, k, m];
            r.use_program(&g.petal, &u, &[]);
            r.quads((nx * ny) as i32);
        }
        r.blend(Blend::Off);
    }

    fn clear_color(&self) -> [f32; 3] {
        [0.004, 0.005, 0.014]
    }
}

#[cfg(test)]
mod preview {
    //! `OG_SCENE_PREVIEW=/some/dir cargo test --release -p og-wallpaper tokyo::preview -- --ignored`
    use super::*;

    #[test]
    #[ignore]
    fn preview() {
        let dir = std::path::PathBuf::from(std::env::var("OG_SCENE_PREVIEW").expect("set OG_SCENE_PREVIEW"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut g = gl::preview::Gpu::new(Tokyo::default());
        // Advance the clock as if running, so petals and rain are mid-flight.
        for _ in 0..3 {
            let c = &mut g.scene().clock;
            c.last = Some(Instant::now() - std::time::Duration::from_millis(200));
            c.tick();
        }
        g.scene().clock.t = 37.0;
        for (w, h) in [(2560u32, 1440u32), (5120, 1440)] {
            let mut board = g.board(w, h);
            for (i, mouse) in [None, Some((w as f32 * 0.4, h as f32 * 0.3))].into_iter().enumerate() {
                let (img, ms) = g.render(&mut board, w, h, 37.0 + i as f32 * 3.0, mouse, &mut (), 120);
                eprintln!("tokyo {w}x{h} mouse={}: {ms:.3} ms/frame", mouse.is_some());
                img.save(dir.join(format!("tokyo_{w}x{h}_{i}.png"))).unwrap();
            }
        }
    }
}
