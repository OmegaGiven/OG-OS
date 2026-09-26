use iced::widget::{button, column, container, image, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Element, Event, Length, Task};
use iced::keyboard;

use crate::config::{Config, APP_TINT_SEED};
use crate::history::{self, ClipEntry};
use og_theme::AppColors;

#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    Copy(ClipEntry),
    Delete(ClipEntry),
    ClearAll,
    Close,
    MoveSelection(i32),
    ActivateSelected,
}

pub struct App {
    config: Config,
    items: Vec<ClipEntry>,
    query: String,
    /// Index into `filtered()` — same arrow-key convention as og-search
    /// and og-notif-center use for their own lists.
    selected: usize,
}

/// A synthetic, searchable label for an image entry — lets typing "image"
/// surface them, since there's no real text to filter against otherwise.
fn label_for(entry: &ClipEntry) -> String {
    match entry {
        ClipEntry::Text { value } => history::preview(value),
        ClipEntry::Image { path } => {
            let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            format!("[Image] {name}")
        }
    }
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        (
            Self {
                config: Config::load(),
                items: history::load(),
                query: String::new(),
                selected: 0,
            },
            text_input::focus(text_input::Id::new("query")),
        )
    }

    fn filtered(&self) -> Vec<&ClipEntry> {
        if self.query.trim().is_empty() {
            return self.items.iter().collect();
        }
        let q = self.query.to_lowercase();
        self.items.iter().filter(|i| label_for(i).to_lowercase().contains(&q)).collect()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::QueryChanged(q) => {
                self.query = q;
                self.selected = 0;
                Task::none()
            }
            Message::Copy(entry) => {
                match &entry {
                    ClipEntry::Text { value } => history::copy_text_to_clipboard(value),
                    ClipEntry::Image { path } => history::copy_image_to_clipboard(path),
                }
                iced::exit()
            }
            Message::Delete(entry) => {
                history::delete(&entry);
                self.items = history::load();
                Task::none()
            }
            Message::ClearAll => {
                history::clear_all();
                self.items.clear();
                self.selected = 0;
                Task::none()
            }
            Message::Close => iced::exit(),
            Message::MoveSelection(delta) => {
                let len = self.filtered().len();
                if len > 0 {
                    self.selected = (self.selected as i32 + delta).rem_euclid(len as i32) as usize;
                }
                Task::none()
            }
            Message::ActivateSelected => {
                match self.filtered().get(self.selected) {
                    Some(entry) => self.update(Message::Copy((*entry).clone())),
                    None => Task::none(),
                }
            }
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        iced::event::listen_with(|event, _status, _id| match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Close),
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::MoveSelection(1)),
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::MoveSelection(-1)),
                _ => None,
            },
            // Popup semantics: clicking anywhere outside (the bar, another
            // window, the desktop) hands focus away from this floating,
            // border-none window — treat that the same as Escape.
            Event::Window(iced::window::Event::Unfocused) => Some(Message::Close),
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

        let header = row![
            text("Clipboard History").size(18).style(move |_| text::Style { color: Some(colors.text) }),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(10)
        .padding(iced::Padding { top: 16.0, right: 16.0, bottom: 0.0, left: 16.0 });

        let input = text_input("Filter clipboard history…", &self.query)
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
            vec![text("No clipboard history.").size(13).style(move |_| text::Style { color: Some(colors.dim_text) }).into()]
        } else {
            filtered.iter().enumerate().map(|(i, entry)| {
                let is_selected = i == self.selected;
                let entry = (*entry).clone();

                let content: Element<Message> = match &entry {
                    ClipEntry::Text { value } => {
                        button(text(history::preview(value)).size(14).style(move |_| text::Style { color: Some(colors.text) }))
                            .style(row_btn_style)
                            .on_press(Message::Copy(entry.clone()))
                            .padding([8, 10])
                            .width(Length::Fill)
                            .into()
                    }
                    ClipEntry::Image { path } => {
                        button(
                            row![
                                image(image::Handle::from_path(path)).width(48).height(48),
                                text("Image").size(14).style(move |_| text::Style { color: Some(colors.text) }),
                            ]
                            .align_y(iced::Alignment::Center)
                            .spacing(10),
                        )
                        .style(row_btn_style)
                        .on_press(Message::Copy(entry.clone()))
                        .padding(8)
                        .width(Length::Fill)
                        .into()
                    }
                };

                container(
                    row![
                        content,
                        button(text("×").size(14).style(move |_| text::Style { color: Some(colors.dim_text) }))
                            .style(btn_style)
                            .on_press(Message::Delete(entry.clone()))
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
