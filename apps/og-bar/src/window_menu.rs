//! Right-click menu on a single app icon in the Workspaces module: close
//! the window, or move it to another workspace. Same popup mechanism as
//! power/settings (PopupState + NewMenu), just parameterized per-window.

use iced::widget::{button, column, container, text};
use iced::{Background, Border, Color, Element, Length};

use crate::message::Message;
use og_theme::AppColors;

pub fn popup_view(colors: AppColors, con_id: i64, other_workspaces: &[i32]) -> Element<'static, Message> {
    let row_btn = move |label: String, msg: Message| -> Element<'static, Message> {
        let fg = colors.text;
        button(text(label).size(14).style(move |_| text::Style { color: Some(fg) }))
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
            .on_press(msg)
            .into()
    };

    let mut rows: Vec<Element<Message>> = vec![row_btn("Close".to_string(), Message::WindowMenuClose(con_id))];

    if !other_workspaces.is_empty() {
        rows.push(
            container(text("Move to workspace").size(11).style(move |_| text::Style { color: Some(colors.dim_text) }))
                .padding([6, 8])
                .into(),
        );
        for ws in other_workspaces {
            let ws = *ws;
            rows.push(row_btn(format!("Workspace {ws}"), Message::WindowMenuMoveToWorkspace(con_id, ws)));
        }
    }

    container(column(rows).spacing(2).padding(6))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(colors.bg_fill),
            border: Border { color: colors.accent, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .into()
}
