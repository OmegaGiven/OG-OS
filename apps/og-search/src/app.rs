use iced::widget::{column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Element, Event, Length, Task};
use iced::keyboard;

use std::path::PathBuf;

use crate::apps::{filter_apps, load_app_registry, AppEntry};
use crate::config::{Config, APP_TINT_SEED};
use crate::file_search;
use crate::launch;
use crate::settings_search::{self, SettingsMatch};
use og_theme::{apply_color_variance, hex_to_color};

const MAX_DISPLAYED_FILES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Column {
    Apps,
    Settings,
    Files,
}

impl Column {
    const ALL: [Column; 3] = [Column::Apps, Column::Settings, Column::Files];
}

#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    MoveSelection(i32),
    MoveColumn(i32),
    /// Click anywhere in a column selects *and* activates that row in one
    /// step, same as a normal launcher's click-to-open — no separate
    /// "select then press Enter" needed with a mouse.
    RowClicked(Column, usize),
    Activate,
    Close,
    FileFound(PathBuf),
}

/// What pressing Enter (or clicking) on a row in the Apps column does.
enum AppAction<'a> {
    GoAlias(String),
    WebSearch(String),
    AskAi(String),
    LaunchApp(&'a AppEntry),
}

pub struct App {
    config: Config,
    apps: Vec<AppEntry>,
    query: String,
    column: Column,
    selected_apps: usize,
    selected_settings: usize,
    selected_files: usize,
    /// Populated incrementally by the file-search subscription (keyed by
    /// query — changing the query cancels the in-flight `find` and starts
    /// a fresh one) as results actually arrive, not all at once.
    files: Vec<PathBuf>,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        (
            Self {
                config: Config::load(),
                apps: load_app_registry(),
                query: String::new(),
                column: Column::Apps,
                selected_apps: 0,
                selected_settings: 0,
                selected_files: 0,
                files: Vec::new(),
            },
            text_input::focus(text_input::Id::new("query")),
        )
    }

    /// `go/<alias>` is its own distinct mode — no suggestion list, just the
    /// one action. Anything else: the single best-matching app first (if
    /// there is one), then web search, then ask AI, then the rest of the
    /// matching apps.
    fn app_actions(&self) -> Vec<AppAction<'_>> {
        let trimmed = self.query.trim();
        if let Some(alias) = trimmed.strip_prefix("go/") {
            if !alias.is_empty() {
                return vec![AppAction::GoAlias(alias.to_string())];
            }
        }
        let matched = filter_apps(&self.apps, &self.query);
        let mut actions = Vec::new();

        let mut rest = matched.into_iter();
        if !self.query.trim().is_empty() {
            if let Some(best) = rest.next() {
                actions.push(AppAction::LaunchApp(best));
            }
        }
        actions.push(AppAction::WebSearch(self.query.clone()));
        actions.push(AppAction::AskAi(self.query.clone()));
        actions.extend(rest.take(8).map(AppAction::LaunchApp));
        actions
    }

    fn settings_matches(&self) -> Vec<SettingsMatch> {
        if self.config.search_settings_enabled {
            settings_search::search(&self.query)
        } else {
            Vec::new()
        }
    }

    fn column_len(&self, col: Column) -> usize {
        match col {
            Column::Apps => self.app_actions().len(),
            Column::Settings => self.settings_matches().len(),
            Column::Files => self.files.len(),
        }
    }

    /// Shared by both Enter (whatever's selected in the current column)
    /// and a direct row click (which supplies its own column/index instead
    /// of trusting current selection state).
    fn activate(&self, col: Column, index: usize) -> Task<Message> {
        match col {
            Column::Apps => match self.app_actions().into_iter().nth(index) {
                Some(AppAction::GoAlias(alias)) => launch::go_alias(&self.config, &alias),
                Some(AppAction::WebSearch(q)) if !q.trim().is_empty() => launch::web_search(&self.config, &q),
                Some(AppAction::AskAi(q)) if !q.trim().is_empty() => launch::ask_ai(&self.config, &q),
                Some(AppAction::LaunchApp(entry)) => launch::launch_app(&self.config, &entry.exec),
                _ => return Task::none(),
            },
            Column::Settings => match self.settings_matches().get(index) {
                Some(m) => launch::open_settings_tab(m.tab_arg),
                None => return Task::none(),
            },
            Column::Files => match self.files.get(index) {
                Some(path) => launch::open_file(path),
                None => return Task::none(),
            },
        }
        iced::exit()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::QueryChanged(q) => {
                self.query = q;
                self.selected_apps = 0;
                self.selected_settings = 0;
                self.selected_files = 0;
                // Not cleared via a message round-trip — the file-search
                // subscription is keyed by query text, so changing it here
                // is what actually cancels the in-flight `find` for the
                // old query and starts a fresh one for the new one.
                self.files.clear();
                Task::none()
            }
            Message::FileFound(path) => {
                // Found while testing: a broad query (e.g. "og") can match
                // well over a thousand files under a real home directory —
                // unbounded growth here would flood the column and re-render
                // an ever-larger widget tree on every single result. The
                // underlying `find` keeps running in the background past
                // this cap (killed instead once the query actually
                // changes), but nothing past it gets displayed.
                if self.files.len() < MAX_DISPLAYED_FILES {
                    self.files.push(path);
                }
                Task::none()
            }
            Message::MoveSelection(delta) => {
                let len = self.column_len(self.column);
                let selected = match self.column {
                    Column::Apps => &mut self.selected_apps,
                    Column::Settings => &mut self.selected_settings,
                    Column::Files => &mut self.selected_files,
                };
                if len > 0 {
                    let next = *selected as i32 + delta;
                    *selected = next.clamp(0, len as i32 - 1) as usize;
                }
                Task::none()
            }
            Message::MoveColumn(delta) => {
                let idx = Column::ALL.iter().position(|c| *c == self.column).unwrap_or(0);
                // Skip a column that's disabled in Search settings (and so
                // always empty) rather than landing on a dead end.
                let mut next = idx as i32;
                for _ in 0..Column::ALL.len() {
                    next = (next + delta).rem_euclid(Column::ALL.len() as i32);
                    let candidate = Column::ALL[next as usize];
                    let enabled = match candidate {
                        Column::Apps => true,
                        Column::Settings => self.config.search_settings_enabled,
                        Column::Files => self.config.search_files_enabled,
                    };
                    if enabled {
                        self.column = candidate;
                        break;
                    }
                }
                Task::none()
            }
            Message::RowClicked(col, index) => self.activate(col, index),
            Message::Activate => self.activate(self.column, self.column_selected(self.column)),
            Message::Close => iced::exit(),
        }
    }

    fn column_selected(&self, col: Column) -> usize {
        match col {
            Column::Apps => self.selected_apps,
            Column::Settings => self.selected_settings,
            Column::Files => self.selected_files,
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        let keyboard = iced::event::listen_with(|event, _status, _id| {
            if let Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = event {
                match key {
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::MoveSelection(1)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::MoveSelection(-1)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowRight) => Some(Message::MoveColumn(1)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => Some(Message::MoveColumn(-1)),
                    keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Close),
                    _ => None,
                }
            } else {
                None
            }
        });

        if self.config.search_files_enabled && self.query.trim().len() >= 2 {
            let file_search = iced::Subscription::run_with_id(self.query.clone(), file_search::stream(self.query.clone()))
                .map(Message::FileFound);
            iced::Subscription::batch([keyboard, file_search])
        } else {
            keyboard
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let bar_bg = apply_color_variance(
            hex_to_color(&self.config.bar_bg), APP_TINT_SEED,
            self.config.color_variance_enabled, self.config.color_variance_amount,
        );
        let sec_bg = apply_color_variance(
            hex_to_color(&self.config.sec_bg), APP_TINT_SEED,
            self.config.color_variance_enabled, self.config.color_variance_amount,
        );
        let text_color = hex_to_color(&self.config.bar_text);
        let accent = hex_to_color(&self.config.accent);
        let dim = Color { a: 0.6, ..text_color };
        let radius = self.config.corner_radius;

        let input = text_input("Search apps, or type go/<alias>…", &self.query)
            .id(text_input::Id::new("query"))
            .on_input(Message::QueryChanged)
            .on_submit(Message::Activate)
            .padding(14)
            .size(20)
            .style(move |_, _| text_input::Style {
                background: Background::Color(sec_bg),
                border: Border { color: accent, width: 2.0, radius: radius.into() },
                icon: dim,
                placeholder: dim,
                value: text_color,
                selection: accent,
            });

        // Plain fn items, not closures — every value that ends up in these
        // rows is owned (String) or `&'static str`, so nothing here
        // actually borrows from `self`; a closure can't express that
        // (`Fn(...) -> Element<'_, ..>` ties the output lifetime to the
        // closure's own elided input lifetimes), but a plain fn with an
        // explicit `'static` return can.
        fn row_widget(
            col: Column, index: usize, label: String, hint: &'static str, is_selected: bool,
            bar_bg: Color, text_color: Color, dim: Color, accent: Color, radius: f32,
        ) -> Element<'static, Message> {
            let content: Element<'static, Message> = container(
                row![
                    text(label).size(15).style(move |_| text::Style {
                        color: Some(if is_selected { bar_bg } else { text_color }),
                    }),
                    iced::widget::horizontal_space(),
                    text(hint).size(11).style(move |_| text::Style {
                        color: Some(if is_selected { bar_bg } else { dim }),
                    }),
                ]
                .align_y(iced::Alignment::Center),
            )
            .padding([8, 10])
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(if is_selected { accent } else { Color::TRANSPARENT })),
                border: Border { radius: radius.into(), ..Default::default() },
                ..Default::default()
            })
            .into();
            iced::widget::mouse_area(content).on_press(Message::RowClicked(col, index)).into()
        }

        let app_rows: Vec<Element<Message>> = self.app_actions().iter().enumerate().map(|(i, action)| {
            let (label, hint): (String, &str) = match action {
                AppAction::GoAlias(alias) => (format!("Open go/{alias}"), "internal link"),
                AppAction::WebSearch(q) => (format!("Web search: {q}"), "web search"),
                AppAction::AskAi(q) => (
                    format!("Ask {}: {q}", if self.config.default_ai_cli.is_empty() { "AI" } else { &self.config.default_ai_cli }),
                    "ai",
                ),
                AppAction::LaunchApp(entry) => (entry.name.clone(), "app"),
            };
            let is_selected = self.column == Column::Apps && i == self.selected_apps;
            row_widget(Column::Apps, i, label, hint, is_selected, bar_bg, text_color, dim, accent, radius)
        }).collect();

        let settings_rows: Vec<Element<Message>> = self.settings_matches().iter().enumerate().map(|(i, m)| {
            let is_selected = self.column == Column::Settings && i == self.selected_settings;
            row_widget(Column::Settings, i, m.label.to_string(), "settings", is_selected, bar_bg, text_color, dim, accent, radius)
        }).collect();

        let file_rows: Vec<Element<Message>> = self.files.iter().enumerate().map(|(i, path)| {
            let is_selected = self.column == Column::Files && i == self.selected_files;
            row_widget(Column::Files, i, path.display().to_string(), "file", is_selected, bar_bg, text_color, dim, accent, radius)
        }).collect();

        fn column_header(label: &'static str, active: bool, accent: Color, dim: Color) -> Element<'static, Message> {
            text(label)
                .size(12)
                .style(move |_| text::Style { color: Some(if active { accent } else { dim }) })
                .into()
        }

        fn column_card(
            header: Element<'static, Message>, rows: Vec<Element<'static, Message>>, empty_hint: &'static str,
            sec_bg: Color, accent: Color, dim: Color, radius: f32,
        ) -> Element<'static, Message> {
            let body: Element<Message> = if rows.is_empty() {
                text(empty_hint).size(12).style(move |_| text::Style { color: Some(dim) }).into()
            } else {
                // Thin accent-colored line with a round "bead" scroller riding
                // it, instead of iced's default thick gray bar.
                scrollable(column(rows).spacing(2))
                    .height(Length::Fill)
                    .direction(scrollable::Direction::Vertical(
                        scrollable::Scrollbar::new().width(4.0).scroller_width(8.0).margin(2.0),
                    ))
                    .style(move |_theme, _status| scrollable::Style {
                        container: container::Style::default(),
                        vertical_rail: scrollable::Rail {
                            background: Some(Background::Color(Color { a: 0.15, ..accent })),
                            border: Border { radius: 2.0.into(), width: 0.0, color: Color::TRANSPARENT },
                            scroller: scrollable::Scroller {
                                color: accent,
                                border: Border { radius: 4.0.into(), width: 0.0, color: Color::TRANSPARENT },
                            },
                        },
                        horizontal_rail: scrollable::Rail {
                            background: None,
                            border: Border::default(),
                            scroller: scrollable::Scroller { color: Color::TRANSPARENT, border: Border::default() },
                        },
                        gap: None,
                    })
                    .into()
            };
            container(column![header, body].spacing(8).padding(10))
                .width(Length::FillPortion(1))
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(sec_bg)),
                    border: Border { color: accent, width: 1.0, radius: radius.into() },
                    ..Default::default()
                })
                .into()
        }

        let apps_empty = if self.query.trim().is_empty() { "Type to search apps…" } else { "No matches" };
        let settings_empty = if !self.config.search_settings_enabled {
            "Disabled in Search settings"
        } else if self.query.trim().is_empty() {
            "Type to search settings…"
        } else {
            "No matches"
        };
        let files_empty = if !self.config.search_files_enabled {
            "Disabled in Search settings"
        } else if self.query.trim().len() < 2 {
            "Type 2+ characters…"
        } else {
            "Searching…"
        };

        let columns = row![
            column_card(
                column_header("APPS / WEB / AI", self.column == Column::Apps, accent, dim),
                app_rows, apps_empty, sec_bg, accent, dim, radius,
            ),
            column_card(
                column_header("SETTINGS", self.column == Column::Settings, accent, dim),
                settings_rows, settings_empty, sec_bg, accent, dim, radius,
            ),
            column_card(
                column_header("FILES", self.column == Column::Files, accent, dim),
                file_rows, files_empty, sec_bg, accent, dim, radius,
            ),
        ]
        .spacing(10)
        .height(Length::Fill);

        container(
            column![input, columns]
                .spacing(10)
                .padding(16)
        )
        .width(980)
        .height(460)
        .style(move |_| container::Style {
            background: Some(Background::Color(bar_bg)),
            border: Border { color: accent, width: 2.0, radius: radius.into() },
            ..Default::default()
        })
        .into()
    }
}
