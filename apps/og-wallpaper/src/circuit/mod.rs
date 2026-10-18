//! "circuit" procedural theme: a futuristic motherboard with data pulses racing
//! along the buses to and from a central CPU, whose neon Ω has an eye that
//! follows the cursor. Rendered by circuit.frag (see direct/).
//!
//! Per output size the board is generated once (see board.rs) and uploaded
//! as textures; every frame after that only the uniforms change.

mod board;

use std::time::Instant;

use crate::direct::gl::{self, Blend, Board, Frame, Generated, Program, Renderer, Tex, TexFormat, Uniforms, U_LEN};

/// Theme colors, straight sRGB 0..1.
#[derive(Clone)]
pub struct Circuit {
    pub accent: [f32; 3],
    pub bg: [f32; 3],
}

/// Smoothed eye position (buffer px), per output.
#[derive(Default)]
pub struct Eye {
    pos: Option<(f32, f32)>,
    last: Option<Instant>,
    seen_cursor: Option<Instant>,
}

pub struct Info {
    cpu_center: (f32, f32),
    cpu_size: f32,
}

fn fract(x: f32) -> f32 {
    x - x.floor()
}

fn hash(x: f32) -> f32 {
    fract((x * 127.1 + 311.7).sin() * 43758.547)
}

fn blink(t: f32) -> f32 {
    // A blink every ~6-11 s, 180 ms long.
    let period = 8.5;
    let k = (t / period).floor();
    let at = k * period + 4.0 * fract(k * 0.618);
    let x = (t - at) / 0.18;
    if (0.0..1.0).contains(&x) { (x * std::f32::consts::PI).sin() } else { 0.0 }
}

fn linear(c: [f32; 3]) -> [f32; 3] {
    c.map(gl::srgb_decode)
}

impl Circuit {
    fn eye(&self, eye: &mut Eye, info: &Info, f: &Frame) -> (f32, f32) {
        let r = info.cpu_size * 0.20;
        // The eye rests where the omega's ring is centred (shader: -0.12r).
        let rest = (info.cpu_center.0, info.cpu_center.1 - r * 0.12);
        let max_off = r * 0.44; // keeps the eye ring inside the omega
        let now = Instant::now();
        let target = match f.mouse {
            Some(p) => {
                eye.seen_cursor = Some(now);
                let (dx, dy) = (p.0 - rest.0, p.1 - rest.1);
                let dist = (dx * dx + dy * dy).sqrt().max(1e-3);
                // Saturates quickly: a cursor 600 px away already looks fully
                // to that side.
                let k = max_off * (dist / 600.0).min(1.0) / dist;
                (rest.0 + dx * k, rest.1 + dy * k)
            }
            None => {
                // No cursor over the desktop: hold the last gaze briefly, then
                // wander lazily.
                let idle = eye.seen_cursor.map_or(f32::MAX, |s| now.duration_since(s).as_secs_f32());
                match eye.pos {
                    Some(p) if idle < 3.0 => p,
                    _ => (
                        rest.0 + max_off * 0.55 * (f.t * 0.23).sin() * (f.t * 0.071).cos(),
                        rest.1 + max_off * 0.35 * (f.t * 0.17 + 1.3).sin(),
                    ),
                }
            }
        };
        let dt = eye.last.map_or(1.0, |l| now.duration_since(l).as_secs_f32()).min(0.25);
        eye.last = Some(now);
        let pos = match eye.pos {
            None => target,
            Some(p) => {
                let a = 1.0 - (-dt * 9.0).exp(); // critically-damped-ish ease
                (p.0 + (target.0 - p.0) * a, p.1 + (target.1 - p.1) * a)
            }
        };
        eye.pos = Some(pos);
        pos
    }
}

impl gl::Scene for Circuit {
    type Out = Eye;
    type Info = Info;
    type Gl = Program;

    fn init_gl(r: &Renderer) -> Result<Program, String> {
        r.program(gl::FULLSCREEN_VERT, include_str!("circuit.frag"), &["board", "lut", "data"])
    }

