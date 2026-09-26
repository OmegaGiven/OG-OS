use iced::widget::{button, container, text};
use iced::{Background, Border, Color, Element, Length};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

pub struct Launcher {
    icon: String,
    command: String,
}

impl Launcher {
    pub fn new(icon: String, command: String) -> Self {
        Self { icon, command }
    }
}

impl Module for Launcher {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = colors.text;
        button(
            container(text(self.icon.clone()).size(16).font(icon_font::font_for(&self.icon)).style(move |_| text::Style { color: Some(fg) }))
                .width(size as u16)
                .height(size as u16)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .padding(0)
        .style(move |_, status| button::Style {
            background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                colors.header_btn_bg
            } else {
                Color::TRANSPARENT
            })),
            border: Border { radius: colors.radius.into(), ..Default::default() },
            text_color: fg,
            ..Default::default()
        })
        .on_press(Message::Launch(self.command.clone()))
        .into()
    }
}
