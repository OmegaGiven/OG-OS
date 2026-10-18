//! Squarified treemap (Bruls, Huizing & van Wijk 2000) and the canvas that
//! draws it. Only files are painted; folders show as 1px-inset frames whose
//! gaps form the borders. The layout is cached per (focus, size, mode,
//! version) and the painted geometry lives in `canvas::Cache`s, so a frame
//! that only moves the pointer redraws just the hover outline + tooltip.

use super::scan::NodeKind;
use super::tree::{ext_of, Mode, Tree};
use super::{Msg, State};
use crate::filetype::Kind;
use crate::fsops::human_size;
use iced::mouse;
use iced::widget::canvas::{self, gradient, Frame, Geometry, Path, Stroke, Text};
use iced::{keyboard, Color, Point, Rectangle, Renderer, Size, Theme};
use std::time::{Duration, Instant};

/// Folders smaller than this (px, either side) are drawn as one blob.
const MIN_NEST: f64 = 6.0;
const MAX_DEPTH: u16 = 64;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Lay `sizes` (sorted biggest first, all > 0) out in `r` so every item's
/// area is proportional to its size and aspect ratios stay close to 1.
/// The rects tile `r` exactly (each row/column's last item takes the
/// remainder, so rounding never leaves gaps).
pub fn squarify(sizes: &[f64], r: R) -> Vec<R> {
    let n = sizes.len();
    let mut out = vec![R::default(); n];
    let total: f64 = sizes.iter().sum();
    if n == 0 || total <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
        return out;
    }
    let scale = r.w * r.h / total;
    let areas: Vec<f64> = sizes.iter().map(|s| s * scale).collect();
    // worst aspect ratio of a row with sum `s`, extremes `max`/`min`, laid
    // along a side of length `side`
    let worst = |s: f64, max: f64, min: f64, side: f64| {
        let s2 = s * s;
        let w2 = side * side;
        (w2 * max / s2).max(s2 / (w2 * min))
    };
    let mut rect = r;
    let mut i = 0;
    while i < n {
        let side = rect.w.min(rect.h);
        let mut j = i + 1;
        let mut sum = areas[i];
        let mut cur = worst(sum, areas[i], areas[i], side);
        while j < n {
            let s = sum + areas[j];
            let w = worst(s, areas[i], areas[j], side);
            if w > cur {
                break;
            }
            sum = s;
            cur = w;
            j += 1;
        }
        let last_row = j == n;
        if rect.w >= rect.h {
            // a column on the left, items stacked top to bottom
            let cw = if last_row {
                rect.w
            } else {
                (sum / rect.h).min(rect.w)
            };
            let mut y = rect.y;
            for k in i..j {
                let h = if k + 1 == j {
                    rect.y + rect.h - y
                } else {
                    areas[k] / cw
                };
                out[k] = R {
                    x: rect.x,
                    y,
                    w: cw,
                    h,
                };
                y += h;
            }
            rect.x += cw;
            rect.w -= cw;
        } else {
            // a row on top, items left to right
            let rh = if last_row {
                rect.h
            } else {
                (sum / rect.w).min(rect.h)
            };
            let mut x = rect.x;
            for k in i..j {
                let w = if k + 1 == j {
                    rect.x + rect.w - x
                } else {
                    areas[k] / rh
                };
                out[k] = R {
                    x,
                    y: rect.y,
                    w,
                    h: rh,
                };
                x += w;
            }
            rect.y += rh;
            rect.h -= rh;
        }
        i = j;
    }
    out
}

/// One placed node. Parents come before their descendants.
#[derive(Debug, Clone, Copy)]
pub struct LRect {
    pub node: u32,
    pub r: Rectangle,
    pub depth: u16,
    /// painted (a file, or a folder too small to nest into)
    pub leaf: bool,
}

/// Treemap of `focus` in a `size` area.
pub fn layout(tree: &Tree, focus: u32, size: Size, mode: Mode) -> Vec<LRect> {
    let mut out = Vec::new();
    if tree.live(focus) {
        let r = R {
            x: 0.0,
            y: 0.0,
            w: size.width as f64,
            h: size.height as f64,
        };
        place(tree, focus, r, 0, mode, &mut out);
    }
    out
}

