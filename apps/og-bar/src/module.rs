//! One trait every bar module implements — adding a new module later means
//! implementing this trait, not touching bar layout code (PLAN.md section 5).

use iced::widget::{column, row, text};
use iced::{Color, Element, Length, Subscription};
use og_config::Edge;
use og_theme::AppColors;

use crate::message::Message;

/// Shared "short label + value" layout for cpu/memory-style modules.
/// Horizontal bars lay the two side by side (room to breathe along the
/// bar's own running direction); vertical bars stack label above value
/// (that's the direction with room there instead). Either way this is
/// sized to its own content (`Length::Shrink` via the caller's
/// `cell_length` override) rather than forced into the fixed square every
/// other module uses — that fixed square is what forced padding to 0 just
/// to keep a value like "100%" from clipping.
pub fn label_value<'a>(label: &'a str, value: String, color: Color, size: u32, orientation: Orientation) -> Element<'a, Message> {
    // Container is Shrink (via the caller's cell_length), so nothing here
    // clips regardless of font size — but the "Item size" slider should
    // still visibly do *something* to these modules, so font size scales
    // off it instead of being fixed constants that ignored it entirely.
    let label_size = (size as f32 * 0.3).clamp(8.0, 18.0);
    let value_size = (size as f32 * 0.38).clamp(9.0, 22.0);
    let label_widget = text(label).size(label_size).style(move |_| text::Style { color: Some(Color { a: 0.65, ..color }) });
    let value_widget = text(value).size(value_size).style(move |_| text::Style { color: Some(color) });
    match orientation {
        Orientation::Horizontal => row![label_widget, value_widget].spacing(4).align_y(iced::Alignment::Center).into(),
        Orientation::Vertical => column![label_widget, value_widget].align_x(iced::Alignment::Center).into(),
    }
}

/// Samples `read` every `every` on a blocking thread (it may shell out) and
/// emits a message only when the sampled value differs from the last one.
/// Every message rebuilds and redraws the whole bar, so an unchanged
/// reading must produce no message at all — this is what keeps an idle bar
/// idle. `read` owns any state it needs between samples (e.g. CPU deltas).
pub fn poll_changes<T, F>(
    id: &'static str,
    every: std::time::Duration,
    read: F,
    to_message: fn(T) -> Message,
) -> Subscription<Message>
where
    T: PartialEq + Clone + Send + 'static,
    F: FnMut() -> T + Send + 'static,
{
    Subscription::run_with_id(
        id,
        iced::stream::channel(1, move |mut sender| async move {
            use iced::futures::SinkExt;
            let mut read = read;
            let mut last: Option<T> = None;
            loop {
                let Ok((returned, value)) = tokio::task::spawn_blocking(move || {
                    let value = read();
                    (read, value)
                })
                .await
                else {
                    return;
                };
                read = returned;
                if last.as_ref() != Some(&value) {
                    last = Some(value.clone());
                    if sender.send(to_message(value)).await.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(every).await;
            }
        }),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

impl From<Edge> for Orientation {
    fn from(edge: Edge) -> Self {
        match edge {
            Edge::Top | Edge::Bottom => Orientation::Horizontal,
            Edge::Left | Edge::Right => Orientation::Vertical,
        }
    }
}

pub trait Module {
    fn view(&self, colors: AppColors, size: u32, orientation: Orientation) -> Element<'_, Message>;

    /// Most modules render into a fixed `size` along the bar's main axis
    /// (section 6) — width for a horizontal bar, height for a vertical
    /// one. Edge-spanning modules like workspaces override this to
    /// `Shrink` regardless of orientation.
    fn cell_length(&self, size: u32, _orientation: Orientation) -> Length {
        Length::Fixed(size as f32)
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    fn update(&mut self, _message: &Message) {}
}
