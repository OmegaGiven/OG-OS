//! App shell: four panes in a pane_grid (files | viewer+inspector / details),
//! keyboard shortcuts, the context menu, modal dialogs, file jobs (with
//! undo), the internal clipboard, drives and the folder watcher.
//!
//! Rule: `update` never blocks and `view` never touches the filesystem —
//! the two things that made og-files feel laggy. See SPEC.md §4.

use crate::details::{self, Details};
use crate::devices::{self, Drive};
use crate::filetype::Kind;
use crate::inspector::{self, Inspector};
use crate::ops::{self, JobControl, JobEvent, JobId, JobKind, JobSpec, Resolution, UndoOp};
use crate::sidebar::{self, Sidebar, Target};
use crate::tabs::{self, Tabs};
use crate::theme;
use iced::keyboard::{self, key, Key, Modifiers};
use iced::widget::pane_grid::{self, Axis, Configuration, PaneGrid};
use iced::widget::{
    button, center, checkbox, column, container, mouse_area, opaque, progress_bar, row, stack,
    text, text_input, Space,
};
use iced::{task, Element, Length, Point, Subscription, Task, Theme};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneKind {
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
    Bookmark,
    CopyPath,
    Mount,
    Unmount,
    Eject,
    EmptyTrash,
    MakePdf,
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
}

