//! Tree view of the files pane: Bookmarks / Devices / Network / Trash,
//! lazily expanded. Children load on first expand via `fsops::list_dir`;
//! the flattened line list is cached and rebuilt only when the tree or the
//! kind filter changes, and only the visible lines are built (virtualized).

use crate::config::{self, Bookmarks};
use crate::devices::Drive;
use crate::filetype::Kind;
use crate::fsops::{self, Entry};
use crate::net::{Conn, Discovered, Source};
use crate::search::Hit;
use crate::theme;
use crate::widgets::{self, ItemAct, Selection, Vp};
use iced::widget::{button, container, row, scrollable, text, Space};
use iced::{Element, Length, Task};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const ROW_H: f32 = 24.0;
pub const SCROLL_ID: &str = "tree-scroll";
/// Stand-in path for the Trash entry.
pub const TRASH: &str = "trash:";
/// Stand-in paths for network rows that aren't folders (yet).
const NET_CONNECT: &str = "netconnect:";
const NET_SAVED: &str = "net:";
const NET_FOUND: &str = "netfound:";

/// What a click on a network row means.
#[derive(Debug, Clone)]
pub enum NetAct {
    /// "Connect to server…"
    ConnectNew,
    /// a saved connection that isn't mounted: mount it
    Mount(Conn),
    /// a discovered machine/share: open the connect dialog prefilled
    Discovered(Discovered),
}

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
    /// set for drive rows; unmounted drives have path = device file
    drive: Option<Drive>,
    /// special icon (network rows)
    glyph: Option<iced_fonts::Bootstrap>,
    /// greyed out (offline tailnet peer)
    dim: bool,
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
            drive: None,
            glyph: None,
            dim: false,
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

    /// loaded + expanded folders under (and including) this node
    fn collect_open(&self, out: &mut Vec<PathBuf>) {
        if self.expanded && self.children.is_some() {
            out.push(self.path.clone());
            for c in self.children.iter().flatten() {
                c.collect_open(out);
            }
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
    expandable: bool,
    drive: bool,
    removable: bool,
    glyph: Option<iced_fonts::Bootstrap>,
    dim: bool,
}

#[derive(Debug, Clone)]
enum Line {
    Heading(&'static str, u16),
    Row(Row),
}

pub struct Tree {
    bookmarks: Bookmarks,
    drives: Vec<Drive>,
    network: Vec<PathBuf>,
    net_saved: Vec<Conn>,
    net_mounted: Vec<(String, PathBuf)>,
    net_found: Vec<Discovered>,
    // (section, subheading, nodes) — subheading is "Folders"/"Files"
    groups: Vec<(Section, Option<&'static str>, Vec<Node>)>,
    lines: Vec<Line>,
    filter: Option<Kind>,
    vp: Vp,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Toggle(Section, PathBuf),
    Loaded(Section, PathBuf, Result<Vec<Entry>, String>),
    Item(ItemAct),
    Network(Vec<PathBuf>),
    BookmarksSaved(Result<(), String>),
    Scrolled(Vp),
}

pub enum Event {
    Item(ItemAct),
    /// an unmounted drive was expanded: mount it
    Mount(Drive),
    Status(String),
}

impl Tree {
    pub fn new() -> (Self, Task<Msg>) {
        let mut t = Self {
            bookmarks: Bookmarks::load(),
            drives: Vec::new(),
            network: Vec::new(),
            net_saved: Vec::new(),
            net_mounted: Vec::new(),
            net_found: Vec::new(),
            groups: Vec::new(),
            lines: Vec::new(),
            filter: None,
            vp: Vp::default(),
        };
        t.rebuild_groups();
        (t, Task::perform(fsops::mounts(), |(_, n)| Msg::Network(n)))
    }

    pub fn set_filter(&mut self, f: Option<Kind>) {
        self.filter = f;
        self.rebuild_lines();
    }

    pub fn set_drives(&mut self, drives: Vec<Drive>) {
        self.drives = drives;
        self.rebuild_groups();
    }

    pub fn drive_for(&self, path: &Path) -> Option<&Drive> {
        self.drives
            .iter()
            .find(|d| d.mount_point.as_deref() == Some(path) || d.device == path)
    }

    pub fn set_network(
        &mut self,
        saved: Vec<Conn>,
        mounted: Vec<(String, PathBuf)>,
        found: Vec<Discovered>,
    ) {
        self.net_saved = saved;
        self.net_mounted = mounted;
        self.net_found = found;
        self.rebuild_groups();
    }

    /// What a click on this row means, if it's a network row.
    pub fn net_action(&self, p: &Path) -> Option<NetAct> {
        let s = p.to_str()?;
        if s == NET_CONNECT {
            return Some(NetAct::ConnectNew);
        }
        if let Some(id) = s.strip_prefix(NET_SAVED) {
            return self
                .net_saved
                .iter()
                .find(|c| c.id == id)
                .cloned()
                .map(NetAct::Mount);
        }
        if s.starts_with(NET_FOUND) {
            return self
                .net_found
                .iter()
                .find(|d| found_key(d) == s)
                .cloned()
                .map(NetAct::Discovered);
        }
        None
    }

    /// The saved connection behind a row (mounted folder or not), for menus.
    pub fn conn_for(&self, p: &Path) -> Option<Conn> {
        if let Some(NetAct::Mount(c)) = self.net_action(p) {
            return Some(c);
        }
        let id = self
            .net_mounted
            .iter()
            .find(|(_, m)| m == p)
            .map(|(id, _)| id.clone())?;
        self.net_saved.iter().find(|c| c.id == id).cloned()
    }

    pub fn is_bookmarked(&self, p: &Path) -> bool {
        self.bookmarks.folders.iter().any(|b| b == p) || self.bookmarks.files.iter().any(|b| b == p)
    }

    /// Paths of the rows in display order (for shift-click ranges and
    /// keyboard navigation).
    pub fn visible_paths(&self) -> Vec<PathBuf> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                Line::Row(r) => Some(r.path.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn kind_of(&self, p: &Path) -> Option<Kind> {
        self.lines.iter().find_map(|l| match l {
            Line::Row(r) if r.path == p => Some(r.kind),
            _ => None,
        })
    }

    /// Scroll offset that shows `p`, if it's off screen.
    pub fn reveal(&self, p: &Path) -> Option<f32> {
        let i = self
            .lines
            .iter()
            .position(|l| matches!(l, Line::Row(r) if r.path == p))?;
        self.vp.reveal(i, ROW_H)
    }

    /// Instant first-pass search: matches among everything already loaded.
    pub fn loaded_matches(&self, q: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        for (_, _, nodes) in &self.groups {
            for n in nodes {
                if n.drive.is_none() && n.name.to_lowercase().contains(q) {
                    out.push(Hit::new(n.path.clone(), n.name.clone(), n.kind));
                }
                n.collect_matches(q, &mut out);
            }
        }
        out
    }

    /// Where the background search walks: home, bookmarked folders, and
    /// mounted removable/network drives. `/` is left out and nested roots
    /// are dropped.
    pub fn search_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = std::iter::once(config::home())
            .chain(self.bookmarks.folders.iter().cloned())
            .chain(
                self.drives
                    .iter()
                    .filter(|d| !d.system)
                    .filter_map(|d| d.mount_point.clone()),
            )
            .chain(self.network.iter().cloned())
            .collect();
        roots.sort();
        roots.dedup();
        let all = roots.clone();
        roots.retain(|r| !all.iter().any(|o| o != r && r.starts_with(o)));
        roots
    }

    /// Folders the tree is showing (for the folder watcher).
    pub fn open_dirs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for (_, _, nodes) in &self.groups {
            for n in nodes {
                n.collect_open(&mut out);
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Re-list any shown folder in `dirs`, keeping expansion state.
    pub fn refresh(&mut self, dirs: &[PathBuf]) -> Task<Msg> {
        let mut tasks = Vec::new();
        for (section, _, nodes) in &self.groups {
            for n in nodes {
                let mut open = Vec::new();
                n.collect_open(&mut open);
                for p in open.into_iter().filter(|p| dirs.contains(p)) {
                    let s = *section;
                    let p2 = p.clone();
                    tasks.push(Task::perform(fsops::list_dir(p), move |r| {
                        Msg::Loaded(s, p2.clone(), r)
                    }));
                }
            }
        }
        Task::batch(tasks)
    }

    /// Expand or collapse the row for `p` (double-click / keyboard).
    pub fn toggle_path(&mut self, p: &Path, expand: Option<bool>) -> (Task<Msg>, Option<Event>) {
        let section = self.lines.iter().find_map(|l| match l {
            Line::Row(r) if r.path == p => Some(r.section),
            _ => None,
        });
        let Some(section) = section else {
            return (Task::none(), None);
        };
        let is_open = self
            .find_mut(section, p)
            .map(|n| n.expanded)
            .unwrap_or(false);
        if expand.is_some_and(|e| e == is_open) {
            return (Task::none(), None);
        }
        self.update(Msg::Toggle(section, p.to_path_buf()))
    }

    fn rebuild_groups(&mut self) {
        // reuse existing top-level nodes so expansion survives a rebuild
        let mut old: HashMap<PathBuf, Node> = self
            .groups
            .drain(..)
            .flat_map(|(_, _, nodes)| nodes)
            .map(|n| (n.path.clone(), n))
            .collect();
        let mut take = |path: PathBuf, name: Option<&str>, kind: Kind| -> Node {
            match old.remove(&path) {
                Some(mut n) => {
                    if let Some(name) = name {
                        n.name = name.to_string();
                    }
                    n
                }
                None => Node::new(path, name, kind),
            }
        };
        let bm_folders = self
            .bookmarks
            .folders
            .clone()
            .into_iter()
            .map(|p| take(p, None, Kind::Folder))
            .collect();
        let bm_files = self
            .bookmarks
            .files
            .clone()
            .into_iter()
            .map(|p| {
                let k = Kind::of(&p, false);
                take(p, None, k)
            })
            .collect();
        let mut devices = vec![
            take(config::home(), Some("Home"), Kind::Folder),
            take("/".into(), Some("Root  /"), Kind::Folder),
        ];
        for d in self.drives.iter().filter(|d| !d.system || d.removable) {
            let path = d.mount_point.clone().unwrap_or_else(|| d.device.clone());
            let label = format!(
                "{}  {}{}",
                d.label,
                fsops::human_size(d.size),
                if d.encrypted {
                    "  · locked"
                } else if d.mount_point.is_none() {
                    "  · not mounted"
                } else {
                    ""
                }
            );
            let mut n = take(path, Some(&label), Kind::Folder);
            n.drive = Some(d.clone());
            if d.mount_point.is_none() {
                n.expanded = false;
                n.children = None;
            }
            devices.push(n);
        }
        devices.push(take(PathBuf::from(TRASH), Some("Trash"), Kind::Folder));
        use iced_fonts::Bootstrap as B;
        // saved connections: mounted ones are real folders, the rest mount on click
        let mut conns = Vec::new();
        let mut connect = Node::new(
            PathBuf::from(NET_CONNECT),
            Some("Connect to server…"),
            Kind::Folder,
        );
        connect.glyph = Some(B::PlusCircle);
        conns.push(connect);
        let rclone_root = crate::net::mount_root();
        for c in self.net_saved.clone() {
            let mounted = self
                .net_mounted
                .iter()
                .find(|(id, _)| *id == c.id)
                .map(|(_, p)| p.clone());
            let mut n = match mounted {
                Some(p) => {
                    let mut n = take(p, Some(&c.name), Kind::Folder);
                    n.glyph = Some(B::CloudCheck);
                    n
                }
                None => {
                    let mut n = Node::new(
                        PathBuf::from(format!("{NET_SAVED}{}", c.id)),
                        Some(&c.name),
                        Kind::Folder,
                    );
                    n.glyph = Some(B::Cloud);
                    n
                }
            };
            n.dim = false;
            conns.push(n);
        }
        // 0 = tailnet, 1 = LAN, 2 = ~/.ssh/config
        let group_of = |src: &Source| match src {
            Source::Tailnet { .. } => 0,
            Source::Lan => 1,
            Source::SshConfig => 2,
        };
        let found = |group: u8| -> Vec<Node> {
            self.net_found
                .iter()
                .filter(|d| group_of(&d.source) == group)
                // phones/tablets don't serve files over SSH
                .filter(|d| !matches!(&d.source, Source::Tailnet { os, .. } if matches!(os.to_ascii_lowercase().as_str(), "ios" | "android" | "ipados" | "tvos")))
                .filter(|d| {
                    !self
                        .net_saved
                        .iter()
                        .any(|c| c.host == d.host && c.proto == d.proto)
                })
                .map(|d| {
                    let (online, glyph) = match &d.source {
                        Source::Tailnet { online, .. } => (*online, B::PcDisplay),
                        _ if d.proto == crate::net::Proto::Smb => (true, B::HddNetwork),
                        _ => (true, B::Server),
                    };
                    let label = match &d.source {
                        Source::Tailnet { online: false, .. } => format!("{}  · offline", d.name),
                        Source::Tailnet { .. } => d.name.clone(),
                        _ => format!("{}  · {}", d.name, short_proto(d.proto)),
                    };
                    let mut n = Node::new(PathBuf::from(found_key(d)), Some(&label), Kind::Folder);
                    n.glyph = Some(glyph);
                    n.dim = !online;
                    n
                })
                .collect()
        };
        let tailnet = found(0);
        let lan = found(1);
        let ssh = found(2);
        let other = self
            .network
            .clone()
            .into_iter()
            .filter(|p| !p.starts_with(&rclone_root))
            .map(|p| take(p, None, Kind::Folder))
            .collect();
        self.groups = vec![
            (Section::Bookmarks, Some("Folders"), bm_folders),
            (Section::Bookmarks, Some("Files"), bm_files),
            (Section::Devices, None, devices),
            (Section::Network, None, conns),
            (Section::Network, Some("Tailnet"), tailnet),
            (Section::Network, Some("On this network"), lan),
            (Section::Network, Some("From ~/.ssh/config"), ssh),
            (Section::Network, Some("Other mounts"), other),
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
            let trash = n.path.as_os_str() == TRASH || is_net_pseudo(&n.path);
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
                expandable: n.kind == Kind::Folder
                    && !trash
                    && !n.drive.as_ref().is_some_and(|d| d.encrypted),
                drive: n.drive.is_some(),
                removable: n.drive.as_ref().is_some_and(|d| d.removable),
                glyph: n.glyph,
                dim: n.dim,
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
                // empty network sub-groups are just left out
                if nodes.is_empty() && s == Section::Network && sub.is_some() {
                    continue;
                }
                if let Some(sub) = sub {
                    lines.push(Line::Heading(sub, 1));
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
                if let Some(d) = &node.drive {
                    if d.mount_point.is_none() {
                        return (
                            Task::none(),
                            (!d.encrypted).then(|| Event::Mount(d.clone())),
                        );
                    }
                }
                if node.path.as_os_str() == TRASH {
                    return (Task::none(), None);
                }
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
                            // merge so expanded subfolders survive a refresh
                            let mut old: HashMap<PathBuf, Node> = node
                                .children
                                .take()
                                .unwrap_or_default()
                                .into_iter()
                                .map(|c| (c.path.clone(), c))
                                .collect();
                            node.children = Some(
                                entries
                                    .into_iter()
                                    .map(|e| match old.remove(&e.path) {
                                        Some(o) if o.kind == e.kind => o,
                                        _ => Node::from_entry(e),
                                    })
                                    .collect(),
                            );
                            node.error = None;
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
            Msg::Item(a) => (Task::none(), Some(Event::Item(a))),
            Msg::Network(n) => {
                self.network = n;
                self.rebuild_groups();
                (Task::none(), None)
            }
            Msg::BookmarksSaved(r) => (
                Task::none(),
                r.err().map(|e| Event::Status(format!("bookmarks: {e}"))),
            ),
            Msg::Scrolled(vp) => {
                self.vp = vp;
                (Task::none(), None)
            }
        }
    }

    /// Add or remove a bookmark.
    pub fn toggle_bookmark(&mut self, path: PathBuf, is_dir: bool) -> Task<Msg> {
        let list = if is_dir {
            &mut self.bookmarks.folders
        } else {
            &mut self.bookmarks.files
        };
        if let Some(i) = list.iter().position(|p| *p == path) {
            list.remove(i);
        } else {
            list.push(path);
        }
        self.rebuild_groups();
        Task::perform(self.bookmarks.clone().save(), Msg::BookmarksSaved)
    }

    pub fn view<'a>(&'a self, sel: &'a Selection) -> Element<'a, Msg> {
        let list =
            widgets::virtual_column(self.lines.len(), ROW_H, self.vp, |i| match &self.lines[i] {
                Line::Heading(t, depth) => {
                    let t = text(*t).size(if *depth == 0 { 14 } else { 12 });
                    let t = if *depth == 0 {
                        t
                    } else {
                        t.color(theme::MUTED)
                    };
                    container(t)
                        .padding(iced::Padding {
                            top: if *depth == 0 { 5.0 } else { 4.0 },
                            left: 4.0 + 14.0 * *depth as f32,
                            ..Default::default()
                        })
                        .into()
                }
                Line::Row(r) => {
                    use iced_fonts::Bootstrap as B;
                    let chevron: Element<'_, Msg> = match (r.expandable, r.expanded, r.loading) {
                        (_, _, true) => text("…").size(11).color(theme::MUTED).into(),
                        (true, true, _) => widgets::icon(B::ChevronDown)
                            .size(10)
                            .color(theme::MUTED)
                            .into(),
                        (true, false, _) => widgets::icon(B::ChevronRight)
                            .size(10)
                            .color(theme::MUTED)
                            .into(),
                        (false, _, _) => Space::with_width(10).into(),
                    };
                    let arrow = button(chevron)
                        .on_press_maybe(
                            r.expandable.then(|| Msg::Toggle(r.section, r.path.clone())),
                        )
                        .style(button::text)
                        .padding([3, 4])
                        .width(22);
                    // special rows get their own icon
                    let glyph = if let Some(g) = r.glyph {
                        widgets::icon(g).color(if r.dim {
                            theme::MUTED.scale_alpha(0.6)
                        } else {
                            theme::MUTED
                        })
                    } else if r.path.as_os_str() == TRASH {
                        widgets::icon(B::Trash).color(theme::MUTED)
                    } else if r.path == config::home() {
                        widgets::icon(B::HouseFill).color(iced::color!(0xe0b85c))
                    } else if r.drive {
                        widgets::icon(if r.removable { B::UsbDrive } else { B::HddFill })
                            .color(theme::MUTED)
                    } else {
                        widgets::kind_icon(r.kind)
                    };
                    let label = row![glyph.size(13).width(18), text(r.label.clone()).size(13)]
                        .spacing(6)
                        .align_y(iced::Alignment::Center);
                    row![
                        Space::with_width(14.0 * r.depth as f32),
                        arrow,
                        widgets::item(label, &r.path, r.kind, sel, Msg::Item),
                    ]
                    .align_y(iced::Alignment::Center)
                    .into()
                }
            });
        scrollable(list)
            .id(scrollable::Id::new(SCROLL_ID))
            .on_scroll(|v| Msg::Scrolled(Vp::from(v)))
            .height(Length::Fill)
            .into()
    }
}

fn is_net_pseudo(p: &Path) -> bool {
    p.to_str()
        .is_some_and(|s| s == NET_CONNECT || s.starts_with(NET_SAVED) || s.starts_with(NET_FOUND))
}

fn found_key(d: &Discovered) -> String {
    format!(
        "{NET_FOUND}{:?}:{}:{}",
        d.proto,
        d.host,
        d.port.unwrap_or(0)
    )
}

fn short_proto(p: crate::net::Proto) -> &'static str {
    match p {
        crate::net::Proto::Sftp => "SSH",
        crate::net::Proto::Smb => "SMB",
        crate::net::Proto::WebDav => "WebDAV",
        crate::net::Proto::Ftp => "FTP",
    }
}