fn place(tree: &Tree, id: u32, r: R, depth: u16, mode: Mode, out: &mut Vec<LRect>) {
    let mut kids: Vec<(u64, u32)> = tree
        .node(id)
        .children
        .iter()
        .map(|&c| (tree.node(c).bytes(mode), c))
        .filter(|&(b, _)| b > 0)
        .collect();
    kids.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let sizes: Vec<f64> = kids.iter().map(|k| k.0 as f64).collect();
    for (&(_, c), cr) in kids.iter().zip(squarify(&sizes, r)) {
        if cr.w * cr.h < 1.0 {
            break; // sub-pixel (and so is everything after: sorted)
        }
        let rect = Rectangle::new(
            Point::new(cr.x as f32, cr.y as f32),
            Size::new(cr.w as f32, cr.h as f32),
        );
        let nest =
            tree.node(c).is_dir() && cr.w >= MIN_NEST && cr.h >= MIN_NEST && depth < MAX_DEPTH;
        out.push(LRect {
            node: c,
            r: rect,
            depth,
            leaf: !nest,
        });
        if nest {
            let inner = R {
                x: cr.x + 1.0,
                y: cr.y + 1.0,
                w: cr.w - 2.0,
                h: cr.h - 2.0,
            };
            place(tree, c, inner, depth + 1, mode, out);
        }
    }
}

/// Deepest rect containing `p`.
pub fn hit(rects: &[LRect], p: Point) -> Option<&LRect> {
    rects.iter().rev().find(|l| l.r.contains(p))
}

/// The file-type colours of `widgets::kind_icon` (kept in sync by hand:
/// that helper only hands out tinted text).
pub fn kind_color(k: Kind) -> Color {
    match k {
        Kind::Folder => iced::color!(0xe0b85c),
        Kind::Image => iced::color!(0x7fcf8f),
        Kind::Video => iced::color!(0xd88ad8),
        Kind::Audio => iced::color!(0x6fc3d8),
        Kind::Table => iced::color!(0x8fd18f),
        Kind::Doc => iced::color!(0xff8a7a),
        Kind::Model3d => iced::color!(0xc0a0ff),
        Kind::Archive => iced::color!(0xd8b080),
        Kind::Text => crate::theme::MUTED,
        // a shade off MUTED so binaries and text don't merge into one slab
        Kind::Binary => iced::color!(0x6f7a8a),
    }
}

fn shade(c: Color, f: f32) -> Color {
    if f >= 0.0 {
        Color::from_rgb(
            c.r + (1.0 - c.r) * f,
            c.g + (1.0 - c.g) * f,
            c.b + (1.0 - c.b) * f,
        )
    } else {
        Color::from_rgb(c.r * (1.0 + f), c.g * (1.0 + f), c.b * (1.0 + f))
    }
}

fn node_color(tree: &Tree, l: &LRect) -> Color {
    match tree.node(l.node).kind {
        NodeKind::File(k) => kind_color(k),
        // a folder too small to open up: neutral
        _ => iced::color!(0x5a564e),
    }
}

/// Cushion-like fill: light top-left → base → dark bottom-right.
fn paint(f: &mut Frame, r: Rectangle, c: Color) {
    let r = if r.width >= 3.0 && r.height >= 3.0 {
        Rectangle::new(r.position(), Size::new(r.width - 1.0, r.height - 1.0))
    } else {
        r
    };
    if r.width * r.height < 24.0 {
        f.fill_rectangle(r.position(), r.size(), c);
        return;
    }
    let g = gradient::Linear::new(r.position(), Point::new(r.x + r.width, r.y + r.height))
        .add_stop(0.0, shade(c, 0.35))
        .add_stop(0.45, c)
        .add_stop(1.0, shade(c, -0.5));
    f.fill(&Path::rectangle(r.position(), r.size()), g);
}

/// The canvas program: borrows the viewer state.
pub struct Map<'a> {
    pub st: &'a State,
}

#[derive(Default)]
pub struct Clicks {
    last: Option<(Instant, u32)>,
}

