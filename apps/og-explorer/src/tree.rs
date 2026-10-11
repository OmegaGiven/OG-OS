//! Tree view of the left pane: Bookmarks / Devices / Network, lazily
//! expanded. Children load on first expand via `fsops::list_dir`; the
//! flattened row list is cached and rebuilt only when the tree or the kind
//! filter changes, never per frame.

use crate::config::{self, Bookmarks};
use crate::filetype::Kind;
use crate::fsops::{self, Entry};
use crate::search::Hit;
use crate::theme;
use iced::widget::{button, column, container, row, scrollable, text, Space};
use iced::{Element, Length, Task};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Bookmarks,
    Devices,
    Network,
}

impl Section {
    const ALL: [Section; 3] = [Section::Bookmarks, Section::Devices, Section::Network];

    fn title(self) -> &'static str {
        match self {
            Section::Bookmarks => "Bookmarks",
            Section::Devices => "Devices",
            Section::Network => "Network",
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    path: PathBuf,
    name: String,
    kind: Kind,
    expanded: bool,
    loading: bool,
    children: Option<Vec<Node>>,
    error: Option<String>,
}

impl Node {
    fn new(path: PathBuf, name: Option<&str>, kind: Kind) -> Self {
        let name = name
            .map(str::to_owned)
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            name,
            kind,
            expanded: false,
            loading: false,
            children: None,
            error: None,
        }
    }

    fn from_entry(e: Entry) -> Self {
        Self::new(e.path, Some(&e.name), e.kind)
    }

    fn find_mut(&mut self, path: &Path) -> Option<&mut Node> {
        if self.path == path {
            return Some(self);
        }
        if !path.starts_with(&self.path) {
            return None;
        }
        self.children
            .as_mut()?
            .iter_mut()
            .find_map(|c| c.find_mut(path))
    }

