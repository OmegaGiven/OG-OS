use iced::{
    widget::{button, column, container, image, mouse_area, row, scrollable, text, text_input, Space},
    Background, Border, Color, Element, Length, Point, Size, Subscription, Task,
};
use iced::futures::SinkExt;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::desktop::{default_app_for_file, load_app_registry, mime_for_file, set_default_app, AppEntry};
use crate::devices::Device;
use crate::filesystem::{self, home_dir, DriveInfo, FileDetails};
use crate::jobs::{self, JobEvent, JobHandle, JobKind, Resolution, UndoOp};
use crate::panes::{filelist, jobs_panel, sidebar, tabstrip, toolbar};
use crate::tab::{SortBy, Tab, ViewMode, ZOOM_STEPS};
use crate::theme;

const SEARCH_RESULT_CAP: usize = 500;
const SIDEBAR_WIDTH_RANGE: std::ops::RangeInclusive<f32> = 140.0..=400.0;
const PREVIEW_WIDTH_RANGE: std::ops::RangeInclusive<f32> = 180.0..=500.0;
const DIVIDER_WIDTH: f32 = 5.0;
const FILE_DRAG_THRESHOLD: f32 = 6.0;
const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");

#[derive(Debug, Clone)]
pub enum ClipboardOp {
    Copy(Vec<PathBuf>),
    Cut(Vec<PathBuf>),
}

#[derive(Debug, Clone)]
pub enum ContextMenuKind {
    Entry { path: PathBuf, is_dir: bool },
    Background,
}

#[derive(Debug, Clone)]
pub struct ContextMenu {
    pub kind: ContextMenuKind,
    pub renaming: bool,
    pub rename_text: String,
}

#[derive(Debug, Clone)]
pub struct OpenWithDialog {
    pub path: PathBuf,
    pub mime: String,
    pub search: String,
}

#[derive(Debug, Clone)]
pub enum PendingAction {
    DeletePermanently(Vec<PathBuf>),
    EmptyTrash,
}

#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub message: String,
    pub action: PendingAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaneDrag {
    Sidebar,
    Preview,
}

// ── Job UI state (jobs.rs runs the actual work on a thread; this is what
// the progress toasts and conflict dialog render from) ─────────────────────
pub struct ConflictUi {
    pub dst: PathBuf,
    pub dst_is_dir: bool,
}

pub struct JobUi {
    pub handle: JobHandle,
    pub kind: JobKind,
    pub current: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_items: u64,
    pub total_items: u64,
    pub conflict: Option<ConflictUi>,
    pub apply_all: bool,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

// ── Messages ─────────────────────────────────────────────────────────────
#[derive(Debug, Clone)]
pub enum Message {
    Navigate(PathBuf),
    NavigateBack,
    NavigateForward,
    NavigateUp,
    NavigateHome,
    TabLoaded(u64, u64, Vec<filesystem::FileEntry>),

    TabNew,
    TabClose(usize),
    TabActivate(usize),

    EntryPressed(usize),
    EntryReleased(usize),
    EntryDoubleClicked(PathBuf),
    EntryHoverEnter(PathBuf),
    EntryHoverExit(PathBuf),
    BackgroundClicked,
    SelectAll,

    SearchChanged(String),
    SearchSubmit,
    SearchBatch(u64, Vec<filesystem::FileEntry>),
    SearchDone(u64),

    ViewModeToggle,
    SortChanged(SortBy),
    ToggleGroupFolders,
    ShowHiddenToggle,
    ZoomIn,
    ZoomOut,

    PathBarEdit(String),
    PathBarSubmit,

    NewFolder,
    NewFile,

    ContextMenuClose,
    ContextMenuOpenEntry(usize),
    ContextMenuOpenBackground,
    OpenDefault(PathBuf),
    OpenWith(PathBuf, String),
    OpenWithBrowse(PathBuf),
    OpenWithSearch(String),
    OpenWithSetDefault(String, String),
    OpenWithClose,
    OpenTerminalHere(PathBuf),
    OpenInTerminalWithPath(PathBuf),
    CopyPath(PathBuf),
    CopyName(PathBuf),
    Compress,
    CompressZip,
    Extract(PathBuf),
    Copy,
    Cut,
    Paste,
    Delete,
    DeletePermanently,
    RestoreFromTrash(PathBuf),
    EmptyTrash,
    ConfirmYes,
    ConfirmNo,
    Duplicate,
    ToggleBookmark(PathBuf),
    RenameStart,
    RenameText(String),
    RenameSubmit,
    Refresh,
    Undo,

    CursorMoved(Point),
    PaneDragStart(PaneDrag),
    PaneDragEnd,
    WindowResized(Size),
    ModifiersChanged(iced::keyboard::Modifiers),
    KeyPressed(iced::keyboard::Key),
    Noop,

    WindowIdReady(Option<iced::window::Id>),
    WaylandDisplayReady(Option<usize>),
    DropTick,

    WatcherBatch(Vec<PathBuf>),
    ThumbnailTick,
    /// (absolute y offset, visible viewport height) — drives the file
    /// list's virtualization, see `panes::filelist`.
    Scrolled(f32, f32),

    DevicesRefreshed(Vec<Device>, Vec<DriveInfo>, Vec<DriveInfo>),
    DeviceMount(String),
    DeviceMounted(String, Result<PathBuf, String>),
    DeviceUnmount(String),
    DeviceEject(String),

    JobEvent(JobEvent),
    JobCancel(u64),
    JobDismiss(u64),
    JobConflictResolve(u64, Resolution),
    JobConflictApplyAll(u64, bool),
}

// ── App ──────────────────────────────────────────────────────────────────
pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,

    pub clipboard: Option<ClipboardOp>,
    pub app_registry: Vec<AppEntry>,
    pub status_message: String,

    pub fixed_devices: Vec<DriveInfo>,
    pub network_drives: Vec<DriveInfo>,
    pub removable: Vec<Device>,
    pub recent: Vec<PathBuf>,
    pub bookmarks: Vec<PathBuf>,

    pub context_menu: Option<ContextMenu>,
    pub open_with: Option<OpenWithDialog>,
    pub confirm: Option<ConfirmDialog>,

    pub cursor_pos: Point,
    pub context_menu_pos: Point,
    pub sidebar_width: f32,
    pub preview_width: f32,
    pub pane_drag: Option<PaneDrag>,
    pub drag_preview_x: f32,
    pub window_size: Size,

    pub press_origin: Option<(usize, Point)>,
    pub file_drag: Option<Vec<PathBuf>>,
    pub drop_hover: Option<PathBuf>,
    pub external_drag_over: bool,

    pub modifiers: iced::keyboard::Modifiers,
    pub items_per_row: std::rc::Rc<std::cell::Cell<usize>>,

    pub drag_tx: Option<calloop::channel::Sender<og_wayland::DragRequest>>,
    pub drop_rx: Option<std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<og_wayland::DropEvent>>>>,

    pub jobs: Vec<JobUi>,
    pub undo_stack: Vec<UndoOp>,

    /// Cached details for the current single-selection, refreshed once per
    /// `update()` call (see `refresh_preview`) — the preview panel borrows
    /// this rather than recomputing (and thus owning-then-dangling) a fresh
    /// `FileDetails` inside `view()` itself.
    pub preview: Option<FileDetails>,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let start = home_dir();
        let app_registry = load_app_registry();
        let recent = filesystem::load_recent();
        let bookmarks = filesystem::load_bookmarks();
        let (fixed_devices, network_drives) = filesystem::list_drives();
        let removable = crate::devices::list();

        let tab = Tab::new(start.clone(), false, ViewMode::Grid);
        let tab_id = tab.id;
        let gen = tab.load_gen;

        let app = App {
            tabs: vec![tab],
            active: 0,
            clipboard: None,
            app_registry,
            status_message: String::new(),
            fixed_devices,
            network_drives,
            removable,
            recent,
            bookmarks,
            context_menu: None,
            open_with: None,
            confirm: None,
            cursor_pos: Point::ORIGIN,
            context_menu_pos: Point::ORIGIN,
            sidebar_width: 200.0,
            preview_width: 260.0,
            pane_drag: None,
            drag_preview_x: 0.0,
            window_size: Size::new(1200.0, 800.0),
            press_origin: None,
            file_drag: None,
            drop_hover: None,
            external_drag_over: false,
            modifiers: iced::keyboard::Modifiers::default(),
            items_per_row: std::rc::Rc::new(std::cell::Cell::new(1)),
            drag_tx: None,
            drop_rx: None,
            jobs: Vec::new(),
            undo_stack: Vec::new(),
            preview: None,
        };