impl canvas::Program<Msg> for Map<'_> {
    type State = Clicks;

    fn update(
        &self,
        clicks: &mut Clicks,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Msg>) {
        use canvas::event::Status;
        match event {
            // the canvas sees every event: track modifiers for list clicks
            canvas::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                (Status::Ignored, Some(Msg::Mods(m)))
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(b)) => {
                let Some(p) = cursor.position_in(bounds) else {
                    return (Status::Ignored, None);
                };
                let rects = self.st.layout(bounds.size()).0;
                let Some(l) = hit(&rects, p) else {
                    return (Status::Captured, Some(Msg::MenuClose));
                };
                let node = l.node;
                match b {
                    mouse::Button::Left => {
                        let double = clicks
                            .last
                            .is_some_and(|(t, n)| n == node && t.elapsed() < DOUBLE_CLICK);
                        clicks.last = if double {
                            None
                        } else {
                            Some((Instant::now(), node))
                        };
                        let msg = if double {
                            Msg::MapDouble(node)
                        } else {
                            Msg::MapClick(node)
                        };
                        (Status::Captured, Some(msg))
                    }
                    mouse::Button::Right => (Status::Captured, Some(Msg::MapMenu(node, p))),
                    _ => (Status::Ignored, None),
                }
            }
            _ => (Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        _clicks: &Clicks,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let st = self.st;
        let tree = &st.tree;
        let (rects, changed) = st.layout(bounds.size());
        if changed {
            st.base.clear();
            st.overlay.clear();
        }
        let pal = theme.extended_palette();
        let bg = shade(pal.background.base.color, -0.35);
        let base = st.base.draw(renderer, bounds.size(), |f| {
            f.fill_rectangle(Point::ORIGIN, bounds.size(), bg);
            for l in rects.iter().filter(|l| l.leaf) {
                paint(f, l.r, node_color(tree, l));
            }
        });
        let accent = pal.primary.base.color;
        let overlay = st.overlay.draw(renderer, bounds.size(), |f| {
            if let Some(ext) = &st.highlight_ext {
                // dim everything, then repaint the matching files
                f.fill_rectangle(
                    Point::ORIGIN,
                    bounds.size(),
                    Color::from_rgba(0.0, 0.0, 0.0, 0.6),
                );
                for l in rects.iter().filter(|l| l.leaf) {
                    let n = tree.node(l.node);
                    if n.file_kind().is_some() && ext_of(&n.name) == *ext {
                        paint(f, l.r, node_color(tree, l));
                    }
                }
            }
            for l in rects.iter().filter(|l| st.sel.contains(&l.node)) {
                f.stroke(
                    &Path::rectangle(l.r.position(), l.r.size()),
                    Stroke::default().with_width(2.0).with_color(Color::WHITE),
                );
                if l.r.width > 4.0 && l.r.height > 4.0 {
                    let i = Rectangle::new(
                        Point::new(l.r.x + 2.0, l.r.y + 2.0),
                        Size::new(l.r.width - 4.0, l.r.height - 4.0),
                    );
                    f.stroke(
                        &Path::rectangle(i.position(), i.size()),
                        Stroke::default().with_width(1.0).with_color(accent),
                    );
                }
            }
        });
        let mut hover = Frame::new(renderer, bounds.size());
        if let Some(p) = cursor.position_in(bounds) {
            if let Some(l) = hit(&rects, p) {
                // the focus child containing the pointer (double-click target)
                if let Some(top) = rects
                    .iter()
                    .find(|t| t.depth == 0 && t.r.contains(p) && t.node != l.node)
                {
                    hover.stroke(
                        &Path::rectangle(top.r.position(), top.r.size()),
                        Stroke::default()
                            .with_width(1.0)
                            .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
                    );
                }
                hover.stroke(
                    &Path::rectangle(l.r.position(), l.r.size()),
                    Stroke::default()
                        .with_width(1.5)
                        .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.9)),
                );
                tooltip(&mut hover, st, l.node, p, bounds.size(), pal);
            }
        }
        vec![base, overlay, hover.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _clicks: &Clicks,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

fn tooltip(
    f: &mut Frame,
    st: &State,
    id: u32,
    p: Point,
    area: Size,
    pal: &iced::theme::palette::Extended,
) {
    let tree = &st.tree;
    let n = tree.node(id);
    let b = n.bytes(st.mode);
    let parent = tree
        .live(n.parent)
        .then(|| tree.node(n.parent).bytes(st.mode))
        .unwrap_or(0);
    let pct = if parent > 0 {
        format!(" · {:.1}% of parent", b as f64 * 100.0 / parent as f64)
    } else {
        String::new()
    };
    let mut path = tree.rel_path(id);
    if path.chars().count() > 90 {
        let tail: String = path
            .chars()
            .rev()
            .take(88)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        path = format!("…{tail}");
    }
    let what = match n.kind {
        NodeKind::File(k) => k.label().to_string(),
        _ => format!("Folder · {} files", n.files),
    };
    let lines = [path, format!("{}{pct}", human_size(b)), what];
    let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32 * 7.0 + 16.0;
    let h = 3.0 * 16.0 + 10.0;
    let mut x = p.x + 14.0;
    let mut y = p.y + 16.0;
    if x + w > area.width {
        x = (p.x - w - 8.0).max(0.0);
    }
    if y + h > area.height {
        y = (p.y - h - 8.0).max(0.0);
    }
    let bgc = pal.background.weak.color;
    f.fill_rectangle(Point::new(x, y), Size::new(w, h), Color { a: 0.95, ..bgc });
    f.stroke(
        &Path::rectangle(Point::new(x, y), Size::new(w, h)),
        Stroke::default()
            .with_width(1.0)
            .with_color(pal.background.strong.color),
    );
    for (i, l) in lines.into_iter().enumerate() {
        f.fill_text(Text {
            content: l,
            position: Point::new(x + 8.0, y + 5.0 + i as f32 * 16.0),
            color: if i == 0 {
                pal.background.base.text
            } else {
                crate::theme::MUTED
            },
            size: 12.0.into(),
            ..Text::default()
        });
    }
}
