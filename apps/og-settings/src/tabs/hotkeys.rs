use iced::widget::{button, column, container, row, text, text_input};
use iced::{Background, Border, Color, Element, Length};

use crate::app::{AppColors, Message};

pub fn view<'a>(
    colors: AppColors,
    variables: &'a [(String, String)],
    capturing_var: Option<usize>,
    bindings: &'a [(String, String)],
    capturing: Option<usize>,
) -> Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.surface)),
        text_color: colors.text,
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let accent_btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.accent)),
        text_color: colors.bar_bg,
        border: Border { radius: colors.radius.into(), ..Default::default() },
        ..Default::default()
    };
    let danger_btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 })),
        text_color: Color::WHITE,
        border: Border { radius: colors.radius.into(), ..Default::default() },
        ..Default::default()
    };

    // ── Capture banner (shown when either capture mode is active) ─────────
    let any_capturing = capturing.is_some() || capturing_var.is_some();
    let note = if any_capturing {
        let msg = if capturing_var.is_some() {
            "[Key] Press a modifier key (Super, Alt, Ctrl, Shift) to set variable value."
        } else {
            "[Key] Press your key combination now. Non-modifier key commits the binding."
        };
        container(
            text(msg).size(13)
                .style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
        )
        .style(move |_| container::Style {
            background: Some(Background::Color(Color {
                r: colors.accent.r * 0.15 + colors.bar_bg.r * 0.85,
                g: colors.accent.g * 0.15 + colors.bar_bg.g * 0.85,
                b: colors.accent.b * 0.15 + colors.bar_bg.b * 0.85,
                a: 1.0,
            })),
            border: Border { color: colors.accent, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .padding([10, 16])
        .width(Length::Fill)
    } else {
        container(
            text("Bindings are loaded from ~/.config/sway/config. Use Capture to detect key combos. Save writes back.")
                .size(12)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
        )
        .padding([10, 16])
        .width(Length::Fill)
    };

    // ── Variables card ────────────────────────────────────────────────────
    let var_rows: Vec<Element<Message>> = variables.iter().enumerate().map(|(i, (name, val))| {
        let is_cap = capturing_var == Some(i);

        let cap_btn: Element<Message> = if is_cap {
            button(
                text("* Press key...").size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) })
            )
            .style(move |_, _| iced::widget::button::Style {
                background: Some(Background::Color(colors.accent)),
                text_color: colors.bar_bg,
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            })
            .width(120)
            .padding([4, 6])
            .into()
        } else {
            button(
                text("Capture").size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            )
            .style(btn_style)
            .on_press(Message::HotkeyVarStartCapture(i))
            .width(120)
            .padding([4, 6])
            .into()
        };

        let name_input: Element<Message> = text_input("$mod", name)
            .on_input(move |v| Message::HotkeyVarNameEdit(i, v))
            .style(move |_, _| iced::widget::text_input::Style {
                background: Background::Color(colors.surface),
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                icon: colors.dim_text, placeholder: colors.dim_text,
                value: colors.accent, selection: colors.accent,
            })
            .width(100)
            .into();

        let val_input: Element<Message> = text_input("Mod4", val)
            .on_input(move |v| Message::HotkeyVarValueEdit(i, v))
            .style(move |_, _| iced::widget::text_input::Style {
                background: Background::Color(colors.surface),
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                icon: colors.dim_text, placeholder: colors.dim_text,
                value: colors.text, selection: colors.accent,
            })
            .width(Length::Fill)
            .into();

        let remove_btn: Element<Message> = button(
            text("x").size(12).style(move |_| iced::widget::text::Style { color: Some(Color::WHITE) })
        )
        .style(danger_btn_style)
        .on_press(Message::HotkeyVarRemove(i))
        .width(32)
        .padding([4, 6])
        .into();

        row![
            name_input,
            container(text("=").size(13).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }))
                .padding([0, 6]),
            val_input,
            cap_btn,
            remove_btn,
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into()
    }).collect();

    let add_var_btn: Element<Message> = button(
        text("+ Add Variable").size(13)
            .style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) })
    )
    .style(accent_btn_style)
    .on_press(Message::HotkeyVarAdd)
    .padding([6, 14])
    .into();

    let vars_card = container(
        column(
            std::iter::once(add_var_btn)
            .chain(std::iter::once(
                row![
                    container(
                        text("Variable").size(13)
                            .style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
                    ).width(100),
                    container(iced::widget::Space::new(18, 1)).width(18),
                    container(
                        text("Value").size(13)
                            .style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
                    ).width(Length::Fill),
                    container(
                        text("Capture").size(13)
                            .style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
                    ).width(120),
                    container(iced::widget::Space::new(40, 1)).width(40),
                ]
                .spacing(8)
                .padding([0, 8])
                .into()
            ))
            .chain(var_rows)
            .collect::<Vec<_>>()
        )
        .spacing(8)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Bindings card ─────────────────────────────────────────────────────
    let header = row![
        container(
            text("Keys").size(13).style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
        ).width(200),
        container(
            text("Capture").size(13).style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
        ).width(110),
        container(
            text("Command").size(13).style(move |_| iced::widget::text::Style { color: Some(colors.accent) })
        ).width(Length::Fill),
        container(text("").size(13)).width(36),
    ]
    .spacing(8)
    .padding([0, 8]);

    let rows: Vec<Element<Message>> = bindings.iter().enumerate().map(|(i, (keys, cmd))| {
        let is_capturing = capturing == Some(i);

        let capture_btn: Element<Message> = if is_capturing {
            button(
                text("* Press keys...").size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) })
            )
            .style(move |_, _| iced::widget::button::Style {
                background: Some(Background::Color(colors.accent)),
                text_color: colors.bar_bg,
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            })
            .width(110)
            .padding([4, 6])
            .into()
        } else {
            button(
                text("Capture").size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            )
            .style(btn_style)
            .on_press(Message::HotkeyStartCapture(i))
            .width(110)
            .padding([4, 6])
            .into()
        };

        let keys_input: Element<Message> = text_input("key+combo", keys)
            .on_input(move |v| Message::HotkeyKeyEdit(i, v))
            .style(move |_, _| iced::widget::text_input::Style {
                background: Background::Color(colors.surface),
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                icon: colors.dim_text, placeholder: colors.dim_text,
                value: colors.text, selection: colors.accent,
            })
            .width(200)
            .into();

        let cmd_input: Element<Message> = text_input("sway command, or: exec <app> to launch", cmd)
            .on_input(move |v| Message::HotkeyCommandEdit(i, v))
            .style(move |_, _| iced::widget::text_input::Style {
                background: Background::Color(colors.surface),
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                icon: colors.dim_text, placeholder: colors.dim_text,
                value: colors.text, selection: colors.accent,
            })
            .width(Length::Fill)
            .into();

        let remove_btn: Element<Message> = button(
            text("x").size(12).style(move |_| iced::widget::text::Style { color: Some(Color::WHITE) })
        )
        .style(danger_btn_style)
        .on_press(Message::HotkeyRemove(i))
        .width(32)
        .padding([4, 6])
        .into();

        row![keys_input, capture_btn, cmd_input, remove_btn]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
    }).collect();

    let add_binding_btn: Element<Message> = button(
        text("+ Add Binding").size(13)
            .style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) })
    )
    .style(accent_btn_style)
    .on_press(Message::HotkeyAdd)
    .padding([6, 14])
    .into();

    let bindings_card = container(
        column(
            std::iter::once(add_binding_btn)
                .chain(std::iter::once(header.into()))
                .chain(rows)
                .collect::<Vec<_>>()
        )
        .spacing(8)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    column![note, vars_card, bindings_card].spacing(16).padding(20).into()
}
