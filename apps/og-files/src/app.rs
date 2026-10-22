//! App shell: four panes in a pane_grid (files | viewer+inspector / details),
//! keyboard shortcuts, the context menu, modal dialogs, file jobs (with
//! undo), the internal clipboard, drives and the folder watcher.
//!
//! Rule: `update` never blocks and `view` never touches the filesystem —
//! the two things that made og-files feel laggy. See SPEC.md §4.

use crate::archops::{self, ArchiveJob};
use crate::details::{self, Details};
use crate::devices::{self, Drive};
use crate::filetype::Kind;
use crate::inspector::{self, Inspector};
use crate::net::{self as netb, Conn, Discovered, LanEvent};
use crate::netui;
use crate::openwith;
use crate::ops::{self, JobControl, JobEvent, JobId, JobKind, JobSpec, Resolution, UndoOp};
use crate::sidebar::{self, Sidebar, Target};
use crate::tabs::{self, Tabs};
use crate::theme;
use crate::viewers::archive::{self, Format};
use iced::keyboard::{self, key, Key, Modifiers};
use iced::widget::pane_grid::{self, Axis, Configuration, PaneGrid};
use iced::widget::{
    button, center, checkbox, column, container, mouse_area, opaque, progress_bar, row, stack,
    text, text_input, Space,
};
use iced::{task, Element, Length, Point, Subscription, Task, Theme};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Files,
    Viewer,
    Inspector,
    Details,
}

/// Width of the files pane: normal (user-resizable), hidden, or "focus"
/// (fills most of the window for pure browsing/searching).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftSize {
    Normal,
    Collapsed,
    Focus,
}

const LEFT_RATIO: f32 = 0.28;
const FOCUS_RATIO: f32 = 0.85;
const INSPECTOR_RATIO: f32 = 0.76;
const DIALOG_INPUT: &str = "dialog-input";
const MENU_W: f32 = 230.0;

/// Files being dragged inside the app.
struct Drag {
    paths: Vec<PathBuf>,
    origin: Point,
    active: bool,
}

/// pointer travel before a press becomes a drag
const DRAG_START: f32 = 6.0;

struct JobUi {
    id: JobId,
    label: String,
    total_items: usize,
    total_bytes: u64,
    done_items: usize,
    done_bytes: u64,
    current: String,
    control: JobControl,
    spec: JobSpec,
    /// archive extract / compress (runs outside `ops`)
    arch: Option<ArchiveJob>,
}

/// State of the "Open with…" dialog (plain data copied from the registry).
#[derive(Debug, Clone)]
struct OpenWithDlg {
    files: Vec<PathBuf>,
    mime: String,
    /// (id, name), the default first
    recommended: Vec<(String, String)>,
    default: Option<String>,
    /// every listed app (id, name), and the indices matching `query`
    all: Vec<(String, String)>,
    filtered: Vec<usize>,
    query: String,
    show_all: bool,
    selected: Option<String>,
    set_default: bool,
}

#[derive(Debug, Clone)]
enum Dialog {
    Rename {
        path: PathBuf,
        name: String,
    },
    NewFolder {
        dir: PathBuf,
        name: String,
    },
    NewFile {
        dir: PathBuf,
        name: String,
        /// extension added when the typed name has none ("" = as typed)
        ext: &'static str,
    },
    ConfirmDelete {
        paths: Vec<PathBuf>,
    },
    ConfirmEmptyTrash,
    CloseTab {
        id: u64,
        title: String,
    },
    Conflict {
        job: JobId,
        src: PathBuf,
        dst: PathBuf,
        all: bool,
    },
    Connect(Box<netui::Form>),
    OpenWith(Box<OpenWithDlg>),
    Compress {
        sources: Vec<PathBuf>,
        dir: PathBuf,
        name: String,
        format: Format,
    },
    ExtractTo {
        archive: PathBuf,
        name: String,
    },
    ArchivePassword {
        job: ArchiveJob,
        pw: String,
    },
    NetPassword {
        conn: Conn,
        pw: String,
        remember: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    Copy,
    Cut,
    Paste,
    Duplicate,
    Rename,
    Trash,
    Delete,
    Restore,
    NewFolder,
    NewFile,
    NewText,
    NewMarkdown,
    Bookmark,
    CopyPath,
    Mount,
    Unmount,
    Eject,
    EmptyTrash,
    MakePdf,
    ConvertPdf,
    Compress,
    DiskUsage,
    OpenDefault,
    OpenWith,
    OpenTerminal,
    ExtractHere,
    ExtractTo,
    NetConnect,
    NetDisconnect,
    NetEdit,
    NetRemove,
    NetCopyUrl,
    NetForgetPassword,
}

pub struct App {
    panes: pane_grid::State<PaneKind>,
    left_split: Option<pane_grid::Split>,
    // viewer | inspector split; focus mode hides the inspector so it isn't
    // squeezed into a sliver next to the shrunken viewer
    inspector_split: Option<pane_grid::Split>,
    inspector_ratio: f32,
    left_ratio: f32,
    left_size: LeftSize,
    sidebar: Sidebar,
    tabs: Tabs,
    inspector: Inspector,
    details: Details,
    cursor: Point,
    menu: Option<(Point, Target)>,
    dialog: Option<Dialog>,
    jobs: Vec<JobUi>,
    undo: Vec<UndoOp>,
    /// (paths, cut)
    clipboard: Option<(Vec<PathBuf>, bool)>,
    watched: Vec<PathBuf>,
    watcher: Option<task::Handle>,
    drag: Option<Drag>,
    /// drag-and-drop with other apps (og-suite builds on Wayland)
    dnd: Option<crate::extdnd::Dnd>,
    win_size: iced::Size,
    /// files from another app are hovering over the window
    ext_hover: bool,
    /// pane under the pointer / pane that receives plain keys
    hover_pane: Option<PaneKind>,
    focus_pane: PaneKind,
    fullscreen: bool,
    net_saved: Vec<Conn>,
    net_mounted: Vec<(String, PathBuf)>,
    net_tailnet: Vec<Discovered>,
    net_lan: Vec<Discovered>,
    net_ssh: Vec<Discovered>,
    /// Err = rclone/fuse missing (shown in the connect dialog)
    net_backend: Result<(), String>,
    /// installed apps / MIME associations (loaded off the UI thread)
    apps: Option<Arc<openwith::Registry>>,
    /// "Open in <default app>" label for the open context menu
    menu_open_label: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Sidebar(sidebar::Msg),
    Tabs(tabs::Msg),
    Inspector(inspector::Msg),
    Details(details::Msg),
    PaneResized(pane_grid::ResizeEvent),
    ToggleLeft,
    ToggleFocus,
    Search,
    EditPath,
    Save,
    ToggleEdit,
    CloseTab,
    Modifiers(Modifiers),
    Cursor(Point),
    Key(sidebar::Key),
    Menu(MenuAction),
    MenuClose,
    BackgroundMenu,
    DialogInput(String),
    DialogFlag(bool),
    DialogOk,
    DialogResolve(Resolution),
    DialogCancel,
    Shortcut(MenuAction),
    Undo,
    Job(JobEvent),
    JobCancel(JobId),
    OpDone(Result<String, String>, Vec<PathBuf>),
    /// "New file" finished: refresh the folder, open the file to edit
    FileCreated(Result<PathBuf, String>, PathBuf),
    Drives(Result<Vec<Drive>, String>),
    DrivesChanged,
    DriveDone(Result<String, String>),
    DirsChanged(Vec<PathBuf>),
    Copied,
    MouseUp,
    /// the suite config changed (og-suite builds): re-read the palette
    ThemeChanged,
    NetLoaded(Vec<Conn>, Result<(), String>, Vec<Discovered>),
    NetMountedNow(Vec<(String, PathBuf)>),
    NetTailnet(Result<Vec<Discovered>, String>),
    NetLan(LanEvent),
    NetRefresh,
    NetNeedPassword(Conn, bool, bool),
    NetMountDone(Conn, Result<PathBuf, String>, bool),
    NetUnmountDone(Conn, Result<(), String>, bool),
    ConnForm(netui::FormMsg),
    ConnTest,
    ConnTested(Result<String, String>),
    ConnSave,
    ConnSaved(Result<Vec<Conn>, String>, Vec<String>, Option<String>, bool),
    /// a key without Ctrl: goes to the focused pane (viewer first, then files)
    RawKey(Key, Modifiers),
    /// pointer entered (true) / left (false) a pane
    PaneHover(PaneKind, bool),
    MouseDown,
    DndReady(Result<crate::extdnd::Dnd, String>),
    ExtDrop(crate::extdnd::DropEvent),
    CursorLeft,
    /// window size (logical), for spotting a drag that reaches the edge
    WinResized(iced::Size),
    AppsLoaded(Arc<openwith::Registry>),
    /// registry, files, MIME type of the first file
    OpenWithReady(Arc<openwith::Registry>, Vec<PathBuf>, String),
    OpenWithPick(String),
    OpenWithShowAll,
    CompressFormat(Format),
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let pane = |k| Box::new(Configuration::Pane(k));
        let panes = pane_grid::State::with_configuration(Configuration::Split {
            axis: Axis::Vertical,
            ratio: LEFT_RATIO,
            a: pane(PaneKind::Files),
            b: Box::new(Configuration::Split {
                axis: Axis::Horizontal,
                ratio: 0.74,
                a: Box::new(Configuration::Split {
                    axis: Axis::Vertical,
                    ratio: INSPECTOR_RATIO,
                    a: pane(PaneKind::Viewer),
                    b: pane(PaneKind::Inspector),
                }),
                b: pane(PaneKind::Details),
            }),
        });
        let (left_split, inspector_split) = match panes.layout() {
            pane_grid::Node::Split { id, b, .. } => {
                let inner = match b.as_ref() {
                    pane_grid::Node::Split { a, .. } => match a.as_ref() {
                        pane_grid::Node::Split { id, .. } => Some(*id),
                        _ => None,
                    },
                    _ => None,
                };
                (Some(*id), inner)
            }
            _ => (None, None),
        };
        let (sidebar, sidebar_task) = Sidebar::new();
        let details = Details::default();
        let mut app = Self {
            panes,
            left_split,
            inspector_split,
            inspector_ratio: INSPECTOR_RATIO,
            left_ratio: LEFT_RATIO,
            left_size: LeftSize::Normal,
            sidebar,
            tabs: Tabs::default(),
            inspector: Inspector::default(),
            details,
            cursor: Point::ORIGIN,
            menu: None,
            dialog: None,
            jobs: Vec::new(),
            undo: Vec::new(),
            clipboard: None,
            watched: Vec::new(),
            watcher: None,
            drag: None,
            dnd: None,
            win_size: iced::Size::ZERO,
            ext_hover: false,
            hover_pane: None,
            focus_pane: PaneKind::Files,
            fullscreen: false,
            net_saved: Vec::new(),
            net_mounted: Vec::new(),
            net_tailnet: Vec::new(),
            net_lan: Vec::new(),
            net_ssh: Vec::new(),
            net_backend: Ok(()),
            apps: None,
            menu_open_label: String::new(),
        };
        // og-files [--view tree|list|grid] [--search TEXT] [--focus] [--usage DIR] [PATH]
        // a folder PATH opens in the files pane, a file PATH in a tab
        let mut tasks = Vec::new();
        let mut args = std::env::args_os().skip(1).filter(|_| !cfg!(test));
        while let Some(a) = args.next() {
            match a.to_str() {
                Some("--view") => {
                    let mode = match args.next().as_ref().and_then(|v| v.to_str()) {
                        Some("list") => sidebar::Mode::List,
                        Some("grid") => sidebar::Mode::Grid,
                        _ => sidebar::Mode::Tree,
                    };
                    tasks.push(app.update(Message::Sidebar(sidebar::Msg::Mode(mode))));
                }
                Some("--search") => {
                    let q = args
                        .next()
                        .map(|q| q.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    tasks.push(app.sidebar.open_search().map(Message::Sidebar));
                    tasks.push(app.update(Message::Sidebar(sidebar::Msg::SearchInput(q))));
                }
                Some("--focus") => tasks.push(app.set_left(LeftSize::Focus)),
                // og-files --usage DIR: open the disk usage analyzer
                Some("--usage") => {
                    let d = args
                        .next()
                        .map(PathBuf::from)
                        .unwrap_or_else(crate::config::home);
                    let d = std::fs::canonicalize(&d).unwrap_or(d);
                    tasks.push(app.open_disk_usage(d));
                }
                _ => {
                    let p = PathBuf::from(a);
                    let p = std::fs::canonicalize(&p).unwrap_or(p);
                    let kind = Kind::of(&p, p.is_dir());
                    tasks.push(app.open(p, kind, true));
                }
            }
        }
        // debug-build screenshot hook: OGX_SHOW=menu|rename|delete|conflict|close
        #[cfg(debug_assertions)]
        if let Ok(show) = std::env::var("OGX_SHOW") {
            let p = crate::config::home().join("Documents");
            let t = Target {
                net: None,
                items: vec![(p.clone(), Kind::Folder)],
                dir: crate::config::home(),
                drive: None,
                in_trash: false,
                bookmarked: true,
            };
            match show.as_str() {
                "menu" => app.menu = Some((Point::new(120.0, 260.0), t)),
                "rename" => {
                    app.dialog = Some(Dialog::Rename {
                        path: p.clone(),
                        name: "Documents".into(),
                    })
                }
                "delete" => {
                    app.dialog = Some(Dialog::ConfirmDelete {
                        paths: vec![p.clone(), p.join("x")],
                    })
                }
                "conflict" => {
                    app.dialog = Some(Dialog::Conflict {
                        job: 0,
                        src: p.join("report.pdf"),
                        dst: crate::config::home().join("report.pdf"),
                        all: false,
                    })
                }
                "connect" => app.dialog = Some(Dialog::Connect(Box::new(netui::Form::new()))),
                "close" => {
                    app.dialog = Some(Dialog::CloseTab {
                        id: 0,
                        title: "notes.md".into(),
                    })
                }
                // menu / dialogs for the file OGX_SHOW_PATH
                "menu-file" | "openwith" | "compress" | "extract-to" | "password" => {
                    let f = std::env::var_os("OGX_SHOW_PATH")
                        .map(PathBuf::from)
                        .unwrap_or(p.clone());
                    let t = Target {
                        net: None,
                        items: vec![(f.clone(), Kind::of(&f, f.is_dir()))],
                        dir: f.parent().map(Path::to_path_buf).unwrap_or_default(),
                        drive: None,
                        in_trash: false,
                        bookmarked: false,
                    };
                    app.apps = Some(Arc::new(openwith::Registry::load(
                        &openwith::Env::from_env(),
                    )));
                    app.cursor = Point::new(120.0, 200.0);
                    match show.as_str() {
                        "menu-file" => app.show_menu(t),
                        "password" => {
                            app.dialog = Some(Dialog::ArchivePassword {
                                job: ArchiveJob::ExtractHere {
                                    archive: f,
                                    parent: PathBuf::new(),
                                    password: None,
                                },
                                pw: String::new(),
                            })
                        }
                        s => {
                            app.menu = Some((app.cursor, t));
                            let a = match s {
                                "openwith" => MenuAction::OpenWith,
                                "compress" => MenuAction::Compress,
                                _ => MenuAction::ExtractTo,
                            };
                            tasks.push(app.update(Message::Menu(a)));
                        }
                    }
                }
                _ => {}
            }
        }
        let boot = Task::batch([
            sidebar_task.map(Message::Sidebar),
            details::load().map(Message::Details),
            Task::perform(devices::list(), Message::Drives),
            crate::extdnd::connect().map(Message::DndReady),
            Task::perform(
                async {
                    tokio::task::spawn_blocking(|| {
                        (
                            netb::load_saved(),
                            netb::backend_available(),
                            netb::ssh_config_hosts(),
                        )
                    })
                    .await
                    .unwrap_or_else(|_| (Vec::new(), Err("failed".into()), Vec::new()))
                },
                |(c, b, ssh)| Message::NetLoaded(c, b, ssh),
            ),
            Task::perform(netb::discover_tailnet(), Message::NetTailnet),
            Self::load_apps(),
            Task::batch(tasks),
        ]);
        (app, boot)
    }

