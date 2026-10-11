//! CSV/TSV viewer. Read-only for now; cell editing + xlsx/ods via
//! `calamine` are phase 2 — SPEC.md §5.

use crate::theme;
use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Length};
use std::path::Path;

const MAX_ROWS: usize = 5_000;

pub fn supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("csv" | "tsv")
    )
}

pub fn delimiter(path: &Path) -> char {
    if path.extension().and_then(|e| e.to_str()) == Some("tsv") {
        '\t'
    } else {
        ','
    }
}

#[derive(Debug, Clone)]
pub struct Data {
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// Minimal RFC-4180-ish parser (quotes, escaped quotes, quoted newlines).
/// Swap for the `csv` crate when editing lands.
pub fn parse(src: &str, delim: char) -> Data {
    let mut rows = Vec::new();
    let (mut row, mut field) = (Vec::new(), String::new());
    let mut quoted = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (c, false) if c == delim => row.push(std::mem::take(&mut field)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                if rows.len() >= MAX_ROWS {
                    return Data {
                        rows,
                        truncated: chars.peek().is_some(),
                    };
                }
            }
            (c, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Data {
        rows,
        truncated: false,
    }
}

pub struct State {
    data: Data,
}

impl State {
    pub fn new(data: Data) -> Self {
        Self { data }
    }

    pub fn view<M: 'static>(&self) -> Element<'_, M> {
        let cell = |s: &str, header: bool| {
            let t = text(s.to_owned()).size(13);
            container(if header {
                t
            } else {
                t.color(iced::Color::WHITE.scale_alpha(0.85))
            })
            .width(160)
            .padding([2, 6])
        };
        let rows = self.data.rows.iter().enumerate().map(|(i, r)| {
            row(r.iter().map(|c| cell(c, i == 0).into()))
                .spacing(1)
                .into()
        });
        let footer = text(format!(
            "{} rows{}",
            self.data.rows.len(),
            if self.data.truncated {
                format!(" (first {MAX_ROWS} shown)")
            } else {
                String::new()
            }
        ))
        .size(12)
        .color(theme::MUTED);
        column![
            scrollable(column(rows).spacing(1))
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .width(Length::Fill)
                .height(Length::Fill),
            footer
        ]
        .spacing(6)
        .into()
    }
}
