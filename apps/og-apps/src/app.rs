use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Element, Length, Task};

use crate::config::{Config, APP_TINT_SEED};
use crate::pkg::{self, AppEntry, Source};

/// `AppColors` now comes from the shared `og-theme` crate — every
/// OG-toolkit app derives its widget colors from the same definition, so
/// adding a field there (like `accent2`/gradient support) benefits this
/// app too without needing the same hand-edit copied in here.
pub use og_theme::AppColors;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Browse,
    Installed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceFilter {
    All,
    Only(SourceKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind { Pacman, Aur, Flatpak }

fn source_kind(s: Source) -> SourceKind {
    match s {
        Source::Pacman => SourceKind::Pacman,
        Source::Aur => SourceKind::Aur,
        Source::Flatpak => SourceKind::Flatpak,
    }
}

#[derive(Debug, Clone)]
enum UninstallStage {
    ChooseTier,
    ConfirmWipe { leftover_dirs: Vec<String> },
}

#[derive(Debug, Clone)]
struct UninstallModal {
    source: Source,
    id: String,
    name: String,
    stage: UninstallStage,
}

#[derive(Debug, Clone)]
pub enum Message {
    TabSelected(Tab),
    Close,
    SearchQueryChanged(String),
    SearchSubmit,
    SearchResults(Vec<AppEntry>),
    SourceFilterChanged(usize),
    Install(Source, String),
    InstalledLoaded(Vec<AppEntry>),
    UninstallClicked(Source, String, String),
    UninstallTierChosen(bool),
    UninstallWipeConfirmed,
    UninstallModalCancel,
}

pub struct App {
    config: Config,
    tab: Tab,
    search_query: String,
    searching: bool,
    results: Vec<AppEntry>,
    source_filter: SourceFilter,
    installed: Vec<AppEntry>,
    installed_loading: bool,
    uninstall_modal: Option<UninstallModal>,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        (
            Self {
                config: Config::load(),
                tab: Tab::Browse,
                search_query: String::new(),
                searching: false,
                results: Vec::new(),
                source_filter: SourceFilter::All,
                installed: Vec::new(),
                installed_loading: false,
                uninstall_modal: None,
            },
            Task::none(),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::TabSelected(tab) => {
                self.tab = tab;
                if tab == Tab::Installed && self.installed.is_empty() && !self.installed_loading {
                    self.installed_loading = true;
                    return Task::perform(
                        async { tokio::task::spawn_blocking(pkg::list_installed_all).await.unwrap_or_default() },
                        Message::InstalledLoaded,
                    );
                }
            }
            Message::Close => return iced::exit(),
            Message::SearchQueryChanged(q) => self.search_query = q,
            Message::SearchSubmit => {
                if self.search_query.trim().is_empty() {
                    return Task::none();
                }
                self.searching = true;
                let query = self.search_query.clone();
                return Task::perform(
                    async move { tokio::task::spawn_blocking(move || pkg::search_all(&query)).await.unwrap_or_default() },
                    Message::SearchResults,
                );
            }
            Message::SearchResults(results) => {
                self.results = results;
                self.searching = false;
            }
            Message::SourceFilterChanged(idx) => {
                self.source_filter = match idx {
                    1 => SourceFilter::Only(SourceKind::Pacman),
                    2 => SourceFilter::Only(SourceKind::Aur),
                    3 => SourceFilter::Only(SourceKind::Flatpak),
                    _ => SourceFilter::All,
                };
            }
            Message::Install(source, id) => {
                let terminal = self.config.terminal.clone();
                match source {
                    Source::Pacman => pkg::install_pacman(&terminal, &id),
                    Source::Aur => pkg::install_aur(&terminal, &id),
                    Source::Flatpak => pkg::install_flatpak(&terminal, &id),
                }
            }
            Message::InstalledLoaded(entries) => {
                self.installed = entries;
                self.installed_loading = false;
            }
            Message::UninstallClicked(source, id, name) => {
                self.uninstall_modal = Some(UninstallModal { source, id, name, stage: UninstallStage::ChooseTier });
            }
            Message::UninstallModalCancel => self.uninstall_modal = None,
            Message::UninstallTierChosen(wipe_config) => {
                let Some(modal) = self.uninstall_modal.clone() else { return Task::none() };
                let terminal = self.config.terminal.clone();
                if !wipe_config {
                    match modal.source {
                        Source::Pacman | Source::Aur => pkg::uninstall_pacman(&terminal, &modal.id, false),
                        Source::Flatpak => pkg::uninstall_flatpak(&terminal, &modal.id, false),
                    }
                    self.uninstall_modal = None;
                } else {
                    match modal.source {
                        Source::Flatpak => {
                            pkg::uninstall_flatpak(&terminal, &modal.id, true);
                            self.uninstall_modal = None;
                        }
                        Source::Pacman | Source::Aur => {
                            let dirs = pkg::find_leftover_config_dirs(&modal.id);
                            self.uninstall_modal = Some(UninstallModal { stage: UninstallStage::ConfirmWipe { leftover_dirs: dirs }, ..modal });
                        }
                    }
                }
            }
            Message::UninstallWipeConfirmed => {
                if let Some(modal) = self.uninstall_modal.take() {
                    let terminal = self.config.terminal.clone();
                    pkg::uninstall_pacman(&terminal, &modal.id, true);
                    if let UninstallStage::ConfirmWipe { leftover_dirs } = modal.stage {
                        pkg::delete_leftover_dirs(&leftover_dirs);
                    }
                }
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let colors = AppColors::from_config(&self.config, APP_TINT_SEED);

        let card_style = move |_: &_| container::Style {
            background: Some(Background::Color(colors.sec_bg)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        };
        let btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(colors.surface)),
            text_color: colors.text,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        };
        let accent_btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(colors.accent)),
            text_color: colors.bar_bg,
            border: Border { radius: colors.radius.into(), ..Default::default() },
            ..Default::default()
        };
        let danger_btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 })),
            text_color: Color::WHITE,
            border: Border { radius: colors.radius.into(), ..Default::default() },
            ..Default::default()
        };

        let tab_btn = |label: &'static str, tab: Tab, active: Tab| -> Element<Message> {
            let is_active = tab == active;
            button(text(label).size(13).style(move |_| iced::widget::text::Style {
                color: Some(if is_active { colors.bar_bg } else { colors.text }),
            }))
            .style(move |_, _| iced::widget::button::Style {
                background: Some(Background::Color(if is_active { colors.accent } else { colors.surface })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            })
            .on_press(Message::TabSelected(tab))
            .padding([8, 18])
            .into()
        };

        let header = row![
            text("OG Apps").size(20).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
            iced::widget::horizontal_space(),
            tab_btn("Browse", Tab::Browse, self.tab),
            tab_btn("Installed", Tab::Installed, self.tab),
            iced::widget::horizontal_space(),
            button(text("×").size(18).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(btn_style)
                .on_press(Message::Close)
                .padding([4, 12]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12)
        .padding(20);

        let body: Element<Message> = match self.tab {
            Tab::Browse => self.view_browse(colors, card_style, btn_style, accent_btn_style),
            Tab::Installed => self.view_installed(colors, card_style, btn_style, danger_btn_style),
        };

        let base: Element<Message> = column![header, body].into();

        if let Some(modal) = &self.uninstall_modal {
            self.view_uninstall_modal(modal, colors, card_style, btn_style, accent_btn_style, danger_btn_style, base)
        } else {
            container(base)
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.bar_bg)),
                    border: Border { color: colors.accent, width: 2.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    }

    fn view_browse<'a>(
        &'a self,
        colors: AppColors,
        card_style: impl Fn(&iced::Theme) -> container::Style + 'a + Copy,
        btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
        accent_btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
    ) -> Element<'a, Message> {
        let search_bar = row![
            text_input("Search pacman, AUR, and Flatpak…", &self.search_query)
                .on_input(Message::SearchQueryChanged)
                .on_submit(Message::SearchSubmit)
                .style(move |_, _| iced::widget::text_input::Style {
                    background: Background::Color(colors.surface),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    icon: colors.dim_text,
                    placeholder: colors.dim_text,
                    value: colors.text,
                    selection: colors.accent,
                })
                .padding(10)
                .width(Length::Fill),
            button(text(if self.searching { "Searching…" } else { "Search" }).style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) }))
                .style(accent_btn_style)
                .on_press_maybe((!self.searching).then_some(Message::SearchSubmit))
                .padding([10, 18]),
        ]
        .spacing(12);

        let filter_row = row![
            filter_chip("All", 0, self.source_filter == SourceFilter::All, colors),
            filter_chip("Pacman", 1, self.source_filter == SourceFilter::Only(SourceKind::Pacman), colors),
            filter_chip("AUR", 2, self.source_filter == SourceFilter::Only(SourceKind::Aur), colors),
            filter_chip("Flatpak", 3, self.source_filter == SourceFilter::Only(SourceKind::Flatpak), colors),
        ]
        .spacing(8);

        let filtered: Vec<&AppEntry> = self.results.iter().filter(|e| match self.source_filter {
            SourceFilter::All => true,
            SourceFilter::Only(k) => source_kind(e.source) == k,
        }).collect();

        let result_rows: Vec<Element<Message>> = if filtered.is_empty() {
            vec![text(if self.searching { "Searching…" } else { "No results yet — try a search." })
                .size(12)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                .into()]
        } else {
            filtered.iter().map(|e| {
                let install_btn: Element<Message> = if e.installed {
                    text("Installed").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()
                } else {
                    button(text("Install").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) }))
                        .style(accent_btn_style)
                        .on_press(Message::Install(e.source, e.id.clone()))
                        .padding([6, 14])
                        .into()
                };
                container(
                    row![
                        column![
                            row![
                                text(e.name.clone()).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
                                source_badge(e.source, colors),
                                text(e.version.clone()).size(11).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                            ].spacing(8).align_y(iced::Alignment::Center),
                            text(e.description.clone()).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                        ].spacing(4).width(Length::Fill),
                        install_btn,
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(12)
                    .padding(14),
                )
                .style(card_style)
                .width(Length::Fill)
                .into()
            }).collect()
        };

        scrollable(
            column![search_bar, filter_row, column(result_rows).spacing(10)]
                .spacing(16)
                .padding(20),
        )
        .into()
    }

    fn view_installed<'a>(
        &'a self,
        colors: AppColors,
        card_style: impl Fn(&iced::Theme) -> container::Style + 'a + Copy,
        btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
        danger_btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
    ) -> Element<'a, Message> {
        let rows: Vec<Element<Message>> = if self.installed_loading {
            vec![text("Loading installed apps…").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()]
        } else if self.installed.is_empty() {
            vec![text("No installed apps found.").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()]
        } else {
            self.installed.iter().map(|e| {
                container(
                    row![
                        column![
                            row![
                                text(e.name.clone()).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
                                source_badge(e.source, colors),
                            ].spacing(8).align_y(iced::Alignment::Center),
                            text(e.version.clone()).size(11).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                        ].spacing(4).width(Length::Fill),
                        button(text("Uninstall").size(12).style(move |_| iced::widget::text::Style { color: Some(Color::WHITE) }))
                            .style(danger_btn_style)
                            .on_press(Message::UninstallClicked(e.source, e.id.clone(), e.name.clone()))
                            .padding([6, 14]),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(12)
                    .padding(14),
                )
                .style(card_style)
                .width(Length::Fill)
                .into()
            }).collect()
        };
        let _ = btn_style;
        scrollable(column(rows).spacing(10).padding(20)).into()
    }

    #[allow(clippy::too_many_arguments)]
    fn view_uninstall_modal<'a>(
        &'a self,
        modal: &UninstallModal,
        colors: AppColors,
        card_style: impl Fn(&iced::Theme) -> container::Style + 'a + Copy,
        btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
        accent_btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
        danger_btn_style: impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style + 'a + Copy,
        base: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let title = text(format!("Uninstall {}", modal.name)).size(16).style(move |_| iced::widget::text::Style { color: Some(colors.text) });

        let content: Element<Message> = match &modal.stage {
            UninstallStage::ChooseTier => column![
                title,
                text("Package configs under /etc are handled either way by pacman. Choose whether to also try removing this app's user config.")
                    .size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                row![
                    button(text("Cancel").style(move |_| iced::widget::text::Style { color: Some(colors.text) })).style(btn_style).on_press(Message::UninstallModalCancel).padding([8, 16]),
                    iced::widget::horizontal_space(),
                    button(text("Remove app").style(move |_| iced::widget::text::Style { color: Some(colors.text) })).style(btn_style).on_press(Message::UninstallTierChosen(false)).padding([8, 16]),
                    button(text("Remove app + config").style(move |_| iced::widget::text::Style { color: Some(Color::WHITE) })).style(danger_btn_style).on_press(Message::UninstallTierChosen(true)).padding([8, 16]),
                ].spacing(10),
            ].spacing(16).into(),
            UninstallStage::ConfirmWipe { leftover_dirs } => {
                let dir_list: Element<Message> = if leftover_dirs.is_empty() {
                    text("No matching config/data directories found under ~/.config or ~/.local/share.")
                        .size(12)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                        .into()
                } else {
                    column(
                        leftover_dirs.iter().map(|d| {
                            text(d.clone()).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()
                        }).collect::<Vec<_>>()
                    ).spacing(4).into()
                };
                column![
                    title,
                    text("This is a name-based guess, not guaranteed complete. These paths will be permanently deleted, plus a full uninstall (-Rns):")
                        .size(12)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                    dir_list,
                    row![
                        button(text("Cancel").style(move |_| iced::widget::text::Style { color: Some(colors.text) })).style(btn_style).on_press(Message::UninstallModalCancel).padding([8, 16]),
                        iced::widget::horizontal_space(),
                        button(text("Delete + Uninstall").style(move |_| iced::widget::text::Style { color: Some(Color::WHITE) })).style(danger_btn_style).on_press(Message::UninstallWipeConfirmed).padding([8, 16]),
                    ].spacing(10),
                ].spacing(16).into()
            }
        };

        let _ = accent_btn_style;
        let modal_box = container(content).style(card_style).width(500).padding(24);

        iced::widget::stack![
            base,
            container(modal_box)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(iced::alignment::Horizontal::Center)
                .align_y(iced::alignment::Vertical::Center)
                .style(move |_| container::Style { background: Some(Background::Color(Color { a: 0.6, ..Color::BLACK })), ..Default::default() }),
        ]
        .into()
    }
}

fn filter_chip(label: &'static str, idx: usize, active: bool, colors: AppColors) -> Element<'static, Message> {
    button(text(label).size(12).style(move |_| iced::widget::text::Style {
        color: Some(if active { colors.bar_bg } else { colors.text }),
    }))
    .style(move |_, _| iced::widget::button::Style {
        background: Some(Background::Color(if active { colors.accent } else { colors.surface })),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    })
    .on_press(Message::SourceFilterChanged(idx))
    .padding([6, 14])
    .into()
}

fn source_badge<'a>(source: Source, colors: AppColors) -> Element<'a, Message> {
    container(
        text(source.label()).size(10).style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) })
    )
    .style(move |_| container::Style {
        background: Some(Background::Color(colors.accent)),
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    })
    .padding([2, 6])
    .into()
}
