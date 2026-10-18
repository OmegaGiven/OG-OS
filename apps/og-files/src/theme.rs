//! Palette. Standalone builds use a built-in dark theme; with the
//! `og-suite` feature the colours come from the OG-OS suite config
//! (`og-config` + `og-theme`, the same source og-bar and og-settings use)
//! and are reloaded when that config file changes (see
//! `App::subscription`).

use iced::theme::Palette;
use iced::{color, Theme};
use std::sync::{LazyLock, RwLock};

pub const MUTED: iced::Color = color!(0x8a857b);

static THEME: LazyLock<RwLock<Theme>> = LazyLock::new(|| RwLock::new(load()));

/// The current theme (cheap: cached; `reload` refreshes it).
pub fn theme() -> Theme {
    THEME
        .read()
        .map(|t| t.clone())
        .unwrap_or_else(|_| builtin())
}

/// Re-read the suite config (no-op for standalone builds).
pub fn reload() {
    if let Ok(mut t) = THEME.write() {
        *t = load();
    }
}

/// Folder holding the suite config, watched for live theme changes.
pub fn config_dir() -> Option<std::path::PathBuf> {
    if cfg!(feature = "og-suite") {
        Some(crate::config::home().join(".config/sway-power"))
    } else {
        None
    }
}

fn builtin() -> Theme {
    Theme::custom(
        "OG Dark".to_string(),
        Palette {
            background: color!(0x15130f),
            text: color!(0xe8e4dc),
            primary: color!(0x8fb3ff),
            success: color!(0x7fcf8f),
            danger: color!(0xff7a7a),
        },
    )
}

#[cfg(feature = "og-suite")]
fn load() -> Theme {
    let cfg = og_config::Config::load();
    let c = og_theme::AppColors::from_config(&cfg, "og-files");
    Theme::custom(
        "OG Suite".to_string(),
        Palette {
            background: c.bar_bg,
            text: c.text,
            primary: c.accent,
            success: color!(0x7fcf8f),
            danger: color!(0xe05a5a),
        },
    )
}

#[cfg(not(feature = "og-suite"))]
fn load() -> Theme {
    builtin()
}

pub fn panel(theme: &Theme) -> iced::widget::container::Style {
    let p = theme.extended_palette();
    iced::widget::container::Style {
        background: Some(p.background.weak.color.scale_alpha(0.35).into()),
        border: iced::Border {
            radius: 10.0.into(),
            width: 1.0,
            color: p.background.strong.color,
        },
        ..Default::default()
    }
}