        let load = load_task(tab_id, start, gen, false);
        (app, Task::batch([load, iced::window::get_latest().map(Message::WindowIdReady)]))
    }

    fn active(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn active_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn watched_dirs(&self) -> Vec<PathBuf> {
        self.tabs.iter().map(|t| t.path.clone()).collect()
    }

    fn resync_watcher(&self) {
        crate::watcher::set_watched(self.watched_dirs());
    }

    fn navigate(&mut self, path: PathBuf) -> Task<Message> {
        let tab = self.active_mut();
        tab.go(path.clone());
        self.resync_watcher();
        let id = self.active().id;
        let gen = self.active().load_gen;
        // Tab::go already zeroed tab.scroll_offset (the value the
        // virtualization math uses), but the scrollable widget itself
        // remembers its own on-screen position by Id across navigations —
        // without this it'd stay scrolled halfway down a long folder
        // after jumping into a short one, showing nothing but the bottom
        // overscan's blank space until the user manually scrolled up.
        let reset_scroll = scrollable::snap_to(scrollable::Id::new("filelist"), scrollable::RelativeOffset::START);
        Task::batch([load_task(id, path, gen, self.active().show_hidden), reset_scroll])
    }

    fn reload_current(&mut self) -> Task<Message> {
        let tab = self.active_mut();
        tab.load_gen += 1;
        let (id, gen, path, hidden) = (tab.id, tab.load_gen, tab.path.clone(), tab.show_hidden);
        load_task(id, path, gen, hidden)
    }

    fn selected_details(&self) -> Option<FileDetails> {
        let tab = self.active();
        if tab.selected.len() == 1 {
            tab.selected.iter().next().and_then(|p| filesystem::get_file_details(p))
        } else {
            None
        }
    }

    fn start_job(&mut self, spec: jobs::JobSpec) {
        let kind = spec.kind;
        let handle = jobs::start(spec);
        self.jobs.push(JobUi {
            handle,
            kind,
            current: String::new(),
            done_bytes: 0,
            total_bytes: 0,
            done_items: 0,
            total_items: 0,
            conflict: None,
            apply_all: false,
            done: false,
            cancelled: false,
            error: None,
        });
    }

    fn job_by_id_mut(&mut self, id: u64) -> Option<&mut JobUi> {
        self.jobs.iter_mut().find(|j| j.handle.id == id)
    }

    fn touch_dirs(&mut self, dirs: &[PathBuf]) {
        let mut affected: Vec<usize> = Vec::new();
        for (i, t) in self.tabs.iter().enumerate() {
            if dirs.iter().any(|d| d == &t.path) {
                affected.push(i);
            }
        }
        for i in affected {
            let tab = &mut self.tabs[i];
            tab.load_gen += 1;
            let (id, gen, path, hidden) = (tab.id, tab.load_gen, tab.path.clone(), tab.show_hidden);
            // Fire-and-forget: these are cheap dir reads, no need to route
            // the resulting Task back through update() by hand here — the
            // caller batches whichever Task variant it needs (see call
            // sites); simple synchronous refresh is fine for the common
            // "operation just finished in *this* tab" case.
            let entries = filesystem::read_dir_entries(&path, hidden);
            let _ = (id, gen);
            self.tabs[i].apply_listing(entries);
        }
        self.refresh_drives_if_needed(dirs);
    }

    fn refresh_drives_if_needed(&mut self, dirs: &[PathBuf]) {
        if dirs.iter().any(|d| d.starts_with("/run/media") || d.starts_with("/media") || d == std::path::Path::new("/")) {
            let (fixed, net) = filesystem::list_drives();
            self.fixed_devices = fixed;
            self.network_drives = net;
        }
    }

    pub fn update(&mut self, msg: Message) -> Task<Message> {
        let task = self.update_inner(msg);
        self.refresh_preview();
        task
    }

    fn refresh_preview(&mut self) {
        self.preview = self.selected_details();
    }

    fn update_inner(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Navigate(path) => return self.navigate(path),
            Message::NavigateBack => {
                let moved = self.active_mut().back();
                if moved {
                    self.resync_watcher();
                    let tab = self.active();
                    return load_task(tab.id, tab.path.clone(), tab.load_gen, tab.show_hidden);
                }
            }
            Message::NavigateForward => {
                let moved = self.active_mut().forward();
                if moved {
                    self.resync_watcher();
                    let tab = self.active();
                    return load_task(tab.id, tab.path.clone(), tab.load_gen, tab.show_hidden);
                }
            }
            Message::NavigateUp => {
                if let Some(parent) = self.active().path.parent().map(|p| p.to_path_buf()) {
                    return self.navigate(parent);
                }
            }
            Message::NavigateHome => return self.navigate(home_dir()),
            Message::TabLoaded(tab_id, gen, entries) => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                    if tab.load_gen == gen {
                        tab.apply_listing(entries);
                        self.status_message = format!("{} items", tab.entries.len());
                    }
                }
            }

            Message::TabNew => {
                let path = self.active().path.clone();
                let hidden = self.active().show_hidden;
                let tab = Tab::new(path.clone(), hidden, ViewMode::Grid);
                let (id, gen) = (tab.id, tab.load_gen);
                self.tabs.push(tab);
                self.active = self.tabs.len() - 1;
                return load_task(id, path, gen, hidden);
            }
            Message::TabClose(idx) => {
                if self.tabs.len() > 1 && idx < self.tabs.len() {
                    self.tabs.remove(idx);
                    if self.active >= self.tabs.len() {
                        self.active = self.tabs.len() - 1;
                    } else if self.active > idx {
                        self.active -= 1;
                    }
                    self.resync_watcher();
                }
            }
            Message::TabActivate(idx) => {
                if idx < self.tabs.len() {
                    self.active = idx;
                }
            }

            Message::EntryPressed(idx) => {
                self.press_origin = Some((idx, self.cursor_pos));
            }
            Message::EntryReleased(released_idx) => {
                let Some((pressed_idx, _)) = self.press_origin.take() else { return Task::none() };
                if let Some(dragged) = self.file_drag.take() {
                    self.drop_hover = None;
                    let target_dir = self.active().displayed().get(released_idx).filter(|e| e.is_dir).map(|e| e.path.clone());
                    if let Some(dir) = target_dir {
                        if !dragged.contains(&dir) {
                            self.move_or_copy_into(dragged, dir, self.modifiers.control());
                        }
                    }
                } else {
                    let ctrl = self.modifiers.control();
                    let shift = self.modifiers.shift();
                    let is_double = self.active_mut().click(pressed_idx, ctrl, shift);
                    if is_double {
                        if let Some(entry) = self.active().displayed().get(pressed_idx).cloned() {
                            if entry.is_dir {
                                return self.navigate(entry.path);
                            } else {
                                filesystem::open_default(&entry.path);
                            }
                        }
                    } else {
                        self.status_message = self.selection_status();
                    }
                }
            }
            Message::EntryHoverEnter(path) => self.drop_hover = Some(path),
            Message::EntryHoverExit(path) => {
                if self.drop_hover.as_ref() == Some(&path) {
                    self.drop_hover = None;
                }
            }
            Message::EntryDoubleClicked(_) => {} // handled via EntryReleased's is_double
            Message::BackgroundClicked => {
                if self.file_drag.is_none() {
                    self.active_mut().clear_selection();
                    self.status_message = self.selection_status();
                }
            }
            Message::SelectAll => {
                self.active_mut().select_all();
                self.status_message = self.selection_status();
            }

            Message::SearchChanged(q) => return self.start_search(q),
            Message::SearchSubmit => {}
            Message::SearchBatch(gen, mut batch) => {
                let tab = self.active_mut();
                if gen == tab.search_gen {
                    tab.search_results.append(&mut batch);
                    self.status_message = format!("{} results (searching subfolders...)", self.active().search_results.len());
                }
            }
            Message::SearchDone(gen) => {
                let tab = self.active_mut();
                if gen == tab.search_gen {
                    tab.searching = false;
                    self.status_message = format!("{} results", tab.search_results.len());
                }
            }

            Message::ViewModeToggle => {
                let tab = self.active_mut();
                tab.view_mode = match tab.view_mode {
                    ViewMode::Grid => ViewMode::List,
                    ViewMode::List => ViewMode::Grid,
                };
            }
            Message::SortChanged(by) => {
                let tab = self.active_mut();
                if tab.sort_by == by {
                    tab.sort_asc = !tab.sort_asc;
                } else {
                    tab.sort_by = by;
                    tab.sort_asc = true;
                }
                tab.sort();
                self.context_menu = None;
            }
            Message::ToggleGroupFolders => {
                let tab = self.active_mut();
                tab.group_folders = !tab.group_folders;
                tab.sort();
                self.context_menu = None;
            }
            Message::ShowHiddenToggle => {
                let tab = self.active_mut();
                tab.show_hidden = !tab.show_hidden;
                self.context_menu = None;
                return self.reload_current();
            }
            Message::ZoomIn => self.active_mut().zoom_in(),
            Message::ZoomOut => self.active_mut().zoom_out(),

            Message::PathBarEdit(s) => self.active_mut().path_edit = Some(s),
            Message::PathBarSubmit => {
                let text = self.active().path_edit.clone().unwrap_or_default();
                let path = PathBuf::from(shellexpand_home(&text));
                self.active_mut().path_edit = None;
                if path.is_dir() {
                    return self.navigate(path);
                } else {
                    self.status_message = "Path not found".to_string();
                }
            }

            Message::NewFolder => {
                let dest = filesystem::unique_name(&self.active().path, "New Folder");
                match filesystem::create_dir(&dest) {
                    Ok(_) => {
                        self.undo_stack.push(UndoOp::Created(dest.clone()));
                        let t1 = self.reload_current();
                        self.select_and_rename(dest, true);
                        return t1;
                    }
                    Err(e) => self.status_message = format!("Error: {}", e),
                }
            }
            Message::NewFile => {
                let dest = filesystem::unique_name(&self.active().path, "New File.txt");
                match filesystem::create_file(&dest) {
                    Ok(_) => {
                        self.undo_stack.push(UndoOp::Created(dest.clone()));
                        let t1 = self.reload_current();
                        self.select_and_rename(dest, false);
                        return t1;
                    }
                    Err(e) => self.status_message = format!("Error: {}", e),
                }
            }

            Message::ContextMenuClose => self.context_menu = None,
            Message::ContextMenuOpenEntry(idx) => {
                if let Some(entry) = self.active().displayed().get(idx).cloned() {
                    if !self.active().selected.contains(&entry.path) {
                        self.active_mut().select_only(idx);
                        self.status_message = self.selection_status();
                    }
                    self.context_menu_pos = self.cursor_pos;
                    self.context_menu = Some(ContextMenu {
                        kind: ContextMenuKind::Entry { path: entry.path, is_dir: entry.is_dir },
                        renaming: false,
                        rename_text: String::new(),
                    });
                }
            }
            Message::ContextMenuOpenBackground => {
                self.context_menu_pos = self.cursor_pos;
                self.context_menu = Some(ContextMenu { kind: ContextMenuKind::Background, renaming: false, rename_text: String::new() });
            }
            Message::OpenDefault(path) => {
                filesystem::open_default(&path);
                self.context_menu = None;
            }
            Message::OpenWith(path, exec) => {
                filesystem::open_with(&path, &exec);
                self.context_menu = None;
                self.open_with = None;
            }
            Message::OpenWithBrowse(path) => {
                let mime = mime_for_file(&path);
                self.context_menu = None;
                self.open_with = Some(OpenWithDialog { path, mime, search: String::new() });
            }
            Message::OpenWithSearch(q) => {
                if let Some(dlg) = &mut self.open_with {
                    dlg.search = q;
                }
            }
            Message::OpenWithSetDefault(mime, id) => {
                set_default_app(&mime, &id);
                self.status_message = "Default app updated".to_string();
            }
            Message::OpenWithClose => self.open_with = None,
            Message::OpenTerminalHere(path) => {
                filesystem::open_terminal_here(&path);
                self.context_menu = None;
            }
            Message::OpenInTerminalWithPath(path) => {
                let dir = path.parent().unwrap_or(&path).to_path_buf();
                filesystem::open_terminal_with_prefill(&dir, &path.to_string_lossy());
                self.context_menu = None;
            }
            Message::CopyPath(path) => {
                self.context_menu = None;
                self.status_message = "Path copied to clipboard".to_string();
                return iced::clipboard::write::<Message>(path.to_string_lossy().to_string());
            }
            Message::CopyName(path) => {
                self.context_menu = None;
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.status_message = "Name copied to clipboard".to_string();
                return iced::clipboard::write::<Message>(name);
            }
            Message::Compress => {
                let paths = self.active().selected_paths();
                if !paths.is_empty() {
                    let dest = self.active().path.clone();
                    if let Err(e) = filesystem::compress_paths(&paths, &dest) {
                        self.status_message = format!("Error: {}", e);
                    } else {
                        self.status_message = "Compressed".to_string();
                    }
                }
                self.context_menu = None;
                return self.reload_current();
            }
            Message::CompressZip => {
                let paths = self.active().selected_paths();
                if !paths.is_empty() {
                    let dest = self.active().path.clone();
                    if let Err(e) = filesystem::compress_paths_zip(&paths, &dest) {
                        self.status_message = format!("Error: {}", e);
                    } else {
                        self.status_message = "Compressed to .zip".to_string();
                    }
                }
                self.context_menu = None;
                return self.reload_current();
            }
            Message::Duplicate => {
                let paths = self.active().selected_paths();
                let mut count = 0;
                for src in &paths {
                    let dst = jobs::keep_both_name(src);
                    if filesystem::copy_file(src, &dst).is_ok() {
                        count += 1;
                    }
                }
                self.status_message = format!("Duplicated {} item(s)", count);
                self.context_menu = None;
                return self.reload_current();
            }
            Message::Extract(path) => {
                let dest = self.active().path.clone();
                match filesystem::extract_archive(&path, &dest) {
                    Ok(_) => self.status_message = "Extracted".to_string(),
                    Err(e) => self.status_message = format!("Error: {}", e),
                }
                self.context_menu = None;
                return self.reload_current();
            }

            Message::Copy => {
                let paths = self.active().selected_paths();
                if !paths.is_empty() {
                    self.clipboard = Some(ClipboardOp::Copy(paths));
                    self.status_message = "Copied to clipboard".to_string();
                }
                self.context_menu = None;
            }
            Message::Cut => {
                let paths = self.active().selected_paths();
                if !paths.is_empty() {
                    self.clipboard = Some(ClipboardOp::Cut(paths));
                    self.status_message = "Cut to clipboard".to_string();
                }
                self.context_menu = None;
            }
            Message::Paste => {
                if let Some(op) = self.clipboard.clone() {
                    let dest = self.active().path.clone();
                    match op {
                        ClipboardOp::Copy(paths) => self.start_job(jobs::JobSpec { kind: JobKind::Copy, sources: paths, dest_dir: Some(dest) }),
                        ClipboardOp::Cut(paths) => {
                            self.start_job(jobs::JobSpec { kind: JobKind::Move, sources: paths, dest_dir: Some(dest) });
                            self.clipboard = None;
                        }
                    }
                }
                self.context_menu = None;
            }
            Message::Delete => {
                let paths = self.active().selected_paths();
                if !paths.is_empty() {
                    self.start_job(jobs::JobSpec { kind: JobKind::Trash, sources: paths, dest_dir: None });
                }
                self.context_menu = None;
            }
            Message::DeletePermanently => {
                let paths = self.active().selected_paths();
                self.context_menu = None;
                if !paths.is_empty() {
                    let noun = if paths.len() == 1 { "item".to_string() } else { format!("{} items", paths.len()) };
                    self.confirm = Some(ConfirmDialog {
                        message: format!("Permanently delete {}? This cannot be undone.", noun),
                        action: PendingAction::DeletePermanently(paths),
                    });
                }
            }
            Message::RestoreFromTrash(path) => {
                self.context_menu = None;
                self.start_job(jobs::JobSpec { kind: JobKind::Restore, sources: vec![path], dest_dir: None });
            }
            Message::EmptyTrash => {
                self.context_menu = None;
                self.confirm = Some(ConfirmDialog { message: "Permanently delete everything in Trash? This cannot be undone.".to_string(), action: PendingAction::EmptyTrash });
            }
            Message::ConfirmYes => {
                if let Some(dlg) = self.confirm.take() {
                    match dlg.action {
                        PendingAction::DeletePermanently(paths) => self.start_job(jobs::JobSpec { kind: JobKind::Delete, sources: paths, dest_dir: None }),
                        PendingAction::EmptyTrash => {
                            match filesystem::empty_trash() {
                                Ok(_) => self.status_message = "Trash emptied".to_string(),
                                Err(e) => self.status_message = format!("Error: {}", e),
                            }
                            return self.reload_current();
                        }
                    }
                }
            }
            Message::ConfirmNo => self.confirm = None,
            Message::ToggleBookmark(path) => {
                if let Some(pos) = self.bookmarks.iter().position(|p| p == &path) {
                    self.bookmarks.remove(pos);
                } else {
                    self.bookmarks.push(path);
                }
                filesystem::save_bookmarks(&self.bookmarks);
                self.context_menu = None;
            }
            Message::RenameStart => {
                if let Some(cm) = &mut self.context_menu {
                    if let ContextMenuKind::Entry { path, .. } = &cm.kind {
                        cm.rename_text = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                        cm.renaming = true;
                    }
                }
            }
            Message::RenameText(s) => {
                if let Some(cm) = &mut self.context_menu {
                    cm.rename_text = s;
                }
            }
            Message::RenameSubmit => {
                if let Some(cm) = self.context_menu.take() {
                    if let ContextMenuKind::Entry { path, .. } = cm.kind {
                        let new_path = path.parent().map(|p| p.join(&cm.rename_text)).unwrap_or_default();
                        match filesystem::rename_entry(&path, &new_path) {
                            Ok(_) => {
                                self.undo_stack.push(UndoOp::Renamed { from: path, to: new_path.clone() });
                                self.status_message = format!("Renamed to {}", cm.rename_text);
                                self.active_mut().selected = HashSet::from([new_path]);
                            }
                            Err(e) => self.status_message = format!("Error: {}", e),
                        }
                        return self.reload_current();
                    }
                }
            }
            Message::Refresh => {
                let (fixed, net) = filesystem::list_drives();
                self.fixed_devices = fixed;
                self.network_drives = net;
                self.removable = crate::devices::list();
                return self.reload_current();
            }
            Message::Undo => return self.undo(),

            Message::CursorMoved(pos) => {
                self.cursor_pos = pos;
                if self.pane_drag.is_some() {
                    self.drag_preview_x = pos.x;
                }
                if self.file_drag.is_none() {
                    if let Some((pressed_idx, press_pos)) = self.press_origin {
                        let dx = pos.x - press_pos.x;
                        let dy = pos.y - press_pos.y;
                        if dx * dx + dy * dy > FILE_DRAG_THRESHOLD * FILE_DRAG_THRESHOLD {
                            let tab = self.active();
                            if let Some(entry) = tab.displayed().get(pressed_idx) {
                                let dragged: Vec<PathBuf> = if tab.selected.contains(&entry.path) {
                                    tab.selected_paths()
                                } else {
                                    vec![entry.path.clone()]
                                };
                                if let Some(tx) = &self.drag_tx {
                                    let _ = tx.send(og_wayland::DragRequest { paths: dragged.clone() });
                                }
                                self.file_drag = Some(dragged);
                            }
                        }
                    }
                }
            }
            Message::PaneDragStart(target) => {
                self.pane_drag = Some(target);
                self.drag_preview_x = match target {
                    PaneDrag::Sidebar => self.sidebar_width,
                    PaneDrag::Preview => self.window_size.width - self.preview_width,
                };
            }
            Message::PaneDragEnd => {
                if let Some(target) = self.pane_drag.take() {
                    match target {
                        PaneDrag::Sidebar => self.sidebar_width = self.drag_preview_x.clamp(*SIDEBAR_WIDTH_RANGE.start(), *SIDEBAR_WIDTH_RANGE.end()),
                        PaneDrag::Preview => {
                            let width = self.window_size.width - self.drag_preview_x;
                            self.preview_width = width.clamp(*PREVIEW_WIDTH_RANGE.start(), *PREVIEW_WIDTH_RANGE.end());
                        }
                    }
                }
                self.press_origin = None;
                self.file_drag = None;
                self.drop_hover = None;
            }
            Message::WindowResized(size) => self.window_size = size,
            Message::ModifiersChanged(m) => self.modifiers = m,
            Message::KeyPressed(key) => return self.handle_key(key),
            Message::Noop => {}

            Message::WindowIdReady(Some(id)) => {
                return iced::window::run_with_handle(id, |handle| og_wayland::display_ptr_from_window_handle(handle)).map(Message::WaylandDisplayReady);
            }
            Message::WindowIdReady(None) => {}
            Message::WaylandDisplayReady(Some(display_ptr)) => {
                let (tx, rx) = og_wayland::spawn_with_drops(display_ptr);
                self.drag_tx = Some(tx);
                self.drop_rx = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
            }
            Message::WaylandDisplayReady(None) => {
                self.status_message = "Cross-app drag-and-drop unavailable (non-Wayland session?)".to_string();
            }
            Message::DropTick => {
                if let Some(rx) = self.drop_rx.clone() {
                    if let Ok(guard) = rx.try_lock() {
                        while let Ok(ev) = guard.try_recv() {
                            match ev {
                                og_wayland::DropEvent::Enter { .. } | og_wayland::DropEvent::Motion { .. } => {
                                    self.external_drag_over = true;
                                }
                                og_wayland::DropEvent::Leave => {
                                    self.external_drag_over = false;
                                }
                                og_wayland::DropEvent::Dropped { paths, .. } => {
                                    self.external_drag_over = false;
                                    if !paths.is_empty() {
                                        let dest = self.active().path.clone();
                                        self.start_job(jobs::JobSpec { kind: JobKind::Copy, sources: paths, dest_dir: Some(dest) });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            Message::WatcherBatch(dirs) => self.touch_dirs(&dirs),
            Message::ThumbnailTick => {} // view() re-reads the thumbs cache on the next frame; nothing to store
            Message::Scrolled(offset_y, viewport_height) => {
                let tab = self.active_mut();
                tab.scroll_offset = offset_y;
                tab.viewport_height = viewport_height;
            }

            Message::DevicesRefreshed(rem, fixed, net) => {
                self.removable = rem;
                self.fixed_devices = fixed;
                self.network_drives = net;
            }
            Message::DeviceMount(dev) => {
                let dev2 = dev.clone();
                return Task::perform(
                    async move { tokio::task::spawn_blocking(move || crate::devices::mount(&dev2)).await.unwrap_or_else(|_| Err("mount task failed".into())) },
                    move |result| Message::DeviceMounted(dev.clone(), result),
                );
            }
            Message::DeviceMounted(_dev, result) => match result {
                Ok(path) => {
                    self.status_message = format!("Mounted at {}", path.display());
                    self.removable = crate::devices::list();
                    return self.navigate(path);
                }
                Err(e) => self.status_message = format!("Mount failed: {}", e),
            },
            Message::DeviceUnmount(dev) => match crate::devices::unmount(&dev) {
                Ok(_) => {
                    self.status_message = "Unmounted".to_string();
                    self.removable = crate::devices::list();
                }
                Err(e) => self.status_message = format!("Unmount failed: {}", e),
            },
            Message::DeviceEject(dev) => match crate::devices::eject(&dev) {
                Ok(_) => {
                    self.status_message = "Safe to unplug".to_string();
                    self.removable = crate::devices::list();
                }
                Err(e) => self.status_message = format!("Eject failed: {}", e),
            },

            Message::JobEvent(ev) => self.apply_job_event(ev),
            Message::JobCancel(id) => {
                if let Some(job) = self.jobs.iter().find(|j| j.handle.id == id) {
                    job.handle.cancel();
                }
            }
            Message::JobDismiss(id) => self.jobs.retain(|j| j.handle.id != id),
            Message::JobConflictResolve(id, res) => {
                let apply_all = self.job_by_id_mut(id).map(|j| j.apply_all).unwrap_or(false);
                if let Some(job) = self.jobs.iter().find(|j| j.handle.id == id) {
                    job.handle.reply_conflict(res, apply_all);
                }
                if let Some(j) = self.job_by_id_mut(id) {
                    j.conflict = None;
                }
            }
            Message::JobConflictApplyAll(id, v) => {
                if let Some(j) = self.job_by_id_mut(id) {
                    j.apply_all = v;
                }
            }
        }
        Task::none()
    }

    fn select_and_rename(&mut self, path: PathBuf, is_dir: bool) {
        self.active_mut().selected = HashSet::from([path.clone()]);
        self.context_menu = Some(ContextMenu {
            kind: ContextMenuKind::Entry { path: path.clone(), is_dir },
            renaming: true,
            rename_text: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        });
    }

    fn selection_status(&self) -> String {
        let tab = self.active();
        let n = tab.selected.len();
        if n == 0 {
            format!("{} items", tab.displayed().len())
        } else if n == 1 {
            format!("{} items  |  1 selected  ({})", tab.displayed().len(), filesystem::format_size(tab.selected_size()))
        } else {
            format!("{} items  |  {} selected  ({})", tab.displayed().len(), n, filesystem::format_size(tab.selected_size()))
        }
    }

    fn move_or_copy_into(&mut self, paths: Vec<PathBuf>, target_dir: PathBuf, force_copy: bool) {
        let kind = if force_copy { JobKind::Copy } else { JobKind::Move };
        self.start_job(jobs::JobSpec { kind, sources: paths, dest_dir: Some(target_dir) });
    }

    fn start_search(&mut self, query: String) -> Task<Message> {
        let active = self.active;
        let tab = &mut self.tabs[active];
        tab.search_query = query;
        if let Some(h) = tab.search_handle.take() {
            h.abort();
        }
        tab.searching = false;

        if tab.search_query.is_empty() {
            tab.search_results.clear();
            let n = tab.entries.len();
            self.tabs[active].searching = false;
            self.status_message = format!("{} items", n);
            return Task::none();
        }

        let tab = &mut self.tabs[active];
        tab.search_results = tab.entries.iter().filter(|e| filesystem::matches_query(&e.name, &tab.search_query)).cloned().collect();
        let n = tab.search_results.len();
        tab.searching = true;
        self.status_message = format!("{} results (searching subfolders...)", n);

        let tab = &mut self.tabs[active];
        let initial_dirs: std::collections::VecDeque<PathBuf> = tab.entries.iter().filter(|e| e.is_dir).map(|e| e.path.clone()).collect();
        tab.search_gen += 1;
        let generation = tab.search_gen;
        let query = tab.search_query.clone();
        let show_hidden = tab.show_hidden;

        let stream = iced::stream::channel(16, move |mut sender| async move {
            let mut queue = initial_dirs;
            let mut found = 0usize;
            while let Some(dir) = queue.pop_front() {
                let query = query.clone();
                let (matches, subdirs) = tokio::task::spawn_blocking(move || filesystem::scan_dir_search(&dir, &query, show_hidden)).await.unwrap_or_default();
                queue.extend(subdirs);
                if !matches.is_empty() {
                    found += matches.len();
                    let _ = sender.send(Message::SearchBatch(generation, matches)).await;
                }
                if found >= SEARCH_RESULT_CAP {
                    break;
                }
            }
            let _ = sender.send(Message::SearchDone(generation)).await;
        });

        let (task, handle) = Task::stream(stream).abortable();
        self.active_mut().search_handle = Some(handle);
        task
    }

    fn apply_job_event(&mut self, ev: JobEvent) {
        match ev {
            JobEvent::Progress { id, current, done_bytes, total_bytes, done_items, total_items } => {
                if let Some(j) = self.job_by_id_mut(id) {
                    j.current = current;
                    j.done_bytes = done_bytes;
                    j.total_bytes = total_bytes;
                    j.done_items = done_items;
                    j.total_items = total_items;
                }
            }
            JobEvent::Conflict { id, dst, dst_is_dir, .. } => {
                if let Some(j) = self.job_by_id_mut(id) {
                    j.conflict = Some(ConflictUi { dst, dst_is_dir });
                }
            }
            JobEvent::Finished { id, kind: _, undo, error, cancelled, touched_dirs } => {
                if let Some(j) = self.job_by_id_mut(id) {
                    j.done = true;
                    j.cancelled = cancelled;
                    j.error = error.clone();
                    j.conflict = None;
                }
                if error.is_none() && !cancelled {
                    if let Some(op) = undo {
                        self.undo_stack.push(op);
                    }
                }
                self.touch_dirs(&touched_dirs);
                if let Some(e) = &error {
                    self.status_message = format!("Error: {e}");
                }
            }
        }
    }

    fn undo(&mut self) -> Task<Message> {
        let Some(op) = self.undo_stack.pop() else {
            self.status_message = "Nothing to undo".to_string();
            return Task::none();
        };
        self.status_message = op.describe();
        let mut touched = Vec::new();
        match op {
            UndoOp::Copied(paths) => {
                for p in &paths {
                    if let Some(parent) = p.parent() {
                        touched.push(parent.to_path_buf());
                    }
                    let _ = jobs::remove_any(p);
                }
            }
            UndoOp::Moved(pairs) => {
                for (from, to) in &pairs {
                    if let Some(p) = from.parent() {
                        touched.push(p.to_path_buf());
                    }
                    if let Some(p) = to.parent() {
                        touched.push(p.to_path_buf());
                    }
                    let _ = std::fs::rename(to, from);
                }
            }
            UndoOp::Trashed(pairs) => {
                for (original, trashed) in &pairs {
                    if let Some(p) = original.parent() {
                        touched.push(p.to_path_buf());
                    }
                    let _ = filesystem::restore_to(trashed, original);
                }
            }
            UndoOp::Renamed { from, to } => {
                if let Some(p) = to.parent() {
                    touched.push(p.to_path_buf());
                }
                let _ = std::fs::rename(&to, &from);
            }
            UndoOp::Created(p) => {
                if let Some(parent) = p.parent() {
                    touched.push(parent.to_path_buf());
                }
                let _ = jobs::remove_any(&p);
            }
        }
        self.touch_dirs(&touched);
        Task::none()
    }

    fn handle_key(&mut self, key: iced::keyboard::Key) -> Task<Message> {
        use iced::keyboard::key::{Key, Named};
        let ctrl = self.modifiers.control();
        let shift = self.modifiers.shift();
        let renaming = self.context_menu.as_ref().is_some_and(|c| c.renaming);
        let editing_path = self.active().path_edit.is_some();
        if renaming || editing_path {
            return Task::none();
        }

        match &key {
            Key::Named(Named::ArrowLeft) if ctrl => return self.update(Message::NavigateBack),
            Key::Named(Named::ArrowRight) if ctrl => return self.update(Message::NavigateForward),
            Key::Named(Named::ArrowLeft) => self.active_mut().move_cursor(-1, shift, false),
            Key::Named(Named::ArrowRight) => self.active_mut().move_cursor(1, shift, false),
            Key::Named(Named::ArrowUp) => {
                let stride = self.active().row_stride();
                self.active_mut().move_cursor(-stride, shift, false);
            }
            Key::Named(Named::ArrowDown) => {
                let stride = self.active().row_stride();
                self.active_mut().move_cursor(stride, shift, false);
            }
            Key::Named(Named::Home) => self.active_mut().cursor_home(shift, false),
            Key::Named(Named::End) => self.active_mut().cursor_end(shift, false),
            Key::Named(Named::Enter) => {
                if let Some(entry) = self.active().cursor_entry().cloned() {
                    if entry.is_dir {
                        return self.navigate(entry.path);
                    } else {
                        filesystem::open_default(&entry.path);
                    }
                }
            }
            Key::Named(Named::Backspace) => return self.update(Message::NavigateUp),
            Key::Named(Named::Delete) if shift => return self.update(Message::DeletePermanently),
            Key::Named(Named::Delete) => return self.update(Message::Delete),
            Key::Named(Named::F2) => {
                if let Some(entry) = self.active().cursor_entry().cloned() {
                    self.context_menu = Some(ContextMenu {
                        kind: ContextMenuKind::Entry { path: entry.path.clone(), is_dir: entry.is_dir },
                        renaming: false,
                        rename_text: String::new(),
                    });
                    return self.update(Message::RenameStart);
                }
            }
            Key::Named(Named::F5) => return self.update(Message::Refresh),
            Key::Named(Named::Escape) => {
                self.context_menu = None;
                self.open_with = None;
                self.confirm = None;
            }
            Key::Character(c) if ctrl && c.as_str() == "a" => return self.update(Message::SelectAll),
            Key::Character(c) if ctrl && c.as_str() == "c" => return self.update(Message::Copy),
            Key::Character(c) if ctrl && c.as_str() == "x" => return self.update(Message::Cut),
            Key::Character(c) if ctrl && c.as_str() == "v" => return self.update(Message::Paste),
            Key::Character(c) if ctrl && c.as_str() == "z" => return self.update(Message::Undo),
            Key::Character(c) if ctrl && c.as_str() == "t" => return self.update(Message::TabNew),
            Key::Character(c) if ctrl && c.as_str() == "w" => return self.update(Message::TabClose(self.active)),
            Key::Character(c) if ctrl && matches!(c.as_str(), "+" | "=") => self.active_mut().zoom_in(),
            Key::Character(c) if ctrl && c.as_str() == "-" => self.active_mut().zoom_out(),
            Key::Character(c) if !ctrl && c.chars().count() == 1 => {
                let ch = c.chars().next().unwrap();
                if ch.is_alphanumeric() {
                    self.active_mut().type_ahead(ch);
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub fn view(&self) -> Element<Message> {
        let tab = self.active();
        let titles: Vec<String> = self.tabs.iter().map(|t| t.title()).collect();
        let tab_strip = tabstrip::view(titles, self.active);

        let display_path = tab.path.to_string_lossy().to_string();
        let tb = toolbar::view(
            !tab.history.is_empty(),
            !tab.forward.is_empty(),
            display_path,
            tab.path_edit.as_deref(),
            &tab.search_query,
            tab.view_mode,
            tab.show_hidden,
            tab.searching,
        );

        let sb = sidebar::view(
            &tab.path,
            &self.fixed_devices,
            &self.network_drives,
            &self.removable,
            &self.recent,
            &self.bookmarks,
            self.sidebar_width,
            self.drop_hover.as_ref(),
        );

        let file_area_width = self.window_size.width - self.sidebar_width - DIVIDER_WIDTH - self.preview_width - DIVIDER_WIDTH;
        let overlay_open = self.context_menu.is_some() || self.open_with.is_some() || self.confirm.is_some();
        let file_area = filelist::view(
            tab.displayed(),
            &tab.selected,
            tab.cursor,
            tab.view_mode,
            file_area_width,
            tab.sort_by,
            tab.sort_asc,
            overlay_open,
            if self.file_drag.is_some() { self.drop_hover.as_ref() } else { None },
            tab.card_size(),
            &self.items_per_row,
            tab.scroll_offset,
            tab.viewport_height,
        );

        let status_str = self.selection_status();
        let path_str = tab.path.to_string_lossy().to_string();
        let status_bar = container(
            row![
                text(status_str).size(12).style(theme::muted_text),
                Space::with_width(Length::Fill),
                text(path_str).size(12).style(theme::muted_text),
            ]
            .spacing(8)
            .padding([4, 12]),
        )
        .width(Length::Fill)
        .style(theme::panel);

        let mut main_children: Vec<Element<Message>> = vec![sb, pane_divider(PaneDrag::Sidebar), file_area];
        main_children.push(pane_divider(PaneDrag::Preview));
        main_children.push(self.preview_panel());
        let main_content = row(main_children).width(Length::Fill).height(Length::Fill);

        let base = column![tab_strip, tb, main_content, status_bar].width(Length::Fill).height(Length::Fill);
        let root: Element<Message> = container(base).width(Length::Fill).height(Length::Fill).style(theme::main_bg).into();

        let root = if let Some(cm) = &self.context_menu { self.context_menu_overlay(root, cm) } else { root };
        let root = if let Some(dlg) = &self.open_with { self.open_with_overlay(root, dlg) } else { root };
        let root = if let Some(dlg) = &self.confirm { self.confirm_overlay(root, dlg) } else { root };

        let root = if let Some(job) = self.jobs.iter().find(|j| j.conflict.is_some()) {
            if let Some(dialog) = jobs_panel::conflict_dialog(job) {
                iced::widget::stack![root, dialog].into()
            } else {
                root
            }
        } else {
            root
        };

        let root = if let Some(toasts) = jobs_panel::toasts(&self.jobs) {
            iced::widget::stack![root, toasts].into()
        } else {
            root
        };

        let root = if let Some(paths) = &self.file_drag {
            let ghost = drag_ghost_files(paths);
            og_drag::with_drag_ghost(root, Some(ghost), self.cursor_pos, iced::Vector::new(14.0, 14.0))
        } else {
            root
        };

        let root = if self.external_drag_over {
            external_drop_overlay(root)
        } else {
            root
        };

        if self.pane_drag.is_some() {
            drag_ghost_line(root, self.drag_preview_x)
        } else {
            root
        }
    }

    fn preview_panel(&self) -> Element<Message> {
        let Some(d) = self.preview.as_ref() else {
            return container(container(text("No selection").size(13).style(theme::muted_text)).center_x(Length::Fill).center_y(Length::Fill))
                .width(self.preview_width)
                .height(Length::Fill)
                .style(theme::panel)
                .into();
        };

        let icon = filesystem::icon_for(&d.mime_type, d.is_dir);
        let mut col = column![].spacing(10).padding(12).width(Length::Fill);

        let thumb = if !d.is_dir && crate::thumbs::wants_thumbnail(&d.mime_type) {
            crate::thumbs::get(&d.path, &d.mime_type).flatten()
        } else {
            None
        };

        if let Some(thumb_path) = thumb {
            col = col.push(container(image(image::Handle::from_path(thumb_path)).width(Length::Fill).content_fit(iced::ContentFit::Contain)).center_x(Length::Fill).max_height(220));
        } else {
            col = col.push(container(text(icon).font(ICON_FONT).size(40).style(theme::normal_text)).center_x(Length::Fill));
        }

        col = col.push(selectable_text(&d.name, 14));
        col = col.push(context_sep());
        col = col.push(detail_row("Type", if d.is_dir { "Folder".to_string() } else { d.mime_type.clone() }));
        if d.is_dir {
            if let Some(n) = d.item_count {
                col = col.push(detail_row("Items", n.to_string()));
            }
        } else {
            col = col.push(detail_row("Size", format!("{} ({} bytes)", filesystem::format_size(d.size), d.size)));
        }
        if let Some((w, h)) = d.dimensions {
            col = col.push(detail_row("Dimensions", format!("{} x {}", w, h)));
        }
        let fmt_dt = |dt: chrono::DateTime<chrono::Local>| dt.format("%Y-%m-%d %H:%M:%S").to_string();
        col = col.push(detail_row("Modified", d.modified.map(fmt_dt).unwrap_or_else(|| "-".to_string())));
        col = col.push(detail_row("Created", d.created.map(fmt_dt).unwrap_or_else(|| "-".to_string())));
        col = col.push(detail_row("Accessed", d.accessed.map(fmt_dt).unwrap_or_else(|| "-".to_string())));
        col = col.push(detail_row("Permissions", format!("{} ({})", d.permissions, d.mode_octal)));
        col = col.push(detail_row("Owner", format!("{} : {}", d.owner, d.group)));
        if let Some(target) = &d.symlink_target {
            col = col.push(detail_row("Links To", target.to_string_lossy().to_string()));
        }
        col = col.push(detail_row("Path", d.path.to_string_lossy().to_string()));

        if !d.exif.is_empty() {
            col = col.push(context_sep());
            col = col.push(text("Metadata").size(11).style(theme::muted_text));
            for tag in &d.exif {
                col = col.push(detail_row(&tag.label, tag.value.clone()));
            }
        }

        container(iced::widget::scrollable(col)).width(self.preview_width).height(Length::Fill).style(theme::panel).into()
    }

    fn confirm_overlay<'a>(&'a self, base: Element<'a, Message>, dlg: &'a ConfirmDialog) -> Element<'a, Message> {
        use iced::widget::stack;
        let yes_btn = button(text("Delete").size(13)).on_press(Message::ConfirmYes).padding([6, 14]).style(theme::danger_button);
        let no_btn = button(text("Cancel").size(13)).on_press(Message::ConfirmNo).padding([6, 14]).style(theme::flat_button);
        let dialog = container(
            column![text(&dlg.message).size(14).style(theme::normal_text), row![Space::with_width(Length::Fill), no_btn, yes_btn].spacing(8)]
                .spacing(16)
                .padding(16)
                .width(320),
        )
        .style(theme::dialog_box);
        let overlay = container(dialog).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).center_y(Length::Fill).style(theme::scrim);
        stack![base, overlay].into()
    }

    fn open_with_overlay<'a>(&'a self, base: Element<'a, Message>, dlg: &'a OpenWithDialog) -> Element<'a, Message> {
        use iced::widget::stack;
        let search_lower = dlg.search.to_lowercase();
        let matches = |app: &AppEntry| search_lower.is_empty() || app.name.to_lowercase().contains(&search_lower);

        let mut matched: Vec<&AppEntry> = crate::desktop::apps_for_file(&dlg.path, &self.app_registry).into_iter().filter(|a| matches(a)).collect();
        matched.sort_by(|a, b| a.name.cmp(&b.name));
        let matched_ids: HashSet<&str> = matched.iter().map(|a| a.desktop_id.as_str()).collect();
        let mut others: Vec<&AppEntry> = self.app_registry.iter().filter(|a| !matched_ids.contains(a.desktop_id.as_str()) && matches(a)).collect();
        others.sort_by(|a, b| a.name.cmp(&b.name));
        let default_id = default_app_for_file(&dlg.path);

        let search = text_input("Search apps...", &dlg.search).on_input(Message::OpenWithSearch).size(13).style(theme::input_style);

        let mut list = column![].spacing(2);
        if matched.is_empty() && others.is_empty() {
            list = list.push(text("No matching apps").size(13).style(theme::muted_text));
        }
        if !matched.is_empty() {
            list = list.push(text("Recommended").size(11).style(theme::muted_text));
            for app in &matched {
                list = list.push(open_with_row(&dlg.path, &dlg.mime, app, default_id.as_deref() == Some(app.desktop_id.as_str())));
            }
        }
        if !others.is_empty() {
            list = list.push(text("Other Applications").size(11).style(theme::muted_text));
            for app in &others {
                list = list.push(open_with_row(&dlg.path, &dlg.mime, app, default_id.as_deref() == Some(app.desktop_id.as_str())));
            }
        }

        let dialog = container(
            column![text("Open With").size(15).style(theme::normal_text), search, iced::widget::scrollable(list).height(Length::Fixed(320.0)), context_btn("Close", Message::OpenWithClose)]
                .spacing(10)
                .padding(14)
                .width(360),
        )
        .style(theme::dialog_box);
        let overlay = container(dialog).width(Length::Fill).height(Length::Fill).center_x(Length::Fill).center_y(Length::Fill).style(theme::scrim);
        stack![base, overlay].into()
    }

    fn context_menu_overlay<'a>(&'a self, base: Element<'a, Message>, cm: &'a ContextMenu) -> Element<'a, Message> {
        use iced::widget::stack;
        let has_clipboard = self.clipboard.is_some();
        let multi = self.active().selected.len() > 1;
        let mut menu_col = column![].spacing(2).padding(6).width(220);
        let mut item_count = 0usize;

        if cm.renaming {
            let rename_input = text_input("New name...", &cm.rename_text).on_input(Message::RenameText).on_submit(Message::RenameSubmit).size(13).style(theme::input_style);
            menu_col = menu_col.push(rename_input);
            menu_col = menu_col.push(context_btn("Confirm Rename", Message::RenameSubmit));
            item_count += 2;
        } else {
            match &cm.kind {
                ContextMenuKind::Entry { path, is_dir } => {
                    if self.active().is_trash() {
                        menu_col = menu_col.push(context_btn("Restore", Message::RestoreFromTrash(path.clone())));
                        menu_col = menu_col.push(context_btn("Delete Permanently", Message::DeletePermanently));
                        item_count += 2;
                    } else {
                        menu_col = menu_col.push(context_btn("Open", Message::OpenDefault(path.clone())));
                        item_count += 1;
                        if !*is_dir {
                            menu_col = menu_col.push(context_btn("Open With...", Message::OpenWithBrowse(path.clone())));
                            menu_col = menu_col.push(context_btn("Open in Terminal", Message::OpenInTerminalWithPath(path.clone())));
                            item_count += 2;
                        }
                        menu_col = menu_col.push(context_sep());
                        menu_col = menu_col.push(context_btn(if multi { "Cut (selected)" } else { "Cut" }, Message::Cut));
                        menu_col = menu_col.push(context_btn(if multi { "Copy (selected)" } else { "Copy" }, Message::Copy));
                        menu_col = menu_col.push(context_btn(if multi { "Duplicate (selected)" } else { "Duplicate" }, Message::Duplicate));
                        item_count += 3;
                        if has_clipboard {
                            menu_col = menu_col.push(context_btn("Paste Into Folder", Message::Paste));
                            item_count += 1;
                        }
                        if !multi {
                            menu_col = menu_col.push(context_btn("Rename", Message::RenameStart));
                            item_count += 1;
                        }
                        menu_col = menu_col.push(context_btn("Copy Path", Message::CopyPath(path.clone())));
                        menu_col = menu_col.push(context_btn("Copy Name", Message::CopyName(path.clone())));
                        item_count += 2;
                        if *is_dir {
                            menu_col = menu_col.push(context_btn("Open Terminal Here", Message::OpenTerminalHere(path.clone())));
                            let bookmarked = self.bookmarks.contains(path);
                            menu_col = menu_col.push(context_btn(if bookmarked { "Remove from Sidebar" } else { "Add to Sidebar" }, Message::ToggleBookmark(path.clone())));
                            item_count += 2;
                        } else if filesystem::is_archive(path) {
                            menu_col = menu_col.push(context_btn("Extract Here", Message::Extract(path.clone())));
                            item_count += 1;
                        }
                        menu_col = menu_col.push(context_btn("Compress to .zip", Message::CompressZip));
                        menu_col = menu_col.push(context_btn("Compress to .tar.gz", Message::Compress));
                        item_count += 2;
                        menu_col = menu_col.push(context_sep());
                        menu_col = menu_col.push(context_btn("Delete (Trash)", Message::Delete));
                        menu_col = menu_col.push(context_btn("Delete Permanently", Message::DeletePermanently));
                        item_count += 2;
                    }
                }
                ContextMenuKind::Background => {
                    if self.active().is_trash() {
                        menu_col = menu_col.push(context_btn("Empty Trash", Message::EmptyTrash));
                        item_count += 1;
                    } else {
                        menu_col = menu_col.push(context_btn("New Folder", Message::NewFolder));
                        menu_col = menu_col.push(context_btn("New File", Message::NewFile));
                        item_count += 2;
                        if has_clipboard {
                            menu_col = menu_col.push(context_btn("Paste", Message::Paste));
                            item_count += 1;
                        }
                        menu_col = menu_col.push(context_btn("Open Terminal Here", Message::OpenTerminalHere(self.active().path.clone())));
                        menu_col = menu_col.push(context_btn("Copy Path", Message::CopyPath(self.active().path.clone())));
                        let bookmarked = self.bookmarks.contains(&self.active().path);
                        menu_col = menu_col.push(context_btn(if bookmarked { "Remove from Sidebar" } else { "Add to Sidebar" }, Message::ToggleBookmark(self.active().path.clone())));
                        item_count += 3;
                    }
                    menu_col = menu_col.push(context_sep());
                    menu_col = menu_col.push(context_btn("Select All", Message::SelectAll));
                    item_count += 2;

                    let sort_label = |label: &str, by: SortBy| {
                        let t = if self.active().sort_by == by { format!("{}  {}", label, if self.active().sort_asc { "\u{25b2}" } else { "\u{25bc}" }) } else { label.to_string() };
                        context_btn(&t, Message::SortChanged(by))
                    };
                    menu_col = menu_col.push(context_sep());
                    menu_col = menu_col.push(sort_label("Sort by Name", SortBy::Name));
                    menu_col = menu_col.push(sort_label("Sort by Size", SortBy::Size));
                    menu_col = menu_col.push(sort_label("Sort by Modified", SortBy::Modified));
                    menu_col = menu_col.push(sort_label("Sort by Kind", SortBy::Kind));
                    let group_label = if self.active().group_folders { "Folders Grouped First \u{2713}" } else { "Folders Mixed With Files" };
                    menu_col = menu_col.push(context_btn(group_label, Message::ToggleGroupFolders));
                    item_count += 5;

                    menu_col = menu_col.push(context_sep());
                    let hidden_label = if self.active().show_hidden { "Hide Hidden Files" } else { "Show Hidden Files" };
                    menu_col = menu_col.push(context_btn(hidden_label, Message::ShowHiddenToggle));
                    menu_col = menu_col.push(context_btn("Refresh", Message::Refresh));
                    item_count += 3;
                }
            }
            menu_col = menu_col.push(context_sep());
            menu_col = menu_col.push(context_btn("Close Menu", Message::ContextMenuClose));
            item_count += 2;
        }

        let menu = container(menu_col).style(theme::dialog_box);
        let menu_width = 232.0;
        let menu_height = (item_count as f32) * 30.0 + 16.0;
        let max_x = (self.window_size.width - menu_width - 10.0).max(0.0);
        let max_y = (self.window_size.height - menu_height - 10.0).max(0.0);
        let x = self.context_menu_pos.x.clamp(0.0, max_x);
        let y = self.context_menu_pos.y.clamp(0.0, max_y);

        let overlay = row![
            Space::with_width(Length::Fixed(x)),
            column![Space::with_height(Length::Fixed(y)), menu, Space::with_height(Length::Fill)].width(Length::Shrink),
            Space::with_width(Length::Fill),
        ]
        .height(Length::Fill);

        let dismiss = mouse_area(container(Space::new(Length::Fill, Length::Fill))).on_press(Message::ContextMenuClose);
        stack![base, dismiss, overlay].into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            iced::event::listen_with(track_events),
            watcher_subscription(),
            thumbs_subscription(),
            jobs_subscription(),
            devices_subscription(),
        ];
        if let Some(rx) = &self.drop_rx {
            let _ = rx; // presence check only — the tick below always polls if Some
            subs.push(iced::time::every(std::time::Duration::from_millis(50)).map(|_| Message::DropTick));
        }
        Subscription::batch(subs)
    }
}

