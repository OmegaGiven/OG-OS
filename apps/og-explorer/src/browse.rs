//! List and grid views of the left pane: one folder at a time, with a
//! breadcrumb and Up. Grid shows thumbnails, generated only while the grid
//! is on screen and cancelled when you navigate away.

use crate::config;
use crate::filetype::Kind;
use crate::fsops::{self, Entry};
use crate::theme;
use crate::thumbs;
use iced::task;
use iced::widget::{button, column, container, image, row, scrollable, text, Space};
use iced::{Element, Length, Task};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct Browse {
    pub location: PathBuf,
    entries: Vec<Entry>,
    visible: Vec<usize>,
    filter: Option<Kind>,
    loading: bool,
    error: Option<String>,
    thumbs: HashMap<PathBuf, image::Handle>,
    thumbs_requested: bool,
    thumbs_abort: Option<task::Handle>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Navigate(PathBuf),
    Up,
    Loaded(PathBuf, Result<Vec<Entry>, String>),
    Thumb(PathBuf, PathBuf),
    Activate(usize),
}

pub enum Event {
    Open(PathBuf, Kind),
}

impl Browse {
    pub fn new() -> Self {
        Self {
            location: config::home(),
            entries: Vec::new(),
            visible: Vec::new(),
            filter: None,
            loading: false,
            error: None,
            thumbs: HashMap::new(),
            thumbs_requested: false,
            thumbs_abort: None,
        }
    }

    pub fn set_filter(&mut self, f: Option<Kind>) {
        self.filter = f;
        self.refilter();
    }

    fn refilter(&mut self) {
        self.visible = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                !e.hidden && (e.is_dir() || self.filter.is_none_or(|k| e.kind == k))
            })
            .collect();
    }

    pub fn navigate(&mut self, path: PathBuf) -> Task<Msg> {
        if let Some(h) = self.thumbs_abort.take() {
            h.abort();
        }
        self.location = path.clone();
        self.loading = true;
        self.error = None;
        self.thumbs_requested = false;
        Task::perform(fsops::list_dir(path.clone()), move |r| {
            Msg::Loaded(path.clone(), r)
        })
    }

    /// Kick off thumbnail generation for the current folder (grid only).
    pub fn request_thumbs(&mut self) -> Task<Msg> {
        if self.thumbs_requested || self.loading {
            return Task::none();
        }
        self.thumbs_requested = true;
        let todo: Vec<Entry> = self
            .visible
            .iter()
            .map(|&i| &self.entries[i])
            .filter(|e| thumbs::wants(e) && !self.thumbs.contains_key(&e.path))
            .cloned()
            .collect();
        if todo.is_empty() {
            return Task::none();
        }
        let (t, h) =
            Task::run(thumbs::stream(todo), |(src, thumb)| Msg::Thumb(src, thumb)).abortable();
        self.thumbs_abort = Some(h);
        t
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Navigate(p) => (self.navigate(p), None),
            Msg::Up => match self.location.parent() {
                Some(p) => (self.navigate(p.to_path_buf()), None),
                None => (Task::none(), None),
            },
            Msg::Loaded(p, r) if p == self.location => {
                self.loading = false;
                match r {
                    Ok(entries) => self.entries = entries,
                    Err(e) => {
                        self.entries.clear();
                        self.error = Some(e);
                    }
                }
                self.refilter();
                (Task::none(), None)
            }
            Msg::Loaded(..) => (Task::none(), None), // navigated away meanwhile
            Msg::Thumb(src, thumb) => {
                self.thumbs.insert(src, image::Handle::from_path(thumb));
                (Task::none(), None)
            }
            Msg::Activate(i) => match self.entries.get(i) {
                Some(e) if e.is_dir() => (self.navigate(e.path.clone()), None),
                Some(e) => (Task::none(), Some(Event::Open(e.path.clone(), e.kind))),
                None => (Task::none(), None),
            },
        }
    }

    fn breadcrumb(&self) -> Element<'_, Msg> {
        let mut crumbs = row![button(text("Up").size(12))
            .on_press(Msg::Up)
            .style(button::secondary)
            .padding([2, 8])]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        let mut acc = PathBuf::new();
        for comp in self.location.components() {
            acc.push(comp);
            let label = match comp {
                std::path::Component::RootDir => "/".to_string(),
                c => format!("{} /", c.as_os_str().to_string_lossy()),
            };
            crumbs = crumbs.push(
                button(text(label).size(12))
                    .on_press(Msg::Navigate(acc.clone()))
                    .style(button::text)
                    .padding([2, 3]),
            );
        }
        scrollable(crumbs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(3).scroller_width(3),
            ))
            .into()
    }

    fn empty_or_error(&self) -> Option<Element<'_, Msg>> {
        if self.loading {
            Some(text("Loading…").color(theme::MUTED).into())
        } else if let Some(e) = &self.error {
            Some(text(e.clone()).into())
        } else if self.visible.is_empty() {
            Some(text("Nothing here").color(theme::MUTED).into())
        } else {
            None
        }
    }

    pub fn view_list(&self) -> Element<'_, Msg> {
        let body: Element<'_, Msg> = self.empty_or_error().unwrap_or_else(|| {
            // TODO(phase 1): virtualize (og-files filelist.rs) for 10k+ entries
            let rows = self.visible.iter().map(|&i| {
                let e = &self.entries[i];
                let size = if e.is_dir() {
                    String::new()
                } else {
                    fsops::human_size(e.size)
                };
                button(
                    row![
                        text(e.kind.icon()).width(32).size(11).color(theme::MUTED),
                        text(e.name.clone()).width(Length::Fill).size(13),
                        text(size).width(80).size(12).color(theme::MUTED),
                        text(fsops::ago(e.modified))
                            .width(100)
                            .size(12)
                            .color(theme::MUTED),
                    ]
                    .spacing(8),
                )
                .on_press(Msg::Activate(i))
                .style(button::text)
                .padding([3, 6])
                .width(Length::Fill)
                .into()
            });
            scrollable(column(rows).spacing(1))
                .height(Length::Fill)
                .into()
        });
        column![self.breadcrumb(), body].spacing(6).into()
    }

    pub fn view_grid(&self) -> Element<'_, Msg> {
        let body: Element<'_, Msg> = self.empty_or_error().unwrap_or_else(|| {
            let cells = self.visible.iter().map(|&i| {
                let e = &self.entries[i];
                let preview: Element<'_, Msg> = match self.thumbs.get(&e.path) {
                    Some(h) => image(h.clone())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into(),
                    None => iced::widget::center(text(e.kind.icon()).size(22).color(theme::MUTED))
                        .into(),
                };
                button(
                    column![
                        container(preview).width(120).height(96).style(theme::panel),
                        text(e.name.clone()).size(12).width(120).height(32),
                    ]
                    .spacing(4),
                )
                .on_press(Msg::Activate(i))
                .style(button::text)
                .padding(4)
                .into()
            });
            scrollable(row(cells).spacing(6).wrap())
                .height(Length::Fill)
                .into()
        });
        column![self.breadcrumb(), body, Space::with_height(0)]
            .spacing(6)
            .into()
    }
}