    fn generate(&self, w: u32, h: u32) -> Generated<Info> {
        let b = board::generate(w, h);
        // bg→accent ramp, mixed in sRGB like the board was drawn; the
        // texture is sRGB so sampling returns linear for free.
        let lut = (0..256)
            .flat_map(|i| {
                let k = i as f32 / 255.0;
                let c = [0, 1, 2].map(|j| self.bg[j] + (self.accent[j] - self.bg[j]) * k);
                [c[0], c[1], c[2], 1.0].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            })
            .collect();
        Generated {
            textures: vec![
                Tex { w, h, format: TexFormat::R8, bytes: b.static_k },
                Tex { w: 256, h: 1, format: TexFormat::Srgba8, bytes: lut },
                Tex { w, h, format: TexFormat::R32Ui, bytes: bytemuck::cast_slice(&b.data).to_vec() },
            ],
            info: Info { cpu_center: (b.cpu_center.x, b.cpu_center.y), cpu_size: b.cpu_size },
        }
    }

    fn draw(&self, r: &Renderer, prog: &Program, board: &mut Board<Info>, eye: &mut Eye, f: &Frame) {
        let mut u: Uniforms = [[0.0; 4]; U_LEN];
        u[0] = f.u0();
        self.uniforms(eye, &board.info, f, &mut u);
        r.blend(Blend::Off);
        r.use_program(prog, &u, &board.textures);
        r.fullscreen();
    }

    fn clear_color(&self) -> [f32; 3] {
        linear(self.bg)
    }
}

impl Circuit {
    fn uniforms(&self, eye: &mut Eye, info: &Info, f: &Frame, u: &mut Uniforms) {
        let t = f.t;
        let accent = linear(self.accent);
        let hot = [0, 1, 2].map(|i| accent[i] + ([1.0, 0.95, 0.85][i] - accent[i]) * 0.45);
        let breathe = 0.5 + 0.5 * (t * 0.9).sin() * (t * 0.37 + 1.0).sin();
        let flicker = 0.93 + 0.07 * hash((t * 24.0).floor()) * if hash((t * 3.0).floor()) >= 0.97 { 1.0 } else { 0.0 };
        let b = blink(t);
        let (ex, ey) = self.eye(eye, info, f);
        u[1] = [accent[0], accent[1], accent[2], breathe];
        u[2] = [hot[0], hot[1], hot[2], flicker];
        u[3] = [info.cpu_center.0, info.cpu_center.1, info.cpu_size * 0.31, info.cpu_size * 0.20];
        u[4] = [ex, ey, b, 1.0 - b * 0.85];
        u[5] = [f.h / f.w * 1.6, 0.0, 0.0, 0.0];
    }
}

#[cfg(test)]
mod preview {
    //! `OG_SCENE_PREVIEW=/some/dir cargo test --release -p og-wallpaper circuit::preview -- --ignored`
    use super::*;

    #[test]
    #[ignore]
    fn preview() {
        let dir = std::path::PathBuf::from(std::env::var("OG_SCENE_PREVIEW").expect("set OG_SCENE_PREVIEW"));
        std::fs::create_dir_all(&dir).unwrap();
        let config = og_config::Config::load();
        let colors = og_theme::AppColors::from_config(&config, "og-wallpaper");
        let c = |c: iced::Color| [c.r, c.g, c.b];
        let mut g = gl::preview::Gpu::new(Circuit { accent: c(colors.accent), bg: c(colors.bar_bg) });
        for (w, h) in [(2560u32, 1440u32), (5120, 1440)] {
            let mut board = g.board(w, h);
            for (i, (t, mouse)) in [(3.0, None), (7.4, Some((w as f32 * 0.8, h as f32 * 0.2))), (11.9, Some((0.0, h as f32)))].into_iter().enumerate() {
                let (img, ms) = g.render(&mut board, w, h, t, mouse, &mut Eye::default(), 120);
                eprintln!("circuit {w}x{h}: {ms:.3} ms/frame");
                img.save(dir.join(format!("circuit_{w}x{h}_{i}.png"))).unwrap();
            }
        }
    }
}
