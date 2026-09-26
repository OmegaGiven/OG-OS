//! Shared theming math: hex parsing, the per-app color-variance tint, and
//! the `AppColors` bundle every OG-toolkit GUI app derives its widget
//! colors from. One definition here means adding a field (like `accent2`
//! or gradient support) benefits every app the moment it adopts this
//! crate, instead of needing the same hand-edit copied into each app's own
//! `app.rs`.

use iced_core::{Background, Color};
use og_config::Config;

pub fn hex_to_color(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    if h.len() < 6 {
        return Color::BLACK;
    }
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(128);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(128);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(128);
    Color::from_rgb8(r, g, b)
}

/// Deterministic small per-app background tint so windows sharing this
/// theme are easier to tell apart at a glance without losing the shared
/// base-color identity — not random, the same seed always gets the same
/// shift. Each app passes its own stable seed string (its own name) to
/// `AppColors::from_config`.
pub fn apply_color_variance(color: Color, seed: &str, enabled: bool, amount: f32) -> Color {
    if !enabled || amount <= 0.0 {
        return color;
    }
    let hash: u32 = seed.bytes().fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(b as u32));
    let t = (hash % 1000) as f32 / 1000.0;
    let shift = (t * 2.0 - 1.0) * amount;
    let clamp = |v: f32| (v + shift).clamp(0.0, 1.0);
    Color { r: clamp(color.r), g: clamp(color.g), b: clamp(color.b), a: color.a }
}

fn diagonal_gradient(from: Color, to: Color) -> Background {
    iced_core::gradient::Linear::new(iced_core::Radians(std::f32::consts::FRAC_PI_4))
        .add_stop(0.0, from)
        .add_stop(1.0, to)
        .into()
}

#[derive(Debug, Clone, Copy)]
pub struct AppColors {
    pub bar_bg: Color,
    pub sec_bg: Color,
    pub text: Color,
    pub dim_text: Color,
    pub accent: Color,
    /// Derived from `accent2` — unfocused window/workspace accents, and
    /// this app's own box borders.
    pub border: Color,
    pub surface: Color,
    pub header_btn_bg: Color,
    pub radius: f32,
    /// Solid `bar_bg`, or a `bar_bg -> sec_bg` gradient when the config's
    /// "Gradient" toggle is on.
    pub bg_fill: Background,
    /// Solid `accent`, or an `accent -> accent2` gradient likewise.
    pub accent_fill: Background,
}

impl AppColors {
    /// `tint_seed` should be a short, stable string unique to the calling
    /// app (its own binary name is the convention) — it's only ever used
    /// to derive this app's slice of the color-variance tint, so two apps
    /// sharing a seed would visually merge together when variance is on.
    pub fn from_config(cfg: &Config, tint_seed: &str) -> Self {
        let bar_bg = apply_color_variance(
            hex_to_color(&cfg.bar_bg), tint_seed, cfg.color_variance_enabled, cfg.color_variance_amount,
        );
        let sec_bg = apply_color_variance(
            hex_to_color(&cfg.sec_bg), tint_seed, cfg.color_variance_enabled, cfg.color_variance_amount,
        );
        let text = hex_to_color(&cfg.bar_text);
        let accent = hex_to_color(&cfg.accent);
        let accent2 = hex_to_color(&cfg.accent2);

        let dim_text = Color { a: 0.55, ..text };
        let border = Color { a: 0.65, ..accent2 };
        let surface = Color {
            r: text.r * 0.08 + bar_bg.r * 0.92,
            g: text.g * 0.08 + bar_bg.g * 0.92,
            b: text.b * 0.08 + bar_bg.b * 0.92,
            a: 1.0,
        };
        let header_btn_bg = Color {
            r: text.r * 0.12 + bar_bg.r * 0.88,
            g: text.g * 0.12 + bar_bg.g * 0.88,
            b: text.b * 0.12 + bar_bg.b * 0.88,
            a: 1.0,
        };

        let (bg_fill, accent_fill) = if cfg.gradient_enabled {
            (diagonal_gradient(bar_bg, sec_bg), diagonal_gradient(accent, accent2))
        } else {
            (Background::Color(bar_bg), Background::Color(accent))
        };

        Self {
            bar_bg, sec_bg, text, dim_text, accent, border, surface, header_btn_bg,
            radius: cfg.corner_radius, bg_fill, accent_fill,
        }
    }
}
