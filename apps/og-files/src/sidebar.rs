//! The files pane: header (view mode, focus, search), kind filter chips,
//! and a body that is the live search results or the tree / list / grid.
//!
//! Selection is shared by all views: click selects (and previews a file),
//! Ctrl toggles, Shift selects a range, double-click opens (pins the tab /
//! enters the folder), right-click asks the app for a context menu. The
//! keyboard (routed from the app) moves a cursor through whichever view is
//! showing.

use crate::browse::{self, Browse};
use crate::devices::Drive;
use crate::filetype::{Kind, FILTERS};
use crate::search::{self, Search};
use crate::tree::{self, Tree, TRASH};
use crate::widgets::{ItemAct, Selection};
use iced::keyboard::Modifiers;
use iced::widget::{button, checkbox, column, row, scrollable, text, text_input, Space};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Tree,
    List,
    Grid,
}

const DOUBLE_CLICK: Duration = Duration::from_millis(450);
const SEARCH_ID: &str = "sidebar-search";

/// What a right-click was on.
#[derive(Debug, Clone)]
pub struct Target {
    /// selected items (empty = clicked the folder background)
    pub items: Vec<(PathBuf, Kind)>,
    /// folder that paste / new folder go into
    pub dir: PathBuf,
    pub drive: Option<Drive>,
    pub in_trash: bool,
    pub bookmarked: bool,
}

/// Keyboard actions the app forwards when no text field has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Parent,
    SelectAll,
    Escape,
    Char(char),
}

pub struct Sidebar {
    mode: Mode,
    filter: Option<Kind>,
    search_open: bool,
    query: String,
    query_gen: u64,
    pub focused: bool,
    pub mods: Modifiers,
    pub sel: Selection,
    last_click: Option<(PathBuf, Instant)>,
    /// set on press: what a drag starting now would carry (the app takes it)
    pub pressed: Option<Vec<PathBuf>>,
    /// pressed an already-selected item: collapse the selection to it on
    /// release unless a drag happened
    pending_collapse: Option<PathBuf>,
    typeahead: (String, Option<Instant>),
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
    SearchFire(u64),
    SearchContent(bool),
    Focus,
    DiskUsage,
    Tree(tree::Msg),
    Browse(browse::Msg),
    Search(search::Msg),
}

pub enum Event {
    /// open a file; `pin` = double-click (keep the tab) vs preview
    Open(PathBuf, Kind, bool),
    /// show this item in the inspector
    Inspect(PathBuf, Kind),
    Menu(Target),
    Mount(Drive),
    EmptyTrash,
    Status(String),
    ToggleFocus,
    /// open the disk-usage analyzer for this folder
    DiskUsage(PathBuf),
}

