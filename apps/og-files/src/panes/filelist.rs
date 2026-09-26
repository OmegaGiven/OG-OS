use iced::widget::{button, column, container, image, mouse_area, row, scrollable, text};
use iced::{Background, Border, Color, Element, Length};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::app::Message;
use crate::filesystem::{format_size, FileEntry};
use crate::tab::{SortBy, ViewMode};
use crate::theme;

const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");

/// Real decoded thumbnail (via the async cache in `thumbs.rs`) for
/// previewable files, icon glyph for everything else and while a
/// thumbnail is still being generated.
fn thumbnail<'a>(entry: &'a FileEntry, size: u16) -> Element<'a, Message> {
    if crate::thumbs::wants_thumbnail(&entry.mime_type) {
        if let Some(Some(path)) = crate::thumbs::get(&entry.path, &entry.mime_type) {
            return image(image::Handle::from_path(path))
                .width(size)
                .height(size)
                .content_fit(iced::ContentFit::Contain)
                .into();
        }
    }
    // Icon-glyph fallback (folders, and any file without a real
    // thumbnail): unlike the image path above, a bare `text()` widget has
    // no width/height of its own — at a large zoom level its glyph point
    // size (== `size`, up to 160) grew taller than the grid card itself,
    // pushing the filename below it clean out of the card's fixed height
    // and off-screen. A glyph reads fine well under the box size, so it's
    // capped, then centered in a box matching the image path's footprint
    // — same layout math for both branches regardless of zoom.
    let glyph_size = (size / 2).clamp(20, 64);
    container(text(entry.icon).font(ICON_FONT).size(glyph_size))
        .width(size)
        .height(size)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// Rows outside the visible viewport (plus this many extra rows of
/// overscan on each side, so a fast scroll flick doesn't show a blank
/// flash before the next frame catches up) get a plain `Space` instead of
/// a real card/row — this is the whole fix for "scrolling feels laggy":
/// without it, every frame rebuilt and CPU-composited *every* thumbnail
/// in the folder (this app renders via tiny-skia, no GPU), not just the
/// ~20 actually on screen.
const OVERSCAN_ROWS: usize = 2;

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    entries: &'a [FileEntry],
    selected: &'a HashSet<PathBuf>,
    cursor: Option<usize>,
    view_mode: ViewMode,
    available_width: f32,
    sort_by: SortBy,
    sort_asc: bool,
    suppress_hover: bool,
    drop_hover: Option<&'a PathBuf>,
    zoom: u16,
    items_per_row_out: &'a std::cell::Cell<usize>,
    scroll_offset: f32,
    viewport_height: f32,
) -> Element<'a, Message> {
    let content: Element<Message> = match view_mode {
        ViewMode::Grid => grid_view(entries, selected, cursor, available_width, suppress_hover, drop_hover, zoom, items_per_row_out, scroll_offset, viewport_height),
        ViewMode::List => list_view(entries, selected, cursor, sort_by, sort_asc, suppress_hover, drop_hover, scroll_offset, viewport_height),
    };

    let area = mouse_area(
        container(content)
            .width(Length::Fill)
            .padding(12)
            .style(theme::main_bg),
    )
    .on_right_press(Message::ContextMenuOpenBackground)
    .on_press(Message::BackgroundClicked);

    scrollable(area)
        .id(scrollable::Id::new("filelist"))
        .width(Length::Fill)
        .height(Length::Fill)
        .on_scroll(|vp| Message::Scrolled(vp.absolute_offset().y, vp.bounds().height))
        .into()
}

const CARD_SPACING: f32 = 8.0;
const GRID_PADDING: f32 = 24.0; // matches the 12px padding on both sides in view()

fn vspace(px: f32) -> Element<'static, Message> {
    iced::widget::Space::with_height(Length::Fixed(px.max(0.0))).into()
}