    pub fn theme(&self) -> Theme {
        theme::theme()
    }

    fn open(&mut self, path: PathBuf, kind: Kind, pin: bool) -> Task<Message> {
        if kind == Kind::Folder {
            return self.sidebar.reveal(path).map(Message::Sidebar);
        }
        if pin {
            self.details
                .record_for(format!("Opened {}", path.display()), Some(&path));
        }
        // opening a file from focus mode brings the viewer back into view
        let unfocus = if self.left_size == LeftSize::Focus && pin {
            self.set_left(LeftSize::Normal)
        } else {
            Task::none()
        };
        let (task, ev) = self.tabs.open(path, kind, pin);
        Task::batch([unfocus, task.map(Message::Tabs), self.on_tabs_event(ev)])
    }

    fn push_network(&mut self) {
        let mut found = self.net_tailnet.clone();
        found.extend(self.net_lan.iter().cloned());
        found.extend(self.net_ssh.iter().cloned());
        self.sidebar
            .set_network(self.net_saved.clone(), self.net_mounted.clone(), found);
    }

    fn refresh_mounted() -> Task<Message> {
        Task::perform(
            async {
                tokio::task::spawn_blocking(netb::mounted)
                    .await
                    .unwrap_or_default()
            },
            Message::NetMountedNow,
        )
    }

    fn is_mounted(&self, c: &Conn) -> bool {
        self.net_mounted.iter().any(|(id, _)| *id == c.id)
    }

    /// Mount `c`; asks for a password first if one is needed and none is
    /// remembered. `interactive` = show the folder afterwards.
    fn start_mount(&mut self, c: Conn, interactive: bool) -> Task<Message> {
        if let Err(e) = &self.net_backend {
            self.details.record(e.clone());
            return Task::none();
        }
        if c.auth == netb::Auth::Password {
            let c2 = c.clone();
            return Task::perform(netb::has_saved_password(c), move |has| {
                Message::NetNeedPassword(c2.clone(), !has, interactive)
            });
        }
        self.details.record(format!("Connecting to {}…", c.name));
        let c2 = c.clone();
        Task::perform(netb::mount(c, None, false), move |r| {
            Message::NetMountDone(c2.clone(), r, interactive)
        })
    }

    fn open_disk_usage(&mut self, root: PathBuf) -> Task<Message> {
        self.details.record_for(
            format!("Analyzing disk usage of {}", root.display()),
            Some(&root),
        );
        let unfocus = if self.left_size == LeftSize::Focus {
            self.set_left(LeftSize::Normal)
        } else {
            Task::none()
        };
        let (t, ev) = self.tabs.open_disk_usage(root);
        Task::batch([unfocus, t.map(Message::Tabs), self.on_tabs_event(ev)])
    }

    fn set_left(&mut self, size: LeftSize) -> Task<Message> {
        self.left_size = size;
        self.sidebar.focused = size == LeftSize::Focus;
        if let Some(split) = self.left_split {
            let ratio = match size {
                LeftSize::Normal => self.left_ratio,
                LeftSize::Collapsed => 0.0,
                LeftSize::Focus => FOCUS_RATIO,
            };
            self.panes.resize(split, ratio);
        }
        if let Some(split) = self.inspector_split {
            let ratio = if size == LeftSize::Focus {
                1.0
            } else {
                self.inspector_ratio
            };
            self.panes.resize(split, ratio);
        }
        Task::none()
    }

    fn on_tabs_event(&mut self, ev: tabs::Event) -> Task<Message> {
        match ev {
            tabs::Event::Status(s) => {
                self.details.record(s);
                Task::none()
            }
            tabs::Event::OpenFile(p) => {
                let k = Kind::of(&p, false);
                self.open(p, k, true)
            }
            tabs::Event::Reveal(p) => self.sidebar.reveal_path(p).map(Message::Sidebar),
            tabs::Event::Trash(paths) => self.start_job(JobSpec {
                kind: JobKind::Trash,
                sources: paths,
                dest: None,
            }),
            tabs::Event::Delete(paths) => {
                if !paths.is_empty() {
                    self.dialog = Some(Dialog::ConfirmDelete { paths });
                }
                Task::none()
            }
            tabs::Event::ConfirmClose(id, title) => {
                self.dialog = Some(Dialog::CloseTab { id, title });
                Task::none()
            }
            tabs::Event::Focus(target) => {
                self.details.focus = target.as_ref().map(|(p, _)| p.clone());
                self.inspector.focus(target).map(Message::Inspector)
            }
        }
    }

    // ------------------------------------------------------------ file ops

    fn start_job(&mut self, spec: JobSpec) -> Task<Message> {
        if spec.sources.is_empty() {
            return Task::none();
        }
        let (control, stream) = ops::start(spec.clone());
        self.jobs.push(JobUi {
            id: control.id,
            label: "Starting…".into(),
            total_items: 0,
            total_bytes: 0,
            done_items: 0,
            done_bytes: 0,
            current: String::new(),
            control,
            spec,
            arch: None,
        });
        Task::run(stream, Message::Job)
    }

    /// Archive extract / compress as a job in the Details list.
    fn start_archive(&mut self, job: ArchiveJob) -> Task<Message> {
        let sources = match &job {
            ArchiveJob::Compress { sources, .. } => sources.clone(),
            _ => vec![job.archive().to_path_buf()],
        };
        let spec = JobSpec {
            kind: JobKind::Copy,
            sources,
            dest: Some(job.out_dir()),
        };
        let (control, stream) = archops::start(job.clone());
        self.jobs.push(JobUi {
            id: control.id,
            label: job.label(),
            total_items: 0,
            total_bytes: 0,
            done_items: 0,
            done_bytes: 0,
            current: String::new(),
            control,
            spec,
            arch: Some(job),
        });
        Task::run(stream, Message::Job)
    }

    /// Installed apps + associations, read on a worker.
    fn load_apps() -> Task<Message> {
        Task::perform(
            async {
                tokio::task::spawn_blocking(|| {
                    Arc::new(openwith::Registry::load(&openwith::Env::from_env()))
                })
                .await
                .unwrap_or_default()
            },
            Message::AppsLoaded,
        )
    }

