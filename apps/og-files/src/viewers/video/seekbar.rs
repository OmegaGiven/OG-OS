//! Seek bar (canvas): played part, demuxer cache, chapter ticks, A-B loop
//! and edit In/Out marks. Press/drag seeks (keyframe-fast while dragging),
//! release seeks exactly.

use super::Msg;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Size, Theme};

pub struct SeekBar {
    pub duration: f64,
    pub pos: f64,
    pub cache_end: Option<f64>,
    pub chapters: Vec<f64>,
    pub ab: (Option<f64>, Option<f64>),
    pub marks: (Option<f64>, Option<f64>),
}

#[derive(Default)]
pub struct Drag {
    active: bool,
}

/// Cursor x → time.
pub fn time_at(x: f32, bounds_w: f32, duration: f64) -> f64 {
    ((x / bounds_w.max(1.0)) as f64).clamp(0.0, 1.0) * duration.max(0.0)
}

impl canvas::Program<Msg> for SeekBar {
    type State = Drag;

    fn update(
        &self,
        st: &mut Drag,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Msg>) {
        use canvas::event::Status;
        if self.duration <= 0.0 {
            return (Status::Ignored, None);
        }
        let t = |p: Point| time_at(p.x - bounds.x, bounds.width, self.duration);
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_over(bounds) {
                    st.active = true;
                    return (Status::Captured, Some(Msg::SeekDrag(t(p))));
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) if st.active => {
                return (Status::Captured, Some(Msg::SeekDrag(t(position))));
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if st.active =>
            {
                st.active = false;
                return (Status::Captured, Some(Msg::SeekRelease));
            }
            _ => {}
        }
        (Status::Ignored, None)
    }

    fn draw(
        &self,
        _st: &Drag,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let pal = theme.extended_palette();
        let mut f = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let dur = self.duration.max(1e-6);
        let x = |t: f64| ((t / dur).clamp(0.0, 1.0) as f32) * w;
        let mid = h / 2.0;
        let bar = 4.0;
        let top = mid - bar / 2.0;
        let rect = |f: &mut Frame, x0: f32, x1: f32, c: Color| {
            if x1 > x0 {
                f.fill_rectangle(Point::new(x0, top), Size::new(x1 - x0, bar), c);
            }
        };
        rect(&mut f, 0.0, w, pal.background.strong.color);
        if let Some(c) = self.cache_end {
            rect(
                &mut f,
                x(self.pos),
                x(c),
                pal.background.base.text.scale_alpha(0.35),
            );
        }
        if let (Some(a), Some(b)) = self.marks {
            rect(&mut f, x(a), x(b), super::ff::AMBER.scale_alpha(0.45));
        }
        let accent = pal.primary.base.color;
        rect(&mut f, 0.0, x(self.pos), accent);
        let tick = |f: &mut Frame, t: f64, c: Color, len: f32| {
            let px = x(t).round() + 0.5;
            f.stroke(
                &Path::line(Point::new(px, mid - len), Point::new(px, mid + len)),
                Stroke::default().with_width(1.5).with_color(c),
            );
        };
        for &c in &self.chapters {
            if c > 0.0 {
                tick(&mut f, c, pal.background.base.text.scale_alpha(0.8), 5.0);
            }
        }
        for t in [self.ab.0, self.ab.1].into_iter().flatten() {
            tick(&mut f, t, Color::from_rgb(0.35, 0.8, 1.0), 7.0);
        }
        for t in [self.marks.0, self.marks.1].into_iter().flatten() {
            tick(&mut f, t, super::ff::AMBER, 7.0);
        }
        f.fill(&Path::circle(Point::new(x(self.pos), mid), 6.0), accent);
        if let Some(p) = cursor.position_in(bounds) {
            let px = p.x.round() + 0.5;
            f.stroke(
                &Path::line(Point::new(px, top - 2.0), Point::new(px, top + bar + 2.0)),
                Stroke::default()
                    .with_width(1.0)
                    .with_color(pal.background.base.text.scale_alpha(0.6)),
            );
        }
        vec![f.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _st: &Drag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) && self.duration > 0.0 {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_seekbar_time_at() {
        assert_eq!(time_at(50.0, 100.0, 10.0), 5.0);
        assert_eq!(time_at(-5.0, 100.0, 10.0), 0.0);
        assert_eq!(time_at(500.0, 100.0, 10.0), 10.0);
        assert_eq!(time_at(5.0, 0.0, 10.0), 10.0);
    }
}
