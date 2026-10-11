//! Standalone palette. With the `og-suite` feature (not wired yet, SPEC.md
//! §7) this would be replaced by the og-config / og-theme palette.

use iced::theme::Palette;
use iced::{color, Theme};

pub fn theme() -> Theme {
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

pub const MUTED: iced::Color = color!(0x8a857b);

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
