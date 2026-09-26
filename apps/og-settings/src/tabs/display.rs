use std::collections::HashMap;

use iced::widget::{button, canvas, column, container, row, stack, text, toggler};
use iced::{Background, Border, Color, Element, Length, Padding, Pixels, Rectangle, Renderer, Theme};
use iced::widget::canvas::{Frame, Geometry};
use iced::mouse;

use crate::app::{AppColors, Message};
use crate::config::Config;
use crate::sway::MonitorInfo;

// ── Drag-arrange canvas ────────────────────────────────────────────────────────
// Drag state lives entirely in App, so draw() always sees the truth.
// Canvas only emits ArrangeDrag* messages; App applies snap on DragEnd.

pub struct ArrangeCanvas<'a> {
    pub monitors: &'a [MonitorInfo],
    pub colors: AppColors,
    pub scale: f32,
    pub positions: &'a HashMap<String, (f32, f32)>,
    pub dragging_name: Option<&'a str>,
}

impl<'a> ArrangeCanvas<'a> {
    fn box_at(&self, m: &MonitorInfo) -> (f32, f32, f32, f32) {
        let (x, y) = self.positions.get(&m.name).copied()
            .unwrap_or((m.x as f32 * self.scale + 10.0, m.y as f32 * self.scale + 10.0));
        let w = (m.width  as f32 * self.scale).max(60.0);
        let h = (m.height as f32 * self.scale).max(36.0);
        (x, y, w, h)
    }

    fn hit_at(&self, px: f32, py: f32) -> Option<String> {
        for m in self.monitors {
            let (x, y, w, h) = self.box_at(m);
            if px >= x && px <= x + w && py >= y && py <= y + h {
                return Some(m.name.clone());
            }
        }
        None
    }
}

