//! macOS-style Notification Center panel. Notifications are grouped by app
//! (newest group first); a group with several entries collapses into one
//! card with a stack edge under it, and expands on click. Cards match
//! mako's popup look (translucent, hairline border, accent border only for
//! critical) so the history reads as the same notifications you saw arrive.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use iced::font::Weight;
use iced::keyboard;
use iced::widget::{button, column, container, horizontal_space, image, mouse_area, responsive, row, scrollable, svg, text, text_input, Space};
use iced::{Alignment, Background, Border, Color, Element, Event, Length, Padding, Task};
use iced_layershell::to_layer_message;

use crate::config::{Config, APP_TINT_SEED};
use crate::fonts;
use crate::notif::{self, Icon, NotifItem};
use og_theme::AppColors;

const PANEL_W: f32 = 400.0;
const EDGE: f32 = 10.0;
const ICON: f32 = 34.0;
const SLIDE: Duration = Duration::from_millis(180);

#[to_layer_message]
#[derive(Debug, Clone)]
pub enum Message {
    QueryChanged(String),
    Copy(String),
    Delete(String),
    ClearGroup(String),
    ToggleGroup(String),
    ClearAll,
    ToggleDnd,
    Hover(Option<String>),
    Close,
    MoveSelection(i32),
    ActivateSelected,
    Tick,
    Shown,
}

pub struct App {
    config: Config,
    items: Vec<NotifItem>,
    preview_on: bool,
    query: String,
    /// Index into `visible_cards()`, for arrow-key navigation (plain
    /// usize, same convention as og-search).
    selected: usize,
    /// Selection highlight only appears once the arrow keys are used.
    keyboard_nav: bool,
    expanded: HashSet<String>,
    hovered: Option<String>,
    /// When the surface first got a frame — the slide starts there, not
    /// at launch (startup alone takes longer than the slide).
    opened: Option<Instant>,
    launched: Instant,
    closing: Option<Instant>,
}

