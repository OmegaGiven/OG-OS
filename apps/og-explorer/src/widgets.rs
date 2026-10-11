//! Shared building blocks for the files pane: virtualized lists/grids (only
//! the rows on screen are built, so 100k-entry folders stay smooth),
//! selection state, and the selectable item button every view uses.

use crate::filetype::Kind;
use iced::widget::{button, column, container, mouse_area, row, scrollable, Space};
use iced::{Element, Length, Theme};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// rows built above and below the visible window
const OVERSCAN: usize = 6;

/// The part of a scrollable that's on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vp {
    pub offset: f32,
    pub height: f32,
    pub width: f32,
}

impl Default for Vp {
    fn default() -> Self {
        // until the first scroll event reports the real size, assume a tall pane
        Self {
            offset: 0.0,
            height: 1600.0,
            width: 400.0,
        }
    }
}

impl Vp {
    pub fn from(v: scrollable::Viewport) -> Self {
        Self {
            offset: v.absolute_offset().y,
            height: v.bounds().height,
            width: v.bounds().width,
        }
    }

    /// Offset that brings row `i` (of height `h`) into view, or None if it
    /// already is.
    pub fn reveal(&self, i: usize, h: f32) -> Option<f32> {
        let top = i as f32 * h;
        if top < self.offset {
            Some(top)
        } else if top + h > self.offset + self.height {
            Some(top + h - self.height)
        } else {
            None
        }
    }
}

/// A column of `len` fixed-height rows where only the visible ones (plus
/// overscan) are built; spacers stand in for the rest.
pub fn virtual_column<'a, M: 'a>(
    len: usize,
    row_h: f32,
    vp: Vp,
    row_fn: impl Fn(usize) -> Element<'a, M>,
) -> Element<'a, M> {
    let first = ((vp.offset / row_h).floor() as usize)
        .saturating_sub(OVERSCAN)
        .min(len);
    let shown = (vp.height / row_h).ceil() as usize + 2 * OVERSCAN;
    let last = (first + shown).min(len);
    column![
        Space::with_height(first as f32 * row_h),
        column((first..last).map(|i| {
            container(row_fn(i))
                .height(row_h)
                .width(Length::Fill)
                .into()
        })),
        Space::with_height((len - last) as f32 * row_h),
    ]
    .width(Length::Fill)
    .into()
}

/// Grid version: `len` tiles of `tile` size, as many per row as fit `vp.width`.
pub fn virtual_grid<'a, M: 'a>(
    len: usize,
    tile: (f32, f32),
    gap: f32,
    vp: Vp,
    tile_fn: impl Fn(usize) -> Element<'a, M>,
) -> Element<'a, M> {
    let cols = grid_cols(vp.width, tile.0, gap);
    let rows = len.div_ceil(cols);
    virtual_column(rows, tile.1 + gap, vp, move |r| {
        row((r * cols..((r + 1) * cols).min(len)).map(&tile_fn))
            .spacing(gap)
            .into()
    })
}

pub fn grid_cols(width: f32, tile_w: f32, gap: f32) -> usize {
    (((width - 4.0) + gap) / (tile_w + gap)).floor().max(1.0) as usize
}

// -------------------------------------------------------------------- icons

/// A glyph from the bundled Bootstrap Icons font (MIT; loaded in main.rs),
/// so icons never depend on a system icon/Nerd font.
pub fn icon<'a>(b: iced_fonts::Bootstrap) -> iced::widget::Text<'a> {
    iced::widget::text(iced_fonts::bootstrap::icon_to_char(b).to_string())
        .font(iced_fonts::BOOTSTRAP_FONT)
        .line_height(1.0)
}

/// File-type icon, tinted per family so a listing scans at a glance.
pub fn kind_icon<'a>(k: Kind) -> iced::widget::Text<'a> {
    let c = match k {
        Kind::Folder => iced::color!(0xe0b85c),
        Kind::Image => iced::color!(0x7fcf8f),
        Kind::Video => iced::color!(0xd88ad8),
        Kind::Audio => iced::color!(0x6fc3d8),
        Kind::Table => iced::color!(0x8fd18f),
        Kind::Doc => iced::color!(0xff8a7a),
        Kind::Model3d => iced::color!(0xc0a0ff),
        Kind::Archive => iced::color!(0xd8b080),
        _ => crate::theme::MUTED,
    };
    icon(k.glyph()).color(c)
}

// ---------------------------------------------------------------- selection

#[derive(Debug, Default, Clone)]
pub struct Selection {
    pub items: BTreeSet<PathBuf>,
    /// shift-click range start
    pub anchor: Option<PathBuf>,
    /// keyboard focus
    pub cursor: Option<PathBuf>,
}

impl Selection {
    pub fn contains(&self, p: &PathBuf) -> bool {
        self.items.contains(p)
    }

    pub fn only(&mut self, p: PathBuf) {
        self.items.clear();
        self.items.insert(p.clone());
        self.anchor = Some(p.clone());
        self.cursor = Some(p);
    }

    pub fn toggle(&mut self, p: PathBuf) {
        if !self.items.remove(&p) {
            self.items.insert(p.clone());
        }
        self.anchor = Some(p.clone());
        self.cursor = Some(p);
    }

    /// Select from the anchor to `p` in display `order`.
    pub fn range(&mut self, p: PathBuf, order: &[PathBuf]) {
        let a = self
            .anchor
            .as_ref()
            .and_then(|a| order.iter().position(|x| x == a));
        let b = order.iter().position(|x| *x == p);
        match (a, b) {
            (Some(a), Some(b)) => {
                self.items = order[a.min(b)..=a.max(b)].iter().cloned().collect();
                self.cursor = Some(p);
            }
            _ => self.only(p),
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.anchor = None;
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.items.iter().cloned().collect()
    }
}

/// What a row/tile reports to the sidebar.
#[derive(Debug, Clone)]
pub enum ItemAct {
    Press(PathBuf, Kind),
    Menu(PathBuf),
}

/// A selectable row/tile: left click → `Press`, right click → `Menu`.
pub fn item<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    path: &PathBuf,
    kind: Kind,
    sel: &Selection,
    wrap: impl Fn(ItemAct) -> M,
) -> Element<'a, M> {
    let selected = sel.contains(path);
    let focused = sel.cursor.as_ref() == Some(path);
    let b = button(content)
        .on_press(wrap(ItemAct::Press(path.clone(), kind)))
        .padding([2, 6])
        .width(Length::Fill)
        .style(move |t: &Theme, s| item_style(t, s, selected, focused));
    mouse_area(b)
        .on_right_press(wrap(ItemAct::Menu(path.clone())))
        .into()
}

fn item_style(t: &Theme, s: button::Status, selected: bool, focused: bool) -> button::Style {
    let p = t.extended_palette();
    let bg = match (selected, s) {
        (true, _) => Some(p.primary.weak.color.scale_alpha(0.45).into()),
        (false, button::Status::Hovered | button::Status::Pressed) => {
            Some(p.background.strong.color.scale_alpha(0.5).into())
        }
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: p.background.base.text,
        border: iced::Border {
            radius: 4.0.into(),
            width: if focused { 1.0 } else { 0.0 },
            color: p.primary.base.color.scale_alpha(0.7),
        },
        ..Default::default()
    }
}
