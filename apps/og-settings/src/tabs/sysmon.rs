use std::sync::{Arc, Mutex};

use iced::widget::{canvas, column, container, text};
use iced::{Color, Element, Font, Length, Pixels, Rectangle, Renderer, Theme};
use iced::widget::canvas::{Frame, Geometry};

use crate::app::{AppColors, Message};

/// Target on-screen font size for the embedded terminal.
pub const FONT_PX: f32 = 10.0;
/// Approximate monospace cell metrics at FONT_PX.
pub const CELL_W: f32 = FONT_PX * 0.60;
pub const CELL_H: f32 = FONT_PX * 1.30;
/// Vertical space taken by header + accent line + tab bar above the canvas.
const CHROME_H: f32 = 92.0;

/// Terminal grid that fits the given window size at FONT_PX.
pub fn grid_for(size: iced::Size) -> (u16, u16) {
    let w = size.width.max(200.0);
    let h = (size.height - CHROME_H).max(150.0);
    let cols = (w / CELL_W).floor().clamp(60.0, 500.0) as u16;
    let rows = (h / CELL_H).floor().clamp(20.0, 150.0) as u16;
    (rows, cols)
}

// ── Canvas program ─────────────────────────────────────────────────────────────

pub struct TerminalCanvas {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
}

impl TerminalCanvas {
    pub fn new(
        parser: Arc<Mutex<vt100::Parser>>,
        writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    ) -> Self {
        Self { parser, writer }
    }

    /// Terminal cell (1-based) under the given canvas point.
    fn cell_at(&self, pos: iced::Point, bounds: Rectangle) -> Option<(u16, u16)> {
        let (rows, cols) = {
            let parser = self.parser.lock().ok()?;
            parser.screen().size()
        };
        if rows == 0 || cols == 0 {
            return None;
        }
        let col = ((pos.x / (bounds.width / cols as f32)) as u16).min(cols - 1) + 1;
        let row = ((pos.y / (bounds.height / rows as f32)) as u16).min(rows - 1) + 1;
        Some((row, col))
    }
}

impl canvas::Program<Message> for TerminalCanvas {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        use iced::mouse;

        let canvas::Event::Mouse(mouse_event) = event else {
            return (canvas::event::Status::Ignored, None);
        };
        let Some(pos) = cursor.position_in(bounds) else {
            return (canvas::event::Status::Ignored, None);
        };
        let Some((row, col)) = self.cell_at(pos, bounds) else {
            return (canvas::event::Status::Ignored, None);
        };

        // btop enables SGR mouse reporting (CSI ?1002/?1006); encode events
        // in that format: CSI < btn ; col ; row M (press) / m (release).
        let seq = match mouse_event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                Some(format!("\x1b[<0;{col};{row}M"))
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                Some(format!("\x1b[<0;{col};{row}m"))
            }
            mouse::Event::WheelScrolled { delta } => {
                let up = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => y > 0.0,
                    mouse::ScrollDelta::Pixels { y, .. } => y > 0.0,
                };
                Some(format!("\x1b[<{};{col};{row}M", if up { 64 } else { 65 }))
            }
            _ => None,
        };

        match seq {
            Some(s) => {
                crate::pty::send_input(&self.writer, s.as_bytes());
                (canvas::event::Status::Captured, None)
            }
            None => (canvas::event::Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        frame.fill_rectangle(iced::Point::ORIGIN, bounds.size(), Color::BLACK);

        let Ok(parser) = self.parser.lock() else {
            return vec![frame.into_geometry()];
        };
        let screen = parser.screen();
        let (rows, cols) = screen.size();

        if rows == 0 || cols == 0 {
            return vec![frame.into_geometry()];
        }

        let cell_w = bounds.width  / cols as f32;
        let cell_h = bounds.height / rows as f32;
        // Cap by cell width too so wide windows don't overlap glyphs horizontally.
        let font_size = Pixels((cell_h * 0.80).min(cell_w * 1.55));

        let default_fg = Color::from_rgb8(200, 200, 200);
        let default_bg = Color::BLACK;

        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else { continue };

                let x = col as f32 * cell_w;
                let y = row as f32 * cell_h;

                let bg = crate::pty::vt_color(cell.bgcolor(), default_bg);
                if bg != default_bg {
                    frame.fill_rectangle(
                        iced::Point::new(x, y),
                        iced::Size::new(cell_w, cell_h),
                        bg,
                    );
                }

                let content = cell.contents();
                if !content.is_empty() && content != " " {
                    let fg = crate::pty::vt_color(cell.fgcolor(), default_fg);
                    frame.fill_text(canvas::Text {
                        content,
                        position: iced::Point::new(x, y),
                        color: fg,
                        size: font_size,
                        font: Font::MONOSPACE,
                        horizontal_alignment: iced::alignment::Horizontal::Left,
                        vertical_alignment: iced::alignment::Vertical::Top,
                        line_height: iced::widget::text::LineHeight::Relative(1.0),
                        shaping: iced::widget::text::Shaping::Basic,
                    });
                }
            }
        }

        vec![frame.into_geometry()]
    }
}

// ── View ───────────────────────────────────────────────────────────────────────

pub fn view<'a>(
    colors: AppColors,
    term: Option<&'a TerminalCanvas>,
) -> Element<'a, Message> {
    match term {
        Some(tc) => {
            canvas(tc)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        None => {
            container(
                column![
                    text("Starting btop...").size(14)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                    text("(btop must be installed and on PATH)").size(11)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                ]
                .spacing(8)
                .padding(40)
            )
            .style(move |_| iced::widget::container::Style {
                background: Some(iced::Background::Color(Color::BLACK)),
                ..Default::default()
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }
}
