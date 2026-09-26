//! One browsing context: a folder, its listing, selection, history and
//! per-tab view settings. The window holds one or two panes (split view),
//! each pane holds a strip of these.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::filesystem::FileEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode { List, Grid }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy { Name, Size, Modified, Kind }

#[derive(Debug, Clone)]
pub struct RenameState {
    pub path: PathBuf,
    pub text: String,
}

/// Grid card sizes the zoom shortcuts step through (Ctrl+wheel / Ctrl+±).
pub const ZOOM_STEPS: [u16; 6] = [48, 64, 80, 96, 128, 160];
pub const DEFAULT_ZOOM: usize = 1;

/// Type-ahead: letters typed within this window accumulate into one prefix.
const TYPE_AHEAD_WINDOW_MS: u128 = 900;

static NEXT_TAB_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug)]
pub struct Tab {
    /// Stable identity independent of position — closing tab 0 must not
    /// make an in-flight async listing for tab 1 land in the wrong slot.
    pub id: u64,
    pub path: PathBuf,
    pub history: Vec<PathBuf>,
    pub forward: Vec<PathBuf>,
    pub entries: Vec<FileEntry>,
    /// Bumped on every load so a stale async listing is ignored.
    pub load_gen: u64,
    pub loading: bool,
    pub load_error: Option<String>,

    pub selected: HashSet<PathBuf>,
    /// Keyboard focus row (index into `displayed()`), drawn with a focus ring.
    pub cursor: Option<usize>,
    /// Start of a Shift-range selection.
    pub anchor: Option<usize>,

    pub search_query: String,
    pub search_results: Vec<FileEntry>,
    pub search_handle: Option<iced::task::Handle>,
    pub search_gen: u64,
    pub searching: bool,

    pub view_mode: ViewMode,
    pub sort_by: SortBy,
    pub sort_asc: bool,
    pub group_folders: bool,
    pub show_hidden: bool,
    pub zoom: usize,

    pub rename: Option<RenameState>,
    /// `Some` while the breadcrumb bar is swapped for a text field.
    pub path_edit: Option<String>,

    /// Last measured grid geometry, so Up/Down know the row stride.
    pub items_per_row: usize,
    /// Scroll offset request: set after keyboard moves so the view can
    /// scroll the cursor into view (consumed by the view layer).
    pub scroll_to_cursor: bool,

    /// Vertical scroll position and visible viewport height, both in
    /// pixels — the file list uses these to only build widgets for rows
    /// actually on screen (plus a little overscan) instead of every entry
    /// in the folder every frame. `viewport_height` starts as a rough
    /// guess from the window size (see `app.rs`'s window-size handler)
    /// and gets replaced with the real measurement the moment `on_scroll`
    /// fires — the guess only matters for the very first paint.
    pub scroll_offset: f32,
    pub viewport_height: f32,

    type_ahead: String,
    type_ahead_at: Option<Instant>,
    last_click: Option<(PathBuf, Instant)>,
}

impl Tab {
    pub fn new(path: PathBuf, show_hidden: bool, view_mode: ViewMode) -> Self {
        Tab {
            id: NEXT_TAB_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            path,
            history: Vec::new(),
            forward: Vec::new(),
            entries: Vec::new(),
            load_gen: 0,
            loading: true,
            load_error: None,
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            search_query: String::new(),
            search_results: Vec::new(),
            search_handle: None,
            search_gen: 0,
            searching: false,
            view_mode,
            sort_by: SortBy::Name,
            sort_asc: true,
            group_folders: true,
            show_hidden,
            zoom: DEFAULT_ZOOM,
            rename: None,
            path_edit: None,
            items_per_row: 1,
            scroll_to_cursor: false,
            scroll_offset: 0.0,
            viewport_height: 600.0,
            type_ahead: String::new(),
            type_ahead_at: None,
            last_click: None,
        }
    }

