//! App shell: four panes in a pane_grid (files | viewer+inspector / details),
//! keyboard shortcuts, and routing of child events.
//!
//! Rule: `update` never blocks and `view` never touches the filesystem —
//! the two things that made og-files feel laggy. See SPEC.md §4.

use crate::details::Details;
use crate::filetype::Kind;
use crate::inspector::{self, Inspector};
use crate::sidebar::{self, Sidebar};
use crate::tabs::{self, Tabs};
use crate::theme;
use iced::keyboard::{self, key, Key, Modifiers};
use iced::widget::pane_grid::{self, Axis, Configuration, PaneGrid};
use iced::widget::{container, text};
use iced::{Element, Length, Subscription, Task, Theme};
use std::path::PathBuf;

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
}

#[derive(Debug, Clone)]
pub enum Message {
    Sidebar(sidebar::Msg),
    Tabs(tabs::Msg),
    Inspector(inspector::Msg),
    PaneResized(pane_grid::ResizeEvent),
    PaneDragged(pane_grid::DragEvent),
    ToggleLeft,
    ToggleFocus,
    Search,
    Save,
    ToggleEdit,
    CloseTab,
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
                ratio: 0.78,
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
        let mut details = Details::default();
        details.record("Started");
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
        };
        // og-explorer [--view tree|list|grid] [--search TEXT] [--focus] [PATH]
        // a folder PATH opens in the files pane, a file PATH in a tab
        let mut tasks = Vec::new();
        let mut args = std::env::args_os().skip(1);
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
                    tasks.push(app.open(p, kind));
                }
            }
        }
        let arg_task = Task::batch(tasks);
        (
            app,
            Task::batch([sidebar_task.map(Message::Sidebar), arg_task]),
        )
    }

    pub fn theme(&self) -> Theme {
        theme::theme()
    }

    fn open(&mut self, path: PathBuf, kind: Kind) -> Task<Message> {
        if kind == Kind::Folder {
            return self.sidebar.reveal(path).map(Message::Sidebar);
        }
        self.details.record(format!("Opened {}", path.display()));
        // opening a file from focus mode brings the viewer back into view
        let unfocus = if self.left_size == LeftSize::Focus {
            self.set_left(LeftSize::Normal)
        } else {
            Task::none()
        };
        let (task, ev) = self.tabs.open(path, kind);
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
            tabs::Event::Focus(target) => self.inspector.focus(target).map(Message::Inspector),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Sidebar(m) => {
                let (task, ev) = self.sidebar.update(m);
                let follow = match ev {
                    Some(sidebar::Event::Open(p, k)) => self.open(p, k),
                    Some(sidebar::Event::Status(s)) => {
                        self.details.record(s);
                        Task::none()
                    }
                    Some(sidebar::Event::ToggleFocus) => self.update(Message::ToggleFocus),
                    None => Task::none(),
                };
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
            Message::Save => self.tabs.save_active().map(Message::Tabs),
            Message::ToggleEdit => self.tabs.toggle_edit_active().map(Message::Tabs),
            Message::CloseTab => match self.tabs.close_active() {
                Some(ev) => self.on_tabs_event(ev),
                None => Task::none(),
            },
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        keyboard::on_key_press(shortcut)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let grid = PaneGrid::new(&self.panes, |_pane, kind, _maximized| {
            let (title, body): (&str, Element<'_, Message>) = match kind {
                PaneKind::Files => (
                    "Files  ·  Ctrl+B hide  ·  Ctrl+F search",
                    self.sidebar.view().map(Message::Sidebar),
                ),
                PaneKind::Viewer => ("Viewer", self.tabs.view().map(Message::Tabs)),
                PaneKind::Inspector => ("Inspector", self.inspector.view().map(Message::Inspector)),
                PaneKind::Details => ("Details", self.details.view()),
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

        container(grid)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn shortcut(key: Key, mods: Modifiers) -> Option<Message> {
    if !mods.command() {
        return None;
    }
    match key.as_ref() {
        Key::Character("s") => Some(Message::Save),
        Key::Character("e") => Some(Message::ToggleEdit),
        Key::Character("w") => Some(Message::CloseTab),
        Key::Character("b") => Some(Message::ToggleLeft),
        Key::Character("f") => Some(Message::Search),
        Key::Named(key::Named::F4) => Some(Message::CloseTab),
        _ => None,
    }
}
