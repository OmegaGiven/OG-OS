//! Mouse & Keyboard tab — split out of Theme (PointerSensitivity/cursor
//! theme/size lived there before) so input behavior has its own home
//! alongside the new sway-native focus/warping/repeat controls.

use iced::widget::{button, column, container, image, pick_list, row, text};
use iced::{Background, Border, Element, Length};

use og_config::{FocusFollowsMouse, MouseWarping};

use crate::app::{AppColors, Message};
use crate::config::Config;
use crate::cursor_theme::CURSOR_ROLES;

pub fn view<'a>(
    config: &'a Config,
    colors: AppColors,
    available_cursor_themes: &'a [String],
    cursor_import_error: Option<&'a str>,
) -> Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let btn_style = move |_theme: &iced::Theme, _status: iced::widget::button::Status| iced::widget::button::Style {
        background: Some(Background::Color(colors.surface)),
        text_color: colors.text,
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let label = move |t: &'static str| -> Element<'a, Message> {
        text(t).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into()
    };
    let spin = move |value: i32, minus: Message, plus: Message| -> Element<'a, Message> {
        row![
            button(text("-").style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(btn_style).on_press(minus),
            container(text(value.to_string()).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.surface)),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .padding([4, 12]),
            button(text("+").style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(btn_style).on_press(plus),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    };
    // Same visual language as the theme tab's preset buttons: filled +
    // accent-colored when active, outlined otherwise.
    let choice_btn = move |lbl: &'static str, active: bool, msg: Message| -> Element<'a, Message> {
        let bg = if active { colors.accent } else { colors.surface };
        let fg = if active { colors.bar_bg } else { colors.text };
        button(text(lbl).size(12).style(move |_| iced::widget::text::Style { color: Some(fg) }))
            .style(move |_, _| iced::widget::button::Style {
                background: Some(Background::Color(bg)),
                text_color: fg,
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                ..Default::default()
            })
            .on_press(msg)
            .padding([6, 12])
            .into()
    };

    let focus_row = row![
        label("Window focus"),
        iced::widget::horizontal_space(),
        row![
            choice_btn(
                FocusFollowsMouse::No.label(),
                config.focus_follows_mouse == FocusFollowsMouse::No,
                Message::FocusFollowsMouseChanged(FocusFollowsMouse::No),
            ),
            choice_btn(
                FocusFollowsMouse::Yes.label(),
                config.focus_follows_mouse == FocusFollowsMouse::Yes,
                Message::FocusFollowsMouseChanged(FocusFollowsMouse::Yes),
            ),
            choice_btn(
                FocusFollowsMouse::Always.label(),
                config.focus_follows_mouse == FocusFollowsMouse::Always,
                Message::FocusFollowsMouseChanged(FocusFollowsMouse::Always),
            ),
        ]
        .spacing(6)
        .wrap(),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(12);

    let warp_row = row![
        label("Cursor warping"),
        iced::widget::horizontal_space(),
        row![
            choice_btn(
                MouseWarping::Output.label(),
                config.mouse_warping == MouseWarping::Output,
                Message::MouseWarpingChanged(MouseWarping::Output),
            ),
            choice_btn(
                MouseWarping::Container.label(),
                config.mouse_warping == MouseWarping::Container,
                Message::MouseWarpingChanged(MouseWarping::Container),
            ),
            choice_btn(
                MouseWarping::None.label(),
                config.mouse_warping == MouseWarping::None,
                Message::MouseWarpingChanged(MouseWarping::None),
            ),
        ]
        .spacing(6)
        .wrap(),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(12);

    let sensitivity_row = {
        use iced::widget::slider;
        row![
            label("Pointer sensitivity"),
            iced::widget::horizontal_space(),
            slider(-1.0f32..=1.0f32, config.mouse_sensitivity, Message::MouseSensitivityChanged)
                .step(0.05)
                .width(220),
            container(
                text(format!("{:+.2}", config.mouse_sensitivity))
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            ).width(50),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12)
    };

    let cursor_theme_options: Vec<String> = if available_cursor_themes.is_empty() {
        vec![config.cursor_theme.clone()]
    } else {
        available_cursor_themes.to_vec()
    };
    let cursor_theme_row = row![
        label("Cursor icon theme"),
        iced::widget::horizontal_space(),
        pick_list(
            cursor_theme_options,
            if config.cursor_theme.is_empty() { None } else { Some(config.cursor_theme.clone()) },
            Message::CursorThemeChanged,
        )
        .style(move |_, _| iced::widget::pick_list::Style {
            background: Background::Color(colors.surface),
            text_color: colors.text,
            placeholder_color: colors.dim_text,
            handle_color: colors.dim_text,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        })
        .menu_style(crate::app::pick_list_menu_style(colors))
        .width(200),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(12);

    let mouse_card = container(
        column![
            text("Mouse").size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
            focus_row,
            warp_row,
            sensitivity_row,
            cursor_theme_row,
            row![label("Cursor size (pixels)"), iced::widget::horizontal_space(),
                spin(config.cursor_size, Message::CursorSizeMinus, Message::CursorSizePlus)]
                .align_y(iced::Alignment::Center).spacing(12),
        ]
        .spacing(16).padding(20),
    )
    .style(card_style).width(Length::Fill);

    let keyboard_card = container(
        column![
            text("Keyboard").size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
            row![label("Key repeat rate (chars/sec)"), iced::widget::horizontal_space(),
                spin(config.keyboard_repeat_rate, Message::KeyboardRepeatRateMinus, Message::KeyboardRepeatRatePlus)]
                .align_y(iced::Alignment::Center).spacing(12),
            row![label("Key repeat delay (ms)"), iced::widget::horizontal_space(),
                spin(config.keyboard_repeat_delay, Message::KeyboardRepeatDelayMinus, Message::KeyboardRepeatDelayPlus)]
                .align_y(iced::Alignment::Center).spacing(12),
        ]
        .spacing(16).padding(20),
    )
    .style(card_style).width(Length::Fill);

    // ── Custom cursor icons ────────────────────────────────────────────────
    let role_row = move |role: &'static crate::cursor_theme::CursorRole| -> Element<'a, Message> {
        let customized = crate::cursor_theme::role_is_customized(role.key);
        let thumb: Element<Message> = if let Some(path) = crate::cursor_theme::preview_path_for_role(role.key) {
            container(image(path).width(28).height(28))
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.surface)),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .padding(4)
                .into()
        } else {
            container(iced::widget::Space::new(28, 28))
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.surface)),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .padding(4)
                .into()
        };

        let mut actions: Vec<Element<Message>> = vec![
            button(text("Choose Image…").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(btn_style)
                .on_press(Message::CursorRoleImagePick(role.key.to_string()))
                .padding([6, 12])
                .into(),
        ];
        if customized {
            actions.push(
                button(text("Reset").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }))
                    .style(btn_style)
                    .on_press(Message::CursorRoleReset(role.key.to_string()))
                    .padding([6, 12])
                    .into(),
            );
        }

        row![
            thumb,
            text(role.label).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
            iced::widget::horizontal_space(),
            row(actions).spacing(8),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12)
        .into()
    };

    let mut cursor_col: Vec<Element<Message>> = vec![
        text("Custom Cursor Icons").size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into(),
        text("Override individual pointer shapes with your own SVG or PNG. Anything you don't override keeps using the OG_Red cursor.")
            .size(11)
            .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
            .into(),
    ];
    if let Some(err) = cursor_import_error {
        cursor_col.push(
            text(err).size(11).style(|_| iced::widget::text::Style { color: Some(iced::Color::from_rgb(0.9, 0.3, 0.3)) }).into(),
        );
    }
    for role in CURSOR_ROLES {
        cursor_col.push(role_row(role));
    }

    let custom_cursor_card = container(
        column(cursor_col).spacing(12).padding(20),
    )
    .style(card_style).width(Length::Fill);

    column![mouse_card, keyboard_card, custom_cursor_card].spacing(16).padding(20).into()
}