fn watcher_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, |mut sender| async move {
            loop {
                let batch = tokio::task::spawn_blocking(crate::watcher::next_batch).await.unwrap_or_default();
                if !batch.is_empty() {
                    let _ = sender.send(Message::WatcherBatch(batch)).await;
                }
            }
        })
    })
}

fn thumbs_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(8, |mut sender| async move {
            loop {
                let ready = tokio::task::spawn_blocking(crate::thumbs::next_ready).await.unwrap_or(None);
                if ready.is_some() {
                    let _ = sender.send(Message::ThumbnailTick).await;
                }
            }
        })
    })
}

/// Polls for removable drives being plugged/unplugged/mounted elsewhere.
/// `devices::list()` shells out to `lsblk` each call — cheap, but not
/// free enough to poll every frame, so this ticks every 2s and only ever
/// sends a message when the list actually changed (a `udev` event stream
/// would be the "properly" reactive way to do this; this is the
/// pragmatic version — same tradeoff as `watcher.rs` polling instead of
/// something fancier). Fixed/network drives are re-read alongside it —
/// cheap enough, and it means the same tick also catches a network share
/// getting mounted/unmounted from outside this app.
fn devices_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(4, |mut sender| async move {
            let mut last: Option<(Vec<Device>, Vec<DriveInfo>, Vec<DriveInfo>)> = None;
            loop {
                let current = tokio::task::spawn_blocking(|| {
                    let removable = crate::devices::list();
                    let (fixed, network) = filesystem::list_drives();
                    (removable, fixed, network)
                })
                .await
                .unwrap_or_default();

                if last.as_ref() != Some(&current) {
                    let (removable, fixed, network) = current.clone();
                    let _ = sender.send(Message::DevicesRefreshed(removable, fixed, network)).await;
                    last = Some(current);
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        })
    })
}