    fn collect_matches(&self, q: &str, out: &mut Vec<Hit>) {
        for c in self.children.iter().flatten() {
            if c.name.to_lowercase().contains(q) {
                out.push(Hit::new(c.path.clone(), c.name.clone(), c.kind));
            }
            c.collect_matches(q, out);
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    section: Section,
    depth: u16,
    path: PathBuf,
    label: String,
    kind: Kind,
    expanded: bool,
    loading: bool,
}

#[derive(Debug, Clone)]
enum Line {
    Heading(&'static str, u16),
    Row(Row),
}

pub struct Tree {
    bookmarks: Bookmarks,
    removable: Vec<PathBuf>,
    network: Vec<PathBuf>,
    // (section, subheading, nodes) — subheading is "Folders"/"Files"
    groups: Vec<(Section, Option<&'static str>, Vec<Node>)>,
    lines: Vec<Line>,
    filter: Option<Kind>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Toggle(Section, PathBuf),
    Loaded(Section, PathBuf, Result<Vec<Entry>, String>),
    Activate(PathBuf, Kind),
    Mounts(Vec<PathBuf>, Vec<PathBuf>),
    BookmarksSaved(Result<(), String>),
}

pub enum Event {
    Open(PathBuf, Kind),
    Status(String),
}

impl Tree {
    pub fn new() -> (Self, Task<Msg>) {
        let mut t = Self {
            bookmarks: Bookmarks::load(),
            removable: Vec::new(),
            network: Vec::new(),
            groups: Vec::new(),
            lines: Vec::new(),
            filter: None,
        };
        t.rebuild_groups();
        (
            t,
            Task::perform(fsops::mounts(), |(r, n)| Msg::Mounts(r, n)),
        )
    }

    pub fn set_filter(&mut self, f: Option<Kind>) {
        self.filter = f;
        self.rebuild_lines();
    }

    /// Instant first-pass search: matches among everything already loaded.
    pub fn loaded_matches(&self, q: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        for (_, _, nodes) in &self.groups {
            for n in nodes {
                if n.name.to_lowercase().contains(q) {
                    out.push(Hit::new(n.path.clone(), n.name.clone(), n.kind));
                }
                n.collect_matches(q, &mut out);
            }
        }
        out
    }

    /// Where the background search walks: home, bookmarked folders, and
    /// mounted drives. `/` is left out (it would walk the whole system) and
    /// roots nested inside another root are dropped.
    pub fn search_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = std::iter::once(config::home())
            .chain(self.bookmarks.folders.iter().cloned())
            .chain(self.removable.iter().cloned())
            .chain(self.network.iter().cloned())
            .collect();
        roots.sort();
        roots.dedup();
        let all = roots.clone();
        roots.retain(|r| !all.iter().any(|o| o != r && r.starts_with(o)));
        roots
    }

    fn rebuild_groups(&mut self) {
        let bm_folders = self
            .bookmarks
            .folders
            .iter()
            .map(|p| Node::new(p.clone(), None, Kind::Folder))
            .collect();
        let bm_files = self
            .bookmarks
            .files
            .iter()
            .map(|p| Node::new(p.clone(), None, Kind::of(p, false)))
            .collect();
        let mut devices = vec![
            Node::new(config::home(), Some("Home"), Kind::Folder),
            Node::new("/".into(), Some("Root  /"), Kind::Folder),
        ];
        devices.extend(
            self.removable
                .iter()
                .map(|p| Node::new(p.clone(), None, Kind::Folder)),
        );
        let network = self
            .network
            .iter()
            .map(|p| Node::new(p.clone(), None, Kind::Folder))
            .collect();
        self.groups = vec![
            (Section::Bookmarks, Some("Folders"), bm_folders),
            (Section::Bookmarks, Some("Files"), bm_files),
            (Section::Devices, None, devices),
            (Section::Network, None, network),
        ];
        self.rebuild_lines();
    }

    fn find_mut(&mut self, section: Section, path: &Path) -> Option<&mut Node> {
        self.groups
            .iter_mut()
            .filter(|(s, _, _)| *s == section)
            .flat_map(|(_, _, nodes)| nodes.iter_mut())
            .find_map(|n| n.find_mut(path))
    }

    fn rebuild_lines(&mut self) {
        fn walk(out: &mut Vec<Line>, s: Section, n: &Node, depth: u16, filter: Option<Kind>) {
            // dotfiles hidden below the top level; filter hides non-matching
            // files but keeps folders so you can still drill down
            if depth > 1 && n.name.starts_with('.') {
                return;
            }
            if n.kind != Kind::Folder && filter.is_some_and(|k| k != n.kind) {
                return;
            }
            out.push(Line::Row(Row {
                section: s,
                depth,
                path: n.path.clone(),
                label: n
                    .error
                    .as_ref()
                    .map(|e| format!("{}  ({e})", n.name))
                    .unwrap_or_else(|| n.name.clone()),
                kind: n.kind,
                expanded: n.expanded,
                loading: n.loading,
            }));
            if n.expanded {
                for c in n.children.iter().flatten() {
                    walk(out, s, c, depth + 1, filter);
                }
            }
        }
        let mut lines = Vec::new();
        for s in Section::ALL {
            lines.push(Line::Heading(s.title(), 0));
            for (gs, sub, nodes) in &self.groups {
                if *gs != s {
                    continue;
                }
                if let Some(sub) = sub {
                    lines.push(Line::Heading(sub, 1));
                }
                if nodes.is_empty() && s == Section::Network {
                    lines.push(Line::Heading("no network mounts", 2));
                }
                for n in nodes {
                    walk(
                        &mut lines,
                        s,
                        n,
                        if sub.is_some() { 2 } else { 1 },
                        self.filter,
                    );
                }
            }
        }
        self.lines = lines;
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Toggle(section, path) => {
                let Some(node) = self.find_mut(section, &path) else {
                    return (Task::none(), None);
                };
                node.expanded = !node.expanded;
                let task = if node.expanded && node.children.is_none() && !node.loading {
                    node.loading = true;
                    let p = path.clone();
                    Task::perform(fsops::list_dir(p.clone()), move |r| {
                        Msg::Loaded(section, p.clone(), r)
                    })
                } else {
                    Task::none()
                };
                self.rebuild_lines();
                (task, None)
            }
            Msg::Loaded(section, path, result) => {
                if let Some(node) = self.find_mut(section, &path) {
                    node.loading = false;
                    match result {
                        Ok(entries) => {
                            node.children =
                                Some(entries.into_iter().map(Node::from_entry).collect())
                        }
                        Err(e) => {
                            node.error = Some(e);
                            node.children = Some(Vec::new());
                        }
                    }
                }
                self.rebuild_lines();
                (Task::none(), None)
            }
            Msg::Activate(path, kind) => (Task::none(), Some(Event::Open(path, kind))),
            Msg::Mounts(removable, network) => {
                self.removable = removable;
                self.network = network;
                self.rebuild_groups();
                (Task::none(), None)
            }
            Msg::BookmarksSaved(r) => (
                Task::none(),
                r.err().map(|e| Event::Status(format!("bookmarks: {e}"))),
            ),
        }
    }

    #[allow(dead_code)] // wired to the context menu in phase 1
    pub fn add_bookmark(&mut self, path: PathBuf, is_dir: bool) -> Task<Msg> {
        let list = if is_dir {
            &mut self.bookmarks.folders
        } else {
            &mut self.bookmarks.files
        };
        if !list.contains(&path) {
            list.push(path);
        }
        self.rebuild_groups();
        Task::perform(self.bookmarks.clone().save(), Msg::BookmarksSaved)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let lines = self.lines.iter().map(|l| match l {
            Line::Heading(t, depth) => {
                let t = text(*t).size(if *depth == 0 { 15 } else { 13 });
                let t = if *depth == 0 {
                    t
                } else {
                    t.color(theme::MUTED)
                };
                container(t)
                    .padding(iced::Padding {
                        top: if *depth == 0 { 10.0 } else { 2.0 },
                        left: 14.0 * *depth as f32,
                        ..Default::default()
                    })
                    .into()
            }
            Line::Row(r) => {
                let marker = match (r.kind, r.expanded, r.loading) {
                    (_, _, true) => "...",
                    (Kind::Folder, true, _) => "v",
                    (Kind::Folder, false, _) => ">",
                    (k, _, _) => k.icon(),
                };
                // folders expand in place; files open in a tab on the right
                let msg = if r.kind == Kind::Folder {
                    Msg::Toggle(r.section, r.path.clone())
                } else {
                    Msg::Activate(r.path.clone(), r.kind)
                };
                let label = row![
                    text(marker).width(28).size(11).color(theme::MUTED),
                    text(r.label.clone()).size(13)
                ]
                .spacing(6);
                row![
                    Space::with_width(14.0 * r.depth as f32),
                    button(label)
                        .on_press(msg)
                        .style(button::text)
                        .padding([2, 4])
                        .width(Length::Fill),
                ]
                .into()
            }
        });
        scrollable(column(lines).spacing(1))
            .height(Length::Fill)
            .into()
    }
}