#[derive(Debug, Clone)]
pub enum Message {
    Sidebar(sidebar::Msg),
    Tabs(tabs::Msg),
    Inspector(inspector::Msg),
    Details(details::Msg),
    PaneResized(pane_grid::ResizeEvent),
    PaneDragged(pane_grid::DragEvent),
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
    Drives(Result<Vec<Drive>, String>),
    DrivesChanged,
    DriveDone(Result<String, String>),
    DirsChanged(Vec<PathBuf>),
    Copied,
    MouseUp,
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
        };
        // og-explorer [--view tree|list|grid] [--search TEXT] [--focus] [PATH]
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
                "close" => {
                    app.dialog = Some(Dialog::CloseTab {
                        id: 0,
                        title: "notes.md".into(),
                    })
                }
                _ => {}
            }
        }
        let boot = Task::batch([
            sidebar_task.map(Message::Sidebar),
            details::load().map(Message::Details),
            Task::perform(devices::list(), Message::Drives),
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
        });
        Task::run(stream, Message::Job)
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
                match result {
                    Ok(s) => {
                        let mut msg = format!("{} — {} done", job.label, s.items_done);
                        if s.items_skipped > 0 {
                            msg += &format!(", {} skipped", s.items_skipped);
                        }
                        if let Some(e) = s.errors.first() {
                            msg += &format!(", {} failed (first: {e})", s.errors.len());
                        }
                        let first = job.spec.sources.first().cloned();
                        self.details.record_for(msg, first.as_deref());
                        if let Some(u) = undo {
                            self.undo.push(u);
                        }
                    }
                    Err(e) => self.details.record(format!("{} failed: {e}", job.label)),
                }
                let dirs = Self::affected(&job.spec);
                self.sidebar.refresh(&dirs).map(Message::Sidebar)
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
            MenuAction::NewFile => {
                self.dialog = Some(Dialog::NewFile {
                    dir,
                    name: "untitled.txt".into(),
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
            Dialog::NewFolder { dir, name } | Dialog::NewFile { dir, name }
                if name.trim().is_empty() =>
            {
                let _ = dir;
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
            Dialog::NewFile { dir, name } => {
                let d2 = dir.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            ops::new_file(&dir, name.trim())
                                .map(|p| format!("Created {}", p.display()))
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    move |r| Message::OpDone(r, vec![d2.clone()]),
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
                self.details.focus = Some(p.clone());
                self.inspector.focus(Some((p, k))).map(Message::Inspector)
            }
            sidebar::Event::Menu(t) => {
                self.menu = Some((self.cursor, t));
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
                Task::batch([task.map(Message::Tabs), follow])
            }
            Message::Inspector(m) => {
                self.inspector.update(m);
                Task::none()
            }
            Message::Details(m) => {
                self.details.update(m);
                Task::none()
            }
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
            // panes can be dragged by their title bars to rearrange the
            // layout — the sketch's "swap direction if better"
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                self.panes.drop(pane, target);
                Task::none()
            }
            Message::PaneDragged(_) => Task::none(),
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
                self.menu = Some((
                    self.cursor,
                    Target {
                        items: vec![],
                        dir,
                        drive: None,
                        in_trash: false,
                        bookmarked: false,
                    },
                ));
                Task::none()
            }
            Message::DialogInput(s) => {
                match &mut self.dialog {
                    Some(
                        Dialog::Rename { name, .. }
                        | Dialog::NewFolder { name, .. }
                        | Dialog::NewFile { name, .. },
                    ) => *name = s,
                    _ => {}
                }
                Task::none()
            }
            Message::DialogFlag(b) => {
                if let Some(Dialog::Conflict { all, .. }) = &mut self.dialog {
                    *all = b;
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
            Message::Copied => {
                self.details.record("Path copied to clipboard");
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
                _ => None,
            }),
            Subscription::run(drive_events),
        ])
    }

    pub fn view(&self) -> Element<'_, Message> {
        let grid = PaneGrid::new(&self.panes, |_pane, kind, _maximized| {
            let (title, body): (&str, Element<'_, Message>) = match kind {
                PaneKind::Files => (
                    "Files  ·  Ctrl+B hide  ·  Ctrl+F search  ·  Ctrl+L path",
                    mouse_area(self.sidebar.view().map(Message::Sidebar))
                        .on_right_press(Message::BackgroundMenu)
                        .into(),
                ),
                PaneKind::Viewer => ("Viewer", self.tabs.view().map(Message::Tabs)),
                PaneKind::Inspector => ("Inspector", self.inspector.view().map(Message::Inspector)),
                PaneKind::Details => (
                    "Details",
                    self.details.view(self.jobs_view(), Message::Details),
                ),
            };
            let body = container(body)
                .style(theme::panel)
                .width(Length::Fill)
                .height(Length::Fill);
            pane_grid::Content::new(body).title_bar(
                pane_grid::TitleBar::new(text(title).size(11).color(theme::MUTED)).padding([2, 10]),
            )
        })
        .spacing(8)
        .on_resize(8, Message::PaneResized)
        .on_drag(Message::PaneDragged);

        let base: Element<'_, Message> = container(grid)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut layers = vec![base];
        if let Some(d) = self.drag.as_ref().filter(|d| d.active) {
            layers.push(self.drag_view(d));
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
            entries.push(("New file…", MenuAction::NewFile, true));
            entries.push(("Paste", MenuAction::Paste, has_clip));
        } else {
            entries.push(("Open", MenuAction::Open, true));
            entries.push(("Copy  Ctrl+C", MenuAction::Copy, true));
            entries.push(("Cut  Ctrl+X", MenuAction::Cut, true));
            entries.push(("Paste into folder  Ctrl+V", MenuAction::Paste, has_clip));
            entries.push(("Duplicate", MenuAction::Duplicate, true));
            entries.push(("Rename…  F2", MenuAction::Rename, n == 1));
            entries.push(("Copy path", MenuAction::CopyPath, true));
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
            if let Some(d) = &t.drive {
                if d.mount_point.is_none() {
                    entries.push(("Mount", MenuAction::Mount, !d.encrypted));
                } else if !d.system {
                    entries.push(("Unmount", MenuAction::Unmount, true));
                }
                if d.ejectable {
                    entries.push(("Eject safely", MenuAction::Eject, true));
                }
            }
            entries.push(("Move to trash  Del", MenuAction::Trash, true));
            entries.push(("Delete permanently…  Shift+Del", MenuAction::Delete, true));
            entries.push(("New folder here…", MenuAction::NewFolder, true));
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

    fn dialog_view<'a>(&'a self, d: &'a Dialog) -> Element<'a, Message> {
        let input = |label: &'a str, value: &'a str, ok: &'a str| -> Element<'a, Message> {
            column![
                text(label).size(16),
                text_input("", value)
                    .id(text_input::Id::new(DIALOG_INPUT))
                    .on_input(Message::DialogInput)
                    .on_submit(Message::DialogOk)
                    .size(14),
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
            Dialog::Rename { name: v, .. } => input("Rename", v, "Rename"),
            Dialog::NewFolder { name: v, .. } => input("New folder", v, "Create"),
            Dialog::NewFile { name: v, .. } => input("New file", v, "Create"),
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
        let card = container(body)
            .width(440)
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
            Key::Character("n") | Key::Character("N") if mods.shift() => {
                Some(Message::Shortcut(MenuAction::NewFolder))
            }
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
