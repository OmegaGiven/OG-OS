//! Reusable "ghost follows cursor" drag visual for in-window drags —
//! extracted after noticing og-bar's workspace-icon drag (move an app to
//! another workspace group) and og-files' drag-to-folder both tracked
//! drag state and a hover target, but neither rendered anything at the
//! cursor to confirm what's actually being dragged. Every app already
//! tracks the two things this needs (the dragged item + the latest
//! cursor position, both required for its own hit-testing anyway), so
//! this only owns the "float a copy of it at the cursor" rendering, not
//! any drag-state bookkeeping — that stays each app's own business, same
//! as the rest of OG-toolkit's split (og-theme owns colors, not layout;
//! og-wayland owns the real cross-app drag protocol, not in-window ones).
//!
//! Not a fit for cross-window/cross-app drags (dragging a file out into
//! another app) — the compositor renders that drag's icon itself via a
//! `wl_surface` handed to `wl_data_source::start_drag`, which is
//! `og-wayland`'s concern, not this crate's; iced can't draw outside its
//! own window's surface regardless.

use iced::widget::{container, stack};
use iced::{Element, Length, Padding, Point, Theme, Vector};

/// Wraps `base` so that, whenever `ghost` is `Some`, a copy of it floats
/// at `cursor + offset` on top of everything else — `offset` lets the
/// ghost sit slightly away from the actual cursor tip (e.g. `(12, 12)`)
/// so it doesn't visually collide with the pointer itself.
///
/// `ghost` is a rendering of whatever's being dragged that the caller
/// builds from its own drag state (typically a dimmed/smaller copy of the
/// same row/icon widget it renders normally) — this function only
/// positions it, it doesn't know what a "file" or "app icon" looks like.
/// `cursor` is the latest pointer position in window coordinates, from
/// whatever `CursorMoved` subscription the caller already has for its own
/// hit-testing.
pub fn with_drag_ghost<'a, Message: 'a, Renderer>(
    base: impl Into<Element<'a, Message, Theme, Renderer>>,
    ghost: Option<Element<'a, Message, Theme, Renderer>>,
    cursor: Point,
    offset: Vector,
) -> Element<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer + 'a,
{
    let base = base.into();
    let Some(ghost) = ghost else { return base };

    // A container's padding offsets its content's top-left corner from
    // the container's own top-left by exactly `padding.top`/`.left` when
    // the container fills its parent and the content is Shrink-sized —
    // the same idiom used for the badge overlay in og-bar's notifications
    // module (see `modules/notifications.rs`), just driven by the cursor
    // position here instead of a fixed corner.
    let x = (cursor.x + offset.x).max(0.0);
    let y = (cursor.y + offset.y).max(0.0);

    stack![
        base,
        container(ghost)
            .padding(Padding { top: y, right: 0.0, bottom: 0.0, left: x })
            .width(Length::Fill)
            .height(Length::Fill),
    ]
    .into()
}
