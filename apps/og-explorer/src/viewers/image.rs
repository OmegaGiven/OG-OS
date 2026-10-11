//! Image viewer: pan/zoom via iced's image viewer widget. Editing (crop,
//! rotate, adjust) is phase 3 — SPEC.md §5.

use iced::widget::{button, column, image, row, text, Space};
use iced::{ContentFit, Element, Length};
use std::path::PathBuf;

pub struct State {
    handle: image::Handle,
    fit: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    ToggleFit,
    /// handled by the registry (needs the path): writes `<name>.pdf`
    ToPdf,
    /// handled by the registry: split photos out of a scan (server)
    SplitScan,
}

impl State {
    pub fn new(path: PathBuf) -> Self {
        Self {
            handle: image::Handle::from_path(path),
            fit: true,
        }
    }

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::ToggleFit => self.fit = !self.fit,
            Msg::ToPdf | Msg::SplitScan => {}
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bar = row![
            Space::with_width(Length::Fill),
            button(text("Split scanned photos (server)").size(12))
                .on_press(Msg::SplitScan)
                .style(button::secondary),
            button(text("Make PDF").size(12))
                .on_press(Msg::ToPdf)
                .style(button::secondary),
            button(text(if self.fit { "Pan / zoom" } else { "Fit" }).size(12))
                .on_press(Msg::ToggleFit),
        ]
        .spacing(6);
        let body: Element<'_, Msg> = if self.fit {
            image(self.handle.clone())
                .content_fit(ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            image::viewer(self.handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };
        column![bar, body].spacing(6).into()
    }
}
