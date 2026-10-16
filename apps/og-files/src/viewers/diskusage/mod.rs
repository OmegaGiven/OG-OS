//! Disk usage analyzer (WinDirStat-style) for a folder or drive: background
//! scan, squarified treemap coloured by file type, sortable size tree,
//! file-type breakdown, largest files, and clean-up actions that go through
//! the app's normal trash/delete jobs (this viewer never touches files).
//!
//! Layout: header (root, totals, free space, size mode, rescan) · breadcrumb
//! of the zoomed folder · left panel with Tree / Types / Largest tabs and the
//! selection's action bar · treemap filling the rest.
//!
//! Data flow: `scan` streams batches of new nodes (~5×/s) which are appended
//! to the `tree` arena (aggregates updated incrementally) and bump
//! `version`; derived views (visible rows, type table, largest files, the
//! treemap layout) are recomputed from the arena, throttled while scanning.

mod scan;
#[cfg(test)]
// unix stat (dev / inode / blocks) behaviour
#[cfg(all(test, unix))]
mod tests;
mod tree;
mod treemap;

use super::Event;
use crate::filetype::Kind;
use crate::fsops::human_size;
use crate::theme;
use crate::widgets::{self, Vp};
use iced::futures::SinkExt;
use iced::keyboard::{key::Named, Key, Modifiers};
use iced::widget::{
    button, canvas, column, container, mouse_area, opaque, progress_bar, row, scrollable, stack,
    text, Space,
};
use iced::{task, Element, Length, Point, Size, Task, Theme};
use iced_fonts::Bootstrap as B;
use scan::{Batch, NodeKind, Probe, Progress, ScanEnd, NONE};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tree::{ExtStat, Mode, Sort, Tree};
use treemap::LRect;

const ROW_H: f32 = 24.0;
const BIG_ROW_H: f32 = 36.0;
const TOP_N: usize = 100;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// while scanning, derived views refresh at most this often
const DERIVED_EVERY: Duration = Duration::from_millis(500);
const AGGR_EVERY: Duration = Duration::from_millis(1000);
const SCROLL_IDS: [&str; 3] = ["du-tree", "du-types", "du-largest"];

/// What to analyze. No async load: the State scans in the background and
/// renders partial results while it runs.
#[derive(Debug, Clone)]
pub struct Opened {
    pub root: PathBuf,
}

pub fn open(root: PathBuf) -> Opened {
    Opened { root }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Tree,
    Types,
    Largest,
}

impl Tab {
    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Open,
    Reveal,
    Trash,
    Delete,
    ZoomIn,
    /// Enter: zoom into a folder / open a file
    Enter,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Batch(u64, Batch),
    JobDone(u64, Result<ScanEnd, String>),
    Probed(Vec<Probe>),
    Rescan,
    Stop,
    Mode(Mode),
    Focus(u32),
    ZoomOut,
    Tab(Tab),
    SortBy(Sort),
    Toggle(u32),
    RowPress(u32),
    ExtPress(String),
    LargestPress(u32),
    Scrolled(Tab, Vp),
    Mods(Modifiers),
    MapClick(u32),
    MapDouble(u32),
    MapMenu(u32, Point),
    MenuClose,
    Act(Act),
}

struct Job {
    map: Vec<u32>,
    attach: u32,
    full: bool,
    handle: task::Handle,
    cancel: Arc<AtomicBool>,
}

struct LayoutCache {
    key: (u32, u32, u32, Mode),
    version: u64,
    at: Instant,
    rects: Arc<Vec<LRect>>,
}

pub struct State {
    root: PathBuf,
    tree: Tree,
    focus: u32,
    mode: Mode,
    /// bumped on every tree mutation (layout/overlay caches key on it)
    version: u64,

    jobs: HashMap<u64, Job>,
    next_job: u64,
    scanning: bool,
    stopped: bool,
    started: Instant,
    elapsed: Option<Duration>,
    progress: Progress,
    error: Option<String>,
    root_dev: Option<u64>,
    space: Option<(u64, u64)>,
    /// fs changes that arrived mid-scan (applied when it finishes)
    pending: Vec<PathBuf>,

    tab: Tab,
    vps: [Vp; 3],
    sort: Sort,
    expanded: HashSet<u32>,
    rows: Vec<(u32, u16)>,
    exts: Vec<ExtStat>,
    largest: Vec<u32>,
    aggr_for: Option<(u32, Mode, u64)>,
    derived_at: Instant,
    aggr_at: Instant,

    sel: BTreeSet<u32>,
    anchor: Option<u32>,
    cursor: Option<u32>,
    mods: Modifiers,
    last_row_click: Option<(Instant, u32)>,
    highlight_ext: Option<String>,
    menu: Option<(u32, Point)>,

    layout: RefCell<Option<LayoutCache>>,
    base: canvas::Cache,
    overlay: canvas::Cache,
}