struct Group<'a> {
    app: &'a str,
    items: Vec<&'a NotifItem>,
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        notif::ensure_dnd_mode_configured();
        notif::dismiss_banners();
        let watch_term = Task::perform(
            async {
                // SIGTERM = the bell was clicked again (see main.rs toggle).
                if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    s.recv().await;
                }
            },
            |_| Message::Close,
        );
        (
            Self {
                config: Config::load(),
                items: notif::load_visible(),
                preview_on: notif::preview_on(),
                query: String::new(),
                selected: 0,
                keyboard_nav: false,
                expanded: HashSet::new(),
                hovered: None,
                opened: None,
                launched: Instant::now(),
                closing: None,
            },
            Task::batch([text_input::focus(text_input::Id::new("query")), watch_term]),
        )
    }

    pub fn namespace(&self) -> String {
        "og-notif-center".into()
    }

    fn groups(&self) -> Vec<Group<'_>> {
        let q = self.query.trim().to_lowercase();
        let mut groups: Vec<Group> = Vec::new();
        for item in self.items.iter().filter(|i| q.is_empty() || i.label.to_lowercase().contains(&q)) {
            match groups.iter_mut().find(|g| g.app == item.app) {
                Some(g) => g.items.push(item),
                None => groups.push(Group { app: &item.app, items: vec![item] }),
            }
        }
        groups
    }

    fn is_expanded(&self, g: &Group) -> bool {
        g.items.len() == 1 || self.expanded.contains(g.app) || !self.query.trim().is_empty()
    }

    /// Cards in display order: one per collapsed group, all of an expanded one.
    fn visible_cards(&self) -> Vec<&NotifItem> {
        self.groups()
            .into_iter()
            .flat_map(|g| if self.is_expanded(&g) { g.items } else { vec![g.items[0]] })
            .collect()
    }

    /// Slide offset in px: panel starts just off the right edge.
    fn offset(&self) -> f32 {
        let span = PANEL_W + EDGE * 2.0;
        match self.closing {
            Some(c) => span * ease_out(c.elapsed().as_secs_f32() / (SLIDE.as_secs_f32() * 0.8)),
            None => span * (1.0 - ease_out(self.opened.map_or(0.0, |o| o.elapsed().as_secs_f32()) / SLIDE.as_secs_f32())),
        }
    }

    fn animating(&self) -> bool {
        self.closing.is_some() || self.opened.is_none_or(|o| o.elapsed() < SLIDE)
    }

    fn reload(&mut self) {
        self.items = notif::load_visible();
        self.selected = self.selected.min(self.visible_cards().len().saturating_sub(1));
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::QueryChanged(q) => {
                self.query = q;
                self.selected = 0;
            }
            Message::Copy(text) => {
                notif::copy_to_clipboard(&text);
                return self.update(Message::Close);
            }
            Message::Delete(raw) => {
                notif::hide(&raw);
                self.reload();
            }
            Message::ClearGroup(app) => {
                for item in self.items.iter().filter(|i| i.app == app) {
                    notif::hide(&item.raw);
                }
                self.expanded.remove(&app);
                self.reload();
            }
            Message::ToggleGroup(app) => {
                if !self.expanded.remove(&app) {
                    self.expanded.insert(app);
                }
            }
            Message::ClearAll => {
                notif::clear_all();
                self.items.clear();
                self.selected = 0;
            }
            Message::ToggleDnd => {
                self.preview_on = !self.preview_on;
                notif::set_preview(self.preview_on);
            }
            Message::Hover(raw) => self.hovered = raw,
            Message::Shown => {
                self.opened.get_or_insert_with(Instant::now);
            }
            Message::Close => {
                if self.closing.is_none() {
                    self.closing = Some(Instant::now());
                }
            }
            Message::Tick => {
                if self.opened.is_none() && self.launched.elapsed() > Duration::from_millis(500) {
                    self.opened = Some(Instant::now());
                }
                if self.closing.is_some_and(|c| c.elapsed() >= SLIDE.mul_f32(0.8)) {
                    crate::exit();
                }
            }
            Message::MoveSelection(delta) => {
                let len = self.visible_cards().len();
                if len > 0 && !self.keyboard_nav {
                    self.keyboard_nav = true;
                } else if len > 0 {
                    self.selected = (self.selected as i32 + delta).rem_euclid(len as i32) as usize;
                }
            }
            Message::ActivateSelected => {
                if let Some(item) = self.visible_cards().get(self.selected) {
                    let label = item.label.clone();
                    return self.update(Message::Copy(label));
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        let keys = iced::event::listen_with(|event, _status, _id| match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Close),
                keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::MoveSelection(1)),
                keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::MoveSelection(-1)),
                _ => None,
            },
            _ => None,
        });
        if self.opened.is_none() {
            // listen_with drops RedrawRequested; listen_raw doesn't.
            let shown = iced::event::listen_raw(|event, _, _| {
                matches!(event, Event::Window(iced::window::Event::RedrawRequested(_))).then_some(Message::Shown)
            });
            iced::Subscription::batch([keys, shown, iced::time::every(Duration::from_millis(8)).map(|_| Message::Tick)])
        } else if self.animating() {
            iced::Subscription::batch([keys, iced::time::every(Duration::from_millis(8)).map(|_| Message::Tick)])
        } else {
            keys
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let offset = self.offset();
        responsive(move |size| {
            // Everything left of the panel is a transparent click-catcher;
            // during the slide the row is wider than the surface and the
            // panel's overhang is simply clipped at the right edge.
            let gap = (size.width - PANEL_W - EDGE).max(0.0) + offset;
            // A row would squeeze the panel to fit; inside a (hidden)
            // horizontal scrollable it keeps its width and overhangs.
            let strip = row![
                mouse_area(Space::new(Length::Fixed(gap), Length::Fill)).on_press(Message::Close),
                container(self.panel()).width(Length::Fixed(PANEL_W)).height(Length::Fill).padding(Padding {
                    top: EDGE,
                    bottom: EDGE,
                    ..Padding::ZERO
                }),
                mouse_area(Space::new(Length::Fixed(EDGE), Length::Fill)).on_press(Message::Close),
            ]
            .height(Length::Fill);
            scrollable(strip)
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0).scroller_width(0).margin(0)))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        })
        .into()
    }

    fn panel(&self) -> Element<'_, Message> {
        let c = AppColors::from_config(&self.config, APP_TINT_SEED);
        let bold = fonts::weighted(Weight::Bold);
        let semibold = fonts::weighted(Weight::Semibold);

        let pill = move |on: bool| {
            move |_: &_, status: button::Status| button::Style {
                background: Some(Background::Color(if on {
                    c.accent
                } else if status == button::Status::Hovered {
                    with_alpha(Color::WHITE, 0.12)
                } else {
                    with_alpha(Color::WHITE, 0.06)
                })),
                text_color: if on { c.bar_bg } else { c.text },
                border: Border { radius: 999.0.into(), ..Default::default() },
                ..Default::default()
            }
        };

        let header = row![
            text("Notifications").size(20).font(bold).color(c.text),
            horizontal_space(),
            button(text("Do Not Disturb").size(12))
                .style(pill(!self.preview_on))
                .on_press(Message::ToggleDnd)
                .padding([5, 12]),
            button(text("Clear All").size(12)).style(pill(false)).on_press(Message::ClearAll).padding([5, 12]),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let search = text_input("Search", &self.query)
            .id(text_input::Id::new("query"))
            .on_input(Message::QueryChanged)
            .on_submit(Message::ActivateSelected)
            .padding([7, 12])
            .size(14)
            .style(move |_, status| text_input::Style {
                background: Background::Color(with_alpha(Color::WHITE, 0.06)),
                border: Border {
                    color: if matches!(status, text_input::Status::Focused) { with_alpha(c.accent, 0.7) } else { with_alpha(Color::WHITE, 0.1) },
                    width: 1.0,
                    radius: 10.0.into(),
                },
                icon: c.dim_text,
                placeholder: c.dim_text,
                value: c.text,
                selection: with_alpha(c.accent, 0.5),
            });

        let selected_raw = self.visible_cards().get(self.selected).filter(|_| self.keyboard_nav).map(|i| i.raw.clone());
        let groups = self.groups();
        let body: Element<Message> = if groups.is_empty() {
            container(text(if self.query.is_empty() { "No Notifications" } else { "No Results" }).size(15).color(c.dim_text))
                .center_x(Length::Fill)
                .padding([40, 0])
                .into()
        } else {
            let mut list = column![].spacing(12);
            for g in &groups {
                let expanded = self.is_expanded(g);
                let mut group = column![].spacing(6);
                if g.items.len() > 1 {
                    let ghost = move |_: &_, status: button::Status| button::Style {
                        background: Some(Background::Color(if status == button::Status::Hovered {
                            with_alpha(Color::WHITE, 0.12)
                        } else {
                            with_alpha(Color::WHITE, 0.05)
                        })),
                        text_color: c.dim_text,
                        border: Border { radius: 999.0.into(), ..Default::default() },
                        ..Default::default()
                    };
                    let toggle = if expanded { "Show Less".to_string() } else { format!("{} more", g.items.len() - 1) };
                    group = group.push(
                        row![
                            text(g.app.to_string()).size(13).font(semibold).color(c.text),
                            horizontal_space(),
                            button(text(toggle).size(11)).style(ghost).on_press(Message::ToggleGroup(g.app.to_string())).padding([3, 10]),
                            button(text("×").size(13)).style(ghost).on_press(Message::ClearGroup(g.app.to_string())).padding([1, 8]),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .padding([0, 4]),
                    );
                }
                if expanded {
                    for item in &g.items {
                        group = group.push(self.card(item, &c, selected_raw.as_deref() == Some(&item.raw), None));
                    }
                } else {
                    group = group.push(self.card(g.items[0], &c, selected_raw.as_deref() == Some(&g.items[0].raw), Some(g.items.len())));
                }
                list = list.push(group);
            }
            scrollable(list)
                .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4).spacing(4)))
                .height(Length::Fill)
                .style(move |_, status| {
                    let rail = |a: f32| scrollable::Rail {
                        background: None,
                        border: Border::default(),
                        scroller: scrollable::Scroller {
                            color: with_alpha(Color::WHITE, a),
                            border: Border { radius: 2.0.into(), ..Default::default() },
                        },
                    };
                    let a = if matches!(status, scrollable::Status::Active) { 0.15 } else { 0.3 };
                    scrollable::Style {
                        container: container::Style::default(),
                        vertical_rail: rail(a),
                        horizontal_rail: rail(a),
                        gap: None,
                    }
                })
                .into()
        };

        container(column![header, search, body].spacing(12))
            .padding(14)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(with_alpha(c.bar_bg, 0.93))),
                border: Border { color: with_alpha(Color::WHITE, 0.10), width: 1.0, radius: 18.0.into() },
                ..Default::default()
            })
            .into()
    }

    /// One notification card. `stacked = Some(n)` draws a collapsed group:
    /// the newest card with the edges of the ones behind it peeking out,
    /// and clicking expands instead of copying.
    fn card<'a>(&self, item: &'a NotifItem, c: &AppColors, selected: bool, stacked: Option<usize>) -> Element<'a, Message> {
        let c = *c;
        let bold = fonts::weighted(Weight::Bold);
        let hovered = self.hovered.as_deref() == Some(&item.raw);

        let icon: Element<Message> = match &item.icon {
            Some(Icon::Raster(p)) => image(p.clone()).width(ICON).height(ICON).into(),
            Some(Icon::Svg(p)) => svg(p.clone()).width(ICON).height(ICON).into(),
            None => container(text(item.app.chars().next().unwrap_or('?').to_uppercase().to_string()).size(16).font(bold).color(c.bar_bg))
                .center_x(ICON)
                .center_y(ICON)
                .style(move |_| container::Style {
                    background: Some(Background::Color(c.accent)),
                    border: Border { radius: 9.0.into(), ..Default::default() },
                    ..Default::default()
                })
                .into(),
        };

        let close = button(text("×").size(12))
            .style(move |_: &_, status: button::Status| button::Style {
                background: Some(Background::Color(with_alpha(Color::WHITE, if status == button::Status::Hovered { 0.2 } else { 0.1 }))),
                text_color: c.text,
                border: Border { radius: 999.0.into(), ..Default::default() },
                ..Default::default()
            })
            .padding([0, 6])
            .on_press(match stacked {
                Some(_) => Message::ClearGroup(item.app.clone()),
                None => Message::Delete(item.raw.clone()),
            });
        let top = row![
            text(item.app.clone()).size(11).color(c.dim_text),
            horizontal_space(),
            if hovered { Element::from(close) } else { Space::new(0, 16).into() },
        ]
        .align_y(Alignment::Center);

        let mut lines = column![top].spacing(2);
        if !item.summary.is_empty() {
            lines = lines.push(text(item.summary.clone()).size(14).font(bold).color(c.text));
        }
        if !item.body.is_empty() {
            lines = lines.push(text(item.body.clone()).size(13).color(with_alpha(c.text, 0.85)));
        }

        let border_color = if selected {
            c.accent
        } else if item.critical {
            with_alpha(c.accent, 0.8)
        } else {
            with_alpha(Color::WHITE, 0.13)
        };
        let lift = if hovered { 0.97 } else { 0.92 };
        let face = container(row![icon, lines].spacing(12).align_y(Alignment::Start))
            .padding([10, 12])
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(with_alpha(c.sec_bg, lift))),
                border: Border { color: border_color, width: if selected { 1.5 } else { 1.0 }, radius: 14.0.into() },
                ..Default::default()
            });

        let press = match stacked {
            Some(_) => Message::ToggleGroup(item.app.clone()),
            None => Message::Copy(item.label.clone()),
        };
        let mut card = column![face];
        if let Some(n) = stacked {
            // Edges of the cards behind: progressively narrower slivers.
            for (k, inset) in [(1usize, 10.0), (2, 20.0)] {
                if n > k {
                    let sliver = container(Space::new(Length::Fill, 6)).width(Length::Fill).style(move |_| container::Style {
                                background: Some(Background::Color(with_alpha(c.sec_bg, 0.85 - 0.2 * k as f32))),
                                border: Border {
                                    color: with_alpha(Color::WHITE, 0.08),
                                    width: 1.0,
                                    radius: iced::border::Radius { top_left: 0.0, top_right: 0.0, bottom_right: 10.0, bottom_left: 10.0 },
                                },
                                ..Default::default()
                            });
                    card = card.push(container(sliver).padding(Padding { left: inset, right: inset, ..Padding::ZERO }));
                }
            }
        }
        mouse_area(card)
            .on_enter(Message::Hover(Some(item.raw.clone())))
            .on_exit(Message::Hover(None))
            .on_press(press)
            .into()
    }
}
