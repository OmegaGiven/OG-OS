use iced::widget::{button, column, container, row, text};
use iced::{Background, Border, Element, Length};

use crate::app::{AppColors, HistoryEntry, Message};

pub fn view<'a>(history: &'a [HistoryEntry], colors: AppColors) -> Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };

    if history.is_empty() {
        return container(
            text("No saved history yet. Apply & Save to record a snapshot.")
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
        )
        .style(card_style)
        .padding(20)
        .width(Length::Fill)
        .into();
    }

    let btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.surface)),
        text_color: colors.text,
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };

    let rows: Vec<Element<Message>> = history
        .iter()
        .enumerate()
        .rev()
        .map(|(i, entry)| {
            let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S").to_string();
            container(
                row![
                    column![
                        text(ts).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
                        text(format!("bg: {}  accent: {}  text: {}", entry.snapshot.bar_bg, entry.snapshot.accent, entry.snapshot.bar_text))
                            .size(12)
                            .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                    ]
                    .spacing(4),
                    iced::widget::horizontal_space(),
                    button(text("Restore").style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                        .style(btn_style)
                        .on_press(Message::HistoryRestore(i)),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(12)
                .padding(16),
            )
            .style(card_style)
            .width(Length::Fill)
            .into()
        })
        .collect();

    column(rows).spacing(8).padding(20).into()
}
