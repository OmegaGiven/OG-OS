//! Shared palette, loaded from the suite config via og-config/og-theme and
//! hot-reloaded when og-settings' Apply & Save changes it (see the
//! `CheckThemeReload` poll in `app.rs`). Every call site goes through a
//! function so a reload is picked up on the next frame with no plumbing.

use iced::{widget::{button, container, text, text_input}, Background, Border, Color};

const APP_TINT_SEED: &str = "og-files";

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color,
    pub sec_bg: Color,
    pub text: Color,
    pub accent: Color,
    pub muted: Color,
    pub selected_bg: Color,
    pub surface: Color,
    pub border: Color,
    pub danger: Color,
}

fn load() -> Palette {
    let cfg = og_config::Config::load();
    let c = og_theme::AppColors::from_config(&cfg, APP_TINT_SEED);
    let mix = |a: Color, b: Color, t: f32| Color {
        r: a.r * t + b.r * (1.0 - t),
        g: a.g * t + b.g * (1.0 - t),
        b: a.b * t + b.b * (1.0 - t),
        a: 1.0,
    };
    Palette {
        bg: c.bar_bg,
        sec_bg: c.sec_bg,
        text: c.text,
        accent: c.accent,
        muted: c.dim_text,
        selected_bg: mix(c.accent, c.bar_bg, 0.25),
        surface: c.surface,
        border: mix(c.text, c.bar_bg, 0.18),
        danger: Color { r: 0.75, g: 0.2, b: 0.2, a: 1.0 },
    }
}

static THEME: std::sync::LazyLock<std::sync::RwLock<Palette>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(load()));

pub fn reload() {
    if let Ok(mut g) = THEME.write() {
        *g = load();
    }
}

pub fn p() -> Palette {
    *THEME.read().unwrap()
}

pub const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");

// ── Reusable style closures ────────────────────────────────────────────────

pub fn muted_text(_: &iced::Theme) -> text::Style {
    text::Style { color: Some(p().muted) }
}

pub fn normal_text(_: &iced::Theme) -> text::Style {
    text::Style { color: Some(p().text) }
}

pub fn accent_text(_: &iced::Theme) -> text::Style {
    text::Style { color: Some(p().accent) }
}

/// Flat toolbar / menu button: secondary background, surface on hover.
pub fn flat_button(_: &iced::Theme, status: button::Status) -> button::Style {
    let pal = p();
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => pal.surface,
            _ => pal.sec_bg,
        })),
        text_color: pal.text,
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    }
}

/// Like `flat_button` but visibly disabled.
pub fn disabled_button(_: &iced::Theme, _: button::Status) -> button::Style {
    let pal = p();
    button::Style {
        background: Some(Background::Color(pal.sec_bg)),
        text_color: pal.muted,
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    }
}

/// Primary (accent-filled) action button.
pub fn accent_button(_: &iced::Theme, status: button::Status) -> button::Style {
    let pal = p();
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => Color { a: 0.85, ..pal.accent },
        _ => pal.accent,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: pal.bg,
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    }
}

pub fn danger_button(_: &iced::Theme, status: button::Status) -> button::Style {
    let pal = p();
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => Color { r: 0.85, g: 0.25, b: 0.25, a: 1.0 },
        _ => pal.danger,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    }
}

pub fn input_style(_: &iced::Theme, status: text_input::Status) -> text_input::Style {
    let pal = p();
    text_input::Style {
        background: Background::Color(pal.surface),
        border: Border {
            color: if matches!(status, text_input::Status::Focused) { pal.accent } else { pal.border },
            width: 1.0,
            radius: 4.0.into(),
        },
        icon: pal.text,
        placeholder: pal.muted,
        value: pal.text,
        selection: Color { a: 0.5, ..pal.accent },
    }
}

/// Dialog / popup chrome.
pub fn dialog_box(_: &iced::Theme) -> container::Style {
    let pal = p();
    container::Style {
        background: Some(Background::Color(pal.sec_bg)),
        border: Border { color: pal.border, width: 1.0, radius: 6.0.into() },
        ..Default::default()
    }
}

pub fn panel(_: &iced::Theme) -> container::Style {
    container::Style { background: Some(Background::Color(p().sec_bg)), ..Default::default() }
}

pub fn main_bg(_: &iced::Theme) -> container::Style {
    container::Style { background: Some(Background::Color(p().bg)), ..Default::default() }
}

/// Dim scrim behind a modal dialog so the underlying view reads as inactive.
pub fn scrim(_: &iced::Theme) -> container::Style {
    container::Style { background: Some(Background::Color(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 })), ..Default::default() }
}

pub fn separator(_: &iced::Theme) -> container::Style {
    container::Style { background: Some(Background::Color(p().border)), ..Default::default() }
}
