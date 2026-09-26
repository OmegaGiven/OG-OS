//! Browser-style tab strip. One row above the toolbar; each tab shows its
//! folder name and a close button, `+` opens a new one in the same folder.

use iced::widget::{button, container, row, text, Space};
use iced::{Background, Border, Color, Element, Length};

use crate::app::Message;
use crate::theme;

const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");
const TAB_MAX_WIDTH: f32 = 180.0;

pub fn view<'a>(titles: Vec<String>, active: usize) -> Element<'a, Message> {
    let pal = theme::p();
    let mut row_items = row![].spacing(2).padding([2, 4]);

    for (i, title) in titles.iter().enumerate() {
        // (owned Vec taken by value — no borrow of the caller's temporary)
        let is_active = i == active;
        let label = if title.chars().count() > 22 {
            format!("{}…", title.chars().take(21).collect::<String>())
        } else {
            title.clone()
        };

        let close_btn = button(text("\u{f00d}").font(ICON_FONT).size(10))
            .padding(2)
            .style(move |_, status| button::Style {
                background: Some(Background::Color(match status {
                    button::Status::Hovered => Color { a: 0.25, ..pal.danger },
                    _ => Color::TRANSPARENT,
                })),
                text_color: pal.muted,
                border: Border { radius: 3.0.into(), ..Default::default() },
                ..Default::default()
            })
            .on_press(Message::TabClose(i));

        let tab_content = row![
            text(label).size(12),
            Space::with_width(Length::Fill),
            close_btn,
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        .width(Length::Shrink);
        let _ = TAB_MAX_WIDTH;

        let tab_btn = button(tab_content)
            .on_press(Message::TabActivate(i))
            .padding([5, 8])
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if is_active {
                    pal.bg
                } else {
                    match status {
                        button::Status::Hovered => pal.surface,
                        _ => pal.sec_bg,
                    }
                })),
                text_color: if is_active { pal.text } else { pal.muted },
                border: Border {
                    color: if is_active { pal.accent } else { Color::TRANSPARENT },
                    width: if is_active { 1.0 } else { 0.0 },
                    radius: 5.0.into(),
                },
                ..Default::default()
            });

        row_items = row_items.push(tab_btn);
    }

    let new_tab_btn = button(text("\u{f067}").font(ICON_FONT).size(12))
        .padding([5, 10])
        .style(theme::flat_button)
        .on_press(Message::TabNew);
    row_items = row_items.push(new_tab_btn);
    row_items = row_items.push(Space::with_width(Length::Fill));

    container(row_items)
        .width(Length::Fill)
        .style(theme::panel)
        .into()
}