    /// The loaded registry, or load it now (on the worker running `f`).
    fn with_apps<T: Send + 'static>(
        &self,
        f: impl FnOnce(Arc<openwith::Registry>) -> T + Send + 'static,
        done: impl Fn(T) -> Message + Send + 'static,
    ) -> Task<Message> {
        let have = self.apps.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let reg = have.unwrap_or_else(|| {
                        Arc::new(openwith::Registry::load(&openwith::Env::from_env()))
                    });
                    f(reg)
                })
                .await
            },
            move |r| match r {
                Ok(v) => done(v),
                Err(e) => Message::OpDone(Err(e.to_string()), vec![]),
            },
        )
    }

    /// Show the context menu for `t` at the pointer; works out the label of
    /// "Open in <app>" from the file names (no disk access).
    fn show_menu(&mut self, t: Target) {
        self.menu_open_label = "Open in default app".into();
        if let Some(reg) = &self.apps {
            let mut names = t.items.iter().map(|(p, _)| {
                reg.mime_by_name(p)
                    .and_then(|m| reg.default_for(&m))
                    .map(|a| a.name.clone())
            });
            if let Some(Some(first)) = names.next() {
                if names.all(|n| n.as_deref() == Some(first.as_str())) {
                    self.menu_open_label = format!("Open in {first}");
                }
            }
        }
        self.menu = Some((self.cursor, t));
    }

    /// Folders whose listing a job touched.
    fn affected(spec: &JobSpec) -> Vec<PathBuf> {
        let mut d: Vec<PathBuf> = spec
            .sources
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        d.extend(spec.dest.clone());
        d.sort();
        d.dedup();
        d
    }

    fn on_job(&mut self, ev: JobEvent) -> Task<Message> {
        match ev {
            JobEvent::Started {
                id,
                label,
                total_items,
                total_bytes,
            } => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
                    j.label = label;
                    j.total_items = total_items;
                    j.total_bytes = total_bytes;
                }
                Task::none()
            }
            JobEvent::Progress {
                id,
                done_items,
                done_bytes,
                current,
            } => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
                    j.done_items = done_items;
                    j.done_bytes = done_bytes;
                    j.current = current
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                }
                Task::none()
            }
            JobEvent::Conflict { id, src, dst } => {
                self.dialog = Some(Dialog::Conflict {
                    job: id,
                    src,
                    dst,
                    all: false,
                });
                Task::none()
            }
            JobEvent::Finished { id, result, undo } => {
                let Some(i) = self.jobs.iter().position(|j| j.id == id) else {
                    return Task::none();
                };
                let job = self.jobs.remove(i);
                if matches!(&self.dialog, Some(Dialog::Conflict { job: j, .. }) if *j == id) {
                    self.dialog = None;
                }
                let mut focus = Task::none();
                match (result, job.arch.clone()) {
                    (Ok(s), Some(_)) => {
                        let out = archops::output_of(&undo).map(Path::to_path_buf);
                        let mut msg = match &out {
                            Some(o) => format!("{} → {}", job.label, o.display()),
                            None => format!("{} — done", job.label),
                        };
                        if let Some(e) = s.errors.first() {
                            msg += &format!(
                                " (skipped {} unsafe/unsupported, first: {e})",
                                s.errors.len()
                            );
                        }
                        self.details.record_for(msg, out.as_deref());
                        if let Some(u) = undo {
                            self.undo.push(u);
                        }
                    }
                    (Err(e), Some(a)) if a.is_extract() && archive::is_password_error(&e) => {
                        self.details.record(format!("{}: {e}", job.label));
                        if self.dialog.is_none() {
                            self.dialog = Some(Dialog::ArchivePassword {
                                job: a,
                                pw: String::new(),
                            });
                            focus = text_input::focus(text_input::Id::new(DIALOG_INPUT));
                        }
                    }
                    (result, _) => match result {
                        Ok(s) => {
                            let mut msg = format!("{} — {} done", job.label, s.items_done);
                            if s.items_skipped > 0 {
                                msg += &format!(", {} skipped", s.items_skipped);
                            }
                            if let Some(e) = s.errors.first() {
                                msg += &format!(", {} failed (first: {e})", s.errors.len());
                            }
                            if job.spec.kind == ops::JobKind::Trash && !ops::TRASH_UNDO {
                                msg +=
                                    " (can't be undone here: restore it from the Finder's Trash)";
                            }
                            let first = job.spec.sources.first().cloned();
                            self.details.record_for(msg, first.as_deref());
                            if let Some(u) = undo {
                                self.undo.push(u);
                            }
                        }
                        Err(e) => self.details.record(format!("{} failed: {e}", job.label)),
                    },
                }
                let dirs = Self::affected(&job.spec);
                // disk-usage tabs drop/rescan what the job removed or moved
                let mut changed = job.spec.sources.clone();
                changed.extend(dirs.iter().cloned());
                Task::batch([
                    focus,
                    self.sidebar.refresh(&dirs).map(Message::Sidebar),
                    self.tabs.fs_changed(&changed).map(Message::Tabs),
                ])
            }
        }
    }

    fn targets(&self) -> Vec<PathBuf> {
        match &self.menu {
            Some((_, t)) => t.items.iter().map(|(p, _)| p.clone()).collect(),
            None => self
                .sidebar
                .sel
                .paths()
                .into_iter()
                .filter(|p| p.as_os_str() != crate::tree::TRASH)
                .collect(),
        }
    }

    fn context_dir(&self) -> PathBuf {
        match &self.menu {
            Some((_, t)) => t.dir.clone(),
            None => self.sidebar.current_dir(),
        }
    }

    fn action(&mut self, a: MenuAction) -> Task<Message> {
        let items = self.targets();
        let dir = self.context_dir();
        let target = self.menu.take().map(|(_, t)| t);
        match a {
            MenuAction::Open => {
                let tasks: Vec<_> = items
                    .into_iter()
                    .map(|p| {
                        let k = Kind::of(&p, p.is_dir());
                        self.open(p, k, true)
                    })
                    .collect();
                Task::batch(tasks)
            }
            MenuAction::Copy | MenuAction::Cut => {
                if items.is_empty() {
                    return Task::none();
                }
                let cut = a == MenuAction::Cut;
                self.details.record(format!(
                    "{} {} item(s)",
                    if cut { "Cut" } else { "Copied" },
                    items.len()
                ));
                self.clipboard = Some((items, cut));
                Task::none()
            }
            MenuAction::Paste => match self.clipboard.clone() {
                Some((paths, cut)) => {
                    if cut {
                        self.clipboard = None;
                    }
                    self.start_job(JobSpec {
                        kind: if cut { JobKind::Move } else { JobKind::Copy },
                        sources: paths,
                        dest: Some(dir),
                    })
                }
                None => Task::none(),
            },
            MenuAction::Duplicate => {
                let groups: Vec<_> = items
                    .iter()
                    .filter_map(|p| p.parent().map(|d| (d.to_path_buf(), p.clone())))
                    .collect();
                let tasks: Vec<_> = groups
                    .into_iter()
                    .map(|(d, p)| {
                        self.start_job(JobSpec {
                            kind: JobKind::Copy,
                            sources: vec![p],
                            dest: Some(d),
                        })
                    })
                    .collect();
                Task::batch(tasks)
            }
            MenuAction::Rename => match items.as_slice() {
                [p] => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.dialog = Some(Dialog::Rename {
                        path: p.clone(),
                        name,
                    });
                    text_input::focus(text_input::Id::new(DIALOG_INPUT))
                }
                _ => Task::none(),
            },
            MenuAction::Trash => {
                if self.sidebar_in_trash(&target) {
                    return self.action(MenuAction::Delete);
                }
                self.start_job(JobSpec {
                    kind: JobKind::Trash,
                    sources: items,
                    dest: None,
                })
            }
            MenuAction::Delete => {
                if !items.is_empty() {
                    self.dialog = Some(Dialog::ConfirmDelete { paths: items });
                }
                Task::none()
            }
            MenuAction::Restore => {
                let infos: Vec<PathBuf> = items
                    .iter()
                    .filter_map(|p| self.sidebar.trash_entry(p))
                    .map(|e| e.info)
                    .collect();
                self.start_job(JobSpec {
                    kind: JobKind::Restore,
                    sources: infos,
                    dest: None,
                })
            }
            MenuAction::NewFolder => {
                self.dialog = Some(Dialog::NewFolder {
                    dir,
                    name: "New folder".into(),
                });
                Task::batch([
                    text_input::focus(text_input::Id::new(DIALOG_INPUT)),
                    text_input::select_all(text_input::Id::new(DIALOG_INPUT)),
                ])
            }
            MenuAction::NewFile | MenuAction::NewText | MenuAction::NewMarkdown => {
                let (name, ext) = match a {
                    MenuAction::NewText => ("Untitled", ".txt"),
                    MenuAction::NewMarkdown => ("Untitled", ".md"),
                    _ => ("untitled.txt", ""),
                };
                self.dialog = Some(Dialog::NewFile {
                    dir,
                    name: name.into(),
                    ext,
                });
                Task::batch([
                    text_input::focus(text_input::Id::new(DIALOG_INPUT)),
                    text_input::select_all(text_input::Id::new(DIALOG_INPUT)),
                ])
            }
            MenuAction::Bookmark => match items.as_slice() {
                [p] => self
                    .sidebar
                    .toggle_bookmark(p.clone(), p.is_dir())
                    .map(Message::Sidebar),
                _ => Task::none(),
            },
            MenuAction::CopyPath => {
                let s = items
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                iced::clipboard::write(s).map(|_: ()| Message::Copied)
            }
            MenuAction::Mount | MenuAction::Unmount | MenuAction::Eject => {
                let Some(d) = target.and_then(|t| t.drive) else {
                    return Task::none();
                };
                let id = d.id.clone();
                let label = d.label.clone();
                match a {
                    MenuAction::Mount => Task::perform(
                        async move {
                            devices::mount(&id)
                                .await
                                .map(|p| format!("Mounted {label} at {}", p.display()))
                        },
                        Message::DriveDone,
                    ),
                    MenuAction::Unmount => Task::perform(
                        async move {
                            devices::unmount(&id)
                                .await
                                .map(|_| format!("Unmounted {label}"))
                        },
                        Message::DriveDone,
                    ),
                    _ => Task::perform(
                        async move {
                            devices::eject(&id)
                                .await
                                .map(|_| format!("{label} can be removed safely"))
                        },
                        Message::DriveDone,
                    ),
                }
            }
            MenuAction::EmptyTrash => {
                self.dialog = Some(Dialog::ConfirmEmptyTrash);
                Task::none()
            }
            MenuAction::NetConnect
            | MenuAction::NetDisconnect
            | MenuAction::NetEdit
            | MenuAction::NetRemove
            | MenuAction::NetCopyUrl
            | MenuAction::NetForgetPassword => {
                let Some(c) = target.and_then(|t| t.net) else {
                    return Task::none();
                };
                match a {
                    MenuAction::NetConnect => self.start_mount(c, true),
                    MenuAction::NetDisconnect => {
                        let c2 = c.clone();
                        Task::perform(netb::unmount(c), move |r| {
                            Message::NetUnmountDone(c2.clone(), r, false)
                        })
                    }
                    MenuAction::NetEdit => {
                        let id = c.id.clone();
                        self.dialog = Some(Dialog::Connect(Box::new(netui::Form::from_conn(
                            &c,
                            Some(id),
                        ))));
                        Task::none()
                    }
                    MenuAction::NetRemove => {
                        if self.is_mounted(&c) {
                            let c2 = c.clone();
                            Task::perform(netb::unmount(c), move |r| {
                                Message::NetUnmountDone(c2.clone(), r, true)
                            })
                        } else {
                            self.update_inner(Message::NetUnmountDone(c, Ok(()), true))
                        }
                    }
                    MenuAction::NetForgetPassword => {
                        let name = c.name.clone();
                        Task::perform(netb::forget_password(c), move |r| {
                            Message::OpDone(
                                r.map(|_| format!("Forgot the saved password for {name}")),
                                vec![],
                            )
                        })
                    }
                    _ => iced::clipboard::write(netb::to_url(&c)).map(|_: ()| Message::Copied),
                }
            }
            MenuAction::DiskUsage => {
                let root = match items.as_slice() {
                    [p] if p.is_dir() => p.clone(),
                    _ => dir,
                };
                self.open_disk_usage(root)
            }
            MenuAction::Compress => {
                if items.is_empty() {
                    return Task::none();
                }
                let name = archops::default_name(&items);
                self.dialog = Some(Dialog::Compress {
                    sources: items,
                    dir,
                    name,
                    format: Format::Zip,
                });
                Task::batch([
                    text_input::focus(text_input::Id::new(DIALOG_INPUT)),
                    text_input::select_all(text_input::Id::new(DIALOG_INPUT)),
                ])
            }
            MenuAction::ExtractHere => {
                let tasks: Vec<_> = items
                    .into_iter()
                    .filter(|p| archive::supported(p))
                    .filter_map(|p| {
                        let parent = p.parent()?.to_path_buf();
                        Some(self.start_archive(ArchiveJob::ExtractHere {
                            archive: p,
                            parent,
                            password: None,
                        }))
                    })
                    .collect();
                Task::batch(tasks)
            }
            MenuAction::ExtractTo => match items.as_slice() {
                [p] if archive::supported(p) => {
                    self.dialog = Some(Dialog::ExtractTo {
                        archive: p.clone(),
                        name: archive::base_name(p),
                    });
                    Task::batch([
                        text_input::focus(text_input::Id::new(DIALOG_INPUT)),
                        text_input::select_all(text_input::Id::new(DIALOG_INPUT)),
                    ])
                }
                _ => Task::none(),
            },
            MenuAction::OpenTerminal => {
                let at = match items.as_slice() {
                    [p] if p.is_dir() => p.clone(),
                    _ => dir,
                };
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::terminal::open_here(&at)
                                .map(|(t, _)| format!("Opened {t} in {}", at.display()))
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    |r| Message::OpDone(r, vec![]),
                )
            }
            MenuAction::OpenDefault => {
                let files: Vec<PathBuf> = items.into_iter().filter(|p| !p.is_dir()).collect();
                if files.is_empty() {
                    return Task::none();
                }
                self.with_apps(
                    move |reg| openwith::open_default(&reg, &files).map(|l| l.join("; ")),
                    |r| Message::OpDone(r, vec![]),
                )
            }
            MenuAction::OpenWith => {
                let files: Vec<PathBuf> = items.into_iter().filter(|p| !p.is_dir()).collect();
                let Some(first) = files.first().cloned() else {
                    return Task::none();
                };
                if openwith::SYSTEM_OPEN_WITH {
                    // Windows: the system "Open with" dialog, one per file
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                for f in &files {
                                    openwith::open_with_dialog(f)?;
                                }
                                Ok(format!("Open with… {}", openwith::describe(&files)))
                            })
                            .await
                            .map_err(|e| e.to_string())?
                        },
                        |r| Message::OpDone(r, vec![]),
                    );
                }
                self.with_apps(
                    move |reg| {
                        let mime = reg.mime_of(&first);
                        (reg, files, mime)
                    },
                    |(reg, files, mime)| Message::OpenWithReady(reg, files, mime),
                )
            }
            MenuAction::ConvertPdf => {
                let files: Vec<PathBuf> = items
                    .into_iter()
                    .filter(|p| crate::viewers::pdf::tools::topdf::supported(p))
                    .collect();
                if files.is_empty() {
                    return Task::none();
                }
                Task::perform(
                    async move {
                        let mut last = None;
                        for p in &files {
                            let out = crate::viewers::convert::to_pdf(p.clone())
                                .await
                                .map_err(|e| format!("{}: {e}", p.display()))?;
                            last = Some(out);
                        }
                        Ok(match (files.len(), last) {
                            (1, Some(out)) => format!("Converted to {}", out.display()),
                            (n, _) => format!("Converted {n} files to PDF"),
                        })
                    },
                    move |r| Message::OpDone(r, vec![dir.clone()]),
                )
            }
            MenuAction::MakePdf => {
                let imgs: Vec<PathBuf> = items
                    .into_iter()
                    .filter(|p| Kind::of(p, false) == Kind::Image)
                    .collect();
                if imgs.is_empty() {
                    return Task::none();
                }
                let out_dir = dir.clone();
                Task::perform(
                    async move {
                        let mut data = Vec::new();
                        for p in &imgs {
                            data.push(
                                tokio::fs::read(p)
                                    .await
                                    .map_err(|e| format!("{}: {e}", p.display()))?,
                            );
                        }
                        let pdf = tokio::task::spawn_blocking(move || {
                            crate::viewers::pdf::tools::layout::images_to_pdf(&data, None, 0.0)
                        })
                        .await
                        .map_err(|e| e.to_string())??;
                        let out = tokio::task::spawn_blocking(move || {
                            ops::unique_name(&out_dir, "images.pdf")
                        })
                        .await
                        .map_err(|e| e.to_string())?;
                        crate::fsops::write_bytes(out.clone(), pdf).await?;
                        Ok(format!("Made {}", out.display()))
                    },
                    move |r| Message::OpDone(r, vec![dir.clone()]),
                )
            }
        }
    }

    /// Dropped `paths` on folder `target`: move (Ctrl = copy), or trash when
    /// dropped on the Trash.
    fn drop_on(&mut self, paths: Vec<PathBuf>, target: PathBuf) -> Task<Message> {
        if target.as_os_str() == crate::tree::TRASH {
            return self.start_job(JobSpec {
                kind: JobKind::Trash,
                sources: paths,
                dest: None,
            });
        }
        if self.sidebar.is_unmounted_drive(&target) {
            self.details.record("Mount the drive first");
            return Task::none();
        }
        // nothing into itself / its own subfolder / where it already is
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| !target.starts_with(p) && p.parent() != Some(target.as_path()))
            .collect();
        if paths.is_empty() {
            return Task::none();
        }
        let kind = if self.sidebar.mods.command() {
            JobKind::Copy
        } else {
            JobKind::Move
        };
        self.start_job(JobSpec {
            kind,
            sources: paths,
            dest: Some(target),
        })
    }

    /// Follow the active viewer's fullscreen wish: maximize the viewer pane
    /// and switch the window mode.
    fn sync_fullscreen(&mut self) -> Task<Message> {
        let want = self.tabs.active_fullscreen();
        if want == self.fullscreen {
            return Task::none();
        }
        self.fullscreen = want;
        let viewer = self
            .panes
            .iter()
            .find(|(_, k)| **k == PaneKind::Viewer)
            .map(|(p, _)| *p);
        if let Some(p) = viewer {
            if want {
                self.panes.maximize(p);
            } else {
                self.panes.restore();
            }
        }
        let mode = if want {
            iced::window::Mode::Fullscreen
        } else {
            iced::window::Mode::Windowed
        };
        iced::window::get_oldest().and_then(move |id| iced::window::change_mode(id, mode))
    }

    fn sidebar_in_trash(&self, t: &Option<Target>) -> bool {
        t.as_ref().is_some_and(|t| t.in_trash)
    }

    fn dialog_ok(&mut self) -> Task<Message> {
        let Some(d) = self.dialog.take() else {
            return Task::none();
        };
        match d {
            Dialog::Rename { path, name } => {
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            ops::rename(&path, name.trim())
                                .map(|p| format!("Renamed to {}", p.display()))
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    move |r| Message::OpDone(r, vec![dir.clone()]),
                )
            }
            Dialog::NewFolder { name, .. } | Dialog::NewFile { name, .. }
                if name.trim().is_empty() =>
            {
                Task::none()
            }
            Dialog::NewFolder { dir, name } => {
                let d2 = dir.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            ops::new_folder(&dir, name.trim())
                                .map(|p| format!("Created {}", p.display()))
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    move |r| Message::OpDone(r, vec![d2.clone()]),
                )
            }
            Dialog::NewFile { dir, name, ext } => {
                let name = with_ext(name.trim(), ext);
                // a new Markdown file starts with its name as the title
                let body = if name.ends_with(".md") {
                    let stem = name.trim_end_matches(".md");
                    format!("# {stem}\n\n")
                } else {
                    String::new()
                };
                let d2 = dir.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || ops::new_file_with(&dir, &name, &body))
                            .await
                            .map_err(|e| e.to_string())?
                    },
                    move |r| Message::FileCreated(r, d2.clone()),
                )
            }
            Dialog::ConfirmDelete { paths } => self.start_job(JobSpec {
                kind: JobKind::Delete,
                sources: paths,
                dest: None,
            }),
            Dialog::ConfirmEmptyTrash => Task::perform(
                async {
                    tokio::task::spawn_blocking(ops::empty_trash)
                        .await
                        .map_err(|e| e.to_string())?
                        .map(|n| format!("Emptied trash ({n} items)"))
                },
                |r| Message::OpDone(r, vec![PathBuf::from(crate::tree::TRASH)]),
            ),
            Dialog::CloseTab { id, .. } => match self.tabs.force_close(id) {
                Some(ev) => self.on_tabs_event(ev),
                None => Task::none(),
            },
            Dialog::Conflict { .. } => Task::none(),
            Dialog::Compress {
                sources,
                dir,
                name,
                format,
            } => {
                let name = name.trim();
                if name.is_empty() || name.contains('/') {
                    self.dialog = Some(Dialog::Compress {
                        sources,
                        dir,
                        name: name.to_string(),
                        format,
                    });
                    return Task::none();
                }
                // "x.zip" typed with zip picked: don't double the suffix
                let base = format.strip(name).unwrap_or_else(|| name.to_string());
                let dest = dir.join(format!("{base}{}", format.ext()));
                self.start_archive(ArchiveJob::Compress {
                    sources,
                    dest,
                    format,
                })
            }
            Dialog::ExtractTo { archive, name } => {
                let name = name.trim().to_string();
                let parent = archive.parent().map(Path::to_path_buf);
                match parent {
                    Some(parent) if !name.is_empty() && !name.contains('/') => {
                        self.start_archive(ArchiveJob::ExtractTo {
                            dest: parent.join(name),
                            archive,
                            password: None,
                        })
                    }
                    _ => {
                        self.dialog = Some(Dialog::ExtractTo { archive, name });
                        Task::none()
                    }
                }
            }
            Dialog::ArchivePassword { job, pw } => self.start_archive(job.with_password(pw)),
            Dialog::OpenWith(d) => {
                let (Some(id), Some(reg)) = (d.selected.clone(), self.apps.clone()) else {
                    self.dialog = Some(Dialog::OpenWith(d));
                    return Task::none();
                };
                let set_default = d.set_default;
                let reload = if set_default {
                    Self::load_apps()
                } else {
                    Task::none()
                };
                let launch = Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let app = reg.get(&id).ok_or("That app is gone")?;
                            let mut note = String::new();
                            if set_default {
                                openwith::set_default(&reg.user_list, &d.mime, &app.id)?;
                                note = format!(" (now the default for {})", d.mime);
                            }
                            openwith::launch(app, &d.files)?;
                            Ok(format!(
                                "Opened {} in {}{note}",
                                openwith::describe(&d.files),
                                app.name
                            ))
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    |r| Message::OpDone(r, vec![]),
                );
                launch.chain(reload)
            }
            Dialog::Connect(f) => {
                // Enter in the form = Connect / Save
                self.dialog = Some(Dialog::Connect(f));
                self.update_inner(Message::ConnSave)
            }
            Dialog::NetPassword { conn, pw, remember } => {
                self.details.record(format!("Connecting to {}…", conn.name));
                let c2 = conn.clone();
                Task::perform(netb::mount(conn, Some(pw), remember), move |r| {
                    Message::NetMountDone(c2.clone(), r, true)
                })
            }
        }
    }

    fn watcher_task(&mut self) -> Task<Message> {
        let dirs = self.sidebar.watched_dirs();
        if dirs == self.watched {
            return Task::none();
        }
        if let Some(h) = self.watcher.take() {
            h.abort();
        }
        self.watched = dirs.clone();
        if dirs.is_empty() {
            return Task::none();
        }
        let (t, h) = Task::run(devices::watch_dirs(dirs), Message::DirsChanged).abortable();
        self.watcher = Some(h);
        t
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        let t = self.update_inner(message);
        // after every update: persist new log lines, keep the watcher on the
        // folders that are actually on screen
        Task::batch([
            t,
            self.details.flush().map(Message::Details),
            self.watcher_task(),
        ])
    }

    fn sidebar_event(&mut self, ev: sidebar::Event) -> Task<Message> {
        match ev {
            sidebar::Event::Open(p, k, pin) => self.open(p, k, pin),
            sidebar::Event::Inspect(p, k) => {
                // sidebar entries like Trash or network items are pseudo
                // paths ("trash:", "net:…"), not files: nothing to inspect
                let target = p.is_absolute().then_some((p, k));
                self.details.focus = target.as_ref().map(|(p, _)| p.clone());
                self.inspector.focus(target).map(Message::Inspector)
            }
            sidebar::Event::Menu(t) => {
                self.show_menu(t);
                Task::none()
            }
            sidebar::Event::Mount(d) => {
                let id = d.id.clone();
                let label = d.label;
                Task::perform(
                    async move {
                        devices::mount(&id)
                            .await
                            .map(|p| format!("Mounted {label} at {}", p.display()))
                    },
                    Message::DriveDone,
                )
            }
            sidebar::Event::DiskUsage(root) => self.open_disk_usage(root),
            sidebar::Event::Net(act) => match act {
                crate::tree::NetAct::ConnectNew => {
                    self.dialog = Some(Dialog::Connect(Box::new(netui::Form::new())));
                    text_input::focus(text_input::Id::new(netui::URL_ID))
                }
                crate::tree::NetAct::Discovered(d) => {
                    self.dialog = Some(Dialog::Connect(Box::new(netui::Form::from_conn(
                        &d.to_conn(),
                        None,
                    ))));
                    Task::none()
                }
                crate::tree::NetAct::Mount(c) => self.start_mount(c, true),
            },
            sidebar::Event::EmptyTrash => {
                self.dialog = Some(Dialog::ConfirmEmptyTrash);
                Task::none()
            }
            sidebar::Event::Status(s) => {
                self.details.record(s);
                Task::none()
            }
            sidebar::Event::ToggleFocus => self.update_inner(Message::ToggleFocus),
        }
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Sidebar(m) => {
                let (task, ev) = self.sidebar.update(m);
                if let Some(paths) = self.sidebar.pressed.take() {
                    self.drag = Some(Drag {
                        paths,
                        origin: self.cursor,
                        active: false,
                    });
                }
                let follow = ev.map(|e| self.sidebar_event(e)).unwrap_or_else(Task::none);
                Task::batch([task.map(Message::Sidebar), follow])
            }
            Message::Key(k) => {
                if self.dialog.is_some() || self.menu.is_some() {
                    if k == sidebar::Key::Escape {
                        self.dialog = None;
                        self.menu = None;
                    }
                    return Task::none();
                }
                let (task, ev) = self.sidebar.key(k);
                let follow = ev.map(|e| self.sidebar_event(e)).unwrap_or_else(Task::none);
                Task::batch([task.map(Message::Sidebar), follow])
            }
            Message::Tabs(m) => {
                let (task, ev) = self.tabs.update(m);
                let follow = ev.map(|e| self.on_tabs_event(e)).unwrap_or_else(Task::none);
                Task::batch([task.map(Message::Tabs), follow, self.sync_fullscreen()])
            }
            Message::RawKey(key, mods) => {
                let esc = matches!(key.as_ref(), Key::Named(key::Named::Escape));
                if self.dialog.is_none() && self.menu.is_none() {
                    if self.focus_pane == PaneKind::Viewer {
                        if let Some(t) = self.tabs.key(&key, mods) {
                            return Task::batch([t.map(Message::Tabs), self.sync_fullscreen()]);
                        }
                    }
                    if esc && self.fullscreen {
                        self.tabs.set_fullscreen(false);
                        return self.sync_fullscreen();
                    }
                }
                match files_key(key, mods) {
                    Some(m) => self.update_inner(m),
                    None => Task::none(),
                }
            }
            Message::PaneHover(p, entered) => {
                if entered {
                    self.hover_pane = Some(p);
                } else if self.hover_pane == Some(p) {
                    // a leave can arrive after the next pane's enter
                    self.hover_pane = None;
                }
                Task::none()
            }
            Message::MouseDown => {
                if let Some(p) = self.hover_pane {
                    self.focus_pane = p;
                }
                Task::none()
            }
            Message::Inspector(m) => {
                self.inspector.update(m);
                Task::none()
            }
            Message::Details(m) => self.details.update(m).map(Message::Details),
            Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                self.panes.resize(split, ratio);
                if Some(split) == self.inspector_split && ratio < 0.97 {
                    self.inspector_ratio = ratio;
                }
                if Some(split) == self.left_split && ratio > 0.05 {
                    if ratio < 0.7 {
                        self.left_ratio = ratio;
                    }
                    self.left_size = LeftSize::Normal;
                    self.sidebar.focused = false;
                }
                Task::none()
            }
            Message::ToggleLeft => {
                let to = if self.left_size == LeftSize::Collapsed {
                    LeftSize::Normal
                } else {
                    LeftSize::Collapsed
                };
                self.set_left(to)
            }
            Message::ToggleFocus => {
                let to = if self.left_size == LeftSize::Focus {
                    LeftSize::Normal
                } else {
                    LeftSize::Focus
                };
                self.set_left(to)
            }
            Message::Search => {
                let show = if self.left_size == LeftSize::Collapsed {
                    self.set_left(LeftSize::Normal)
                } else {
                    Task::none()
                };
                Task::batch([show, self.sidebar.open_search().map(Message::Sidebar)])
            }
            Message::EditPath => self.sidebar.edit_path().map(Message::Sidebar),
            Message::Save => self.tabs.save_active().map(Message::Tabs),
            Message::ToggleEdit => self.tabs.toggle_edit_active().map(Message::Tabs),
            Message::CloseTab => match self.tabs.close_active() {
                Some(ev) => self.on_tabs_event(ev),
                None => Task::none(),
            },
            Message::Modifiers(m) => {
                self.sidebar.mods = m;
                Task::none()
            }
            Message::Cursor(p) => {
                self.cursor = p;
                if let Some(d) = &mut self.drag {
                    if !d.active
                        && (p.x - d.origin.x).hypot(p.y - d.origin.y) > DRAG_START
                        && !d.paths.is_empty()
                    {
                        d.active = true;
                        self.sidebar.sel.dragging = true;
                    }
                }
                // While the button is held, Wayland keeps the pointer grabbed
                // by our window: there is no CursorLeft until release, but
                // motion keeps coming with positions at/past the edge. That's
                // where an in-app drag becomes a drag to other apps.
                if self.drag.as_ref().is_some_and(|d| d.active) && at_edge(p, self.win_size) {
                    self.drag_out();
                }
                Task::none()
            }
            Message::WinResized(size) => {
                self.win_size = size;
                Task::none()
            }
            Message::MouseUp => {
                let drag = self.drag.take();
                let dragged = drag.as_ref().is_some_and(|d| d.active);
                let target = self.sidebar.drop_target();
                self.sidebar.mouse_up(dragged);
                match (drag, target) {
                    (Some(d), Some(t)) if d.active => self.drop_on(d.paths, t),
                    _ => Task::none(),
                }
            }
            Message::Menu(a) => self.action(a),
            Message::Shortcut(a) => {
                if self.dialog.is_some() {
                    return Task::none();
                }
                self.menu = None;
                self.action(a)
            }
            Message::MenuClose => {
                self.menu = None;
                Task::none()
            }
            Message::BackgroundMenu => {
                self.sidebar.sel.clear();
                let dir = self.sidebar.current_dir();
                self.show_menu(Target {
                    net: None,
                    items: vec![],
                    dir,
                    drive: None,
                    in_trash: false,
                    bookmarked: false,
                });
                Task::none()
            }
            Message::DialogInput(s) => {
                match &mut self.dialog {
                    Some(
                        Dialog::Rename { name, .. }
                        | Dialog::NewFolder { name, .. }
                        | Dialog::NewFile { name, .. }
                        | Dialog::Compress { name, .. }
                        | Dialog::ExtractTo { name, .. },
                    ) => *name = s,
                    Some(Dialog::NetPassword { pw, .. } | Dialog::ArchivePassword { pw, .. }) => {
                        *pw = s
                    }
                    Some(Dialog::OpenWith(d)) => {
                        let q = s.to_lowercase();
                        d.filtered = d
                            .all
                            .iter()
                            .enumerate()
                            .filter(|(_, (id, n))| {
                                q.is_empty()
                                    || n.to_lowercase().contains(&q)
                                    || id.to_lowercase().contains(&q)
                            })
                            .map(|(i, _)| i)
                            .collect();
                        d.query = s;
                        d.show_all = true;
                    }
                    _ => {}
                }
                Task::none()
            }
            Message::DialogFlag(b) => {
                match &mut self.dialog {
                    Some(Dialog::Conflict { all, .. }) => *all = b,
                    Some(Dialog::NetPassword { remember, .. }) => *remember = b,
                    Some(Dialog::OpenWith(d)) => d.set_default = b,
                    _ => {}
                }
                Task::none()
            }
            Message::DialogOk => self.dialog_ok(),
            Message::DialogResolve(r) => {
                if let Some(Dialog::Conflict { job, all, .. }) = self.dialog.take() {
                    if let Some(j) = self.jobs.iter().find(|j| j.id == job) {
                        j.control.resolve(r, all);
                    }
                }
                Task::none()
            }
            Message::DialogCancel => {
                if let Some(Dialog::Conflict { job, .. }) = self.dialog.take() {
                    if let Some(j) = self.jobs.iter().find(|j| j.id == job) {
                        j.control.resolve(Resolution::Cancel, false);
                    }
                }
                Task::none()
            }
            Message::Undo => match self.undo.pop() {
                Some(op) => {
                    self.details.record("Undo");
                    self.start_job(ops::undo_spec(&op))
                }
                None => {
                    self.details.record("Nothing to undo");
                    Task::none()
                }
            },
            Message::Job(ev) => self.on_job(ev),
            Message::JobCancel(id) => {
                if let Some(j) = self.jobs.iter().find(|j| j.id == id) {
                    j.control.cancel();
                }
                Task::none()
            }
            Message::FileCreated(r, dir) => {
                let refresh = self.sidebar.refresh(&[dir]).map(Message::Sidebar);
                match r {
                    Ok(p) => {
                        self.details
                            .record_for(format!("Created {}", p.display()), Some(&p));
                        let k = Kind::of(&p, false);
                        let (t, ev) = self.tabs.open_to_edit(p, k);
                        Task::batch([refresh, t.map(Message::Tabs), self.on_tabs_event(ev)])
                    }
                    Err(e) => {
                        self.details.record(format!("Failed: {e}"));
                        refresh
                    }
                }
            }
            Message::OpDone(r, dirs) => {
                match r {
                    Ok(s) => self.details.record(s),
                    Err(e) => self.details.record(format!("Failed: {e}")),
                }
                self.sidebar.refresh(&dirs).map(Message::Sidebar)
            }
            Message::Drives(r) => {
                match r {
                    Ok(d) => self.sidebar.set_drives(d),
                    Err(e) => self.details.record(format!("Drives: {e}")),
                }
                Task::none()
            }
            Message::DrivesChanged => Task::perform(devices::list(), Message::Drives),
            Message::DriveDone(r) => {
                match r {
                    Ok(s) => self.details.record(s),
                    Err(e) => self.details.record(format!("Drive: {e}")),
                }
                Task::perform(devices::list(), Message::Drives)
            }
            Message::DirsChanged(dirs) => self.sidebar.refresh(&dirs).map(Message::Sidebar),
            Message::DndReady(r) => match r {
                Ok(d) => {
                    self.dnd = Some(d.clone());
                    Task::run(crate::extdnd::drops(d), Message::ExtDrop)
                }
                // standalone build / not Wayland: in-app drag and drop only
                Err(_) => Task::none(),
            },
            Message::ExtDrop(ev) => {
                use crate::extdnd::DropEvent as E;
                match ev {
                    E::Enter { .. } | E::Motion { .. } => {
                        self.ext_hover = true;
                        Task::none()
                    }
                    E::Leave => {
                        self.ext_hover = false;
                        Task::none()
                    }
                    E::Dropped { paths, .. } => {
                        self.ext_hover = false;
                        // the compositor owns the pointer during an external drag,
                        // so per-folder targeting isn't available: copy into the
                        // folder being viewed
                        let dir = self.sidebar.current_dir();
                        let paths: Vec<PathBuf> = paths
                            .into_iter()
                            .filter(|p| p.parent() != Some(dir.as_path()))
                            .collect();
                        self.start_job(JobSpec {
                            kind: JobKind::Copy,
                            sources: paths,
                            dest: Some(dir),
                        })
                    }
                }
            }
            Message::CursorLeft => {
                // X11 and others do report leaving during a drag
                self.drag_out();
                Task::none()
            }
            Message::NetLoaded(conns, backend, ssh_hosts) => {
                self.net_saved = conns;
                self.net_backend = backend;
                self.net_ssh = ssh_hosts;
                self.push_network();
                // connect the ones marked "connect when og-files starts"
                let auto: Vec<Conn> = self
                    .net_saved
                    .iter()
                    .filter(|c| c.auto_mount)
                    .cloned()
                    .collect();
                let mut tasks = vec![Self::refresh_mounted()];
                if self.net_backend.is_ok() {
                    for c in auto {
                        tasks.push(self.start_mount(c, false));
                    }
                }
                Task::batch(tasks)
            }
            Message::NetMountedNow(m) => {
                self.net_mounted = m;
                self.push_network();
                Task::none()
            }
            Message::NetTailnet(r) => {
                if let Ok(peers) = r {
                    self.net_tailnet = peers;
                    self.push_network();
                }
                Task::none()
            }
            Message::NetLan(ev) => {
                match ev {
                    LanEvent::Found(d) => {
                        if !self
                            .net_lan
                            .iter()
                            .any(|x| x.name == d.name && x.proto == d.proto)
                        {
                            self.net_lan.push(d);
                        }
                    }
                    LanEvent::Lost { name, proto } => self
                        .net_lan
                        .retain(|x| !(x.name == name && x.proto == proto)),
                }
                self.push_network();
                Task::none()
            }
            Message::NetRefresh => Task::batch([
                Task::perform(netb::discover_tailnet(), Message::NetTailnet),
                Self::refresh_mounted(),
            ]),
            Message::NetNeedPassword(c, need, interactive) => {
                if need {
                    self.dialog = Some(Dialog::NetPassword {
                        conn: c,
                        pw: String::new(),
                        remember: true,
                    });
                    text_input::focus(text_input::Id::new(DIALOG_INPUT))
                } else {
                    self.details.record(format!("Connecting to {}…", c.name));
                    let c2 = c.clone();
                    Task::perform(netb::mount(c, None, false), move |r| {
                        Message::NetMountDone(c2.clone(), r, interactive)
                    })
                }
            }
            Message::NetMountDone(c, r, interactive) => {
                let follow = match r {
                    Ok(p) => {
                        self.details.record_for(
                            format!("Connected {} at {}", c.name, netui::short(&p)),
                            Some(&p),
                        );
                        self.net_mounted.retain(|(id, _)| *id != c.id);
                        self.net_mounted.push((c.id.clone(), p.clone()));
                        self.push_network();
                        if interactive {
                            self.sidebar.reveal(p).map(Message::Sidebar)
                        } else {
                            Task::none()
                        }
                    }
                    Err(e) => {
                        self.details
                            .record(format!("Couldn't connect {}: {e}", c.name));
                        Task::none()
                    }
                };
                Task::batch([follow, Self::refresh_mounted()])
            }
            Message::NetUnmountDone(c, r, then_remove) => {
                match r {
                    Ok(()) => {
                        self.details.record(format!("Disconnected {}", c.name));
                        self.net_mounted.retain(|(id, _)| *id != c.id);
                    }
                    Err(e) => self
                        .details
                        .record(format!("Couldn't disconnect {}: {e}", c.name)),
                }
                let mut tasks = vec![Self::refresh_mounted()];
                if then_remove {
                    let rest: Vec<Conn> = self
                        .net_saved
                        .iter()
                        .filter(|x| x.id != c.id)
                        .cloned()
                        .collect();
                    let prev: Vec<String> = self.net_saved.iter().map(|x| x.id.clone()).collect();
                    tasks.push(Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || netb::save_all(&rest))
                                .await
                                .map_err(|e| e.to_string())?
                        },
                        move |r| Message::ConnSaved(r, prev.clone(), None, false),
                    ));
                }
                self.push_network();
                Task::batch(tasks)
            }
            Message::ConnForm(m) => {
                if let Some(Dialog::Connect(f)) = &mut self.dialog {
                    f.update(m);
                }
                Task::none()
            }
            Message::ConnTest => {
                let Some(Dialog::Connect(f)) = &mut self.dialog else {
                    return Task::none();
                };
                match f.conn() {
                    Ok(c) => {
                        f.busy = true;
                        Task::perform(netb::test(c, f.password()), Message::ConnTested)
                    }
                    Err(e) => {
                        f.status = Some(Err(e));
                        Task::none()
                    }
                }
            }
            Message::ConnTested(r) => {
                if let Some(Dialog::Connect(f)) = &mut self.dialog {
                    f.busy = false;
                    f.status = Some(r);
                }
                Task::none()
            }
            Message::ConnSave => {
                let Some(Dialog::Connect(f)) = &mut self.dialog else {
                    return Task::none();
                };
                let c = match f.conn() {
                    Ok(c) => c,
                    Err(e) => {
                        f.status = Some(Err(e));
                        return Task::none();
                    }
                };
                let (pw, remember) = (f.password(), f.remember);
                let mut all = self.net_saved.clone();
                match all.iter_mut().find(|x| !c.id.is_empty() && x.id == c.id) {
                    Some(slot) => *slot = c,
                    None => all.push(c),
                }
                let prev: Vec<String> = self.net_saved.iter().map(|x| x.id.clone()).collect();
                self.dialog = None;
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || netb::save_all(&all))
                            .await
                            .map_err(|e| e.to_string())?
                    },
                    move |r| Message::ConnSaved(r, prev.clone(), pw.clone(), remember),
                )
            }
            Message::ConnSaved(r, prev, pw, remember) => match r {
                Ok(conns) => {
                    self.net_saved = conns;
                    self.push_network();
                    // a brand-new connection connects right away
                    match self
                        .net_saved
                        .iter()
                        .find(|c| !prev.contains(&c.id))
                        .cloned()
                    {
                        Some(new) if pw.is_some() || new.auth != netb::Auth::Password => {
                            if let Err(e) = &self.net_backend {
                                self.details.record(e.clone());
                                return Task::none();
                            }
                            self.details.record(format!("Connecting to {}…", new.name));
                            let c2 = new.clone();
                            Task::perform(netb::mount(new, pw, remember), move |r| {
                                Message::NetMountDone(c2.clone(), r, true)
                            })
                        }
                        Some(new) => self.start_mount(new, true),
                        None => Task::none(),
                    }
                }
                Err(e) => {
                    self.details
                        .record(format!("Couldn't save connections: {e}"));
                    Task::none()
                }
            },
            Message::ThemeChanged => {
                theme::reload();
                Task::none()
            }
            Message::Copied => {
                self.details.record("Path copied to clipboard");
                Task::none()
            }
            Message::AppsLoaded(reg) => {
                self.apps = Some(reg);
                Task::none()
            }
            Message::OpenWithReady(reg, files, mime) => {
                let c = reg.choices(&mime);
                let name = |id: &String| reg.get(id).map(|a| (id.clone(), a.name.clone()));
                let recommended: Vec<(String, String)> = c.apps.iter().filter_map(name).collect();
                let all: Vec<(String, String)> = reg
                    .listed()
                    .map(|a| (a.id.clone(), a.name.clone()))
                    .collect();
                let filtered = (0..all.len()).collect();
                let show_all = recommended.is_empty();
                self.dialog = Some(Dialog::OpenWith(Box::new(OpenWithDlg {
                    files,
                    mime: c.mime,
                    selected: c.default.clone(),
                    default: c.default,
                    recommended,
                    all,
                    filtered,
                    query: String::new(),
                    show_all,
                    set_default: false,
                })));
                self.apps = Some(reg);
                Task::none()
            }
            Message::OpenWithPick(id) => {
                if let Some(Dialog::OpenWith(d)) = &mut self.dialog {
                    if d.selected.as_deref() == Some(id.as_str()) {
                        // second click on the same app opens it
                        return self.dialog_ok();
                    }
                    d.selected = Some(id);
                }
                Task::none()
            }
            Message::OpenWithShowAll => {
                if let Some(Dialog::OpenWith(d)) = &mut self.dialog {
                    d.show_all = !d.show_all;
                    if d.show_all {
                        return text_input::focus(text_input::Id::new(DIALOG_INPUT));
                    }
                }
                Task::none()
            }
            Message::CompressFormat(f) => {
                if let Some(Dialog::Compress { format, .. }) = &mut self.dialog {
                    *format = f;
                }
                Task::none()
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::on_key_press(shortcut),
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                    Some(Message::Modifiers(m))
                }
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::Cursor(position))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Message::MouseUp),
                iced::Event::Mouse(iced::mouse::Event::CursorLeft) => Some(Message::CursorLeft),
                iced::Event::Window(
                    iced::window::Event::Resized(size) | iced::window::Event::Opened { size, .. },
                ) => Some(Message::WinResized(size)),
                iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) => {
                    Some(Message::MouseDown)
                }
                _ => None,
            }),
            Subscription::run(drive_events),
            Subscription::run(theme_events),
            Subscription::run(lan_events),
            // tailnet peers come and go; mounts can drop (network loss)
            iced::time::every(std::time::Duration::from_secs(60)).map(|_| Message::NetRefresh),
        ])
    }

    pub fn view(&self) -> Element<'_, Message> {
        let grid = PaneGrid::new(&self.panes, |_pane, kind, _maximized| {
            // no title bars: each pane's box carries its own heading
            let body: Element<'_, Message> = match kind {
                PaneKind::Files => mouse_area(self.sidebar.view().map(Message::Sidebar))
                    .on_right_press(Message::BackgroundMenu)
                    .into(),
                PaneKind::Viewer => self.tabs.view().map(Message::Tabs),
                PaneKind::Inspector => self.inspector.view().map(Message::Inspector),
                PaneKind::Details => self.details.view(self.jobs_view(), Message::Details),
            };
            let k = *kind;
            // the pane that receives plain keys gets an accent border
            let focused = k == self.focus_pane && matches!(k, PaneKind::Files | PaneKind::Viewer);
            let body = container(body)
                .style(move |t: &Theme| {
                    let mut st = theme::panel(t);
                    if focused {
                        st.border.color = t.extended_palette().primary.base.color.scale_alpha(0.6);
                    }
                    st
                })
                .width(Length::Fill)
                .height(Length::Fill);
            // which pane the pointer is over decides where plain keys go after a click
            let body = mouse_area(body)
                .on_enter(Message::PaneHover(k, true))
                .on_exit(Message::PaneHover(k, false));
            pane_grid::Content::new(body)
        })
        .spacing(6)
        .on_resize(8, Message::PaneResized);

        let base: Element<'_, Message> = container(grid)
            .padding(6)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut layers = vec![base];
        if let Some(d) = self.drag.as_ref().filter(|d| d.active) {
            layers.push(self.drag_view(d));
        }
        if self.ext_hover {
            let dir = self.sidebar.current_dir();
            let hint = container(text(format!("Drop to copy into {}", dir.display())).size(14))
                .padding([10, 16])
                .style(|t: &Theme| container::Style {
                    background: Some(t.extended_palette().primary.strong.color.into()),
                    text_color: Some(t.extended_palette().primary.strong.text),
                    border: iced::Border {
                        radius: 10.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            layers.push(
                container(hint)
                    .center_x(Length::Fill)
                    .padding(iced::Padding {
                        top: 40.0,
                        ..Default::default()
                    })
                    .into(),
            );
        }
        if let Some((at, t)) = &self.menu {
            layers.push(self.menu_view(*at, t));
        }
        if let Some(d) = &self.dialog {
            layers.push(self.dialog_view(d));
        }
        stack(layers).into()
    }

    fn drag_view(&self, d: &Drag) -> Element<'_, Message> {
        let what = match d.paths.as_slice() {
            [p] => format!(
                "\"{}\"",
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            ps => format!("{} items", ps.len()),
        };
        let verb = if self.sidebar.mods.command() {
            "Copy"
        } else {
            "Move"
        };
        let label = match self.sidebar.drop_target() {
            Some(t) if t.as_os_str() == crate::tree::TRASH => format!("Trash {what}"),
            Some(t) => format!(
                "{verb} {what} → {}",
                t.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| t.display().to_string())
            ),
            None => format!("{what}  (drop on a folder · Ctrl copies)"),
        };
        let chip = container(text(label).size(12))
            .padding([4, 10])
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().primary.strong.color.into()),
                text_color: Some(t.extended_palette().primary.strong.text),
                border: iced::Border {
                    radius: 12.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        // purely visual: doesn't take the pointer, so hover targets below still work
        container(chip)
            .padding(iced::Padding {
                top: self.cursor.y + 14.0,
                left: self.cursor.x + 14.0,
                ..Default::default()
            })
            .into()
    }

    fn jobs_view(&self) -> Element<'_, Message> {
        if self.jobs.is_empty() {
            return Space::with_height(0).into();
        }
        column(self.jobs.iter().map(|j| {
            let frac = if j.total_bytes > 0 {
                j.done_bytes as f32 / j.total_bytes as f32
            } else if j.total_items > 0 {
                j.done_items as f32 / j.total_items as f32
            } else {
                0.0
            };
            column![
                row![
                    text(j.label.clone()).size(12),
                    Space::with_width(Length::Fill),
                    text(format!(
                        "{}/{} · {}",
                        j.done_items, j.total_items, j.current
                    ))
                    .size(11)
                    .color(theme::MUTED),
                    button(text("Cancel").size(11))
                        .on_press(Message::JobCancel(j.id))
                        .style(button::text),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                progress_bar(0.0..=1.0, frac.clamp(0.0, 1.0)).height(6),
            ]
            .spacing(3)
            .into()
        }))
        .spacing(6)
        .into()
    }

    /// Hand an active in-app drag over to the compositor as a drag to
    /// other apps (browser upload areas, other file managers, …).
    fn drag_out(&mut self) {
        let (Some(d), Some(dnd)) = (self.drag.as_ref().filter(|d| d.active), &self.dnd) else {
            return;
        };
        let paths = d.paths.clone();
        if let Err(e) = crate::extdnd::start_drag(dnd, paths) {
            self.details.record(format!("Drag out: {e}"));
        }
        // the compositor takes the drag; iced won't see the release
        self.drag = None;
        self.sidebar.mouse_up(true);
    }

    fn menu_view<'a>(&'a self, at: Point, t: &'a Target) -> Element<'a, Message> {
        let mut entries: Vec<(&str, MenuAction, bool)> = Vec::new();
        let n = t.items.len();
        let has_clip = self.clipboard.is_some();
        if t.in_trash {
            if n > 0 {
                entries.push(("Restore", MenuAction::Restore, true));
                entries.push(("Delete permanently…", MenuAction::Delete, true));
            }
            entries.push(("Empty trash…", MenuAction::EmptyTrash, true));
        } else if n == 0 {
            entries.push(("New folder…", MenuAction::NewFolder, true));
            entries.push(("New text file…  Ctrl+N", MenuAction::NewText, true));
            entries.push(("New Markdown file…", MenuAction::NewMarkdown, true));
            entries.push(("New empty file…", MenuAction::NewFile, true));
            entries.push(("Paste", MenuAction::Paste, has_clip));
            entries.push((
                "Open terminal here  Ctrl+Alt+T",
                MenuAction::OpenTerminal,
                true,
            ));
            entries.push(("Analyze disk usage here", MenuAction::DiskUsage, true));
        } else {
            entries.push(("Open", MenuAction::Open, true));
            let files_only = t.items.iter().all(|(_, k)| *k != Kind::Folder);
            if files_only {
                entries.push((self.menu_open_label.as_str(), MenuAction::OpenDefault, true));
                entries.push(("Open with…", MenuAction::OpenWith, true));
            }
            if n == 1 && t.items[0].1 == Kind::Folder && t.drive.is_none() && t.net.is_none() {
                entries.push(("Open terminal here", MenuAction::OpenTerminal, true));
            }
            if files_only && t.items.iter().all(|(p, _)| archive::supported(p)) {
                entries.push(("Extract here", MenuAction::ExtractHere, true));
                if n == 1 {
                    entries.push(("Extract to folder…", MenuAction::ExtractTo, true));
                }
            }
            entries.push(("Copy  Ctrl+C", MenuAction::Copy, true));
            entries.push(("Cut  Ctrl+X", MenuAction::Cut, true));
            entries.push(("Paste into folder  Ctrl+V", MenuAction::Paste, has_clip));
            entries.push(("Duplicate", MenuAction::Duplicate, true));
            entries.push(("Rename…  F2", MenuAction::Rename, n == 1));
            entries.push(("Copy path", MenuAction::CopyPath, true));
            entries.push(("Compress…", MenuAction::Compress, true));
            if n == 1 && t.items[0].1 == Kind::Folder {
                entries.push(("Analyze disk usage", MenuAction::DiskUsage, true));
            }
            if n == 1 {
                entries.push((
                    if t.bookmarked {
                        "Remove bookmark"
                    } else {
                        "Add bookmark"
                    },
                    MenuAction::Bookmark,
                    true,
                ));
            }
            if t.items.iter().all(|(_, k)| *k == Kind::Image) {
                entries.push(("Combine into PDF", MenuAction::MakePdf, true));
            }
            if files_only
                && t.items
                    .iter()
                    .all(|(p, _)| crate::viewers::pdf::tools::topdf::supported(p))
            {
                entries.push(("Convert to PDF", MenuAction::ConvertPdf, true));
            }
            if let Some(c) = &t.net {
                if self.is_mounted(c) {
                    entries.push(("Disconnect", MenuAction::NetDisconnect, true));
                } else {
                    entries.push(("Connect", MenuAction::NetConnect, true));
                }
                entries.push(("Edit connection…", MenuAction::NetEdit, true));
                entries.push(("Copy address", MenuAction::NetCopyUrl, true));
                if c.auth == netb::Auth::Password {
                    entries.push(("Forget saved password", MenuAction::NetForgetPassword, true));
                }
                entries.push(("Remove connection", MenuAction::NetRemove, true));
            }
            if let Some(d) = &t.drive {
                if d.mount_point.is_none() {
                    entries.push(("Mount", MenuAction::Mount, !d.encrypted));
                } else if !d.system && devices::CAN_UNMOUNT {
                    entries.push(("Unmount", MenuAction::Unmount, true));
                }
                if d.ejectable {
                    entries.push(("Eject safely", MenuAction::Eject, true));
                }
            }
            entries.push(("Move to trash  Del", MenuAction::Trash, true));
            entries.push(("Delete permanently…  Shift+Del", MenuAction::Delete, true));
            entries.push(("New folder here…", MenuAction::NewFolder, true));
            entries.push(("New text file here…", MenuAction::NewText, true));
            entries.push(("New Markdown file here…", MenuAction::NewMarkdown, true));
        }
        let menu = container(
            column(entries.into_iter().map(|(label, a, enabled)| {
                button(text(label).size(13))
                    .on_press_maybe(enabled.then_some(Message::Menu(a)))
                    .style(button::text)
                    .width(Length::Fill)
                    .padding([4, 10])
                    .into()
            }))
            .spacing(1),
        )
        .width(MENU_W)
        .padding(4)
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
        // click anywhere else closes the menu
        let backdrop = mouse_area(Space::new(Length::Fill, Length::Fill))
            .on_press(Message::MenuClose)
            .on_right_press(Message::MenuClose);
        let placed = container(opaque(menu)).padding(iced::Padding {
            top: at.y,
            left: at.x,
            ..Default::default()
        });
        stack![backdrop, placed].into()
    }

    fn open_with_view<'a>(&'a self, d: &'a OpenWithDlg) -> Element<'a, Message> {
        let app_row = |id: &'a String, label: &'a str, is_default: bool| -> Element<'a, Message> {
            let selected = d.selected.as_deref() == Some(id.as_str());
            let mut r = row![text(label).size(13)].spacing(8);
            if is_default {
                r = r.push(text("default").size(11).color(theme::MUTED));
            }
            button(r)
                .on_press(Message::OpenWithPick(id.clone()))
                .style(if selected {
                    button::primary
                } else {
                    button::text
                })
                .width(Length::Fill)
                .padding([4, 10])
                .into()
        };
        let what = openwith::describe(&d.files);
        let mut col = column![
            text(format!("Open \"{what}\" with")).size(16),
            text(d.mime.clone()).size(12).color(theme::MUTED),
        ]
        .spacing(8);
        if d.recommended.is_empty() {
            col = col.push(
                text("No app is registered for this type.")
                    .size(13)
                    .color(theme::MUTED),
            );
        } else {
            col = col.push(
                iced::widget::scrollable(column(
                    d.recommended
                        .iter()
                        .map(|(id, n)| app_row(id, n, d.default.as_deref() == Some(id.as_str()))),
                ))
                .height(Length::Shrink),
            );
        }
        col = col.push(
            button(
                text(if d.show_all {
                    "Other app ▾"
                } else {
                    "Other app…"
                })
                .size(13),
            )
            .on_press(Message::OpenWithShowAll)
            .style(button::text),
        );
        if d.show_all {
            col = col.push(
                text_input("Search apps", &d.query)
                    .id(text_input::Id::new(DIALOG_INPUT))
                    .on_input(Message::DialogInput)
                    .on_submit(Message::DialogOk)
                    .size(13),
            );
            col = col.push(
                iced::widget::scrollable(column(
                    d.filtered
                        .iter()
                        .map(|&i| app_row(&d.all[i].0, &d.all[i].1, false)),
                ))
                .height(220),
            );
        }
        let ext = d
            .files
            .first()
            .and_then(|p| p.extension())
            .map(|e| format!(".{} files", e.to_string_lossy()))
            .unwrap_or_else(|| "this type".into());
        if openwith::CAN_SET_DEFAULT {
            col = col.push(
                checkbox(
                    format!("Always open {ext} ({}) with this app", d.mime),
                    d.set_default,
                )
                .on_toggle(Message::DialogFlag)
                .size(15)
                .text_size(13),
            );
        }
        col.push(
            row![
                Space::with_width(Length::Fill),
                button(text("Cancel"))
                    .on_press(Message::DialogCancel)
                    .style(button::secondary),
                button(text("Open")).on_press_maybe(d.selected.as_ref().map(|_| Message::DialogOk)),
            ]
            .spacing(8),
        )
        .into()
    }

    fn dialog_view<'a>(&'a self, d: &'a Dialog) -> Element<'a, Message> {
        let input = |label: &'a str,
                     value: &'a str,
                     ok: &'a str,
                     suffix: &'a str|
         -> Element<'a, Message> {
            // `suffix`: extension added on create unless the name has one
            let suffix = if suffix.is_empty() || value.contains('.') {
                ""
            } else {
                suffix
            };
            column![
                text(label).size(16),
                row![
                    text_input("", value)
                        .id(text_input::Id::new(DIALOG_INPUT))
                        .on_input(Message::DialogInput)
                        .on_submit(Message::DialogOk)
                        .size(14),
                    text(suffix).size(14).color(theme::MUTED),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
                row![
                    Space::with_width(Length::Fill),
                    button(text("Cancel"))
                        .on_press(Message::DialogCancel)
                        .style(button::secondary),
                    button(text(ok)).on_press(Message::DialogOk),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .into()
        };
        let confirm =
            |title: String, body: String, ok: &'a str, danger: bool| -> Element<'a, Message> {
                column![
                    text(title).size(16),
                    text(body).size(13).color(theme::MUTED),
                    row![
                        Space::with_width(Length::Fill),
                        button(text("Cancel"))
                            .on_press(Message::DialogCancel)
                            .style(button::secondary),
                        button(text(ok))
                            .on_press(Message::DialogOk)
                            .style(if danger {
                                button::danger
                            } else {
                                button::primary
                            }),
                    ]
                    .spacing(8),
                ]
                .spacing(12)
                .into()
            };
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        let body: Element<'a, Message> = match d {
            Dialog::Rename { name: v, .. } => input("Rename", v, "Rename", ""),
            Dialog::NewFolder { name: v, .. } => input("New folder", v, "Create", ""),
            Dialog::NewFile { name: v, ext, .. } => input(
                match *ext {
                    ".txt" => "New text file",
                    ".md" => "New Markdown file",
                    _ => "New file",
                },
                v,
                "Create",
                ext,
            ),
            Dialog::ConfirmDelete { paths } => confirm(
                if paths.len() == 1 {
                    format!("Delete \"{}\" permanently?", name(&paths[0]))
                } else {
                    format!("Delete {} items permanently?", paths.len())
                },
                "This can't be undone. Use Move to trash to keep a way back.".into(),
                "Delete",
                true,
            ),
            Dialog::Compress {
                sources,
                name: v,
                format,
                ..
            } => {
                let pills = row(Format::ALL.iter().map(|&f| {
                    button(text(f.ext()).size(13))
                        .on_press(Message::CompressFormat(f))
                        .style(if f == *format {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .padding([4, 10])
                        .into()
                }))
                .spacing(6);
                column![
                    text(match sources.as_slice() {
                        [one] => format!("Compress \"{}\"", name(one)),
                        s => format!("Compress {} items", s.len()),
                    })
                    .size(16),
                    row![
                        text_input("name", v)
                            .id(text_input::Id::new(DIALOG_INPUT))
                            .on_input(Message::DialogInput)
                            .on_submit(Message::DialogOk)
                            .size(14),
                        text(format.ext()).size(14).color(theme::MUTED),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                    pills,
                    row![
                        Space::with_width(Length::Fill),
                        button(text("Cancel"))
                            .on_press(Message::DialogCancel)
                            .style(button::secondary),
                        button(text("Create")).on_press(Message::DialogOk),
                    ]
                    .spacing(8),
                ]
                .spacing(12)
                .into()
            }
            Dialog::ExtractTo { archive, name: v } => column![
                text(format!("Extract \"{}\" to a new folder", name(archive))).size(16),
                text(format!(
                    "in {}",
                    archive
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ))
                .size(12)
                .color(theme::MUTED),
                text_input("folder name", v)
                    .id(text_input::Id::new(DIALOG_INPUT))
                    .on_input(Message::DialogInput)
                    .on_submit(Message::DialogOk)
                    .size(14),
                row![
                    Space::with_width(Length::Fill),
                    button(text("Cancel"))
                        .on_press(Message::DialogCancel)
                        .style(button::secondary),
                    button(text("Extract")).on_press(Message::DialogOk),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .into(),
            Dialog::ArchivePassword { job, pw } => column![
                text(format!("Password for \"{}\"", name(job.archive()))).size(16),
                text("The archive is encrypted.")
                    .size(12)
                    .color(theme::MUTED),
                text_input("password", pw)
                    .id(text_input::Id::new(DIALOG_INPUT))
                    .secure(true)
                    .on_input(Message::DialogInput)
                    .on_submit(Message::DialogOk)
                    .size(14),
                row![
                    Space::with_width(Length::Fill),
                    button(text("Cancel"))
                        .on_press(Message::DialogCancel)
                        .style(button::secondary),
                    button(text("Extract")).on_press(Message::DialogOk),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .into(),
            Dialog::OpenWith(d) => self.open_with_view(d),
            Dialog::Connect(f) => f.view(
                self.net_backend.as_ref().err().map(String::as_str),
                Message::ConnForm,
                |a| match a {
                    netui::Action::Test => Message::ConnTest,
                    netui::Action::Save => Message::ConnSave,
                    netui::Action::Cancel => Message::DialogCancel,
                },
            ),
            Dialog::NetPassword { conn, pw, remember } => column![
                text(format!("Password for {}", conn.name)).size(16),
                text(netb::to_url(conn)).size(12).color(theme::MUTED),
                text_input("password", pw)
                    .id(text_input::Id::new(DIALOG_INPUT))
                    .secure(true)
                    .on_input(Message::DialogInput)
                    .on_submit(Message::DialogOk)
                    .size(14),
                checkbox("Remember in the keyring", *remember)
                    .on_toggle(Message::DialogFlag)
                    .size(15)
                    .text_size(13),
                row![
                    Space::with_width(Length::Fill),
                    button(text("Cancel"))
                        .on_press(Message::DialogCancel)
                        .style(button::secondary),
                    button(text("Connect")).on_press(Message::DialogOk),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .into(),
            Dialog::ConfirmEmptyTrash => confirm(
                "Empty the trash?".into(),
                "Everything in every trash is deleted for good.".into(),
                "Empty trash",
                true,
            ),
            Dialog::CloseTab { title, .. } => confirm(
                format!("Close \"{title}\" without saving?"),
                "Your unsaved changes will be lost. Cancel and press Ctrl+S to keep them.".into(),
                "Discard & close",
                true,
            ),
            Dialog::Conflict { src, dst, all, .. } => column![
                text(format!("\"{}\" already exists", name(dst))).size(16),
                text(format!(
                    "in {}",
                    dst.parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ))
                .size(12)
                .color(theme::MUTED),
                text(format!("from {}", src.display()))
                    .size(12)
                    .color(theme::MUTED),
                checkbox("Do this for all conflicts", *all)
                    .on_toggle(Message::DialogFlag)
                    .size(15)
                    .text_size(13),
                row![
                    button(text("Stop"))
                        .on_press(Message::DialogResolve(Resolution::Cancel))
                        .style(button::secondary),
                    Space::with_width(Length::Fill),
                    button(text("Skip"))
                        .on_press(Message::DialogResolve(Resolution::Skip))
                        .style(button::secondary),
                    button(text("Keep both"))
                        .on_press(Message::DialogResolve(Resolution::KeepBoth))
                        .style(button::secondary),
                    button(text("Replace"))
                        .on_press(Message::DialogResolve(Resolution::Replace))
                        .style(button::danger),
                ]
                .spacing(8),
            ]
            .spacing(10)
            .into(),
        };
        let tall = matches!(d, Dialog::Connect(_));
        let body: Element<'a, Message> = if tall {
            iced::widget::scrollable(body).height(Length::Shrink).into()
        } else {
            body
        };
        let card = container(body)
            .width(if tall { 520 } else { 440 })
            .max_height(760)
            .padding(18)
            .style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.base.color.into()),
                border: iced::Border {
                    radius: 10.0.into(),
                    width: 1.0,
                    color: t.extended_palette().background.strong.color,
                },
                shadow: iced::Shadow {
                    color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.6),
                    offset: iced::Vector::new(0.0, 6.0),
                    blur_radius: 24.0,
                },
                ..Default::default()
            });
        let backdrop =
            container(Space::new(Length::Fill, Length::Fill)).style(|_| container::Style {
                background: Some(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.45).into()),
                ..Default::default()
            });
        stack![opaque(backdrop), center(opaque(card))].into()
    }
}

/// Servers and shares advertised on the LAN (Avahi).
fn lan_events() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::StreamExt;
    netb::discover_lan().map(Message::NetLan)
}

/// Suite config changes → theme reload (never fires in standalone builds).
fn theme_events() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::StreamExt;
    match theme::config_dir() {
        Some(d) => devices::watch_dirs(vec![d])
            .map(|_| Message::ThemeChanged)
            .boxed(),
        None => iced::futures::stream::empty().boxed(),
    }
}

fn drive_events() -> impl iced::futures::Stream<Item = Message> {
    use iced::futures::StreamExt;
    devices::watch_drives().map(|_| Message::DrivesChanged)
}

fn shortcut(key: Key, mods: Modifiers) -> Option<Message> {
    use sidebar::Key as K;
    if mods.command() {
        return match key.as_ref() {
            Key::Character("s") => Some(Message::Save),
            Key::Character("e") => Some(Message::ToggleEdit),
            Key::Character("w") => Some(Message::CloseTab),
            Key::Character("b") => Some(Message::ToggleLeft),
            Key::Character("f") => Some(Message::Search),
            Key::Character("l") => Some(Message::EditPath),
            Key::Character("c") => Some(Message::Shortcut(MenuAction::Copy)),
            Key::Character("x") => Some(Message::Shortcut(MenuAction::Cut)),
            Key::Character("v") => Some(Message::Shortcut(MenuAction::Paste)),
            Key::Character("z") => Some(Message::Undo),
            Key::Character("a") => Some(Message::Key(K::SelectAll)),
            Key::Character("t") | Key::Character("T") if mods.alt() => {
                Some(Message::Shortcut(MenuAction::OpenTerminal))
            }
            Key::Character("n") | Key::Character("N") if mods.shift() => {
                Some(Message::Shortcut(MenuAction::NewFolder))
            }
            Key::Character("n") => Some(Message::Shortcut(MenuAction::NewText)),
            Key::Named(key::Named::F4) => Some(Message::CloseTab),
            _ => None,
        };
    }
    if mods.alt() {
        return match key.as_ref() {
            Key::Named(key::Named::ArrowUp) => Some(Message::Key(K::Parent)),
            _ => None,
        };
    }
    Some(Message::RawKey(key, mods))
}

/// Plain keys for the files pane (also the fallback when the viewer
/// doesn't use a key).
fn files_key(key: Key, mods: Modifiers) -> Option<Message> {
    use sidebar::Key as K;
    match key.as_ref() {
        Key::Named(key::Named::ArrowUp) => Some(Message::Key(K::Up)),
        Key::Named(key::Named::ArrowDown) => Some(Message::Key(K::Down)),
        Key::Named(key::Named::ArrowLeft) => Some(Message::Key(K::Left)),
        Key::Named(key::Named::ArrowRight) => Some(Message::Key(K::Right)),
        Key::Named(key::Named::Home) => Some(Message::Key(K::Home)),
        Key::Named(key::Named::End) => Some(Message::Key(K::End)),
        Key::Named(key::Named::PageUp) => Some(Message::Key(K::PageUp)),
        Key::Named(key::Named::PageDown) => Some(Message::Key(K::PageDown)),
        Key::Named(key::Named::Enter) => Some(Message::Key(K::Enter)),
        Key::Named(key::Named::Backspace) => Some(Message::Key(K::Parent)),
        Key::Named(key::Named::Escape) => Some(Message::Key(K::Escape)),
        Key::Named(key::Named::F2) => Some(Message::Shortcut(MenuAction::Rename)),
        Key::Named(key::Named::Delete) if mods.shift() => {
            Some(Message::Shortcut(MenuAction::Delete))
        }
        Key::Named(key::Named::Delete) => Some(Message::Shortcut(MenuAction::Trash)),
        Key::Character(c) => c
            .chars()
            .next()
            .filter(|c| !c.is_control())
            .map(|c| Message::Key(K::Char(c))),
        _ => None,
    }
}

#[cfg(test)]
#[test]
fn new_file_names_get_extension_once() {
    assert_eq!(with_ext("Untitled", ".md"), "Untitled.md");
    assert_eq!(with_ext("notes.txt", ".md"), "notes.txt");
    assert_eq!(with_ext("todo", ".txt"), "todo.txt");
    assert_eq!(with_ext("Makefile", ""), "Makefile");
}

/// `name` plus `ext` unless the name already has an extension.
fn with_ext(name: &str, ext: &str) -> String {
    if ext.is_empty() || Path::new(name).extension().is_some() {
        name.to_string()
    } else {
        format!("{name}{ext}")
    }
}

/// Is `p` at (within 2 px of) or past the window's edge? Unknown size → no.
fn at_edge(p: Point, size: iced::Size) -> bool {
    const M: f32 = 2.0;
    size.width > 0.0
        && size.height > 0.0
        && (p.x <= M || p.y <= M || p.x >= size.width - M || p.y >= size.height - M)
}

#[cfg(test)]
#[test]
fn drag_out_edge_detection() {
    let s = iced::Size::new(800.0, 600.0);
    assert!(!at_edge(Point::new(400.0, 300.0), s));
    assert!(at_edge(Point::new(-5.0, 300.0), s));
    assert!(at_edge(Point::new(400.0, 601.0), s));
    assert!(at_edge(Point::new(799.0, 10.0), s));
    assert!(!at_edge(Point::new(-5.0, 300.0), iced::Size::ZERO));
}