#[allow(clippy::too_many_arguments)]
fn grid_view<'a>(
    entries: &'a [FileEntry],
    selected: &'a HashSet<PathBuf>,
    cursor: Option<usize>,
    available_width: f32,
    suppress_hover: bool,
    drop_hover: Option<&'a PathBuf>,
    zoom: u16,
    items_per_row_out: &'a std::cell::Cell<usize>,
    scroll_offset: f32,
    viewport_height: f32,
) -> Element<'a, Message> {
    if entries.is_empty() {
        return empty_message();
    }

    let card_width = zoom as f32 + 30.0;
    let usable = (available_width - GRID_PADDING).max(card_width);
    let items_per_row = (((usable + CARD_SPACING) / (card_width + CARD_SPACING)) as usize).max(1);
    items_per_row_out.set(items_per_row);

    let row_height = zoom as f32 + 40.0 + CARD_SPACING;
    let total_rows = entries.len().div_ceil(items_per_row);

    let first_row = ((scroll_offset / row_height).floor() as isize - OVERSCAN_ROWS as isize).max(0) as usize;
    let visible_span = (viewport_height / row_height).ceil() as usize + 1;
    let last_row = (first_row + visible_span + OVERSCAN_ROWS * 2).min(total_rows.saturating_sub(1));

    let mut rows: Vec<Element<Message>> = Vec::new();
    // Slop-tolerant: these two Space heights don't perfectly account for
    // the column's own inter-row spacing at the seam (a few px either
    // way), which only affects scrollbar-length accuracy by an amount
    // nobody will notice — not which rows are actually rendered. Only
    // pushed when non-zero so column.spacing() doesn't add a spurious gap
    // at the very top/bottom when nothing's actually culled there.
    if first_row > 0 {
        rows.push(vspace(first_row as f32 * row_height));
    }

    for (row_i, chunk) in entries.chunks(items_per_row).enumerate().skip(first_row).take(last_row - first_row + 1) {
        let mut row_items = row![].spacing(8);
        for (col_i, entry) in chunk.iter().enumerate() {
            let idx = row_i * items_per_row + col_i;
            let is_drop_target = entry.is_dir && drop_hover == Some(&entry.path);
            row_items = row_items.push(grid_card(
                entry,
                idx,
                selected.contains(&entry.path),
                cursor == Some(idx),
                suppress_hover,
                is_drop_target,
                card_width,
                zoom,
            ));
        }
        rows.push(row_items.into());
    }

    let trailing_rows = total_rows.saturating_sub(last_row + 1);
    if trailing_rows > 0 {
        rows.push(vspace(trailing_rows as f32 * row_height));
    }

    column(rows).spacing(8).into()
}

fn empty_message<'a>() -> Element<'a, Message> {
    container(text("Empty folder").style(theme::muted_text))
        .center_x(Length::Fill)
        .padding(40)
        .into()
}

fn grid_card<'a>(
    entry: &'a FileEntry,
    idx: usize,
    selected: bool,
    is_cursor: bool,
    suppress_hover: bool,
    is_drop_target: bool,
    card_width: f32,
    zoom: u16,
) -> Element<'a, Message> {
    let path = entry.path.clone();
    let name = if entry.name.chars().count() > 16 {
        format!("{}…", entry.name.chars().take(15).collect::<String>())
    } else {
        entry.name.clone()
    };
    let pal = theme::p();

    let border_color = if is_drop_target || selected { pal.accent } else if is_cursor { pal.muted } else { pal.border };
    let bg = if is_drop_target || selected { pal.selected_bg } else { pal.surface };
    let border_width = if is_drop_target || is_cursor { 2.0 } else if selected { 2.0 } else { 1.0 };

    let btn = button(
        column![
            container(thumbnail(entry, zoom)).width(Length::Fill).center_x(Length::Fill),
            text(name).size(11),
        ]
        .spacing(4)
        .align_x(iced::Alignment::Center)
        .width(Length::Fill),
    )
    .width(card_width)
    .height(zoom as f32 + 40.0)
    .style(move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered if !suppress_hover => Color { r: bg.r + 0.05, g: bg.g + 0.05, b: bg.b + 0.08, a: 1.0 },
            _ => bg,
        })),
        text_color: pal.text,
        border: Border { color: border_color, width: border_width, radius: 6.0.into() },
        ..Default::default()
    });

    mouse_area(btn)
        .on_press(Message::EntryPressed(idx))
        .on_release(Message::EntryReleased(idx))
        .on_enter(Message::EntryHoverEnter(path.clone()))
        .on_exit(Message::EntryHoverExit(path))
        .on_right_press(Message::ContextMenuOpenEntry(idx))
        .into()
}

fn sort_header_btn<'a>(label: &'a str, by: SortBy, width: Length, sort_by: SortBy, sort_asc: bool) -> Element<'a, Message> {
    let is_active = sort_by == by;
    let text_label = if is_active {
        format!("{}  {}", label, if sort_asc { "\u{25b2}" } else { "\u{25bc}" })
    } else {
        label.to_string()
    };
    let pal = theme::p();

    button(text(text_label).size(12).style(move |_| iced::widget::text::Style {
        color: Some(if is_active { pal.text } else { pal.muted }),
    }))
    .on_press(Message::SortChanged(by))
    .width(width)
    .padding(0)
    .style(theme::flat_button)
    .into()
}

