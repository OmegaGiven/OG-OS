use iced::widget::{button, column, container, row, text, text_input, toggler};
use iced::{Background, Border, Color, Element, Length};

use crate::app::{AppColors, Message};
use crate::config::Config;

fn hex_to_color(hex: &str) -> Color {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return Color::BLACK;
    }
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(0);
    Color::from_rgb8(r, g, b)
}

pub fn view<'a>(config: &'a Config, colors: AppColors, daemon_running: bool, notifications_enabled: bool) -> Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };

    let section_title = |t: &'a str| -> Element<'a, Message> {
        text(t)
            .size(15)
            .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            .into()
    };
    let label_text = |t: &'a str| -> Element<'a, Message> {
        text(t)
            .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            .into()
    };

    let master_card = container(
        column![
            section_title("Notifications"),
            row![
                column![
                    label_text("Enable notifications"),
                    text("Master switch for the whole system (mako do-not-disturb) — off silences everything, not just the effect. Also in the bell icon's menu as \"Notification Preview\".")
                        .size(11)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                ]
                .spacing(2)
                .width(Length::Fill),
                toggler(notifications_enabled).on_toggle(Message::NotificationsMasterToggled),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
        ]
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    let main_card = container(
        column![
            section_title("Notification Effects"),
            text("Flashes a rain-streak or ambient edge-glow overlay across every monitor whenever a notification is about to show.")
                .size(11)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
            row![
                column![
                    label_text("Enable notification effects"),
                    text(if daemon_running { "Running" } else { "Not running" })
                        .size(11)
                        .style(move |_| iced::widget::text::Style {
                            color: Some(if daemon_running { colors.accent } else { colors.dim_text })
                        }),
                ]
                .spacing(2)
                .width(Length::Fill),
                toggler(config.notif_fx_enabled).on_toggle(Message::NotifFxEnabledToggled),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            row![
                label_text("Effect"),
                iced::widget::horizontal_space(),
                effect_choice("Rain", "rain", &config.notif_fx_effect, colors),
                effect_choice("Glow", "glow", &config.notif_fx_effect, colors),
                effect_choice("Wind", "wind", &config.notif_fx_effect, colors),
                effect_choice("Sparkle", "sparkle", &config.notif_fx_effect, colors),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(8),
            row![
                label_text("Color"),
                iced::widget::horizontal_space(),
                container(iced::widget::Space::new(24, 24))
                    .style(move |_| container::Style {
                        background: Some(Background::Color(hex_to_color(&config.notif_fx_color))),
                        border: Border { color: colors.border, width: 1.0, radius: 6.0.into() },
                        ..Default::default()
                    }),
                text_input("#ff7800", &config.notif_fx_color)
                    .width(Length::Fixed(100.0))
                    .on_input(Message::NotifFxColorChanged)
                    .style(move |_, _| iced::widget::text_input::Style {
                        background: Background::Color(colors.surface),
                        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                        icon: colors.dim_text,
                        placeholder: colors.dim_text,
                        value: colors.text,
                        selection: colors.accent,
                    }),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            row![
                label_text("Duration (ms)"),
                iced::widget::horizontal_space(),
                spin_row(config.notif_fx_duration_ms as i64, Message::NotifFxDurationMinus, Message::NotifFxDurationPlus, colors),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            button(text("Send test notification").color(colors.text))
                .style(move |_, _| iced::widget::button::Style {
                    background: Some(Background::Color(colors.surface)),
                    text_color: colors.text,
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .on_press(Message::NotifFxTestNotification),
        ]
        .spacing(16)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    iced::widget::scrollable(
        column![master_card, main_card].spacing(16).padding(20)
    )
    .into()
}

fn effect_choice<'a>(label: &'a str, value: &'static str, current: &str, colors: AppColors) -> Element<'a, Message> {
    let selected = current == value;
    button(text(label).color(if selected { colors.bar_bg } else { colors.text }))
        .style(move |_, _| iced::widget::button::Style {
            background: Some(Background::Color(if selected { colors.accent } else { colors.surface })),
            text_color: if selected { colors.bar_bg } else { colors.text },
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .padding([6, 14])
        .on_press(Message::NotifFxEffectSelected(value.to_string()))
        .into()
}

fn spin_row<'a>(value: i64, on_minus: Message, on_plus: Message, colors: AppColors) -> Element<'a, Message> {
    let btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.surface)),
        text_color: colors.text,
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    row![
        button(text("-").color(colors.text)).style(btn_style).on_press(on_minus),
        container(
            text(value.to_string())
                .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
        )
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.surface)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .padding([4, 12]),
        button(text("+").color(colors.text)).style(btn_style).on_press(on_plus),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}
