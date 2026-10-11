//! The left pane: header (view mode, focus, search), kind filter chips, and
//! a body that is either the live search results or the tree / list / grid.
//! The kind filter applies to whatever is showing.

use crate::browse::{self, Browse};
use crate::filetype::{Kind, FILTERS};
use crate::search::{self, Search};
use crate::tree::{self, Tree};
use iced::widget::{button, column, row, text, text_input, Space};
use iced::{Element, Length, Task};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Tree,
    List,
    Grid,
}

pub struct Sidebar {
    mode: Mode,
    filter: Option<Kind>,
    search_open: bool,
    pub focused: bool,
    tree: Tree,
    browse: Browse,
    search: Search,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Mode(Mode),
    Filter(Option<Kind>),
    SearchToggle,
    SearchInput(String),
    Focus,
    Tree(tree::Msg),
    Browse(browse::Msg),
    Search(search::Msg),
}

pub enum Event {
    Open(PathBuf, Kind),
    Status(String),
    ToggleFocus,
}

const SEARCH_ID: &str = "sidebar-search";

impl Sidebar {
    pub fn new() -> (Self, Task<Msg>) {
        let (tree, t) = Tree::new();
        let mut s = Self {
            mode: Mode::Tree,
            filter: None,
            search_open: false,
            focused: false,
            tree,
            browse: Browse::new(),
            search: Search::default(),
        };
        let home = s.browse.location.clone();
        let b = s.browse.navigate(home).map(Msg::Browse);
        (s, Task::batch([t.map(Msg::Tree), b]))
    }

    /// Show a folder (from the command line or elsewhere) in list view.
    pub fn reveal(&mut self, path: PathBuf) -> Task<Msg> {
        if self.mode == Mode::Tree {
            self.mode = Mode::List;
        }
        self.browse.navigate(path).map(Msg::Browse)
    }

    pub fn open_search(&mut self) -> Task<Msg> {
        self.search_open = true;
        text_input::focus(text_input::Id::new(SEARCH_ID))
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Mode(m) => {
                self.mode = m;
                let t = if m == Mode::Grid {
                    self.browse.request_thumbs().map(Msg::Browse)
                } else {
                    Task::none()
                };
                (t, None)
            }
            Msg::Filter(f) => {
                self.filter = if self.filter == f { None } else { f };
                self.tree.set_filter(self.filter);
                self.browse.set_filter(self.filter);
                self.search.set_filter(self.filter);
                (Task::none(), None)
            }
            Msg::SearchToggle => {
                if self.search_open {
                    self.search_open = false;
                    let t = self.search.start(String::new(), Vec::new(), Vec::new());
                    (t.map(Msg::Search), None)
                } else {
                    (self.open_search(), None)
                }
            }
            Msg::SearchInput(q) => {
                let lower = q.to_lowercase();
                let quick = if lower.is_empty() {
                    Vec::new()
                } else {
                    self.tree.loaded_matches(&lower)
                };
                let t = self.search.start(q, quick, self.tree.search_roots());
                (t.map(Msg::Search), None)
            }
            Msg::Focus => {
                self.focused = !self.focused;
                (Task::none(), Some(Event::ToggleFocus))
            }
            Msg::Tree(m) => {
                let (t, ev) = self.tree.update(m);
                let ev = ev.map(|e| match e {
                    tree::Event::Open(p, k) => Event::Open(p, k),
                    tree::Event::Status(s) => Event::Status(s),
                });
                (t.map(Msg::Tree), ev)
            }
            Msg::Browse(m) => {
                let (t, ev) = self.browse.update(m);
                // a freshly loaded folder in grid mode needs its thumbnails
                let more = if self.mode == Mode::Grid {
                    self.browse.request_thumbs()
                } else {
                    Task::none()
                };
                let ev = ev.map(|browse::Event::Open(p, k)| Event::Open(p, k));
                (Task::batch([t, more]).map(Msg::Browse), ev)
            }
            Msg::Search(m) => {
                let ev = self
                    .search
                    .update(m)
                    .map(|search::Event::Open(p, k)| Event::Open(p, k));
                (Task::none(), ev)
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mode_btn = |label: &'static str, m: Mode| {
            button(text(label).size(12))
                .on_press(Msg::Mode(m))
                .style(if self.mode == m {
                    button::primary
                } else {
                    button::secondary
                })
                .padding([3, 8])
        };
        // two rows so nothing wraps letter-by-letter when the pane is narrow
        let header = column![
            row![
                text("File manager").size(18),
                Space::with_width(Length::Fill),
                button(text(if self.focused { "Unfocus" } else { "Focus" }).size(12))
                    .on_press(Msg::Focus)
                    .style(button::secondary)
                    .padding([3, 8]),
                button(text("Search").size(12))
                    .on_press(Msg::SearchToggle)
                    .style(if self.search_open {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .padding([3, 8]),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
            row![
                mode_btn("Tree", Mode::Tree),
                mode_btn("List", Mode::List),
                mode_btn("Grid", Mode::Grid),
            ]
            .spacing(4),
        ]
        .spacing(6);

        let chip = |label: &'static str, f: Option<Kind>| {
            button(text(label).size(11))
                .on_press(Msg::Filter(f))
                .style(if self.filter == f {
                    button::primary
                } else {
                    button::secondary
                })
                .padding([2, 8])
        };
        let mut chips = row![chip("All", None)].spacing(4);
        for k in FILTERS {
            chips = chips.push(chip(k.label(), Some(k)));
        }
        let chips = row(vec![chips.wrap().into()]);

        let mut col = column![header].spacing(8).padding(12);
        if self.search_open {
            col = col.push(
                text_input(
                    "search names under home, bookmarks, drives…",
                    &self.search.query,
                )
                .id(text_input::Id::new(SEARCH_ID))
                .on_input(Msg::SearchInput)
                .size(13),
            );
        }
        col = col.push(chips);

        let body = if self.search.is_active() {
            self.search.view().map(Msg::Search)
        } else {
            match self.mode {
                Mode::Tree => self.tree.view().map(Msg::Tree),
                Mode::List => self.browse.view_list().map(Msg::Browse),
                Mode::Grid => self.browse.view_grid().map(Msg::Browse),
            }
        };
        col.push(body).into()
    }
}
