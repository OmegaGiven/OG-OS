//! List and grid views of the files pane: one folder at a time with a
//! breadcrumb (Ctrl+L turns it into an editable path), virtualized rows,
//! thumbnails in the grid, and a Trash view with restore.

use crate::config;
use crate::filetype::Kind;
use crate::fsops::{self, Entry};
use crate::ops::{self, TrashEntry};
use crate::theme;
use crate::thumbs;
use crate::tree::TRASH;
use crate::widgets::{self, ItemAct, Selection, Vp};
use iced::task;
use iced::widget::{button, column, container, image, row, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const ROW_H: f32 = 26.0;
pub const TILE: (f32, f32) = (124.0, 140.0);
pub const GAP: f32 = 6.0;
pub const SCROLL_ID: &str = "browse-scroll";
pub const PATH_ID: &str = "browse-path";

pub struct Browse {
    pub location: PathBuf,
    entries: Vec<Entry>,
    trash: Vec<TrashEntry>,
    visible: Vec<usize>,
    filter: Option<Kind>,
    loading: bool,
    error: Option<String>,
    thumbs: HashMap<PathBuf, image::Handle>,
    thumbs_requested: bool,
    thumbs_abort: Option<task::Handle>,
    vp: Vp,
    editing_path: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Navigate(PathBuf),
    Up,
    Loaded(PathBuf, Result<Vec<Entry>, String>),
    TrashLoaded(Result<Vec<TrashEntry>, String>),
    Thumb(PathBuf, PathBuf),
    Item(ItemAct),
    Scrolled(Vp),
    PathEdit(String),
    PathSubmit,
    EmptyTrash,
}

pub enum Event {
    Item(ItemAct),
    EmptyTrash,
    Status(String),
}

impl Browse {
    pub fn new() -> Self {
        Self {
            location: config::home(),
            entries: Vec::new(),
            trash: Vec::new(),
            visible: Vec::new(),
            filter: None,
            loading: false,
            error: None,
            thumbs: HashMap::new(),
            thumbs_requested: false,
            thumbs_abort: None,
            vp: Vp::default(),
            editing_path: None,
        }
    }

    pub fn is_trash(&self) -> bool {
        self.location.as_os_str() == TRASH
    }

    pub fn set_filter(&mut self, f: Option<Kind>) {
        self.filter = f;
        self.refilter();
    }

    fn refilter(&mut self) {
        let f = self.filter;
        self.visible = if self.is_trash() {
            (0..self.trash.len())
                .filter(|&i| {
                    f.is_none_or(|k| {
                        Kind::of(&self.trash[i].original, self.trash[i].trashed.is_dir()) == k
                    })
                })
                .collect()
        } else {
            (0..self.entries.len())
                .filter(|&i| {
                    let e = &self.entries[i];
                    !e.hidden && (e.is_dir() || f.is_none_or(|k| e.kind == k))
                })
                .collect()
        };
    }

    /// (path, kind) of the rows in display order.
    pub fn visible_items(&self) -> Vec<(PathBuf, Kind)> {
        self.visible
            .iter()
            .map(|&i| {
                if self.is_trash() {
                    let t = &self.trash[i];
                    (t.trashed.clone(), Kind::of(&t.original, t.trashed.is_dir()))
                } else {
                    (self.entries[i].path.clone(), self.entries[i].kind)
                }
            })
            .collect()
    }

    pub fn trash_entry(&self, trashed: &Path) -> Option<&TrashEntry> {
        self.trash.iter().find(|t| t.trashed == trashed)
    }

    /// Scroll offset that shows `p` in list (`grid` false) or grid layout.
    pub fn reveal(&self, p: &Path, grid: bool) -> Option<f32> {
        let i = self.visible_items().iter().position(|(x, _)| x == p)?;
        if grid {
            let cols = widgets::grid_cols(self.vp.width, TILE.0, GAP);
            self.vp.reveal(i / cols, TILE.1 + GAP)
        } else {
            self.vp.reveal(i, ROW_H)
        }
    }

    pub fn grid_cols(&self) -> usize {
        widgets::grid_cols(self.vp.width, TILE.0, GAP)
    }

    pub fn navigate(&mut self, path: PathBuf) -> Task<Msg> {
        if let Some(h) = self.thumbs_abort.take() {
            h.abort();
        }
        let changed = path != self.location;
        self.location = path.clone();
        self.loading = true;
        self.error = None;
        self.thumbs_requested = false;
        self.editing_path = None;
        if changed {
            self.vp.offset = 0.0;
        }
        let reset = if changed {
            scrollable::scroll_to(
                scrollable::Id::new(SCROLL_ID),
                scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
            )
        } else {
            Task::none()
        };
        let load = if self.is_trash() {
            Task::perform(
                async {
                    tokio::task::spawn_blocking(ops::list_trash)
                        .await
                        .map_err(|e| e.to_string())?
                },
                Msg::TrashLoaded,
            )
        } else {
            Task::perform(fsops::list_dir(path.clone()), move |r| {
                Msg::Loaded(path.clone(), r)
            })
        };
        Task::batch([reset, load])
    }

    /// Reload in place (watcher / finished job) — keeps scroll and selection.
    pub fn refresh(&mut self) -> Task<Msg> {
        let loc = self.location.clone();
        self.navigate(loc)
    }

    pub fn start_path_edit(&mut self) -> Task<Msg> {
        if self.is_trash() {
            return Task::none();
        }
        self.editing_path = Some(self.location.display().to_string());
        Task::batch([
            text_input::focus(text_input::Id::new(PATH_ID)),
            text_input::select_all(text_input::Id::new(PATH_ID)),
        ])
    }

    /// Kick off thumbnail generation for the current folder (grid only).
    pub fn request_thumbs(&mut self) -> Task<Msg> {
        if self.thumbs_requested || self.loading || self.is_trash() {
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
                Some(p) if !self.is_trash() => (self.navigate(p.to_path_buf()), None),
                _ => (Task::none(), None),
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
            Msg::TrashLoaded(r) => {
                if self.is_trash() {
                    self.loading = false;
                    match r {
                        Ok(mut t) => {
                            t.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
                            self.trash = t;
                        }
                        Err(e) => self.error = Some(e),
                    }
                    self.refilter();
                }
                (Task::none(), None)
            }
            Msg::Thumb(src, thumb) => {
                self.thumbs.insert(src, image::Handle::from_path(thumb));
                (Task::none(), None)
            }
            Msg::Item(a) => (Task::none(), Some(Event::Item(a))),
            Msg::Scrolled(vp) => {
                self.vp = vp;
                (Task::none(), None)
            }
            Msg::PathEdit(s) => {
                self.editing_path = Some(s);
                (Task::none(), None)
            }
            Msg::PathSubmit => {
                let Some(s) = self.editing_path.take() else {
                    return (Task::none(), None);
                };
                let p = config::expand_home(s.trim());
                if p.is_dir() {
                    (self.navigate(p), None)
                } else {
                    (
                        Task::none(),
                        Some(Event::Status(format!("{} isn't a folder", p.display()))),
                    )
                }
            }
            Msg::EmptyTrash => (Task::none(), Some(Event::EmptyTrash)),
        }
    }

    fn header(&self) -> Element<'_, Msg> {
        if self.is_trash() {
            return row![
                text("Trash").size(14),
                text(format!("  {} items", self.trash.len()))
                    .size(12)
                    .color(theme::MUTED),
                Space::with_width(Length::Fill),
                button(text("Empty trash").size(12))
                    .on_press_maybe((!self.trash.is_empty()).then_some(Msg::EmptyTrash))
                    .style(button::danger),
            ]
            .align_y(iced::Alignment::Center)
            .into();
        }
        if let Some(s) = &self.editing_path {
            return text_input("path", s)
                .id(text_input::Id::new(PATH_ID))
                .on_input(Msg::PathEdit)
                .on_submit(Msg::PathSubmit)
                .size(13)
                .into();
        }
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
        if self.loading && self.entries.is_empty() && self.trash.is_empty() {
            Some(text("Loading…").color(theme::MUTED).into())
        } else if let Some(e) = &self.error {
            Some(text(e.clone()).into())
        } else if self.visible.is_empty() {
            Some(
                text(if self.is_trash() {
                    "Trash is empty"
                } else {
                    "Nothing here"
                })
                .color(theme::MUTED)
                .into(),
            )
        } else {
            None
        }
    }

    fn scroller<'a>(&'a self, content: Element<'a, Msg>) -> Element<'a, Msg> {
        scrollable(content)
            .id(scrollable::Id::new(SCROLL_ID))
            .on_scroll(|v| Msg::Scrolled(Vp::from(v)))
            .height(Length::Fill)
            .into()
    }

    pub fn view_list<'a>(&'a self, sel: &'a Selection) -> Element<'a, Msg> {
        let body = self.empty_or_error().unwrap_or_else(|| {
            let list = widgets::virtual_column(self.visible.len(), ROW_H, self.vp, |n| {
                let i = self.visible[n];
                if self.is_trash() {
                    let t = &self.trash[i];
                    let kind = Kind::of(&t.original, t.trashed.is_dir());
                    let name = t
                        .original
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let from = t
                        .original
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let content = row![
                        widgets::kind_icon(kind).width(26).size(14),
                        text(name).width(Length::FillPortion(3)).size(13),
                        text(from)
                            .width(Length::FillPortion(3))
                            .size(11)
                            .color(theme::MUTED),
                        text(t.deleted_at.replace('T', " "))
                            .width(130)
                            .size(11)
                            .color(theme::MUTED),
                    ]
                    .spacing(8);
                    widgets::item(content, &t.trashed, kind, sel, Msg::Item)
                } else {
                    let e = &self.entries[i];
                    let size = if e.is_dir() {
                        String::new()
                    } else {
                        fsops::human_size(e.size)
                    };
                    let content = row![
                        widgets::kind_icon(e.kind).width(26).size(14),
                        text(e.name.clone()).width(Length::Fill).size(13),
                        text(size).width(76).size(12).color(theme::MUTED),
                        text(fsops::ago(e.modified))
                            .width(96)
                            .size(12)
                            .color(theme::MUTED),
                    ]
                    .spacing(8);
                    widgets::item(content, &e.path, e.kind, sel, Msg::Item)
                }
            });
            self.scroller(list)
        });
        column![self.header(), body].spacing(6).into()
    }

    pub fn view_grid<'a>(&'a self, sel: &'a Selection) -> Element<'a, Msg> {
        if self.is_trash() {
            return self.view_list(sel);
        }
        let body = self.empty_or_error().unwrap_or_else(|| {
            let grid = widgets::virtual_grid(self.visible.len(), TILE, GAP, self.vp, |n| {
                let e = &self.entries[self.visible[n]];
                let preview: Element<'_, Msg> = match self.thumbs.get(&e.path) {
                    Some(h) => image(h.clone())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into(),
                    None => iced::widget::center(widgets::kind_icon(e.kind).size(40)).into(),
                };
                let tile = column![
                    container(preview)
                        .width(TILE.0 - 12.0)
                        .height(92)
                        .style(theme::panel),
                    text(e.name.clone())
                        .size(12)
                        .width(TILE.0 - 12.0)
                        .height(32),
                ]
                .spacing(4);
                container(widgets::item(tile, &e.path, e.kind, sel, Msg::Item))
                    .width(TILE.0)
                    .height(TILE.1)
                    .into()
            });
            self.scroller(grid)
        });
        column![self.header(), body].spacing(6).into()
    }
}
