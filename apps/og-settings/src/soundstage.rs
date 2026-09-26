//! Top-down "soundstage" canvas for the Spatial subtab.
//!
//! A listener's head in the middle, seven speaker directions around it
//! (FL FR FC RL RR SL SR — LFE isn't directional). Click a direction and
//! the parent fires that channel through the HRTF chain.
//!
//! Two uses, same widget:
//!   * **Free play** — click anywhere, hear that position.
//!   * **Localization test** — a hidden position plays, you click your
//!     guess, it reveals hit/miss and keeps score.

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};
use iced::mouse;

use crate::app::{AppColors, Message};

const SIZE: f32 = 320.0;

/// channel slot -> (label, angle from "forward" in degrees, clockwise +)
/// slots match `surround`/`CHANNEL_LABELS`: 0 FL 1 FR 2 FC 4 RL 5 RR 6 SL 7 SR
pub const SPOTS: [(u8, &str, f32); 7] = [
    (2, "C", 0.0),
    (1, "FR", 30.0),
    (7, "SR", 90.0),
    (5, "RR", 135.0),
    (4, "RL", -135.0),
    (6, "SL", -90.0),
    (0, "FL", -30.0),
];

/// the 7 directional slots in clockwise ring order (FC FR SR RR RL SL FL)
pub const RING: [u8; 7] = [2, 1, 7, 5, 4, 6, 0];

/// true if slots `a` and `b` sit next to each other on the ring
pub fn adjacent(a: u8, b: u8) -> bool {
    let (ia, ib) = (RING.iter().position(|&x| x == a), RING.iter().position(|&x| x == b));
    match (ia, ib) {
        (Some(i), Some(j)) => {
            let d = (i as i32 - j as i32).rem_euclid(RING.len() as i32);
            d == 1 || d == RING.len() as i32 - 1
        }
        _ => false,
    }
}

fn spot_point(center: Point, radius: f32, deg: f32) -> Point {
    let r = deg.to_radians();
    Point::new(center.x + r.sin() * radius, center.y - r.cos() * radius)
}

pub struct SoundStage {
    /// green — the correct / just-played position
    pub reveal: Option<u8>,
    /// amber — the user's guess (test mode, after answering)
    pub guess: Option<u8>,
    /// true while a test round is waiting for a guess (dims nothing, just
    /// affects the hint text drawn)
    pub awaiting: bool,
    pub colors: AppColors,
}

impl SoundStage {
    fn nearest_slot(&self, bounds: Rectangle, p: Point) -> Option<u8> {
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let dx = p.x - center.x;
        let dy = p.y - center.y;
        if (dx * dx + dy * dy).sqrt() < 18.0 {
            return None; // clicked the head itself
        }
        let ang = dx.atan2(-dy).to_degrees(); // 0 = forward, cw +
        SPOTS
            .iter()
            .min_by(|a, b| {
                let da = (a.2 - ang).rem_euclid(360.0).min((ang - a.2).rem_euclid(360.0));
                let db = (b.2 - ang).rem_euclid(360.0).min((ang - b.2).rem_euclid(360.0));
                da.partial_cmp(&db).unwrap()
            })
            .map(|s| s.0)
    }
}

impl canvas::Program<Message> for SoundStage {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        if let canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
            if let Some(p) = cursor.position_in(bounds) {
                if let Some(slot) = self.nearest_slot(bounds, p) {
                    return (canvas::event::Status::Captured, Some(Message::SpatialStageClick(slot)));
                }
            }
        }
        (canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let c = self.colors;
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let radius = (bounds.width.min(bounds.height) / 2.0) - 34.0;

        // outer ring
        frame.stroke(
            &Path::circle(center, radius),
            Stroke::default().with_width(1.0).with_color(c.border),
        );

        // head: circle + nose triangle pointing forward (up)
        frame.fill(&Path::circle(center, 16.0), c.sec_bg);
        frame.stroke(
            &Path::circle(center, 16.0),
            Stroke::default().with_width(1.5).with_color(c.text),
        );
        let nose = Path::new(|b| {
            b.move_to(Point::new(center.x - 6.0, center.y - 13.0));
            b.line_to(Point::new(center.x + 6.0, center.y - 13.0));
            b.line_to(Point::new(center.x, center.y - 24.0));
            b.close();
        });
        frame.fill(&nose, c.text);

        // direction markers
        for (slot, label, deg) in SPOTS {
            let p = spot_point(center, radius, deg);
            let (fill, ring) = if self.reveal == Some(slot) {
                (Color { r: 0.16, g: 0.55, b: 0.32, a: 1.0 }, Color::WHITE)
            } else if self.guess == Some(slot) {
                (Color { r: 0.75, g: 0.5, b: 0.12, a: 1.0 }, Color::WHITE)
            } else {
                (c.sec_bg, c.border)
            };
            frame.fill(&Path::circle(p, 15.0), fill);
            frame.stroke(&Path::circle(p, 15.0), Stroke::default().with_width(1.0).with_color(ring));
            frame.fill_text(Text {
                content: label.to_string(),
                position: Point::new(p.x, p.y - 6.0),
                color: c.text,
                size: 11.0.into(),
                horizontal_alignment: iced::alignment::Horizontal::Center,
                ..Default::default()
            });
        }

        let hint = if self.awaiting {
            "listening… click where you heard it"
        } else {
            "click a direction to play it there"
        };
        frame.fill_text(Text {
            content: hint.to_string(),
            position: Point::new(center.x, bounds.height - 14.0),
            color: c.dim_text,
            size: 11.0.into(),
            horizontal_alignment: iced::alignment::Horizontal::Center,
            ..Default::default()
        });

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, _state: &(), bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if cursor.position_in(bounds).is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

pub fn view<'a>(reveal: Option<u8>, guess: Option<u8>, awaiting: bool, colors: AppColors) -> Element<'a, Message> {
    iced::widget::canvas(SoundStage { reveal, guess, awaiting, colors })
        .width(Length::Fixed(SIZE))
        .height(Length::Fixed(SIZE))
        .into()
}