fn jobs_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(16, |mut sender| async move {
            loop {
                let ev = tokio::task::spawn_blocking(jobs::next_event).await.unwrap_or(None);
                if let Some(ev) = ev {
                    let _ = sender.send(Message::JobEvent(ev)).await;
                }
            }
        })
    })
}

fn load_task(tab_id: u64, path: PathBuf, gen: u64, show_hidden: bool) -> Task<Message> {
    Task::perform(
        async move { tokio::task::spawn_blocking(move || filesystem::read_dir_entries(&path, show_hidden)).await.unwrap_or_default() },
        move |entries| Message::TabLoaded(tab_id, gen, entries),
    )
}

fn shellexpand_home(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        home_dir().join(rest).to_string_lossy().to_string()
    } else if s == "~" {
        home_dir().to_string_lossy().to_string()
    } else {
        s.to_string()
    }
}

static CURSOR_EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static LAST_CURSOR_EVENT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn track_events(event: iced::Event, status: iced::event::Status, _window: iced::window::Id) -> Option<Message> {
    match event {
        iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
            let epoch = CURSOR_EPOCH.get_or_init(std::time::Instant::now);
            let now_ms = epoch.elapsed().as_millis() as u64;
            let last_ms = LAST_CURSOR_EVENT_MS.load(std::sync::atomic::Ordering::Relaxed);
            if now_ms.saturating_sub(last_ms) < 40 {
                return None;
            }
            LAST_CURSOR_EVENT_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);
            Some(Message::CursorMoved(position))
        }
        iced::Event::Window(iced::window::Event::Resized(size)) => Some(Message::WindowResized(size)),
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => Some(Message::PaneDragEnd),
        iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(m)) => Some(Message::ModifiersChanged(m)),
        // Only act on keys iced's own focused widget (a text_input, etc.)
        // left alone — Ignored means nothing else claimed it, so it's fair
        // game for list navigation / shortcuts. A focused search box, path
        // bar, or rename field marks these Captured and we stay out of it.
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) if status == iced::event::Status::Ignored => Some(Message::KeyPressed(key)),
        _ => None,
    }
}

