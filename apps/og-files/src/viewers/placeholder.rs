//! Stand-in for files no viewer handles (yet): says so, and offers Convert
//! to PDF where that works.

use crate::filetype::Kind;
use crate::theme;
use iced::widget::{button, center, column, text};
use iced::Element;
use std::path::PathBuf;

pub struct State {
    kind: Kind,
    /// "Convert to PDF" is possible
    convert: bool,
}

impl State {
    pub fn new(path: PathBuf, kind: Kind) -> Self {
        Self {
            kind,
            convert: super::convert::convertible(&path, kind),
        }
    }

    pub fn view<M: Clone + 'static>(&self, on_convert: M) -> Element<'_, M> {
        let plan = if self.convert {
            "OG Files can't show this file yet, but it can convert it to PDF."
        } else {
            "OG Files can't show this file yet. Open it in another app from the right-click menu."
        };
        let mut c = column![
            crate::widgets::kind_icon(self.kind).size(56),
            text(format!("No viewer for this {} yet", self.kind.label().to_lowercase())).size(18),
            text(plan).color(theme::MUTED),
        ]
        .spacing(8)
        .align_x(iced::Alignment::Center);
        if self.convert {
            c = c.push(button(text("Convert to PDF")).on_press(on_convert));
        }
        center(c).into()
    }
}
