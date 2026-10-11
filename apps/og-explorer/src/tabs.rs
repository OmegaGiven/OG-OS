//! Tab strip + per-tab viewer. Tabs are addressed by a stable id, not an
//! index, so async loads land on the right tab even after others close.

use crate::filetype::Kind;
use crate::viewers::{self, Viewer};
use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};

pub struct Tab {
    id: u64,
    pub path: PathBuf,
    pub kind: Kind,
    title: String,
    viewer: Viewer,
}

#[derive(Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Select(u64),
    Close(u64),
    Viewer(u64, viewers::Msg),
}

pub enum Event {
    Status(String),
    /// active tab changed (or closed to nothing) — inspector/details follow
    Focus(Option<(PathBuf, Kind)>),
}

impl Tabs {
    pub fn active(&self) -> Option<&Tab> {
        self.active
            .and_then(|id| self.tabs.iter().find(|t| t.id == id))
    }

    fn active_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active?;
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn focus_event(&self) -> Event {
        Event::Focus(self.active().map(|t| (t.path.clone(), t.kind)))
    }

    pub fn open(&mut self, path: PathBuf, kind: Kind) -> (Task<Msg>, Event) {
        if let Some(t) = self.tabs.iter().find(|t| t.path == path) {
            self.active = Some(t.id);
            return (Task::none(), self.focus_event());
        }
        let id = self.next_id;
        self.next_id += 1;
        let title = title_for(&path);
        self.tabs.push(Tab {
            id,
            path: path.clone(),
            kind,
            title,
            viewer: Viewer::Loading,
        });
        self.active = Some(id);
        let task = viewers::load(path, kind).map(move |m| Msg::Viewer(id, m));
        (task, self.focus_event())
    }

    pub fn close_active(&mut self) -> Option<Event> {
        let id = self.active?;
        self.close(id)
    }

    fn close(&mut self, id: u64) -> Option<Event> {
        let idx = self.tabs.iter().position(|t| t.id == id)?;
        if self.tabs[idx].viewer.is_dirty() {
            // TODO(phase 1): confirm dialog instead of refusing
            return Some(Event::Status(
                "Unsaved changes: Save (Ctrl+S) or Discard before closing".into(),
            ));
        }
        self.tabs.remove(idx);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(idx.min(self.tabs.len().saturating_sub(1)))
                .map(|t| t.id);
        }
        Some(self.focus_event())
    }

    pub fn save_active(&mut self) -> Task<Msg> {
        let Some(tab) = self.active_mut() else {
            return Task::none();
        };
        let id = tab.id;
        tab.viewer.save().map(move |m| Msg::Viewer(id, m))
    }

    pub fn toggle_edit_active(&mut self) -> Task<Msg> {
        let Some(tab) = self.active_mut() else {
            return Task::none();
        };
        let id = tab.id;
        tab.viewer.toggle_edit().map(move |m| Msg::Viewer(id, m))
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Select(id) => {
                self.active = Some(id);
                (Task::none(), Some(self.focus_event()))
            }
            Msg::Close(id) => (Task::none(), self.close(id)),
            Msg::Viewer(id, m) => {
                let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) else {
                    return (Task::none(), None); // tab closed while loading
                };
                let (task, ev) = tab.viewer.update(&tab.path, m);
                let ev = ev.map(|e| match e {
                    viewers::Event::Status(s) => Event::Status(s),
                    viewers::Event::Renamed(p) => {
                        tab.title = title_for(&p);
                        tab.path = p.clone();
                        Event::Status(format!("Renamed to {}", p.display()))
                    }
                });
                (task.map(move |m| Msg::Viewer(id, m)), ev)
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let strip = row(self.tabs.iter().map(|t| {
            let active = self.active == Some(t.id);
            let dirty = match (t.viewer.is_dirty(), t.viewer.is_editing()) {
                (true, _) => "* ",
                (false, Some(true)) => "EDIT ",
                _ => "",
            };
            row![
                button(text(format!("{dirty}{}", t.title)).size(13))
                    .on_press(Msg::Select(t.id))
                    .style(if active {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .padding([4, 10]),
                button(text("×").size(13))
                    .on_press(Msg::Close(t.id))
                    .style(button::text)
                    .padding([4, 6]),
            ]
            .into()
        }))
        .spacing(4);

        let body: Element<'_, Msg> = match self.active() {
            Some(t) => {
                let id = t.id;
                column![
                    text(t.path.display().to_string())
                        .size(12)
                        .color(crate::theme::MUTED),
                    t.viewer.view().map(move |m| Msg::Viewer(id, m)),
                ]
                .spacing(6)
                .into()
            }
            None => iced::widget::center(
                text("Pick a file on the left to open it here.").color(crate::theme::MUTED),
            )
            .into(),
        };

        column![
            scrollable(strip).direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4)
            )),
            body,
        ]
        .spacing(8)
        .padding(12)
        .height(Length::Fill)
        .into()
    }
}

fn title_for(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
