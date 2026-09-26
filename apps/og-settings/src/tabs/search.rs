use iced::widget::{column, container, row, text, toggler};
use iced::{Background, Border, Length};

use crate::app::{AppColors, Message};
use crate::config::Config;

fn section_title<'a>(label: &'static str, colors: AppColors) -> iced::Element<'a, Message> {
    text(label).size(14).style(move |_| text::Style { color: Some(colors.text) }).into()
}

pub fn view<'a>(config: &'a Config, colors: AppColors) -> iced::Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };

    let card = container(
        column![
            section_title("og-search", colors),
            row![
                text("Also search files on disk").style(move |_| text::Style { color: Some(colors.text) }),
                iced::widget::horizontal_space(),
                toggler(config.search_files_enabled).on_toggle(Message::SearchFilesToggled),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            text("Real filesystem I/O, unlike everything else og-search already searches — shown last, below apps/web/AI results.")
                .size(11)
                .style(move |_| text::Style { color: Some(colors.dim_text) }),
            row![
                text("Also search settings tabs").style(move |_| text::Style { color: Some(colors.text) }),
                iced::widget::horizontal_space(),
                toggler(config.search_settings_enabled).on_toggle(Message::SearchSettingsToggled),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            text("e.g. typing \"bluetooth\" surfaces the Network tab. Shown last, below file results.")
                .size(11)
                .style(move |_| text::Style { color: Some(colors.dim_text) }),
        ]
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    column![card].spacing(16).padding(20).into()
}
