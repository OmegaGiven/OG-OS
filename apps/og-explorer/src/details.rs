//! Bottom strip: file details and history. For now history is this
//! session's activity log; per-file version history (snapshots / git /
//! app-kept revisions) is an open design question — SPEC.md §8.

use crate::theme;
use iced::widget::{column, row, scrollable, text};
use iced::{Element, Length};
use std::time::SystemTime;

const KEEP: usize = 200;

#[derive(Default)]
pub struct Details {
    log: Vec<(SystemTime, String)>,
    status: String,
}

impl Details {
    pub fn record(&mut self, what: impl Into<String>) {
        let what = what.into();
        self.status = what.clone();
        self.log.push((SystemTime::now(), what));
        if self.log.len() > KEEP {
            self.log.remove(0);
        }
    }

    pub fn view<M: 'static>(&self) -> Element<'_, M> {
        let rows = self.log.iter().rev().map(|(t, s)| {
            row![
                text(crate::fsops::ago(Some(*t)))
                    .size(12)
                    .color(theme::MUTED)
                    .width(90),
                text(s.clone()).size(12)
            ]
            .into()
        });
        column![
            row![
                text("File details & history").size(15),
                text(self.status.clone()).size(12).color(theme::MUTED)
            ]
            .spacing(16),
            scrollable(column(rows).spacing(2)).height(Length::Fill),
        ]
        .spacing(8)
        .padding(12)
        .into()
    }
}
