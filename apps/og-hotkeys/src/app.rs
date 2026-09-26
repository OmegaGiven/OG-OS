use iced::widget::{column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Element, Event, Length, Task};
use iced::keyboard;

use crate::bindings::{self, Binding};
use crate::config::{Config, APP_TINT_SEED};
use og_theme::AppColors;

#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    Close,
}

pub struct App {
    config: Config,
    bindings: Vec<Binding>,
    query: String,
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        (
            Self {
                config: Config::load(),
                bindings: bindings::load(),
                query: String::new(),
            },
            text_input::focus(text_input::Id::new("query")),
        )
    }

    fn filtered(&self) -> Vec<&Binding> {
        if self.query.trim().is_empty() {
            return self.bindings.iter().collect();
        }
        let q = self.query.to_lowercase();
        self.bindings
            .iter()
            .filter(|b| {
                b.keys.to_lowercase().contains(&q)
                    || b.description.to_lowercase().contains(&q)
                    || b.category.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::QueryChanged(q) => {
                self.query = q;
                Task::none()
            }
            Message::Close => iced::exit(),
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        iced::event::listen_with(|event, _status, _id| match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) =>
            {
                Some(Message::Close)
            }
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

        let header = row![
            text("Keyboard Shortcuts").size(18).style(move |_| text::Style { color: Some(colors.text) }),
            iced::widget::horizontal_space(),
            iced::widget::button(text("×").size(18).style(move |_| text::Style { color: Some(colors.text) }))
                .style(btn_style)
                .on_press(Message::Close)
                .padding([4, 12]),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(10)
        .padding([16, 16]);

        let input = text_input("Filter shortcuts…", &self.query)
            .id(text_input::Id::new("query"))
            .on_input(Message::QueryChanged)
            .padding(10)
            .size(16)
            .style(move |_, _| text_input::Style {
                background: Background::Color(colors.sec_bg),
                border: Border { color: colors.accent, width: 2.0, radius: colors.radius.into() },
                icon: colors.dim_text,
                placeholder: colors.dim_text,
                value: colors.text,
                selection: colors.accent,
            });

        let filtered = self.filtered();

        // Grouped by category (Focus/Windows/Workspaces/Toolkit/...) —
        // computed from each binding's own command, not hand-maintained
        // section headers like the old static cheatsheet had, so a
        // category never silently goes stale relative to what's actually
        // bound.
        let mut categories: Vec<&str> = Vec::new();
        for b in &filtered {
            if !categories.contains(&b.category) {
                categories.push(b.category);
            }
        }

        let rows: Vec<Element<Message>> = if filtered.is_empty() {
            vec![text("No shortcuts match.").size(13).style(move |_| text::Style { color: Some(colors.dim_text) }).into()]
        } else {
            categories
                .iter()
                .flat_map(|category| {
                    let mut section: Vec<Element<Message>> = vec![
                        text(*category)
                            .size(13)
                            .style(move |_| text::Style { color: Some(Color { a: 0.7, ..colors.accent }) })
                            .into(),
                    ];
                    for b in filtered.iter().filter(|b| b.category == *category) {
                        section.push(
                            container(
                                row![
                                    container(
                                        text(b.keys.clone()).size(13).style(move |_| text::Style { color: Some(colors.text) }),
                                    )
                                    .padding([4, 10])
                                    .style(move |_| container::Style {
                                        background: Some(Background::Color(colors.surface)),
                                        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                                        ..Default::default()
                                    })
                                    .width(Length::Fixed(180.0)),
                                    if b.description == b.command {
                                        // "Other"/generic-launch fallback: the
                                        // description already *is* the raw
                                        // command, showing both would just
                                        // repeat the same line twice.
                                        column![text(b.description.clone()).size(13).style(move |_| text::Style { color: Some(colors.text) })]
                                    } else {
                                        column![
                                            text(b.description.clone()).size(13).style(move |_| text::Style { color: Some(colors.text) }),
                                            text(b.command.clone()).size(11).style(move |_| text::Style { color: Some(colors.dim_text) }),
                                        ]
                                        .spacing(2)
                                    },
                                ]
                                .align_y(iced::Alignment::Center)
                                .spacing(12),
                            )
                            .padding([4, 4])
                            .into(),
                        );
                    }
                    section
                })
                .collect()
        };

        let body = scrollable(
            column(rows).spacing(8).padding(iced::Padding { top: 0.0, right: 16.0, bottom: 16.0, left: 16.0 }),
        )
        .height(Length::Fill);

        container(column![header, container(input).padding([0, 16]), body].spacing(10))
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
