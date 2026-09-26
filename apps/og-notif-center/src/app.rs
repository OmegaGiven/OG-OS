use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::window;
use iced::{Background, Border, Color, Element, Event, Length, Task};
use iced::keyboard;

use crate::config::{Config, APP_TINT_SEED};
use crate::notif::{self, NotifItem};
use crate::placement;
use og_theme::AppColors;

pub const SIZE: (f32, f32) = (560.0, 560.0);

#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    Copy(String),
    Delete(String),
    ClearAll,
    TogglePreview,
    Close,
    MoveSelection(i32),
    ActivateSelected,
    /// Positioning finished — reveal the window now (see main.rs: it
    /// starts with `visible: false` specifically so this is the first
    /// time it's actually shown, already at the right spot, instead of
    /// popping up center-screen and then jumping).
    Positioned,
    Reveal(window::Id),
}

pub struct App {
    config: Config,
    items: Vec<NotifItem>,
    preview_on: bool,
    query: String,
    /// Index into `filtered()`, for arrow-key navigation — same convention
    /// as og-search's own result-list selection (a plain `usize`, not an
    /// `Option`; an empty list just means every `.get(selected)` below is
    /// `None`, no separate empty-state to track), so the keyboard flow
    /// reads the same across the suite.
    selected: usize,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        notif::ensure_dnd_mode_configured();
        (
            Self {
                config: Config::load(),
                items: notif::load_visible(),
                preview_on: notif::preview_on(),
                query: String::new(),
                selected: 0,
            },
            Task::batch([
                text_input::focus(text_input::Id::new("query")),
                Task::perform(async { placement::move_near_bar(SIZE) }, |_| Message::Positioned)
                    .chain(window::get_latest().map(|id| match id {
                        Some(id) => Message::Reveal(id),
                        None => Message::Positioned,
                    })),
            ]),
        )
    }

    fn filtered(&self) -> Vec<&NotifItem> {
        if self.query.trim().is_empty() {
            return self.items.iter().collect();
        }
        let q = self.query.to_lowercase();
        self.items.iter().filter(|i| i.label.to_lowercase().contains(&q)).collect()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::QueryChanged(q) => {
                self.query = q;
                self.selected = 0;
                Task::none()
            }
            Message::Copy(text) => {
                notif::copy_to_clipboard(&text);
                iced::exit()
            }
            Message::Delete(raw) => {
                notif::hide(&raw);
                self.items = notif::load_visible();
                Task::none()
            }
            Message::ClearAll => {
                notif::clear_all();
                self.items.clear();
                self.selected = 0;
                Task::none()
            }
            Message::TogglePreview => {
                self.preview_on = !self.preview_on;
                notif::set_preview(self.preview_on);
                Task::none()
            }
            Message::Close => iced::exit(),
            Message::Positioned => Task::none(),
            Message::Reveal(id) => window::change_mode(id, window::Mode::Windowed),
            Message::MoveSelection(delta) => {
                let len = self.filtered().len();
                if len > 0 {
                    self.selected = (self.selected as i32 + delta).rem_euclid(len as i32) as usize;
                }
                Task::none()
            }
            Message::ActivateSelected => {
                match self.filtered().get(self.selected) {
                    Some(item) => self.update(Message::Copy(item.label.clone())),
                    None => Task::none(),
                }
            }
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        iced::event::listen_with(|event, _status, _id| match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Close),
                // Same convention as og-search's own result list: arrows
                // move the selection regardless of which widget technically
                // has focus (the filter text_input doesn't use Up/Down for
                // anything itself, so this doesn't fight it).
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::MoveSelection(1)),
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::MoveSelection(-1)),
                _ => None,
            },
            _ => None,
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let colors = AppColors::from_config(&self.config, APP_TINT_SEED);

        let btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(colors.surface)),
            text_color: colors.text,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        };
        let toggle_btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(if self.preview_on { colors.accent } else { colors.surface })),
            text_color: if self.preview_on { colors.bar_bg } else { colors.text },
            border: Border { radius: colors.radius.into(), ..Default::default() },
            ..Default::default()
        };
        let danger_btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 })),
            text_color: Color::WHITE,
            border: Border { radius: colors.radius.into(), ..Default::default() },
            ..Default::default()
        };
        let row_btn_style = move |_: &_, _| iced::widget::button::Style {
            background: Some(Background::Color(Color::TRANSPARENT)),
            text_color: colors.text,
            border: Border { radius: colors.radius.into(), ..Default::default() },
            ..Default::default()
        };

        let preview_label = if self.preview_on { "Preview: On" } else { "Preview: Off" };

        let header = row![
            text("Notifications").size(18).style(move |_| text::Style { color: Some(colors.text) }),
            iced::widget::horizontal_space(),
            button(text(preview_label).size(12).style(move |_| text::Style {
                color: Some(if self.preview_on { colors.bar_bg } else { colors.text }),
            }))
            .style(toggle_btn_style)
            .on_press(Message::TogglePreview)
            .padding([6, 14]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(10)
        .padding(iced::Padding { top: 16.0, right: 16.0, bottom: 0.0, left: 16.0 });

        let input = text_input("Filter notifications…", &self.query)
            .id(text_input::Id::new("query"))
            .on_input(Message::QueryChanged)
            .on_submit(Message::ActivateSelected)
            .padding(10)
            .size(16)
            .width(Length::Fill)
            .style(move |_, _| text_input::Style {
                background: Background::Color(colors.sec_bg),
                border: Border { color: colors.accent, width: 2.0, radius: colors.radius.into() },
                icon: colors.dim_text,
                placeholder: colors.dim_text,
                value: colors.text,
                selection: colors.accent,
            });

        // Clear All + close live in the same row as the filter bar (not
        // the header) — the header only ever had the title/preview toggle
        // once these moved, so grouping all three "act on the list" /
        // "leave" controls together next to the thing they act on reads
        // cleaner than spreading them across two separate rows.
        let filter_row = row![
            input,
            button(text("Clear All").size(12).style(move |_| text::Style { color: Some(Color::WHITE) }))
                .style(danger_btn_style)
                .on_press(Message::ClearAll)
                .padding([10, 14]),
            button(text("×").size(18).style(move |_| text::Style { color: Some(colors.text) }))
                .style(btn_style)
                .on_press(Message::Close)
                .padding([6, 14]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(10)
        .padding([0, 16]);

        let filtered = self.filtered();
        let rows: Vec<Element<Message>> = if filtered.is_empty() {
            vec![text("No notification history.").size(13).style(move |_| text::Style { color: Some(colors.dim_text) }).into()]
        } else {
            filtered.iter().enumerate().map(|(i, item)| {
                let is_selected = i == self.selected;
                container(
                    row![
                        button(text(item.label.clone()).size(14).style(move |_| text::Style { color: Some(colors.text) }))
                            .style(row_btn_style)
                            .on_press(Message::Copy(item.label.clone()))
                            .padding([8, 10])
                            .width(Length::Fill),
                        button(text("×").size(14).style(move |_| text::Style { color: Some(colors.dim_text) }))
                            .style(btn_style)
                            .on_press(Message::Delete(item.raw.clone()))
                            .padding([6, 10]),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(8),
                )
                .style(move |_: &_| container::Style {
                    background: Some(Background::Color(colors.sec_bg)),
                    border: Border {
                        color: if is_selected { colors.accent } else { colors.border },
                        width: if is_selected { 2.0 } else { 1.0 },
                        radius: colors.radius.into(),
                    },
                    ..Default::default()
                })
                .width(Length::Fill)
                .into()
            }).collect()
        };

        let body = scrollable(
            column(rows).spacing(6).padding(iced::Padding { top: 0.0, right: 16.0, bottom: 16.0, left: 16.0 }),
        )
        .height(Length::Fill);

        container(column![header, filter_row, body].spacing(10))
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
