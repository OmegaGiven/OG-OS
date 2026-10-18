//! Live search. Two passes:
//!  1. instant — matches among tree nodes already loaded (`Tree::loaded_matches`)
//!  2. background — a cancellable walk of the search roots that streams more
//!     hits in batches while you keep working.
//!
//! Results are APPEND-ONLY: new hits always go to the end, never re-sorted
//! or inserted above, so the list never shifts under a user who has
//! scrolled. A new query cancels the old walk (abort handle + the walker
//! notices its channel closed).

use crate::filetype::Kind;
use crate::theme;
use crate::widgets::{self, ItemAct, Selection, Vp};
use iced::futures::SinkExt;
use iced::task;
use iced::widget::{column, row, scrollable, text};
use iced::{Element, Length, Task};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_HITS: usize = 10_000;
pub const ROW_H: f32 = 40.0;
pub const SCROLL_ID: &str = "search-scroll";
/// text files bigger than this aren't searched inside
const CONTENT_MAX: u64 = 4 * 1024 * 1024;
const SKIP_DIRS: [&str; 6] = [
    "node_modules",
    "target",
    ".git",
    ".cache",
    "__pycache__",
    ".local/share/Trash",
];

#[derive(Debug, Clone)]
pub struct Hit {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    parent: String,
}

