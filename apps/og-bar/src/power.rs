//! Power menu: bar button (the module) + popup content + the actions
//! themselves. First consumer of the popup mechanism (PLAN.md step 4) —
//! deliberately the simplest of the three menus (power/network/bluetooth).

use iced::widget::{button, column, container, text};
use iced::{Background, Border, Color, Element, Length};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    Suspend,
    Logout,
    Reboot,
    Shutdown,
}

impl PowerAction {
    pub fn label(&self) -> &'static str {
        match self {
            PowerAction::Lock => "Lock",
            PowerAction::Suspend => "Suspend",
            PowerAction::Logout => "Logout",
            PowerAction::Reboot => "Reboot",
            PowerAction::Shutdown => "Shutdown",
        }
    }

    pub fn run(&self) {
        let cmd = match self {
            PowerAction::Lock => "swaylock -f -c 000000",
            PowerAction::Suspend => "systemctl suspend",
            PowerAction::Logout => "swaymsg exit",
            PowerAction::Reboot => "systemctl reboot",
            PowerAction::Shutdown => "systemctl poweroff",
        };
        let _ = std::process::Command::new("sh").arg("-c").arg(cmd).spawn();
    }
}

pub struct PowerButton;

impl Module for PowerButton {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = colors.text;
        button(
            container(text("\u{23fb}").size(16).font(icon_font::nerd_font()).style(move |_| text::Style { color: Some(fg) }))
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
        .on_press(Message::OpenPowerMenu)
        .into()
    }
}

pub fn popup_view(colors: AppColors) -> Element<'static, Message> {
    let actions = [
        PowerAction::Lock,
        PowerAction::Suspend,
        PowerAction::Logout,
        PowerAction::Reboot,
        PowerAction::Shutdown,
    ];

    let buttons = actions.into_iter().map(|action| {
        let fg = colors.text;
        button(text(action.label()).size(14).style(move |_| text::Style { color: Some(fg) }))
            .width(Length::Fill)
            .padding(8)
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
            .on_press(Message::PowerAction(action))
            .into()
    });

    container(column(buttons).spacing(2).padding(6))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(colors.bg_fill),
            border: Border { color: colors.accent, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .into()
}
