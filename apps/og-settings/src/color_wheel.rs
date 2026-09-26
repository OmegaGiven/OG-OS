use iced::widget::canvas::{self, Frame, Geometry};
use iced::{Color, Element, Length, Rectangle, Renderer, Theme};
use iced::mouse;

use crate::app::{AppColors, Message};

const SIZE: f32 = 200.0;
const CELL: f32 = 3.0;

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color::from_rgb(r + m, g + m, b + m)
}

pub fn rgb_to_hsv(color: Color) -> (f32, f32, f32) {
    let (r, g, b) = (color.r, color.g, color.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta.abs() < f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let s = if max.abs() < f32::EPSILON { 0.0 } else { delta / max };
    let v = max;
    (h, s, v)
}

/// Draggable hue/saturation disc — angle is hue, distance from center is
/// saturation. Rendered at full value; a separate brightness slider handles
/// V so the wheel itself never needs redrawing when only brightness changes.
pub struct ColorWheel {
    pub hue: f32,
    pub sat: f32,
    pub colors: AppColors,
}

impl ColorWheel {
    fn polar_from(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let center = SIZE / 2.0;
        let radius = SIZE / 2.0;
        let dx = x - center;
        let dy = y - center;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist > radius {
            return None;
        }
        let sat = (dist / radius).min(1.0);
        let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
        Some((hue, sat))
    }
}

impl canvas::Program<Message> for ColorWheel {
    type State = bool; // dragging

    fn update(
        &self,
        dragging: &mut bool,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        let local = cursor.position_in(bounds);

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = local {
                    *dragging = true;
                    if let Some((h, s)) = self.polar_from(pos.x, pos.y) {
                        return (canvas::event::Status::Captured, Some(Message::ColorWheelChanged(h, s)));
                    }
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if *dragging {
                    if let Some(pos) = local {
                        // Clamp to the disc even if the cursor strays outside it.
                        let center = SIZE / 2.0;
                        let dx = pos.x - center;
                        let dy = pos.y - center;
                        let dist = (dx * dx + dy * dy).sqrt().max(0.0001);
                        let radius = SIZE / 2.0;
                        let sat = (dist / radius).min(1.0);
                        let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                        return (canvas::event::Status::Captured, Some(Message::ColorWheelChanged(hue, sat)));
                    }
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                *dragging = false;
            }
            _ => {}
        }
        (canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        _state: &bool,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = SIZE / 2.0;
        let radius = SIZE / 2.0;

        let mut y = 0.0f32;
        while y < SIZE {
            let mut x = 0.0f32;
            while x < SIZE {
                let cx = x + CELL / 2.0;
                let cy = y + CELL / 2.0;
                let dx = cx - center;
                let dy = cy - center;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= radius {
                    let sat = (dist / radius).min(1.0);
                    let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                    let color = hsv_to_rgb(hue, sat, 1.0);
                    frame.fill_rectangle(iced::Point::new(x, y), iced::Size::new(CELL, CELL), color);
                }
                x += CELL;
            }
            y += CELL;
        }

        // Cursor indicator at the current hue/saturation.
        let angle = self.hue.to_radians();
        let dist = self.sat * radius;
        let px = center + angle.cos() * dist;
        let py = center + angle.sin() * dist;
        let ring_color = if self.colors.text.r + self.colors.text.g + self.colors.text.b > 1.5 {
            Color::WHITE
        } else {
            Color::BLACK
        };
        let cursor_ring = canvas::Path::circle(iced::Point::new(px, py), 6.0);
        frame.stroke(&cursor_ring, canvas::Stroke::default().with_width(2.0).with_color(ring_color));

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, _state: &bool, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if cursor.position_in(bounds).is_some() {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

pub fn view<'a>(hue: f32, sat: f32, colors: AppColors) -> Element<'a, Message> {
    iced::widget::canvas(ColorWheel { hue, sat, colors })
        .width(Length::Fixed(SIZE))
        .height(Length::Fixed(SIZE))
        .into()
}