    pub fn title(&self) -> String {
        if self.path == Path::new("/") {
            return "/".into();
        }
        if self.path == crate::filesystem::home_dir() {
            return "Home".into();
        }
        if self.path == crate::filesystem::trash_dir().join("files") {
            return "Trash".into();
        }
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.to_string_lossy().to_string())
    }

    pub fn is_trash(&self) -> bool {
        self.path == crate::filesystem::trash_dir().join("files")
    }

    pub fn displayed(&self) -> &[FileEntry] {
        if self.search_query.is_empty() { &self.entries } else { &self.search_results }
    }

    pub fn card_size(&self) -> u16 {
        ZOOM_STEPS[self.zoom.min(ZOOM_STEPS.len() - 1)]
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom + 1).min(ZOOM_STEPS.len() - 1);
    }

    pub fn zoom_out(&mut self) {
        self.zoom = self.zoom.saturating_sub(1);
    }

    // ── Navigation bookkeeping (the actual load is async, driven by app) ──

    /// Records history and switches `path`; caller kicks off the listing.
    pub fn go(&mut self, path: PathBuf) {
        if path == self.path {
            return;
        }
        let old = std::mem::replace(&mut self.path, path);
        self.history.push(old);
        self.forward.clear();
        self.reset_for_new_dir();
    }

    pub fn back(&mut self) -> bool {
        let Some(prev) = self.history.pop() else { return false };
        let cur = std::mem::replace(&mut self.path, prev);
        self.forward.push(cur);
        self.reset_for_new_dir();
        true
    }

    pub fn forward(&mut self) -> bool {
        let Some(next) = self.forward.pop() else { return false };
        let cur = std::mem::replace(&mut self.path, next);
        self.history.push(cur);
        self.reset_for_new_dir();
        true
    }

    fn reset_for_new_dir(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        self.search_query.clear();
        self.search_results.clear();
        self.searching = false;
        if let Some(h) = self.search_handle.take() {
            h.abort();
        }
        self.rename = None;
        self.path_edit = None;
        self.load_error = None;
        self.loading = true;
        self.load_gen += 1;
        self.type_ahead.clear();
        self.scroll_offset = 0.0;
    }

    /// A fresh listing arrived. Keeps selection/cursor where it still makes
    /// sense (refresh after an op) — a brand-new folder starts empty.
    pub fn apply_listing(&mut self, entries: Vec<FileEntry>) {
        self.entries = entries;
        self.loading = false;
        self.sort();
        let existing: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
        self.selected.retain(|p| existing.contains(p));
        if let Some(c) = self.cursor {
            if c >= self.displayed().len() {
                self.cursor = if self.displayed().is_empty() { None } else { Some(self.displayed().len() - 1) };
            }
        }
    }

    pub fn sort(&mut self) {
        let asc = self.sort_asc;
        let group = self.group_folders;
        let by = self.sort_by;
        let cmp = move |a: &FileEntry, b: &FileEntry| {
            let ord = match by {
                SortBy::Name => natural_cmp(&a.name, &b.name),
                SortBy::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Modified => a.modified.cmp(&b.modified).then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Kind => a.mime_type.cmp(&b.mime_type).then_with(|| natural_cmp(&a.name, &b.name)),
            };
            let ord = if asc { ord } else { ord.reverse() };
            if group {
                match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => ord,
                }
            } else {
                ord
            }
        };
        self.entries.sort_by(cmp);
        self.search_results.sort_by(cmp);
    }

    // ── Selection model ────────────────────────────────────────────────────

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.displayed().iter().position(|e| e.path == path)
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        // Keep display order — matters for undo journaling and status text.
        self.displayed().iter().filter(|e| self.selected.contains(&e.path)).map(|e| e.path.clone()).collect()
    }

    pub fn select_only(&mut self, idx: usize) {
        self.selected.clear();
        if let Some(e) = self.displayed().get(idx) {
            self.selected.insert(e.path.clone());
        }
        self.cursor = Some(idx);
        self.anchor = Some(idx);
    }

    pub fn toggle(&mut self, idx: usize) {
        if let Some(e) = self.displayed().get(idx) {
            let p = e.path.clone();
            if !self.selected.remove(&p) {
                self.selected.insert(p);
            }
        }
        self.cursor = Some(idx);
        self.anchor = Some(idx);
    }

    /// Shift-click / Shift+arrow: select everything between the anchor and
    /// `idx` (replacing the previous range unless `additive`).
    pub fn select_range_to(&mut self, idx: usize, additive: bool) {
        let anchor = self.anchor.unwrap_or(idx);
        if !additive {
            self.selected.clear();
        }
        let (lo, hi) = if anchor <= idx { (anchor, idx) } else { (idx, anchor) };
        let paths: Vec<PathBuf> = self.displayed().iter().skip(lo).take(hi - lo + 1).map(|e| e.path.clone()).collect();
        for p in paths {
            self.selected.insert(p);
        }
        self.cursor = Some(idx);
    }

    pub fn select_all(&mut self) {
        self.selected = self.displayed().iter().map(|e| e.path.clone()).collect();
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Mouse click semantics: plain = select only; Ctrl = toggle; Shift =
    /// range. Returns true when this was the second click of a double-click.
    pub fn click(&mut self, idx: usize, ctrl: bool, shift: bool) -> bool {
        let Some(path) = self.displayed().get(idx).map(|e| e.path.clone()) else { return false };
        let now = Instant::now();
        let is_double = !ctrl && !shift
            && self.last_click.as_ref().is_some_and(|(p, t)| *p == path && now.duration_since(*t).as_millis() < 400);
        self.last_click = if is_double { None } else { Some((path, now)) };

        if shift {
            self.select_range_to(idx, ctrl);
        } else if ctrl {
            self.toggle(idx);
        } else if !is_double {
            self.select_only(idx);
        }
        is_double
    }

    /// Keyboard move by `delta` rows/cells. Shift extends the range, Ctrl
    /// moves the cursor without touching the selection.
    pub fn move_cursor(&mut self, delta: isize, shift: bool, ctrl: bool) {
        let n = self.displayed().len();
        if n == 0 {
            return;
        }
        let cur = self.cursor.map(|c| c as isize).unwrap_or(if delta > 0 { -1 } else { n as isize });
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        self.set_cursor(next, shift, ctrl);
    }

    pub fn cursor_home(&mut self, shift: bool, ctrl: bool) {
        if !self.displayed().is_empty() {
            self.set_cursor(0, shift, ctrl);
        }
    }

    pub fn cursor_end(&mut self, shift: bool, ctrl: bool) {
        let n = self.displayed().len();
        if n > 0 {
            self.set_cursor(n - 1, shift, ctrl);
        }
    }

    fn set_cursor(&mut self, idx: usize, shift: bool, ctrl: bool) {
        if shift {
            self.select_range_to(idx, false);
        } else if ctrl {
            self.cursor = Some(idx);
        } else {
            self.select_only(idx);
        }
        self.scroll_to_cursor = true;
    }

    pub fn row_stride(&self) -> isize {
        match self.view_mode {
            ViewMode::List => 1,
            ViewMode::Grid => self.items_per_row.max(1) as isize,
        }
    }

    /// Letters typed while the list has focus jump to the first matching
    /// name (accumulating into a prefix if typed quickly).
    pub fn type_ahead(&mut self, ch: char) {
        let now = Instant::now();
        let fresh = self.type_ahead_at.is_none_or(|t| now.duration_since(t).as_millis() > TYPE_AHEAD_WINDOW_MS);
        if fresh {
            self.type_ahead.clear();
        }
        self.type_ahead.push(ch);
        self.type_ahead_at = Some(now);
        let prefix = self.type_ahead.to_lowercase();
        // Start searching after the cursor when the prefix is a single
        // repeated key ("n", "n", "n" cycles through N... entries).
        let start = if prefix.chars().all(|c| Some(c) == prefix.chars().next()) && prefix.len() > 1 {
            self.type_ahead = prefix.chars().next().unwrap().to_string();
            self.cursor.map(|c| c + 1).unwrap_or(0)
        } else {
            0
        };
        let first = self.type_ahead.chars().next().unwrap().to_lowercase().to_string();
        let n = self.displayed().len();
        let needle = if self.type_ahead.len() > 1 { prefix.clone() } else { first };
        let found = (0..n).map(|i| (start + i) % n.max(1)).find(|&i| {
            self.displayed()[i].name.to_lowercase().starts_with(&needle)
        });
        if let Some(i) = found {
            self.select_only(i);
            self.scroll_to_cursor = true;
        }
    }

    pub fn cursor_entry(&self) -> Option<&FileEntry> {
        self.cursor.and_then(|c| self.displayed().get(c))
    }

    /// Bytes of the selected files (folders count as 0 — sizes on demand).
    pub fn selected_size(&self) -> u64 {
        self.displayed().iter().filter(|e| self.selected.contains(&e.path)).map(|e| e.size).sum()
    }

    pub fn total_size(&self) -> u64 {
        self.displayed().iter().map(|e| e.size).sum()
    }
}

/// "file2" < "file10" — what people expect from a file manager.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = 0u128;
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na = na.saturating_mul(10).saturating_add(c as u128 - '0' as u128);
                    ai.next();
                }
                let mut nb = 0u128;
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb = nb.saturating_mul(10).saturating_add(c as u128 - '0' as u128);
                    bi.next();
                }
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(ca), Some(cb)) => {
                let (la, lb) = (ca.to_lowercase().next().unwrap_or(ca), cb.to_lowercase().next().unwrap_or(cb));
                if la != lb {
                    return la.cmp(&lb);
                }
                ai.next();
                bi.next();
            }
        }
    }
}