fn context_btn(label: &str, msg: Message) -> Element<'static, Message> {
    let label = label.to_string();
    button(text(label).size(13)).on_press(msg).width(Length::Fill).style(theme::flat_button).into()
}

fn open_with_row<'a>(path: &PathBuf, mime: &str, app: &'a AppEntry, is_default: bool) -> Element<'a, Message> {
    let name_line = if is_default { format!("{}  (default)", app.name) } else { app.name.clone() };
    let open_btn = button(column![text(name_line).size(13), text(&app.comment).size(10).style(theme::muted_text)].spacing(1))
        .on_press(Message::OpenWith(path.clone(), app.exec.clone()))
        .width(Length::Fill)
        .style(theme::flat_button);

    let content: Element<Message> = if is_default {
        open_btn.into()
    } else {
        let default_btn = button(text("Set Default").size(11)).on_press(Message::OpenWithSetDefault(mime.to_string(), app.desktop_id.clone())).style(theme::flat_button).padding([2, 6]);
        row![open_btn, default_btn].spacing(6).align_y(iced::Alignment::Center).into()
    };
    content
}

fn detail_row<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    column![text(label).size(10).style(theme::muted_text), selectable_text(&value, 12)].spacing(1).into()
}

fn selectable_text<'a>(value: &str, size: u16) -> Element<'a, Message> {
    text_input("", value).on_input(|_| Message::Noop).size(size).padding(0).style(|_, _| text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border { color: Color::TRANSPARENT, width: 0.0, radius: 0.0.into() },
        icon: theme::p().text,
        placeholder: theme::p().muted,
        value: theme::p().text,
        selection: theme::p().accent,
    })
    .into()
}

