//! Bottom-right progress toasts for running copy/move/delete/trash jobs,
//! plus the modal "file already exists" conflict dialog a job blocks on.

use iced::widget::{button, checkbox, column, container, progress_bar, row, text, Space};
use iced::{Background, Border, Element, Length};

use crate::app::{JobUi, Message};
use crate::theme;

const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");

pub fn toasts(jobs: &[JobUi]) -> Option<Element<'_, Message>> {
    if jobs.is_empty() {
        return None;
    }
    let pal = theme::p();
    let mut col = column![].spacing(6);

    for job in jobs {
        let pct = if job.total_bytes > 0 {
            job.done_bytes as f32 / job.total_bytes as f32
        } else if job.total_items > 0 {
            job.done_items as f32 / job.total_items as f32
        } else {
            0.0
        };

        let title = if let Some(err) = &job.error {
            format!("{}: {}", job.kind.verb(), err)
        } else if job.cancelled {
            format!("{} — cancelled", job.kind.verb())
        } else if job.done {
            format!("{} — done", job.kind.verb())
        } else {
            format!("{} {}", job.kind.verb(), job.current)
        };

        let subtitle = if job.done || job.error.is_some() || job.cancelled {
            String::new()
        } else if job.total_bytes > 0 {
            format!("{} / {}", crate::filesystem::format_size(job.done_bytes), crate::filesystem::format_size(job.total_bytes))
        } else {
            format!("{} / {} items", job.done_items, job.total_items.max(job.done_items))
        };

        let text_color = if job.error.is_some() { pal.danger } else { pal.text };

        let mut body = column![
            row![
                text(title).size(12).style(move |_| iced::widget::text::Style { color: Some(text_color) }),
                Space::with_width(Length::Fill),
                button(text("\u{f00d}").font(ICON_FONT).size(10))
                    .padding(3)
                    .style(theme::flat_button)
                    .on_press(Message::JobDismiss(job.handle.id)),
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(4)
        .width(280);

        if !job.done && job.error.is_none() && !job.cancelled {
            body = body.push(
                progress_bar(0.0..=1.0, pct)
                    .height(6)
                    .style(move |_| progress_bar::Style {
                        background: Background::Color(pal.surface),
                        bar: Background::Color(pal.accent),
                        border: Border { radius: 3.0.into(), ..Default::default() },
                    }),
            );
            if !subtitle.is_empty() {
                body = body.push(text(subtitle).size(10).style(theme::muted_text));
            }
            body = body.push(
                button(text("Cancel").size(11))
                    .padding([2, 8])
                    .style(theme::flat_button)
                    .on_press(Message::JobCancel(job.handle.id)),
            );
        }

        col = col.push(
            container(body)
                .padding(10)
                .style(theme::dialog_box),
        );
    }

    Some(
        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right)
            .align_y(iced::alignment::Vertical::Bottom)
            .padding(14)
            .into(),
    )
}

pub fn conflict_dialog(job: &JobUi) -> Option<Element<'_, Message>> {
    let c = job.conflict.as_ref()?;
    let pal = theme::p();
    let name = c.dst.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let kind_word = if c.dst_is_dir { "folder" } else { "file" };

    let btn = |label: &'static str, msg: Message, primary: bool| {
        button(text(label).size(13))
            .padding([6, 14])
            .style(if primary { theme::accent_button } else { theme::flat_button })
            .on_press(msg)
    };

    let dialog = container(
        column![
            text(format!("A {kind_word} named \"{name}\" already exists")).size(14),
            text(c.dst.to_string_lossy().to_string()).size(11).style(theme::muted_text),
            checkbox("Apply to all conflicts in this operation", job.apply_all)
                .on_toggle(move |v| Message::JobConflictApplyAll(job.handle.id, v))
                .size(14)
                .text_size(12),
            row![
                btn("Skip", Message::JobConflictResolve(job.handle.id, crate::jobs::Resolution::Skip), false),
                Space::with_width(Length::Fill),
                btn("Keep Both", Message::JobConflictResolve(job.handle.id, crate::jobs::Resolution::KeepBoth), false),
                btn("Overwrite", Message::JobConflictResolve(job.handle.id, crate::jobs::Resolution::Overwrite), true),
            ]
            .spacing(8),
        ]
        .spacing(14)
        .padding(18)
        .width(380),
    )
    .style(theme::dialog_box);

    let _ = pal;
    Some(
        container(dialog)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(theme::scrim)
            .into(),
    )
}