impl<'a> canvas::Program<Message> for ArrangeCanvas<'a> {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        let local = cursor.position_in(bounds);

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = local {
                    if let Some(name) = self.hit_at(pos.x, pos.y) {
                        let (bx, by, _, _) = self.box_at(
                            self.monitors.iter().find(|m| m.name == name).unwrap(),
                        );
                        return (canvas::event::Status::Captured,
                            Some(Message::ArrangeDragStart(name, pos.x - bx, pos.y - by)));
                    }
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if self.dragging_name.is_some() {
                    // Clamp to canvas bounds even if cursor leaves
                    let cx = local.map(|p| p.x).unwrap_or_else(|| {
                        cursor.position().map(|p| (p.x - bounds.x).clamp(0.0, bounds.width)).unwrap_or(0.0)
                    });
                    let cy = local.map(|p| p.y).unwrap_or_else(|| {
                        cursor.position().map(|p| (p.y - bounds.y).clamp(0.0, bounds.height)).unwrap_or(0.0)
                    });
                    return (canvas::event::Status::Captured, Some(Message::ArrangeDragMove(cx, cy)));
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if self.dragging_name.is_some() {
                    return (canvas::event::Status::Captured, Some(Message::ArrangeDragEnd));
                }
            }
            _ => {}
        }
        (canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let colors = self.colors;
        let bg_color = Color {
            r: colors.bar_bg.r * 0.6,
            g: colors.bar_bg.g * 0.6,
            b: colors.bar_bg.b * 0.6,
            a: 1.0,
        };

        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(iced::Point::ORIGIN, bounds.size(), bg_color);

        for m in self.monitors {
            let (x, y, w, h) = self.box_at(m);
            let is_dragging = self.dragging_name == Some(m.name.as_str());
            let hovered = !is_dragging && cursor.position_in(bounds)
                .map(|p| p.x >= x && p.x <= x + w && p.y >= y && p.y <= y + h)
                .unwrap_or(false);

            // Draw border as an outline rect (more reliable than frame.stroke)
            let border_color = if is_dragging {
                colors.accent
            } else if hovered {
                Color { a: 0.7, ..colors.accent }
            } else {
                Color { a: 0.35, ..colors.accent }
            };
            let bw = if is_dragging { 3.0f32 } else { 1.5 };
            frame.fill_rectangle(
                iced::Point::new(x - bw, y - bw),
                iced::Size::new(w + bw * 2.0, h + bw * 2.0),
                border_color,
            );

            // Fill
            let fill = if is_dragging {
                colors.accent
            } else if hovered {
                Color { r: colors.surface.r * 0.6 + colors.accent.r * 0.4, g: colors.surface.g * 0.6 + colors.accent.g * 0.4, b: colors.surface.b * 0.6 + colors.accent.b * 0.4, a: 1.0 }
            } else if m.active {
                colors.surface
            } else {
                Color { a: 0.55, ..colors.surface }
            };
            frame.fill_rectangle(iced::Point::new(x, y), iced::Size::new(w, h), fill);

            // Label
            let label_color = if is_dragging { colors.bar_bg } else { colors.text };
            frame.fill_text(canvas::Text {
                content: m.name.clone(),
                position: iced::Point::new(x + w / 2.0, y + h / 2.0 - 7.0),
                color: label_color,
                size: Pixels(11.0),
                font: iced::Font::MONOSPACE,
                horizontal_alignment: iced::alignment::Horizontal::Center,
                vertical_alignment: iced::alignment::Vertical::Top,
                line_height: iced::widget::text::LineHeight::default(),
                shaping: iced::widget::text::Shaping::Basic,
            });
            frame.fill_text(canvas::Text {
                content: m.current_mode.clone(),
                position: iced::Point::new(x + w / 2.0, y + h / 2.0 + 4.0),
                color: Color { a: if is_dragging { 0.8 } else { 0.6 }, ..label_color },
                size: Pixels(9.0),
                font: iced::Font::MONOSPACE,
                horizontal_alignment: iced::alignment::Horizontal::Center,
                vertical_alignment: iced::alignment::Vertical::Top,
                line_height: iced::widget::text::LineHeight::default(),
                shaping: iced::widget::text::Shaping::Basic,
            });
        }

        // Hint
        if self.dragging_name.is_none() {
            frame.fill_text(canvas::Text {
                content: "Drag monitors · Release to snap · Exit Arrange to apply".to_string(),
                position: iced::Point::new(bounds.width / 2.0, bounds.height - 14.0),
                color: Color { a: 0.45, ..colors.text },
                size: Pixels(11.0),
                font: iced::Font::default(),
                horizontal_alignment: iced::alignment::Horizontal::Center,
                vertical_alignment: iced::alignment::Vertical::Bottom,
                line_height: iced::widget::text::LineHeight::default(),
                shaping: iced::widget::text::Shaping::Basic,
            });
        } else {
            frame.fill_text(canvas::Text {
                content: "Release to snap to nearest edge".to_string(),
                position: iced::Point::new(bounds.width / 2.0, bounds.height - 14.0),
                color: colors.accent,
                size: Pixels(11.0),
                font: iced::Font::default(),
                horizontal_alignment: iced::alignment::Horizontal::Center,
                vertical_alignment: iced::alignment::Vertical::Bottom,
                line_height: iced::widget::text::LineHeight::default(),
                shaping: iced::widget::text::Shaping::Basic,
            });
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if self.dragging_name.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if let Some(pos) = cursor.position_in(bounds) {
            if self.hit_at(pos.x, pos.y).is_some() {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::default()
    }
}

// ── Main view ─────────────────────────────────────────────────────────────────

pub fn view<'a>(
    config: &'a Config,
    colors: AppColors,
    monitors: &'a [MonitorInfo],
    arrange_mode: bool,
    arrange_scale: f32,
    arrange_positions: &'a HashMap<String, (f32, f32)>,
    dragging_name: Option<&'a str>,
    has_backlight: bool,
    brightness: i32,
) -> Element<'a, Message> {
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

    // ── Monitor arrangement card ───────────────────────────────────────────
    let arrangement_card = if monitors.is_empty() {
        container(
            text("No monitors detected.")
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
        )
        .style(card_style).padding(20).width(Length::Fill)
    } else {
        // Compute scale from current monitors (same formula as arrange_scale)
        let max_x = monitors.iter().map(|m| m.x + m.width).max().unwrap_or(1920).max(1) as f32;
        let max_y = monitors.iter().map(|m| m.y + m.height).max().unwrap_or(1080).max(1) as f32;
        let preview_scale = (660.0f32 / max_x).min(140.0 / max_y);

        let arrange_btn = if arrange_mode {
            button(
                text("Exit Arrange").size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.bar_bg) })
            )
            .style(move |_, _| iced::widget::button::Style {
                background: Some(Background::Color(colors.accent)),
                text_color: colors.bar_bg,
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            })
            .on_press(Message::ArrangeModeToggle)
            .padding([6, 14])
        } else {
            button(
                text("Arrange").size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            )
            .style(btn_style)
            .on_press(Message::ArrangeModeToggle)
            .padding([6, 14])
        };

        let title_row = row![
            text("Monitor Arrangement").size(15)
                .style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
            iced::widget::horizontal_space(),
            arrange_btn,
        ]
        .align_y(iced::Alignment::Center);

        let visual: Element<Message> = if arrange_mode {
            canvas(ArrangeCanvas {
                monitors,
                colors,
                scale: arrange_scale,
                positions: arrange_positions,
                dragging_name,
            })
            .width(Length::Fill)
            .height(200)
            .into()
        } else {
            // Static preview
            let bg_color = Color {
                r: colors.bar_bg.r * 0.8,
                g: colors.bar_bg.g * 0.8,
                b: colors.bar_bg.b * 0.8,
                a: 1.0,
            };
            let border_col = colors.border;
            let text_col = colors.text;
            let accent = colors.accent;
            let surface = colors.surface;
            let sc = preview_scale;

            let mut layers: Vec<Element<Message>> = vec![
                container(iced::widget::Space::new(Length::Fill, 150u16))
                    .width(Length::Fill).height(150)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(bg_color)),
                        border: Border { color: border_col, width: 1.0, radius: colors.radius.into() },
                        ..Default::default()
                    })
                    .into(),
            ];

            for m in monitors {
                let x_off = m.x as f32 * sc + 10.0;
                let y_off = m.y as f32 * sc + 10.0;
                let w = ((m.width  as f32 * sc) as u16).max(60);
                let h = ((m.height as f32 * sc) as u16).max(36);
                let active = m.active;
                let name = m.name.clone();
                let mode = m.current_mode.clone();
                layers.push(
                    container(
                        container(
                            column![
                                text(name).size(10).style(move |_| iced::widget::text::Style { color: Some(text_col) }),
                                text(mode).size(9).style(move |_| iced::widget::text::Style { color: Some(text_col) }),
                            ]
                            .spacing(2).align_x(iced::Alignment::Center)
                        )
                        .width(w).height(h)
                        .style(move |_| container::Style {
                            background: Some(Background::Color(if active { accent } else { surface })),
                            border: Border { color: border_col, width: 1.0, radius: 3.0.into() },
                            ..Default::default()
                        })
                    )
                    .padding(Padding { top: y_off, left: x_off, bottom: 0.0, right: 0.0 })
                    .into()
                );
            }
            stack(layers).into()
        };

        let hint = if arrange_mode {
            "Drag monitors to reposition. Release snaps to edges. Click Exit Arrange to apply."
        } else {
            "Active monitors shown in accent color. Click Arrange to drag and reposition."
        };

        container(
            column![
                title_row,
                visual,
                text(hint).size(11)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
            ]
            .spacing(12).padding(20),
        )
        .style(card_style)
        .width(Length::Fill)
    };

    // ── Per-monitor resolution selection ──────────────────────────────────
    let monitor_cards: Vec<Element<Message>> = monitors.iter().map(|m| {
        let name = m.name.clone();
        let current = m.current_mode.clone();

        let mode_buttons: Vec<Element<Message>> = m.modes.iter().take(20).map(|mode| {
            let is_current = mode == &current;
            let mode_clone = mode.clone();
            let name_clone = name.clone();
            button(
                text(mode).size(12).style(move |_| iced::widget::text::Style {
                    color: Some(if is_current { colors.bar_bg } else { colors.text }),
                })
            )
            .style(move |_, _| iced::widget::button::Style {
                background: Some(Background::Color(if is_current { colors.accent } else { colors.surface })),
                text_color: if is_current { colors.bar_bg } else { colors.text },
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                ..Default::default()
            })
            .on_press(Message::MonitorResolutionChanged(name_clone, mode_clone))
            .padding([4, 8])
            .into()
        }).collect();

        container(
            column![
                row![
                    text(format!("{} — {} {}", m.name, m.make, m.model)).size(14)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
                    iced::widget::horizontal_space(),
                    text(format!("Current: {}", current)).size(12)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.accent) }),
                ]
                .align_y(iced::Alignment::Center),
                row(mode_buttons).spacing(6).wrap(),
            ]
            .spacing(10).padding(16),
        )
        .style(card_style).width(Length::Fill).into()
    }).collect();

    // ── Brightness ───────────────────────────────────────────────────────
    let dim_or_text = move |t: &'a str| -> Element<'a, Message> {
        text(t).style(move |_| iced::widget::text::Style {
            color: Some(if has_backlight { colors.text } else { colors.dim_text }),
        }).into()
    };
    let disabled_spin_btn = move |label_txt: &'static str| -> Element<'a, Message> {
        button(text(label_txt).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }))
            .style(move |_: &_, _| iced::widget::button::Style {
                background: Some(Background::Color(colors.surface)),
                text_color: colors.dim_text,
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                ..Default::default()
            })
            .into()
    };
    let spin = move |value: i32, minus: Message, plus: Message| -> Element<'a, Message> {
        row![
            button(text("-").style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(btn_style).on_press(minus),
            container(text(value.to_string()).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.surface)),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .padding([4, 12]),
            button(text("+").style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
                .style(btn_style).on_press(plus),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    };
    let brightness_row: Element<Message> = row![
        dim_or_text("Screen brightness"),
        iced::widget::horizontal_space(),
        if has_backlight {
            let el: Element<Message> = row![
                spin(brightness, Message::BrightnessMinus, Message::BrightnessPlus),
                text(format!("{brightness}%")).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
            ].spacing(8).align_y(iced::Alignment::Center).into();
            el
        } else {
            row![
                disabled_spin_btn("-"),
                container(text("—").style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }))
                    .style(move |_| container::Style {
                        background: Some(Background::Color(colors.surface)),
                        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                        ..Default::default()
                    })
                    .padding([4, 12]),
                disabled_spin_btn("+"),
            ].spacing(4).align_y(iced::Alignment::Center).into()
        },
    ]
    .align_y(iced::Alignment::Center)
    .spacing(12)
    .into();

    let show_backlight_module = config.modules_left.iter().any(|m| m == "backlight")
        || config.modules_center.iter().any(|m| m == "backlight")
        || config.modules_right.iter().any(|m| m == "backlight");

    let mut brightness_col: Vec<Element<Message>> = vec![
        text("Brightness").size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into(),
        brightness_row,
        row![
            dim_or_text("Show as taskbar module"),
            iced::widget::horizontal_space(),
            toggler(show_backlight_module).on_toggle_maybe(
                has_backlight.then_some(Message::BrightnessModuleToggled)
            ),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12)
        .into(),
    ];
    if !has_backlight {
        brightness_col.push(
            text("No backlight device detected — this machine uses external monitors (DDC/OSD), not a controllable panel. Controls stay wired for laptop builds of this same app.")
                .size(11)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                .into()
        );
    }

    let brightness_card = container(
        column(brightness_col)
        .spacing(16)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    let mut all: Vec<Element<Message>> = vec![arrangement_card.into()];
    all.extend(monitor_cards);
    all.push(brightness_card.into());
    column(all).spacing(16).padding(20).into()
}
