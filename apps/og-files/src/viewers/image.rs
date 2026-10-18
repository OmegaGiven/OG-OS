//! Image viewer. The picture is drawn at its exact fitted size (shrunk to
//! fit, never stretched), centred on the app background: no bars, so the
//! true aspect ratio and edges are obvious. Transparent pixels show a
//! checkerboard *inside* the image bounds, and an optional hairline marks
//! the exact edge. "Pan / zoom" switches to iced's interactive viewer.
//! Editing (crop, rotate, adjust) is phase 3 — SPEC.md §5.

use crate::theme;
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry};
use iced::widget::{button, center, column, container, image, responsive, row, stack, text, Space};
use iced::{Color, Element, Length, Rectangle, Renderer, Size, Theme};
use std::path::{Path, PathBuf};

/// What the file header says (read off the UI thread when the tab opens).
#[derive(Debug, Clone, Copy, Default)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
}

/// Dimensions and alpha channel from the header only (no full decode).
pub async fn probe(path: PathBuf) -> Info {
    tokio::task::spawn_blocking(move || probe_sync(&path))
        .await
        .unwrap_or_default()
}

fn probe_sync(path: &Path) -> Info {
    let Ok(reader) = ::image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) else {
        return Info::default();
    };
    match reader.into_decoder() {
        Ok(d) => {
            use ::image::ImageDecoder;
            let (width, height) = d.dimensions();
            Info {
                width,
                height,
                alpha: d.color_type().has_alpha(),
            }
        }
        Err(_) => Info::default(),
    }
}

pub struct State {
    handle: image::Handle,
    info: Info,
    fit: bool,
    edge: bool,
    checker: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    ToggleFit,
    ToggleEdge,
    ToggleChecker,
    /// handled by the registry (needs the path): writes `<name>.pdf`
    ToPdf,
    /// handled by the registry: split photos out of a scan
    SplitScan,
}

/// Size of the image fitted into `avail`: shrink to fit, never enlarge
/// (so small images show at true size), aspect preserved.
pub fn fitted(info: Info, avail: Size) -> Size {
    if info.width == 0 || info.height == 0 || avail.width <= 0.0 || avail.height <= 0.0 {
        return avail;
    }
    let (w, h) = (info.width as f32, info.height as f32);
    let s = (avail.width / w).min(avail.height / h).min(1.0);
    Size::new((w * s).floor().max(1.0), (h * s).floor().max(1.0))
}

impl State {
    pub fn new(path: PathBuf, info: Info) -> Self {
        Self {
            handle: image::Handle::from_path(path),
            info,
            fit: true,
            edge: true,
            checker: info.alpha,
        }
    }

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::ToggleFit => self.fit = !self.fit,
            Msg::ToggleEdge => self.edge = !self.edge,
            Msg::ToggleChecker => self.checker = !self.checker,
            Msg::ToPdf | Msg::SplitScan => {}
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let toggle = |label: &'static str, on: bool, m: Msg| {
            button(text(label).size(12)).on_press(m).style(if on {
                button::primary
            } else {
                button::secondary
            })
        };
        let dims = if self.info.width > 0 {
            format!(
                "{} × {} px{}",
                self.info.width,
                self.info.height,
                if self.info.alpha {
                    " · transparency"
                } else {
                    ""
                }
            )
        } else {
            String::new()
        };
        let mut bar = row![
            text(dims).size(12).color(theme::MUTED),
            Space::with_width(Length::Fill)
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        if self.info.alpha {
            bar = bar.push(toggle("Checker", self.checker, Msg::ToggleChecker));
        }
        bar = bar
            .push(toggle("Edge", self.edge, Msg::ToggleEdge))
            .push(
                button(text("Split scanned photos").size(12))
                    .on_press(Msg::SplitScan)
                    .style(button::secondary),
            )
            .push(
                button(text("Make PDF").size(12))
                    .on_press(Msg::ToPdf)
                    .style(button::secondary),
            )
            .push(
                button(text(if self.fit { "Pan / zoom" } else { "Fit" }).size(12))
                    .on_press(Msg::ToggleFit),
            );

        let body: Element<'_, Msg> = if self.fit {
            responsive(move |avail| {
                let size = fitted(self.info, avail);
                let mut layers: Vec<Element<'_, Msg>> = Vec::new();
                if self.checker && self.info.alpha {
                    layers.push(
                        Canvas::new(Checker)
                            .width(size.width)
                            .height(size.height)
                            .into(),
                    );
                }
                layers.push(
                    image(self.handle.clone())
                        .width(size.width)
                        .height(size.height)
                        .into(),
                );
                if self.edge {
                    layers.push(
                        container(Space::new(size.width, size.height))
                            .style(|t: &Theme| container::Style {
                                border: iced::Border {
                                    width: 1.0,
                                    color: t
                                        .extended_palette()
                                        .background
                                        .base
                                        .text
                                        .scale_alpha(0.3),
                                    radius: 0.0.into(),
                                },
                                ..Default::default()
                            })
                            .into(),
                    );
                }
                center(stack(layers).width(size.width).height(size.height)).into()
            })
            .into()
        } else {
            image::viewer(self.handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };
        column![bar, body].spacing(6).into()
    }
}

/// Classic transparency checkerboard, drawn only under the image.
struct Checker;

impl<M> canvas::Program<M> for Checker {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        const SQ: f32 = 8.0;
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(
            iced::Point::ORIGIN,
            bounds.size(),
            Color::from_rgb8(0xcc, 0xcc, 0xcc),
        );
        let dark = Color::from_rgb8(0x99, 0x99, 0x99);
        let (cols, rows) = (
            (bounds.width / SQ).ceil() as usize,
            (bounds.height / SQ).ceil() as usize,
        );
        for r in 0..rows {
            for c in (r % 2..cols).step_by(2) {
                frame.fill_rectangle(
                    iced::Point::new(c as f32 * SQ, r as f32 * SQ),
                    Size::new(SQ, SQ),
                    dark,
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_shrinks_never_enlarges_and_keeps_aspect() {
        let big = Info {
            width: 4000,
            height: 1000,
            alpha: false,
        };
        let s = fitted(big, Size::new(800.0, 600.0));
        assert_eq!((s.width, s.height), (800.0, 200.0));
        let small = Info {
            width: 64,
            height: 32,
            alpha: true,
        };
        let s = fitted(small, Size::new(800.0, 600.0));
        assert_eq!((s.width, s.height), (64.0, 32.0)); // true size, no bars
        let tall = Info {
            width: 1000,
            height: 3000,
            alpha: false,
        };
        let s = fitted(tall, Size::new(900.0, 600.0));
        assert_eq!((s.width, s.height), (200.0, 600.0));
    }

    #[test]
    fn probe_reads_header() {
        let dir = std::env::temp_dir().join(format!("ogx-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        ::image::RgbaImage::from_pixel(30, 20, ::image::Rgba([0, 0, 0, 0]))
            .save(&p)
            .unwrap();
        let i = probe_sync(&p);
        assert_eq!((i.width, i.height, i.alpha), (30, 20, true));
        let j = dir.join("b.jpg");
        ::image::RgbImage::new(10, 5).save(&j).unwrap();
        let i = probe_sync(&j);
        assert_eq!((i.width, i.height, i.alpha), (10, 5, false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