impl Drop for State {
    fn drop(&mut self) {
        self.stop_jobs(|_| true);
    }
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let now = Instant::now();
        let mut s = Self {
            tree: Tree::new(o.root.clone()),
            root: o.root,
            focus: 0,
            mode: Mode::Alloc,
            version: 0,
            jobs: HashMap::new(),
            next_job: 0,
            scanning: false,
            stopped: false,
            started: now,
            elapsed: None,
            progress: Progress::default(),
            error: None,
            root_dev: None,
            space: None,
            pending: Vec::new(),
            tab: Tab::Tree,
            vps: [Vp::default(); 3],
            sort: Sort::Size,
            expanded: HashSet::new(),
            rows: Vec::new(),
            exts: Vec::new(),
            largest: Vec::new(),
            aggr_for: None,
            derived_at: now,
            aggr_at: now,
            sel: BTreeSet::new(),
            anchor: None,
            cursor: None,
            mods: Modifiers::default(),
            last_row_click: None,
            highlight_ext: None,
            menu: None,
            layout: RefCell::new(None),
            base: canvas::Cache::new(),
            overlay: canvas::Cache::new(),
        };
        let t = s.rescan();
        (s, t)
    }

    // ------------------------------------------------------------ scanning

    fn stop_jobs(&mut self, which: impl Fn(&Job) -> bool) {
        self.jobs.retain(|_, j| {
            if which(j) {
                j.cancel.store(true, Ordering::Relaxed);
                j.handle.abort();
                false
            } else {
                true
            }
        });
    }

    fn rescan(&mut self) -> Task<Msg> {
        self.stop_jobs(|_| true);
        self.tree = Tree::new(self.root.clone());
        self.focus = 0;
        self.version += 1;
        self.sel.clear();
        self.anchor = None;
        self.cursor = None;
        self.menu = None;
        self.highlight_ext = None;
        self.expanded = HashSet::from([0]);
        self.rows.clear();
        self.exts.clear();
        self.largest.clear();
        self.aggr_for = None;
        self.scanning = true;
        self.stopped = false;
        self.started = Instant::now();
        self.elapsed = None;
        self.progress = Progress::default();
        self.error = None;
        self.pending.clear();
        self.overlay.clear();
        let name = self.root.display().to_string().into_boxed_str();
        self.start_job(self.root.clone(), name, NONE, true)
    }

    fn start_job(&mut self, path: PathBuf, name: Box<str>, attach: u32, full: bool) -> Task<Msg> {
        let id = self.next_job;
        self.next_job += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let dev = if full { None } else { self.root_dev };
        let stream = iced::stream::channel(8, move |mut out| async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let h = tokio::task::spawn_blocking(move || {
                scan::scan(&path, name, dev, &c, |b| tx.send(b).is_ok())
            });
            while let Some(b) = rx.recv().await {
                if out.send(Msg::Batch(id, b)).await.is_err() {
                    return;
                }
            }
            let end = h.await.map_err(|e| e.to_string()).and_then(|r| r);
            let _ = out.send(Msg::JobDone(id, end)).await;
        });
        let (t, handle) = Task::run(stream, |m| m).abortable();
        self.jobs.insert(
            id,
            Job {
                map: Vec::new(),
                attach,
                full,
                handle,
                cancel,
            },
        );
        t
    }

    // ------------------------------------------------------------- derived

    /// Rebuild the visible tree rows and (if shown) the aggregates.
    /// Throttled while scanning unless `force`.
    fn refresh(&mut self, force: bool) {
        if !force && self.scanning && self.derived_at.elapsed() < DERIVED_EVERY {
            return;
        }
        self.derived_at = Instant::now();
        self.rebuild_rows();
        let key = (self.focus, self.mode, self.version);
        if self.tab != Tab::Tree
            && self.aggr_for != Some(key)
            && (force || !self.scanning || self.aggr_at.elapsed() >= AGGR_EVERY)
            && self.tree.live(self.focus)
        {
            self.aggr_at = Instant::now();
            self.aggr_for = Some(key);
            self.exts = self.tree.ext_stats(self.focus, self.mode);
            self.largest = self.tree.largest(self.focus, TOP_N, self.mode);
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        if self.tree.root().is_none() {
            return;
        }
        let mut stack = vec![(0u32, 0u16)];
        while let Some((id, d)) = stack.pop() {
            self.rows.push((id, d));
            if self.expanded.contains(&id) {
                let kids = self.tree.sorted_children(id, self.sort, self.mode);
                stack.extend(kids.into_iter().rev().map(|c| (c, d + 1)));
            }
        }
    }

    /// Layout for the current focus/size, recomputed only when the key or
    /// version changed (and at most 1×/s for version bumps mid-scan).
    /// The bool says whether it was recomputed.
    fn layout(&self, size: Size) -> (Arc<Vec<LRect>>, bool) {
        let key = (
            self.focus,
            size.width.round() as u32,
            size.height.round() as u32,
            self.mode,
        );
        let mut c = self.layout.borrow_mut();
        if let Some(lc) = c.as_ref() {
            if lc.key == key
                && (lc.version == self.version
                    || (self.scanning && lc.at.elapsed() < Duration::from_secs(1)))
            {
                return (lc.rects.clone(), false);
            }
        }
        let rects = Arc::new(treemap::layout(&self.tree, self.focus, size, self.mode));
        *c = Some(LayoutCache {
            key,
            version: self.version,
            at: Instant::now(),
            rects: rects.clone(),
        });
        (rects, true)
    }

    /// Drop references to removed nodes after a tree mutation.
    fn prune(&mut self) {
        let t = &self.tree;
        self.sel.retain(|&i| t.live(i));
        if self.anchor.is_some_and(|a| !t.live(a)) {
            self.anchor = None;
        }
        if self.cursor.is_some_and(|a| !t.live(a)) {
            self.cursor = self.sel.iter().next().copied();
        }
        if self.menu.is_some_and(|(n, _)| !t.live(n)) {
            self.menu = None;
        }
        while self.focus != 0 && !t.live(self.focus) {
            self.focus = t.node(self.focus).parent;
        }
        self.expanded.retain(|&i| t.live(i));
        self.overlay.clear();
    }

    // ----------------------------------------------------------- selection

    fn select(&mut self, id: u32, order: &[u32]) {
        if self.mods.control() || self.mods.command() {
            if !self.sel.remove(&id) {
                self.sel.insert(id);
            }
            self.anchor = Some(id);
        } else if self.mods.shift() {
            let a = self.anchor.and_then(|a| order.iter().position(|&x| x == a));
            let b = order.iter().position(|&x| x == id);
            match (a, b) {
                (Some(a), Some(b)) => {
                    self.sel = order[a.min(b)..=a.max(b)].iter().copied().collect();
                }
                _ => {
                    self.sel = BTreeSet::from([id]);
                    self.anchor = Some(id);
                }
            }
        } else {
            self.sel = BTreeSet::from([id]);
            self.anchor = Some(id);
        }
        self.cursor = Some(id);
        self.overlay.clear();
    }

    fn row_ids(&self) -> Vec<u32> {
        self.rows.iter().map(|r| r.0).collect()
    }

    /// Expand the tree down to `id` and scroll its row into view.
    fn reveal(&mut self, id: u32) -> Task<Msg> {
        let chain = self.tree.chain(id);
        let mut changed = false;
        for &a in &chain[..chain.len().saturating_sub(1)] {
            changed |= self.expanded.insert(a);
        }
        if changed {
            self.rebuild_rows();
        }
        if self.tab != Tab::Tree {
            return Task::none();
        }
        match self.rows.iter().position(|r| r.0 == id) {
            Some(i) => self.scroll_to(Tab::Tree, i, ROW_H),
            None => Task::none(),
        }
    }

    fn scroll_to(&self, tab: Tab, i: usize, h: f32) -> Task<Msg> {
        match self.vps[tab.idx()].reveal(i, h) {
            Some(y) => scrollable::scroll_to(
                scrollable::Id::new(SCROLL_IDS[tab.idx()]),
                scrollable::AbsoluteOffset { x: 0.0, y },
            ),
            None => Task::none(),
        }
    }

    fn zoom(&mut self, id: u32) -> Task<Msg> {
        if !self.tree.live(id) || !self.tree.node(id).is_dir() {
            return Task::none();
        }
        self.focus = id;
        self.menu = None;
        self.overlay.clear();
        self.expanded.insert(id);
        let t = self.reveal(id);
        self.refresh(true);
        t
    }

    /// Selected nodes as paths for an action: never the analyzed root or a
    /// mount point, and not children of something already selected.
    fn targets(&self) -> Vec<PathBuf> {
        let t = &self.tree;
        self.sel
            .iter()
            .copied()
            .filter(|&i| i != 0 && t.live(i) && t.node(i).kind != NodeKind::OtherFs)
            .filter(|&i| {
                let p = t.node(i).parent;
                !self
                    .sel
                    .iter()
                    .any(|&o| o != i && p != NONE && t.is_ancestor(o, p))
            })
            .map(|i| t.path_of(i))
            .collect()
    }

    fn primary(&self) -> Option<u32> {
        self.cursor
            .filter(|c| self.sel.contains(c))
            .or_else(|| self.sel.iter().next().copied())
            .filter(|&i| self.tree.live(i))
    }

    fn act(&mut self, a: Act) -> (Task<Msg>, Option<Event>) {
        self.menu = None;
        let none = (Task::none(), None);
        match a {
            Act::Trash | Act::Delete => {
                let t = self.targets();
                if t.is_empty() {
                    return none;
                }
                let ev = if a == Act::Trash {
                    Event::Trash(t)
                } else {
                    Event::Delete(t)
                };
                (Task::none(), Some(ev))
            }
            Act::Reveal => match self.primary() {
                Some(i) => (Task::none(), Some(Event::Reveal(self.tree.path_of(i)))),
                None => none,
            },
            Act::Open => match self.primary() {
                Some(i) if self.tree.node(i).file_kind().is_some() => {
                    (Task::none(), Some(Event::OpenFile(self.tree.path_of(i))))
                }
                _ => none,
            },
            Act::ZoomIn => match self.primary() {
                Some(i) => (self.zoom(i), None),
                None => none,
            },
            Act::Enter => match self.primary() {
                Some(i) if self.tree.node(i).is_dir() => (self.zoom(i), None),
                Some(_) => self.act(Act::Open),
                None => none,
            },
        }
    }

    fn move_cursor(&mut self, delta: i64) -> Task<Msg> {
        let (order, h) = match self.tab {
            Tab::Tree => (self.row_ids(), ROW_H),
            Tab::Largest => (self.largest.clone(), BIG_ROW_H),
            Tab::Types => {
                if self.exts.is_empty() {
                    return Task::none();
                }
                let cur = self
                    .highlight_ext
                    .as_ref()
                    .and_then(|e| self.exts.iter().position(|x| &x.ext == e));
                let i = match cur {
                    Some(i) => (i as i64 + delta).clamp(0, self.exts.len() as i64 - 1) as usize,
                    None => 0,
                };
                self.highlight_ext = Some(self.exts[i].ext.clone());
                self.overlay.clear();
                return self.scroll_to(Tab::Types, i, ROW_H);
            }
        };
        if order.is_empty() {
            return Task::none();
        }
        let cur = self.cursor.and_then(|c| order.iter().position(|&x| x == c));
        let i = match cur {
            Some(i) => (i as i64 + delta).clamp(0, order.len() as i64 - 1) as usize,
            None => 0,
        };
        let id = order[i];
        // arrows move (shift extends); ctrl doesn't toggle here
        let saved = self.mods;
        self.mods = if saved.shift() {
            Modifiers::SHIFT
        } else {
            Modifiers::empty()
        };
        self.select(id, &order);
        self.mods = saved;
        self.scroll_to(self.tab, i, h)
    }

    // -------------------------------------------------------------- update

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let none = (Task::none(), None);
        match msg {
            Msg::Batch(id, mut b) => {
                let Some(job) = self.jobs.get_mut(&id) else {
                    return none;
                };
                if job.full {
                    self.progress = std::mem::take(&mut b.progress);
                }
                self.tree.apply(&mut job.map, job.attach, b);
                self.version += 1;
                self.refresh(false);
                none
            }
            Msg::JobDone(id, r) => {
                let Some(job) = self.jobs.remove(&id) else {
                    return none;
                };
                self.version += 1;
                let mut ev = None;
                if job.full {
                    self.scanning = false;
                    match r {
                        Ok(end) => {
                            self.root_dev = Some(end.root_dev);
                            self.space = end.space;
                            self.elapsed = Some(end.elapsed);
                            self.stopped = end.cancelled;
                            let r = self.tree.root().map(|n| n.alloc).unwrap_or(0);
                            ev = Some(Event::Status(format!(
                                "Disk usage: {} in {:.1} s",
                                human_size(r),
                                end.elapsed.as_secs_f64()
                            )));
                        }
                        Err(e) => {
                            self.elapsed = Some(self.started.elapsed());
                            ev = Some(Event::Status(format!("Disk usage: {e}")));
                            self.error = Some(e);
                        }
                    }
                }
                self.refresh(true);
                let pending = std::mem::take(&mut self.pending);
                let t = if job.full && !pending.is_empty() {
                    self.fs_changed(&pending)
                } else {
                    Task::none()
                };
                (t, ev)
            }
            Msg::Probed(probes) => {
                let subs = self.tree.apply_probe(&probes);
                self.version += 1;
                self.prune();
                self.refresh(true);
                let tasks: Vec<_> = subs
                    .into_iter()
                    .map(|s| self.start_job(s.path, s.name, s.attach, false))
                    .collect();
                (Task::batch(tasks), None)
            }
            Msg::Rescan => (self.rescan(), None),
            Msg::Stop => {
                self.stop_jobs(|j| j.full);
                if self.scanning {
                    self.scanning = false;
                    self.stopped = true;
                    self.elapsed = Some(self.started.elapsed());
                    self.version += 1;
                    self.refresh(true);
                }
                none
            }
            Msg::Mode(m) => {
                self.mode = m;
                self.refresh(true);
                none
            }
            Msg::Focus(id) => (self.zoom(id), None),
            Msg::ZoomOut => {
                if self.focus != 0 && self.tree.live(self.focus) {
                    let p = self.tree.node(self.focus).parent;
                    return (self.zoom(p), None);
                }
                none
            }
            Msg::Tab(t) => {
                self.tab = t;
                self.refresh(true);
                none
            }
            Msg::SortBy(s) => {
                self.sort = s;
                self.rebuild_rows();
                none
            }
            Msg::Toggle(id) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                }
                self.rebuild_rows();
                none
            }
            Msg::RowPress(id) => {
                let double = !self.mods.shift()
                    && !self.mods.control()
                    && self
                        .last_row_click
                        .is_some_and(|(t, n)| n == id && t.elapsed() < DOUBLE_CLICK);
                self.last_row_click = (!double).then(|| (Instant::now(), id));
                let order = self.row_ids();
                self.select(id, &order);
                if double {
                    return self.act(Act::Enter);
                }
                none
            }
            Msg::LargestPress(id) => {
                let order = self.largest.clone();
                self.select(id, &order);
                none
            }
            Msg::ExtPress(e) => {
                self.highlight_ext = if self.highlight_ext.as_ref() == Some(&e) {
                    None
                } else {
                    Some(e)
                };
                self.overlay.clear();
                none
            }
            Msg::Scrolled(t, vp) => {
                self.vps[t.idx()] = vp;
                none
            }
            Msg::Mods(m) => {
                self.mods = m;
                none
            }
            Msg::MapClick(id) => {
                self.menu = None;
                // treemap clicks have no display order: shift acts like ctrl
                let order = vec![id];
                if self.mods.shift() && !self.mods.control() {
                    self.sel.insert(id);
                    self.cursor = Some(id);
                    self.overlay.clear();
                } else {
                    self.select(id, &order);
                }
                (self.reveal(id), None)
            }
            Msg::MapDouble(id) => {
                let chain = self.tree.chain(id);
                let target = chain
                    .iter()
                    .position(|&c| c == self.focus)
                    .and_then(|i| chain.get(i + 1))
                    .copied();
                match target {
                    Some(t) if self.tree.node(t).is_dir() => (self.zoom(t), None),
                    _ => self.act(Act::Open),
                }
            }
            Msg::MapMenu(id, p) => {
                if !self.sel.contains(&id) {
                    self.sel = BTreeSet::from([id]);
                    self.anchor = Some(id);
                }
                self.cursor = Some(id);
                self.menu = Some((id, p));
                self.overlay.clear();
                (self.reveal(id), None)
            }
            Msg::MenuClose => {
                self.menu = None;
                none
            }
            Msg::Act(a) => self.act(a),
        }
    }

    /// Paths were trashed/deleted/moved by a finished job (or changed on
    /// disk): drop them from the tree / re-scan their parents so the sizes
    /// update without a full rescan.
    pub fn fs_changed(&mut self, paths: &[PathBuf]) -> Task<Msg> {
        let paths: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.starts_with(&self.root))
            .cloned()
            .collect();
        if paths.is_empty() {
            return Task::none();
        }
        if self.scanning {
            self.pending.extend(paths);
            return Task::none();
        }
        let (root, dev) = (self.root.clone(), self.root_dev);
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    use scan::StatExt;
                    let dev = dev
                        .or_else(|| std::fs::symlink_metadata(&root).ok().map(|m| m.dev()))
                        .unwrap_or(0);
                    scan::probe(&paths, dev)
                })
                .await
                .unwrap_or_default()
            },
            Msg::Probed,
        )
    }

    /// Keys while the viewer has focus (None = not consumed).
    pub fn key(&mut self, key: &Key, mods: Modifiers) -> Option<Task<Msg>> {
        self.mods = mods;
        let Key::Named(k) = key else {
            return None;
        };
        match k {
            Named::Delete => Some(Task::done(Msg::Act(if mods.shift() {
                Act::Delete
            } else {
                Act::Trash
            }))),
            Named::Enter => Some(Task::done(Msg::Act(Act::Enter))),
            Named::Backspace => Some(Task::done(Msg::ZoomOut)),
            Named::ArrowUp => Some(self.move_cursor(-1)),
            Named::ArrowDown => Some(self.move_cursor(1)),
            Named::Escape if self.menu.is_some() || self.highlight_ext.is_some() => {
                self.menu = None;
                self.highlight_ext = None;
                self.overlay.clear();
                Some(Task::none())
            }
            _ => None,
        }
    }

    // ---------------------------------------------------------------- view

    pub fn view(&self) -> Element<'_, Msg> {
        // side by side only when the list gets enough width for names and
        // columns; otherwise treemap on top, list underneath at full width
        let body = iced::widget::responsive(move |size| -> Element<'_, Msg> {
            if size.width >= 1100.0 {
                row![
                    container(self.panel())
                        .width(Length::FillPortion(2))
                        .height(Length::Fill),
                    container(self.map_view())
                        .width(Length::FillPortion(3))
                        .height(Length::Fill),
                ]
                .spacing(8)
                .height(Length::Fill)
                .into()
            } else {
                column![
                    container(self.map_view())
                        .width(Length::Fill)
                        .height(Length::FillPortion(3)),
                    container(self.panel())
                        .width(Length::Fill)
                        .height(Length::FillPortion(2)),
                ]
                .spacing(8)
                .height(Length::Fill)
                .into()
            }
        });
        column![self.header(), self.crumbs(), body]
            .spacing(6)
            .padding(8)
            .into()
    }

    fn header(&self) -> Element<'_, Msg> {
        let mut parts = Vec::new();
        if let Some(e) = &self.error {
            parts.push(format!("Error: {e}"));
        }
        let r = self.tree.root();
        let (files, dirs) = r.map(|n| (n.files, n.dirs)).unwrap_or((0, 0));
        let total = r.map(|n| n.bytes(self.mode)).unwrap_or(0);
        parts.push(human_size(total));
        parts.push(format!("{} files", thousands(files)));
        parts.push(format!("{} folders", thousands(dirs)));
        if self.scanning {
            parts.push(format!(
                "scanning… {} items, {:.0} s",
                thousands(self.progress.items),
                self.started.elapsed().as_secs_f64()
            ));
        } else if let Some(e) = self.elapsed {
            parts.push(format!(
                "{} in {:.1} s",
                if self.stopped { "stopped" } else { "scanned" },
                e.as_secs_f64()
            ));
        }
        if self.tree.unreadable > 0 {
            parts.push(format!("{} unreadable", thousands(self.tree.unreadable)));
        }
        if let Some((free, tot)) = self.space {
            parts.push(format!("{} free of {}", human_size(free), human_size(tot)));
        }
        let stats = text(parts.join(" · ")).size(12).color(theme::MUTED);
        let current = if self.scanning {
            text(self.progress.current.clone())
                .size(11)
                .color(theme::MUTED)
                .wrapping(text::Wrapping::None)
        } else {
            text("")
        };
        let seg = |label: &'static str, m: Mode| {
            button(text(label).size(12))
                .padding([3, 8])
                .style(if self.mode == m {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Msg::Mode(m))
        };
        let scan_btn = if self.scanning {
            button(row![widgets::icon(B::StopFill).size(12), text("Stop").size(12)].spacing(4))
                .padding([3, 8])
                .style(button::secondary)
                .on_press(Msg::Stop)
        } else {
            button(
                row![
                    widgets::icon(B::ArrowClockwise).size(12),
                    text("Rescan").size(12)
                ]
                .spacing(4),
            )
            .padding([3, 8])
            .style(button::secondary)
            .on_press(Msg::Rescan)
        };
        row![
            widgets::icon(B::PieChartFill)
                .size(22)
                .color(iced::color!(0xe0b85c)),
            column![
                text(self.root.display().to_string()).size(15),
                stats,
                current
            ]
            .spacing(1)
            .width(Length::Fill),
            row![
                seg("Size on disk", Mode::Alloc),
                seg("Apparent size", Mode::Apparent)
            ],
            scan_btn,
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center)
        .into()
    }

    fn crumbs(&self) -> Element<'_, Msg> {
        if !self.tree.live(self.focus) {
            return Space::with_height(0).into();
        }
        let chain = self.tree.chain(self.focus);
        let mut r = row![button(widgets::icon(B::ArrowUp).size(12))
            .padding([3, 6])
            .style(button::text)
            .on_press_maybe((self.focus != 0).then_some(Msg::ZoomOut))]
        .spacing(2)
        .align_y(iced::Alignment::Center);
        for (i, &id) in chain.iter().enumerate() {
            if i > 0 {
                r = r.push(text("›").size(12).color(theme::MUTED));
            }
            let n = self.tree.node(id);
            let label = if id == 0 {
                self.root.display().to_string()
            } else {
                n.name.to_string()
            };
            r = r.push(
                button(text(label).size(12))
                    .padding([3, 6])
                    .style(if id == self.focus {
                        button::secondary
                    } else {
                        button::text
                    })
                    .on_press(Msg::Focus(id)),
            );
        }
        let n = self.tree.node(self.focus);
        r = r.push(Space::with_width(Length::Fill)).push(
            text(format!(
                "{} · {} files",
                human_size(n.bytes(self.mode)),
                thousands(n.files)
            ))
            .size(12)
            .color(theme::MUTED),
        );
        r.into()
    }

    fn map_view(&self) -> Element<'_, Msg> {
        if self.tree.root().is_none() {
            let msg = match &self.error {
                Some(e) => format!("Can't scan: {e}"),
                None => "Scanning…".into(),
            };
            return container(text(msg).color(theme::MUTED))
                .center(Length::Fill)
                .style(theme::panel)
                .into();
        }
        let map = canvas(treemap::Map { st: self })
            .width(Length::Fill)
            .height(Length::Fill);
        let Some((id, at)) = self.menu else {
            return map.into();
        };
        let area = self
            .layout
            .borrow()
            .as_ref()
            .map(|l| Size::new(l.key.1 as f32, l.key.2 as f32))
            .unwrap_or(Size::new(2000.0, 2000.0));
        let n = self.tree.node(id);
        let item = |icon: B, label: &'static str, m: Option<Msg>, danger: bool| {
            button(row![widgets::icon(icon).size(12), text(label).size(13)].spacing(8))
                .width(Length::Fill)
                .padding([4, 10])
                .style(if danger { button::danger } else { button::text })
                .on_press_maybe(m)
        };
        let multi = self.sel.len() > 1;
        let menu = column![
            text(if multi {
                format!("{} selected", self.sel.len())
            } else {
                n.name.to_string()
            })
            .size(11)
            .color(theme::MUTED),
            item(
                B::ZoomIn,
                "Zoom in",
                n.is_dir().then_some(Msg::Act(Act::ZoomIn)),
                false
            ),
            item(
                B::BoxArrowUpRight,
                "Open",
                n.file_kind().is_some().then_some(Msg::Act(Act::Open)),
                false
            ),
            item(
                B::FoldertwoOpen,
                "Show in files pane",
                Some(Msg::Act(Act::Reveal)),
                false
            ),
            item(
                B::Trash,
                "Move to trash",
                (id != 0).then_some(Msg::Act(Act::Trash)),
                false
            ),
            item(
                B::XOctagon,
                "Delete permanently…",
                (id != 0).then_some(Msg::Act(Act::Delete)),
                true
            ),
        ]
        .spacing(2)
        .width(200);
        let menu = container(menu)
            .padding(6)
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.weak.color.into()),
                border: iced::Border {
                    radius: 8.0.into(),
                    width: 1.0,
                    color: t.extended_palette().background.strong.color,
                },
                shadow: iced::Shadow {
                    color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                    offset: iced::Vector::new(0.0, 4.0),
                    blur_radius: 16.0,
                },
                ..Default::default()
            });
        let backdrop = mouse_area(Space::new(Length::Fill, Length::Fill))
            .on_press(Msg::MenuClose)
            .on_right_press(Msg::MenuClose);
        let placed = container(opaque(menu)).padding(iced::Padding {
            top: at.y.min(area.height - 190.0).max(0.0),
            left: at.x.min(area.width - 215.0).max(0.0),
            ..Default::default()
        });
        stack![map, backdrop, placed].into()
    }

    fn panel(&self) -> Element<'_, Msg> {
        let tab = |label: &'static str, t: Tab| {
            button(text(label).size(12))
                .padding([4, 10])
                .style(if self.tab == t {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Msg::Tab(t))
        };
        let tabs = row![
            tab("Tree", Tab::Tree),
            tab("Types", Tab::Types),
            tab("Largest", Tab::Largest)
        ]
        .spacing(4);
        let content = match self.tab {
            Tab::Tree => self.tree_view(),
            Tab::Types => self.types_view(),
            Tab::Largest => self.largest_view(),
        };
        container(column![tabs, content, self.actions()].spacing(6))
            .padding(8)
            .height(Length::Fill)
            .style(theme::panel)
            .into()
    }

    fn list<'a>(&'a self, t: Tab, body: Element<'a, Msg>) -> Element<'a, Msg> {
        scrollable(body)
            .id(scrollable::Id::new(SCROLL_IDS[t.idx()]))
            .on_scroll(move |v| Msg::Scrolled(t, Vp::from(v)))
            .height(Length::Fill)
            .into()
    }

    fn tree_view(&self) -> Element<'_, Msg> {
        let head = |label: &'static str, s: Sort, w: Length| {
            let mark = if self.sort == s { " ▾" } else { "" };
            button(text(format!("{label}{mark}")).size(11).color(theme::MUTED))
                .padding([2, 4])
                .width(w)
                .style(button::text)
                .on_press(Msg::SortBy(s))
        };
        let header = row![
            head("Name", Sort::Name, Length::Fill),
            head("Size", Sort::Size, Length::Fixed(140.0)),
            head("Files", Sort::Files, Length::Fixed(60.0)),
            head("Modified", Sort::Modified, Length::Fixed(84.0)),
        ]
        .spacing(4);
        let list = widgets::virtual_column(self.rows.len(), ROW_H, self.vps[0], |i| {
            let (id, depth) = self.rows[i];
            self.tree_row(id, depth)
        });
        column![header, self.list(Tab::Tree, list)]
            .spacing(2)
            .into()
    }

    fn tree_row(&self, id: u32, depth: u16) -> Element<'_, Msg> {
        let t = &self.tree;
        let n = t.node(id);
        let b = n.bytes(self.mode);
        let parent = t
            .live(n.parent)
            .then(|| t.node(n.parent).bytes(self.mode))
            .unwrap_or(b);
        let pct = if parent > 0 {
            b as f32 * 100.0 / parent as f32
        } else {
            0.0
        };
        let dimmed = n.kind == NodeKind::OtherFs || n.unreadable();
        let chevron: Element<'_, Msg> = if n.is_dir() && !n.children.is_empty() {
            let open = self.expanded.contains(&id);
            button(
                widgets::icon(if open {
                    B::ChevronDown
                } else {
                    B::ChevronRight
                })
                .size(10)
                .color(theme::MUTED),
            )
            .padding([2, 3])
            .style(button::text)
            .on_press(Msg::Toggle(id))
            .into()
        } else {
            Space::with_width(16).into()
        };
        let icon = match n.kind {
            NodeKind::File(k) => widgets::kind_icon(k),
            NodeKind::Dir => widgets::kind_icon(Kind::Folder),
            NodeKind::OtherFs => widgets::icon(B::Hdd).color(theme::MUTED),
        }
        .size(12)
        .width(18);
        let mut name = n.name.to_string();
        if n.kind == NodeKind::OtherFs {
            name += "  (other filesystem)";
        } else if n.unreadable() {
            name += "  (unreadable)";
        }
        let name = text(name)
            .size(12)
            .wrapping(text::Wrapping::None)
            .width(Length::Fill);
        let name = if dimmed {
            name.color(theme::MUTED)
        } else {
            name
        };
        let size_txt = if n.kind == NodeKind::OtherFs {
            String::new()
        } else {
            human_size(b)
        };
        let files = if n.file_kind().is_some() {
            String::new()
        } else {
            thousands(n.files)
        };
        let content = row![
            Space::with_width(depth as f32 * 12.0),
            chevron,
            icon,
            name,
            text(size_txt)
                .size(11)
                .width(64)
                .align_x(iced::Alignment::End),
            progress_bar(0.0..=100.0, pct).height(6).width(40),
            text(format!("{pct:.0}%"))
                .size(10)
                .color(theme::MUTED)
                .width(32),
            text(files)
                .size(11)
                .color(theme::MUTED)
                .width(60)
                .align_x(iced::Alignment::End),
            text(widgets_ago(n.mtime))
                .size(11)
                .color(theme::MUTED)
                .width(84),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center);
        self.selectable(content.into(), id, Msg::RowPress(id))
    }

    fn selectable<'a>(&self, content: Element<'a, Msg>, id: u32, press: Msg) -> Element<'a, Msg> {
        let selected = self.sel.contains(&id);
        let focused = self.cursor == Some(id);
        mouse_area(
            container(content)
                .padding([1, 4])
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(iced::Alignment::Center)
                .style(move |t: &Theme| row_style(t, selected, focused)),
        )
        .on_press(press)
        .into()
    }

    fn types_view(&self) -> Element<'_, Msg> {
        let total: u64 = self.exts.iter().map(|e| e.bytes).sum();
        let header = row![
            text("Type")
                .size(11)
                .color(theme::MUTED)
                .width(Length::Fill),
            text("Files").size(11).color(theme::MUTED).width(60),
            text("Total").size(11).color(theme::MUTED).width(70),
            text("%").size(11).color(theme::MUTED).width(44),
        ]
        .spacing(4)
        .padding([0, 4]);
        let list = widgets::virtual_column(self.exts.len(), ROW_H, self.vps[1], |i| {
            let e = &self.exts[i];
            let c = treemap::kind_color(e.kind);
            let swatch = container(Space::new(12, 12)).style(move |_: &Theme| container::Style {
                background: Some(c.into()),
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            let label = if e.ext.is_empty() {
                "(no extension)".to_string()
            } else {
                format!(".{}", e.ext)
            };
            let pct = if total > 0 {
                e.bytes as f64 * 100.0 / total as f64
            } else {
                0.0
            };
            let on = self.highlight_ext.as_ref() == Some(&e.ext);
            let content = row![
                swatch,
                text(label).size(12).width(Length::Fill),
                text(thousands(e.count))
                    .size(11)
                    .color(theme::MUTED)
                    .width(60),
                text(human_size(e.bytes)).size(11).width(70),
                text(format!("{pct:.1}%"))
                    .size(11)
                    .color(theme::MUTED)
                    .width(44),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);
            mouse_area(
                container(content)
                    .padding([1, 4])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_y(iced::Alignment::Center)
                    .style(move |t: &Theme| row_style(t, on, false)),
            )
            .on_press(Msg::ExtPress(e.ext.clone()))
            .into()
        });
        let hint = text(if self.highlight_ext.is_some() {
            "Highlighted in the treemap · click again (or Esc) to clear"
        } else {
            "Click a type to highlight its files in the treemap"
        })
        .size(11)
        .color(theme::MUTED);
        column![hint, header, self.list(Tab::Types, list)]
            .spacing(2)
            .into()
    }

    fn largest_view(&self) -> Element<'_, Msg> {
        let list = widgets::virtual_column(self.largest.len(), BIG_ROW_H, self.vps[2], |i| {
            let id = self.largest[i];
            let n = self.tree.node(id);
            let parent = self.tree.rel_path(n.parent);
            let content = row![
                widgets::kind_icon(n.file_kind().unwrap_or(Kind::Binary))
                    .size(13)
                    .width(20),
                column![
                    text(n.name.to_string())
                        .size(12)
                        .wrapping(text::Wrapping::None),
                    text(parent)
                        .size(10)
                        .color(theme::MUTED)
                        .wrapping(text::Wrapping::None),
                ]
                .width(Length::Fill),
                text(human_size(n.bytes(self.mode)))
                    .size(12)
                    .width(72)
                    .align_x(iced::Alignment::End),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);
            self.selectable(content.into(), id, Msg::LargestPress(id))
        });
        let head = text(format!(
            "Top {} files under {}",
            self.largest.len(),
            if self.focus == 0 {
                "the analyzed folder".to_string()
            } else {
                self.tree.rel_path(self.focus)
            }
        ))
        .size(11)
        .color(theme::MUTED);
        column![head, self.list(Tab::Largest, list)]
            .spacing(2)
            .into()
    }

    fn actions(&self) -> Element<'_, Msg> {
        let live: Vec<u32> = self
            .sel
            .iter()
            .copied()
            .filter(|&i| self.tree.live(i))
            .collect();
        let bytes: u64 = live
            .iter()
            .map(|&i| self.tree.node(i).bytes(self.mode))
            .sum();
        let summary = match live.len() {
            0 => "Nothing selected".to_string(),
            1 => format!("{} · {}", self.tree.node(live[0]).name, human_size(bytes)),
            n => format!("{n} selected · {}", human_size(bytes)),
        };
        let any = !self.targets().is_empty();
        let file = self
            .primary()
            .is_some_and(|i| self.tree.node(i).file_kind().is_some());
        let btn = |icon: B, label: &'static str, m: Msg, on: bool, danger: bool| {
            button(row![widgets::icon(icon).size(11), text(label).size(11)].spacing(4))
                .padding([3, 7])
                .style(if danger {
                    button::danger
                } else {
                    button::secondary
                })
                .on_press_maybe(on.then_some(m))
        };
        column![
            text(summary)
                .size(11)
                .color(theme::MUTED)
                .wrapping(text::Wrapping::None),
            row![
                btn(B::BoxArrowUpRight, "Open", Msg::Act(Act::Open), file, false),
                btn(
                    B::FoldertwoOpen,
                    "Show in files",
                    Msg::Act(Act::Reveal),
                    !live.is_empty(),
                    false
                ),
                btn(B::Trash, "Trash", Msg::Act(Act::Trash), any, false),
                btn(B::XOctagon, "Delete…", Msg::Act(Act::Delete), any, true),
            ]
            .spacing(4)
            // wraps instead of cutting buttons off in a narrow panel
            .wrap(),
        ]
        .spacing(4)
        .into()
    }
}

fn row_style(t: &Theme, selected: bool, focused: bool) -> container::Style {
    let p = t.extended_palette();
    container::Style {
        background: selected.then(|| p.primary.weak.color.scale_alpha(0.45).into()),
        text_color: Some(p.background.base.text),
        border: iced::Border {
            radius: 4.0.into(),
            width: if focused { 1.0 } else { 0.0 },
            color: p.primary.base.color.scale_alpha(0.7),
        },
        ..Default::default()
    }
}

fn widgets_ago(mtime: i64) -> String {
    let t = if mtime >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_secs(mtime as u64)
    } else {
        SystemTime::UNIX_EPOCH
    };
    crate::fsops::ago(Some(t))
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