impl Hit {
    pub fn new(path: PathBuf, name: String, kind: Kind) -> Self {
        let parent = path
            .parent()
            .map(|p| {
                let home = crate::config::home();
                match p.strip_prefix(&home) {
                    Ok(rel) => format!("~/{}", rel.display()),
                    Err(_) => p.display().to_string(),
                }
            })
            .unwrap_or_default();
        Self {
            path,
            name,
            kind,
            parent,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Batch(u64, Vec<Hit>, usize),
    Done(u64),
    Item(ItemAct),
    Scrolled(Vp),
}

pub enum Event {
    Item(ItemAct),
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    gen: u64,
    hits: Vec<Hit>,
    seen: HashSet<PathBuf>,
    // indices into `hits` passing the kind filter; append-only like `hits`
    visible: Vec<usize>,
    filter: Option<Kind>,
    scanned: usize,
    running: bool,
    abort: Option<task::Handle>,
    vp: Vp,
    /// also match file contents (text files only)
    pub content: bool,
}

impl Search {
    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn set_filter(&mut self, f: Option<Kind>) {
        self.filter = f;
        // a filter change is a deliberate re-scope, so a rebuild is fine here
        self.visible = (0..self.hits.len())
            .filter(|&i| self.passes(&self.hits[i]))
            .collect();
    }

    fn passes(&self, h: &Hit) -> bool {
        self.filter.is_none_or(|k| h.kind == k)
    }

    fn push(&mut self, hits: Vec<Hit>) {
        for h in hits {
            if self.hits.len() >= MAX_HITS || !self.seen.insert(h.path.clone()) {
                continue;
            }
            if self.passes(&h) {
                self.visible.push(self.hits.len());
            }
            self.hits.push(h);
        }
    }

    pub fn start(&mut self, query: String, quick: Vec<Hit>, roots: Vec<PathBuf>) -> Task<Msg> {
        self.vp.offset = 0.0;
        if let Some(h) = self.abort.take() {
            h.abort();
        }
        self.gen += 1;
        self.query = query;
        self.hits.clear();
        self.seen.clear();
        self.visible.clear();
        self.scanned = 0;
        self.running = false;
        if self.query.is_empty() {
            return Task::none();
        }
        self.push(quick);
        self.running = true;
        let (gen, q, content) = (self.gen, self.query.to_lowercase(), self.content);
        let stream = iced::stream::channel(16, move |mut out| async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::task::spawn_blocking(move || walk(roots, q, content, tx));
            while let Some((hits, scanned)) = rx.recv().await {
                if out.send(Msg::Batch(gen, hits, scanned)).await.is_err() {
                    return;
                }
            }
            let _ = out.send(Msg::Done(gen)).await;
        });
        let (task, handle) = Task::run(stream, |m| m).abortable();
        self.abort = Some(handle);
        task
    }

    pub fn update(&mut self, msg: Msg) -> Option<Event> {
        match msg {
            Msg::Batch(g, hits, scanned) if g == self.gen => {
                self.scanned = scanned;
                self.push(hits);
                None
            }
            Msg::Done(g) if g == self.gen => {
                self.running = false;
                None
            }
            Msg::Item(a) => Some(Event::Item(a)),
            Msg::Scrolled(vp) => {
                self.vp = vp;
                None
            }
            Msg::Batch(..) | Msg::Done(..) => None, // stale generation
        }
    }

    /// (path, kind) of the visible hits, in display order.
    pub fn visible_items(&self) -> Vec<(PathBuf, Kind)> {
        self.visible
            .iter()
            .map(|&i| (self.hits[i].path.clone(), self.hits[i].kind))
            .collect()
    }

    pub fn reveal(&self, p: &std::path::Path) -> Option<f32> {
        let i = self.visible.iter().position(|&i| self.hits[i].path == p)?;
        self.vp.reveal(i, ROW_H)
    }

    pub fn view<'a>(&'a self, sel: &'a Selection) -> Element<'a, Msg> {
        let status = format!(
            "{} found{} · {} scanned{}",
            self.visible.len(),
            if self.hits.len() >= MAX_HITS {
                " (limit)"
            } else {
                ""
            },
            self.scanned,
            if self.running {
                " · still searching…"
            } else {
                ""
            },
        );
        // append-only + virtualized: new hits extend the list below without
        // moving what the user is looking at
        let list = widgets::virtual_column(self.visible.len(), ROW_H, self.vp, |n| {
            let h = &self.hits[self.visible[n]];
            let content = column![
                row![
                    widgets::kind_icon(h.kind).width(24).size(13),
                    text(h.name.clone()).size(13)
                ]
                .spacing(6),
                text(h.parent.clone()).size(11).color(theme::MUTED),
            ]
            .spacing(1);
            widgets::item(content, &h.path, h.kind, sel, Msg::Item)
        });
        column![
            text(status).size(12).color(theme::MUTED),
            scrollable(list)
                .id(scrollable::Id::new(SCROLL_ID))
                .on_scroll(|v| Msg::Scrolled(Vp::from(v)))
                .height(Length::Fill)
        ]
        .spacing(6)
        .into()
    }
}

/// Blocking walk; runs on the blocking pool. Sends batches at most every
/// 150 ms (or 64 hits) so the UI isn't redrawn per file. Stops as soon as
/// the receiver is gone (query changed / search cleared).
fn walk(
    roots: Vec<PathBuf>,
    q: String,
    content: bool,
    tx: tokio::sync::mpsc::UnboundedSender<(Vec<Hit>, usize)>,
) {
    let mut stack = roots;
    let mut batch = Vec::new();
    let mut scanned = 0usize;
    let mut last = Instant::now();
    while let Some(dir) = stack.pop() {
        if tx.is_closed() {
            return;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            scanned += 1;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || (cfg!(windows) && crate::platform::is_hidden(&name, e.metadata().ok().as_ref()))
            {
                continue;
            }
            // file_type() doesn't follow symlinks: no loops, no wandering off-tree
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                if !SKIP_DIRS.iter().any(|s| path.ends_with(s)) {
                    stack.push(path.clone());
                }
            }
            let kind = Kind::of(&path, ft.is_dir());
            if name.to_lowercase().contains(&q)
                || (content
                    && ft.is_file()
                    && matches!(kind, Kind::Text | Kind::Table | Kind::Doc)
                    && file_contains(&path, &q))
            {
                batch.push(Hit::new(path, name, kind));
            }
        }
        if batch.len() >= 64 || (!batch.is_empty() && last.elapsed() > Duration::from_millis(150)) {
            if tx.send((std::mem::take(&mut batch), scanned)).is_err() {
                return;
            }
            last = Instant::now();
        }
    }
    let _ = tx.send((batch, scanned));
}

/// Case-insensitive (ASCII) substring search in a small text file.
fn file_contains(path: &std::path::Path, q: &str) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > CONTENT_MAX || q.is_empty() {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    if bytes.iter().take(1024).any(|&b| b == 0) {
        return false; // binary
    }
    let needle = q.as_bytes();
    bytes
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}