fn drag_ghost_files<'a>(paths: &[PathBuf]) -> Element<'a, Message> {
    let label = if let [single] = paths {
        single.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        format!("{} items", paths.len())
    };
    let icon = if paths.first().is_some_and(|p| p.is_dir()) { "\u{f07b}" } else { "\u{f0f6}" };
    let pal = theme::p();
    container(row![text(icon).font(ICON_FONT).size(15).style(theme::normal_text), text(label).size(13).style(theme::normal_text)].spacing(6).align_y(iced::Alignment::Center))
        .padding([6, 10])
        .style(move |_| container::Style { background: Some(Background::Color(Color { a: 0.9, ..pal.accent })), border: Border { radius: 6.0.into(), ..Default::default() }, ..Default::default() })
        .into()
}

fn drag_ghost_line<'a>(base: Element<'a, Message>, x: f32) -> Element<'a, Message> {
    use iced::widget::stack;
    let line = container(Space::with_width(Length::Fixed(2.0))).height(Length::Fill).style(|_| container::Style { background: Some(Background::Color(theme::p().accent)), ..Default::default() });
    let overlay = row![Space::with_width(Length::Fixed(x)), line, Space::with_width(Length::Fill)].height(Length::Fill);
    stack![base, overlay].into()
}

/// Banner shown while a file is being dragged in from another app —
/// og-wayland reports enter/motion/leave, but not fine-grained position
/// relative to iced's own widget tree, so the drop target is "wherever the
/// window is" rather than a specific row; dropping copies into the current
/// tab's folder.
fn external_drop_overlay<'a>(base: Element<'a, Message>) -> Element<'a, Message> {
    use iced::widget::stack;
    let pal = theme::p();
    let banner = container(text("Drop to copy here").size(15).style(theme::normal_text))
        .padding(16)
        .style(move |_| container::Style {
            background: Some(Background::Color(Color { a: 0.92, ..pal.accent })),
            border: Border { radius: 8.0.into(), ..Default::default() },
            ..Default::default()
        });
    let overlay = container(container(banner).style(|_| container::Style::default()))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(theme::scrim);
    stack![base, overlay].into()
}

fn pane_divider(target: PaneDrag) -> Element<'static, Message> {
    mouse_area(
        container(Space::with_width(Length::Fixed(DIVIDER_WIDTH))).height(Length::Fill).style(|_| container::Style { background: Some(Background::Color(theme::p().border)), ..Default::default() }),
    )
    .interaction(iced::mouse::Interaction::ResizingHorizontally)
    .on_press(Message::PaneDragStart(target))
    .into()
}

fn context_sep() -> Element<'static, Message> {
    container(Space::with_height(1)).width(Length::Fill).style(theme::separator).into()
}

// Kept for potential re-use by the zoom step table (view code reads
// tab.card_size(), which already indexes ZOOM_STEPS — this just documents
// the constant is intentionally referenced from tab.rs, not dead).
#[allow(dead_code)]
const _: [u16; 6] = ZOOM_STEPS;