/// Fixed row height for list mode — `list_row`'s button has no explicit
/// height (it sizes to its content), but virtualization needs a
/// deterministic stride to compute which rows are on screen without
/// laying out every row just to measure it. Matches what that content
/// (18px icon/thumb, size-13 text, 8px spacing, no padding) actually
/// renders at in practice.
const LIST_ROW_HEIGHT: f32 = 30.0;
const LIST_ROW_SPACING: f32 = 1.0;

#[allow(clippy::too_many_arguments)]
fn list_view<'a>(
    entries: &'a [FileEntry],
    selected: &'a HashSet<PathBuf>,
    cursor: Option<usize>,
    sort_by: SortBy,
    sort_asc: bool,
    suppress_hover: bool,
    drop_hover: Option<&'a PathBuf>,
    scroll_offset: f32,
    viewport_height: f32,
) -> Element<'a, Message> {
    let header = row![
        text("").width(30),
        sort_header_btn("Name", SortBy::Name, Length::Fill, sort_by, sort_asc),
        sort_header_btn("Size", SortBy::Size, Length::Fixed(80.0), sort_by, sort_asc),
        sort_header_btn("Type", SortBy::Kind, Length::Fixed(120.0), sort_by, sort_asc),
        sort_header_btn("Modified", SortBy::Modified, Length::Fixed(150.0), sort_by, sort_asc),
    ]
    .spacing(8)
    .padding([4, 8])
    .align_y(iced::Alignment::Center);

    let mut col = column![header].spacing(LIST_ROW_SPACING);

    if entries.is_empty() {
        col = col.push(container(text("Empty folder").style(theme::muted_text)).padding([20, 8]));
        return col.into();
    }

    let row_stride = LIST_ROW_HEIGHT + LIST_ROW_SPACING;
    // The header above isn't part of this stride math — a header's worth
    // of slop is well inside the overscan buffer, not worth tracking
    // separately.
    let first = ((scroll_offset / row_stride).floor() as isize - OVERSCAN_ROWS as isize).max(0) as usize;
    let visible_span = (viewport_height / row_stride).ceil() as usize + 1;
    let last = (first + visible_span + OVERSCAN_ROWS * 2).min(entries.len().saturating_sub(1));

    if first > 0 {
        col = col.push(vspace(first as f32 * row_stride));
    }
    for (idx, entry) in entries.iter().enumerate().skip(first).take(last - first + 1) {
        let is_drop_target = entry.is_dir && drop_hover == Some(&entry.path);
        col = col.push(list_row(entry, idx, selected.contains(&entry.path), cursor == Some(idx), suppress_hover, is_drop_target));
    }
    let trailing = entries.len().saturating_sub(last + 1);
    if trailing > 0 {
        col = col.push(vspace(trailing as f32 * row_stride));
    }

    col.into()
}

fn list_row<'a>(entry: &'a FileEntry, idx: usize, selected: bool, is_cursor: bool, suppress_hover: bool, is_drop_target: bool) -> Element<'a, Message> {
    let path = entry.path.clone();
    let pal = theme::p();
    let bg = if is_drop_target || selected { pal.selected_bg } else { pal.bg };
    let size_str = if entry.is_dir { "-".to_string() } else { format_size(entry.size) };
    let mime_short = if entry.is_dir {
        "Folder".to_string()
    } else {
        entry.mime_type.split('/').next_back().unwrap_or("file").to_string()
    };
    let modified = entry.modified.format("%Y-%m-%d %H:%M").to_string();

    let btn = button(
        row![
            container(thumbnail(entry, 18)).width(30),
            text(&entry.name).size(13).width(Length::Fill),
            text(size_str).size(12).width(80).style(theme::muted_text),
            text(mime_short).size(12).width(120).style(theme::muted_text),
            text(modified).size(12).width(150).style(theme::muted_text),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .style(move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered if !suppress_hover => pal.surface,
            _ => bg,
        })),
        text_color: pal.text,
        border: Border {
            color: if is_drop_target || selected { pal.accent } else if is_cursor { pal.muted } else { Color::TRANSPARENT },
            width: if is_drop_target { 2.0 } else if selected || is_cursor { 1.0 } else { 0.0 },
            radius: 3.0.into(),
        },
        ..Default::default()
    });

    mouse_area(btn)
        .on_press(Message::EntryPressed(idx))
        .on_release(Message::EntryReleased(idx))
        .on_enter(Message::EntryHoverEnter(path.clone()))
        .on_exit(Message::EntryHoverExit(path))
        .on_right_press(Message::ContextMenuOpenEntry(idx))
        .into()
}