impl Sidebar {
    pub fn new() -> (Self, Task<Msg>) {
        let (tree, t) = Tree::new();
        let mut s = Self {
            mode: Mode::Tree,
            filter: None,
            search_open: false,
            query: String::new(),
            query_gen: 0,
            focused: false,
            mods: Modifiers::default(),
            sel: Selection::default(),
            last_click: None,
            pressed: None,
            pending_collapse: None,
            typeahead: (String::new(), None),
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
        self.sel.clear();
        self.browse.navigate(path).map(Msg::Browse)
    }

    /// Show `p` selected in list view (from "Show in files pane").
    pub fn reveal_path(&mut self, p: PathBuf) -> Task<Msg> {
        let dir = if p.is_dir() {
            p.clone()
        } else {
            p.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(crate::config::home)
        };
        if self.mode == Mode::Tree {
            self.mode = Mode::List;
        }
        let t = self.browse.navigate(dir).map(Msg::Browse);
        self.sel.only(p);
        t
    }

    pub fn open_search(&mut self) -> Task<Msg> {
        self.search_open = true;
        text_input::focus(text_input::Id::new(SEARCH_ID))
    }

    pub fn edit_path(&mut self) -> Task<Msg> {
        if self.mode == Mode::Tree {
            self.mode = Mode::List;
        }
        self.browse.start_path_edit().map(Msg::Browse)
    }

    pub fn set_drives(&mut self, d: Vec<Drive>) {
        self.tree.set_drives(d);
    }

    pub fn toggle_bookmark(&mut self, p: PathBuf, is_dir: bool) -> Task<Msg> {
        self.tree.toggle_bookmark(p, is_dir).map(Msg::Tree)
    }

    pub fn trash_entry(&self, trashed: &Path) -> Option<crate::ops::TrashEntry> {
        self.browse.trash_entry(trashed).cloned()
    }

    /// Folders currently on screen, for the folder watcher.
    pub fn watched_dirs(&self) -> Vec<PathBuf> {
        let mut d = self.tree.open_dirs();
        if !self.browse.is_trash() && self.mode != Mode::Tree {
            d.push(self.browse.location.clone());
        }
        d.sort();
        d.dedup();
        d
    }

    /// Something changed in `dirs` (watcher, finished job): reload them.
    pub fn refresh(&mut self, dirs: &[PathBuf]) -> Task<Msg> {
        let t = self.tree.refresh(dirs).map(Msg::Tree);
        let b = if self.browse.is_trash() || dirs.contains(&self.browse.location) {
            self.browse.refresh().map(Msg::Browse)
        } else {
            Task::none()
        };
        Task::batch([t, b])
    }

    /// Folder that "paste" / "new folder" target right now.
    pub fn current_dir(&self) -> PathBuf {
        if self.mode != Mode::Tree && !self.browse.is_trash() {
            return self.browse.location.clone();
        }
        match &self.sel.cursor {
            Some(p) if p.is_dir() => p.clone(),
            Some(p) => p
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(crate::config::home),
            None => crate::config::home(),
        }
    }

    fn items(&self) -> Vec<(PathBuf, Kind)> {
        if self.search.is_active() {
            self.search.visible_items()
        } else {
            match self.mode {
                Mode::Tree => self
                    .tree
                    .visible_paths()
                    .into_iter()
                    .map(|p| {
                        let k = self.tree.kind_of(&p).unwrap_or(Kind::Folder);
                        (p, k)
                    })
                    .collect(),
                _ => self.browse.visible_items(),
            }
        }
    }

    fn kind_of(&self, p: &Path) -> Kind {
        self.items()
            .into_iter()
            .find(|(x, _)| x == p)
            .map(|(_, k)| k)
            .unwrap_or_else(|| Kind::of(p, p.is_dir()))
    }

    pub fn selected_items(&self) -> Vec<(PathBuf, Kind)> {
        self.sel
            .paths()
            .into_iter()
            .map(|p| {
                let k = self.kind_of(&p);
                (p, k)
            })
            .collect()
    }

    fn scroll_to_cursor(&self) -> Task<Msg> {
        let Some(c) = &self.sel.cursor else {
            return Task::none();
        };
        let (id, off) = if self.search.is_active() {
            (search::SCROLL_ID, self.search.reveal(c))
        } else {
            match self.mode {
                Mode::Tree => (tree::SCROLL_ID, self.tree.reveal(c)),
                Mode::List => (browse::SCROLL_ID, self.browse.reveal(c, false)),
                Mode::Grid => (browse::SCROLL_ID, self.browse.reveal(c, true)),
            }
        };
        match off {
            Some(y) => scrollable::scroll_to(
                scrollable::Id::new(id),
                scrollable::AbsoluteOffset { x: 0.0, y },
            ),
            None => Task::none(),
        }
    }

    /// Activate an item: folders enter/expand, files open.
    fn activate(&mut self, p: PathBuf, kind: Kind, pin: bool) -> (Task<Msg>, Option<Event>) {
        if p.as_os_str() == TRASH {
            if self.mode == Mode::Tree {
                self.mode = Mode::List;
            }
            self.sel.clear();
            return (self.browse.navigate(p).map(Msg::Browse), None);
        }
        if kind == Kind::Folder {
            if let Some(d) = self
                .tree
                .drive_for(&p)
                .filter(|d| d.mount_point.is_none())
                .cloned()
            {
                return (Task::none(), Some(Event::Mount(d)));
            }
            if self.mode == Mode::Tree && !self.search.is_active() {
                let (t, ev) = self.tree.toggle_path(&p, None);
                let ev = ev.and_then(|e| self.tree_event(e));
                return (t.map(Msg::Tree), ev);
            }
            // search hit or list/grid: browse into it
            if self.search.is_active() {
                self.close_search_now();
            }
            if self.mode == Mode::Tree {
                self.mode = Mode::List;
            }
            self.sel.clear();
            return (self.browse.navigate(p).map(Msg::Browse), None);
        }
        if self.browse.is_trash() && self.mode != Mode::Tree {
            return (
                Task::none(),
                Some(Event::Status(
                    "Restore it first (right-click → Restore)".into(),
                )),
            );
        }
        (Task::none(), Some(Event::Open(p, kind, pin)))
    }

    fn close_search_now(&mut self) {
        self.search_open = false;
        self.query.clear();
        self.query_gen += 1;
        let _ = self.search.start(String::new(), Vec::new(), Vec::new());
    }

    fn item(&mut self, a: ItemAct) -> (Task<Msg>, Option<Event>) {
        match a {
            ItemAct::Press(p, kind) => {
                let now = Instant::now();
                let double = self
                    .last_click
                    .as_ref()
                    .is_some_and(|(lp, t)| *lp == p && now.duration_since(*t) < DOUBLE_CLICK);
                self.last_click = if double { None } else { Some((p.clone(), now)) };
                if self.mods.shift() {
                    let order: Vec<PathBuf> = self.items().into_iter().map(|(p, _)| p).collect();
                    self.sel.range(p.clone(), &order);
                    return (Task::none(), None);
                }
                if self.mods.command() {
                    self.sel.toggle(p.clone());
                    return (Task::none(), None);
                }
                if self.sel.contains(&p) && self.sel.items.len() > 1 && !double {
                    // keep the multi-selection so it can be dragged
                    self.pending_collapse = Some(p.clone());
                    self.sel.cursor = Some(p.clone());
                } else {
                    self.sel.only(p.clone());
                }
                if p.as_os_str() != TRASH {
                    self.pressed = Some(
                        self.sel
                            .paths()
                            .into_iter()
                            .filter(|x| x.as_os_str() != TRASH)
                            .collect(),
                    );
                }
                if double {
                    return self.activate(p, kind, true);
                }
                // single click: preview files, inspect folders
                let in_trash_view = self.browse.is_trash() && self.mode != Mode::Tree;
                if kind != Kind::Folder && !in_trash_view && p.as_os_str() != TRASH {
                    (Task::none(), Some(Event::Open(p, kind, false)))
                } else {
                    (Task::none(), Some(Event::Inspect(p, kind)))
                }
            }
            ItemAct::Hover(p, kind, entered) => {
                if entered {
                    self.sel.hover = Some(p);
                    self.sel.hover_kind = Some(kind);
                } else if self.sel.hover.as_ref() == Some(&p) {
                    // leave events can arrive after the next enter
                    self.sel.hover = None;
                    self.sel.hover_kind = None;
                }
                (Task::none(), None)
            }
            ItemAct::Menu(p) => {
                if !self.sel.contains(&p) {
                    self.sel.only(p.clone());
                }
                (Task::none(), Some(self.menu_event()))
            }
        }
    }

    /// Mouse released anywhere (`dragged` = a drag-and-drop just ended).
    pub fn mouse_up(&mut self, dragged: bool) {
        if let Some(p) = self.pending_collapse.take() {
            if !dragged {
                self.sel.only(p);
            }
        }
        self.pressed = None;
        self.sel.dragging = false;
    }

    pub fn is_unmounted_drive(&self, p: &Path) -> bool {
        self.tree
            .drive_for(p)
            .is_some_and(|d| d.mount_point.is_none())
    }

    /// Folder (or the Trash) under the pointer, as a drop target.
    pub fn drop_target(&self) -> Option<PathBuf> {
        match (&self.sel.hover, self.sel.hover_kind) {
            (Some(p), Some(Kind::Folder)) => Some(p.clone()),
            _ => None,
        }
    }

    fn menu_event(&self) -> Event {
        let items: Vec<(PathBuf, Kind)> = self
            .selected_items()
            .into_iter()
            .filter(|(p, _)| p.as_os_str() != TRASH)
            .collect();
        let drive = match items.as_slice() {
            [(p, _)] => self.tree.drive_for(p).cloned(),
            _ => None,
        };
        let trash_clicked = self.sel.items.iter().any(|p| p.as_os_str() == TRASH);
        Event::Menu(Target {
            bookmarked: items.len() == 1 && self.tree.is_bookmarked(&items[0].0),
            dir: self.current_dir(),
            in_trash: (self.browse.is_trash() && self.mode != Mode::Tree) || trash_clicked,
            drive,
            items,
        })
    }

    fn tree_event(&mut self, e: tree::Event) -> Option<Event> {
        match e {
            tree::Event::Item(_) => None,
            tree::Event::Mount(d) => Some(Event::Mount(d)),
            tree::Event::Status(s) => Some(Event::Status(s)),
        }
    }

    pub fn key(&mut self, k: Key) -> (Task<Msg>, Option<Event>) {
        let items = self.items();
        if items.is_empty() {
            return (Task::none(), None);
        }
        let pos = self
            .sel
            .cursor
            .as_ref()
            .and_then(|c| items.iter().position(|(p, _)| p == c));
        let step = match self.mode {
            Mode::Grid if !self.search.is_active() => self.browse.grid_cols(),
            _ => 1,
        };
        let page = 12;
        let last = items.len() - 1;
        let target = match k {
            Key::Down => Some(pos.map_or(0, |i| (i + step).min(last))),
            Key::Up => Some(pos.map_or(0, |i| i.saturating_sub(step))),
            Key::Right if self.mode == Mode::Grid => Some(pos.map_or(0, |i| (i + 1).min(last))),
            Key::Left if self.mode == Mode::Grid => Some(pos.map_or(0, |i| i.saturating_sub(1))),
            Key::Home => Some(0),
            Key::End => Some(last),
            Key::PageDown => Some(pos.map_or(0, |i| (i + page * step).min(last))),
            Key::PageUp => Some(pos.map_or(0, |i| i.saturating_sub(page * step))),
            Key::Char(c) => {
                // type-ahead: jump to the next name starting with what was typed
                let fresh = self
                    .typeahead
                    .1
                    .is_none_or(|t| t.elapsed() > Duration::from_millis(900));
                if fresh {
                    self.typeahead.0.clear();
                }
                self.typeahead.0.push(c.to_ascii_lowercase());
                self.typeahead.1 = Some(Instant::now());
                let pre = self.typeahead.0.clone();
                let start = if fresh {
                    pos.map_or(0, |i| i + 1)
                } else {
                    pos.unwrap_or(0)
                };
                (0..items.len())
                    .map(|o| (start + o) % items.len())
                    .find(|&i| {
                        items[i]
                            .0
                            .file_name()
                            .map(|n| n.to_string_lossy().to_lowercase().starts_with(&pre))
                            .unwrap_or(false)
                    })
            }
            _ => None,
        };
        if let Some(i) = target {
            let (p, kind) = items[i].clone();
            if self.mods.shift() && !matches!(k, Key::Char(_)) {
                let order: Vec<PathBuf> = items.iter().map(|(p, _)| p.clone()).collect();
                if self.sel.anchor.is_none() {
                    self.sel.anchor = self.sel.cursor.clone();
                }
                self.sel.range(p.clone(), &order);
            } else {
                self.sel.only(p.clone());
            }
            return (self.scroll_to_cursor(), Some(Event::Inspect(p, kind)));
        }
        match k {
            Key::Enter => match pos {
                Some(i) => {
                    let (p, kind) = items[i].clone();
                    self.activate(p, kind, true)
                }
                None => (Task::none(), None),
            },
            Key::Right | Key::Left if self.mode == Mode::Tree && !self.search.is_active() => {
                match self.sel.cursor.clone() {
                    Some(c) => {
                        let (t, ev) = self.tree.toggle_path(&c, Some(k == Key::Right));
                        let ev = ev.and_then(|e| self.tree_event(e));
                        (t.map(Msg::Tree), ev)
                    }
                    None => (Task::none(), None),
                }
            }
            Key::Parent if self.mode != Mode::Tree => {
                self.sel.clear();
                let (t, _) = self.browse.update(browse::Msg::Up);
                (t.map(Msg::Browse), None)
            }
            Key::SelectAll => {
                self.sel.items = items
                    .into_iter()
                    .map(|(p, _)| p)
                    .filter(|p| p.as_os_str() != TRASH)
                    .collect();
                (Task::none(), None)
            }
            Key::Escape => {
                if self.search.is_active() || self.search_open {
                    self.close_search_now();
                } else {
                    self.sel.clear();
                }
                (Task::none(), None)
            }
            _ => (Task::none(), None),
        }
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
                    self.close_search_now();
                    (Task::none(), None)
                } else {
                    (self.open_search(), None)
                }
            }
            Msg::SearchInput(q) => {
                // short pause so a walk doesn't start on every keystroke
                self.query = q;
                self.query_gen += 1;
                let gen = self.query_gen;
                (
                    Task::perform(tokio::time::sleep(Duration::from_millis(180)), move |_| {
                        Msg::SearchFire(gen)
                    }),
                    None,
                )
            }
            Msg::SearchFire(gen) if gen == self.query_gen => {
                let lower = self.query.to_lowercase();
                let quick = if lower.is_empty() {
                    Vec::new()
                } else {
                    self.tree.loaded_matches(&lower)
                };
                let t = self
                    .search
                    .start(self.query.clone(), quick, self.tree.search_roots());
                (t.map(Msg::Search), None)
            }
            Msg::SearchFire(_) => (Task::none(), None),
            Msg::SearchContent(on) => {
                self.search.content = on;
                self.query_gen += 1;
                (Task::done(Msg::SearchFire(self.query_gen)), None)
            }
            Msg::Focus => {
                self.focused = !self.focused;
                (Task::none(), Some(Event::ToggleFocus))
            }
            Msg::DiskUsage => (Task::none(), Some(Event::DiskUsage(self.current_dir()))),
            Msg::Tree(m) => {
                let (t, ev) = self.tree.update(m);
                let t = t.map(Msg::Tree);
                match ev {
                    Some(tree::Event::Item(a)) => {
                        let (t2, ev) = self.item(a);
                        (Task::batch([t, t2]), ev)
                    }
                    Some(e) => {
                        let ev = self.tree_event(e);
                        (t, ev)
                    }
                    None => (t, None),
                }
            }
            Msg::Browse(m) => {
                let (t, ev) = self.browse.update(m);
                // a freshly loaded folder in grid mode needs its thumbnails
                let more = if self.mode == Mode::Grid {
                    self.browse.request_thumbs()
                } else {
                    Task::none()
                };
                let t = Task::batch([t, more]).map(Msg::Browse);
                match ev {
                    Some(browse::Event::Item(a)) => {
                        let (t2, ev) = self.item(a);
                        (Task::batch([t, t2]), ev)
                    }
                    Some(browse::Event::EmptyTrash) => (t, Some(Event::EmptyTrash)),
                    Some(browse::Event::Status(s)) => (t, Some(Event::Status(s))),
                    None => (t, None),
                }
            }
            Msg::Search(m) => match self.search.update(m) {
                Some(search::Event::Item(a)) => self.item(a),
                None => (Task::none(), None),
            },
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
                Space::with_width(Length::Fill),
                button(text("Disk usage").size(12))
                    .on_press(Msg::DiskUsage)
                    .style(button::secondary)
                    .padding([3, 8]),
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
                text_input("search names under home, bookmarks, drives…", &self.query)
                    .id(text_input::Id::new(SEARCH_ID))
                    .on_input(Msg::SearchInput)
                    .size(13),
            );
            col = col.push(
                checkbox("Also search inside text files", self.search.content)
                    .on_toggle(Msg::SearchContent)
                    .size(14)
                    .text_size(12),
            );
        }
        col = col.push(chips);

        let body = if self.search.is_active() {
            self.search.view(&self.sel).map(Msg::Search)
        } else {
            match self.mode {
                Mode::Tree => self.tree.view(&self.sel).map(Msg::Tree),
                Mode::List => self.browse.view_list(&self.sel).map(Msg::Browse),
                Mode::Grid => self.browse.view_grid(&self.sel).map(Msg::Browse),
            }
        };
        col.push(body).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(s: &mut Sidebar, p: &str, mods: Modifiers) -> Option<Event> {
        s.mods = mods;
        s.item(ItemAct::Press(PathBuf::from(p), Kind::Text)).1
    }

    #[test]
    fn click_ctrl_shift_and_drag_selection() {
        let (mut s, _) = Sidebar::new();
        // single press selects and previews
        assert!(matches!(
            press(&mut s, "/a", Modifiers::empty()),
            Some(Event::Open(_, _, false))
        ));
        assert_eq!(s.sel.paths(), vec![PathBuf::from("/a")]);
        assert_eq!(s.pressed.take(), Some(vec![PathBuf::from("/a")]));
        // ctrl adds
        press(&mut s, "/b", Modifiers::CTRL);
        assert_eq!(s.sel.items.len(), 2);
        // pressing a selected item keeps the group for dragging…
        press(&mut s, "/a", Modifiers::empty());
        assert_eq!(s.sel.items.len(), 2);
        assert_eq!(s.pressed.take().map(|p| p.len()), Some(2));
        // …and a drag keeps it after release
        s.mouse_up(true);
        assert_eq!(s.sel.items.len(), 2);
        // a plain click (no drag) on a selected item collapses to it
        press(&mut s, "/b", Modifiers::empty());
        s.mouse_up(false);
        assert_eq!(s.sel.paths(), vec![PathBuf::from("/b")]);
    }

    #[test]
    fn hover_leave_after_enter_keeps_new_target() {
        let (mut s, _) = Sidebar::new();
        s.item(ItemAct::Hover(PathBuf::from("/new"), Kind::Folder, true));
        // the old item's leave arrives late
        s.item(ItemAct::Hover(PathBuf::from("/old"), Kind::Folder, false));
        assert_eq!(s.drop_target(), Some(PathBuf::from("/new")));
        s.item(ItemAct::Hover(PathBuf::from("/new"), Kind::Folder, false));
        assert_eq!(s.drop_target(), None);
        // files aren't drop targets
        s.item(ItemAct::Hover(PathBuf::from("/f.txt"), Kind::Text, true));
        assert_eq!(s.drop_target(), None);
    }

    #[test]
    fn double_press_opens_pinned() {
        let (mut s, _) = Sidebar::new();
        press(&mut s, "/a.txt", Modifiers::empty());
        assert!(matches!(
            press(&mut s, "/a.txt", Modifiers::empty()),
            Some(Event::Open(_, _, true))
        ));
    }
}
