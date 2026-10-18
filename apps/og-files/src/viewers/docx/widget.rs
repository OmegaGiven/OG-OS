//! The page widget: lays the document out as rich text on a centred
//! "paper" column, draws selection/caret, and turns mouse/keyboard input
//! into `Act`s for `State::update`.
//!
//! Layout is lazy and cached per paragraph (keyed by id + revision +
//! width): only blocks near the viewport are shaped, the rest use height
//! estimates that are corrected (scroll-anchored) once they come into view,
//! so 100+ page documents open and scroll instantly.
//!
//! Each hard line of a paragraph is one iced `Paragraph` (cosmic-text
//! buffer) that wraps by itself; centred/right-aligned lines are re-shaped
//! one visual line each so every line can be placed on its own.

use super::model::{Align, Attr, Block, Editor as Model, Inline, Motion, ObjKind, Para, Path, Pos};
use super::Msg;
use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::text::{Paragraph as _, Renderer as _, Span, Text};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{clipboard, mouse, Clipboard, Shell, Widget};
use iced::font::{Style as FontStyle, Weight};
use iced::keyboard::{self, key::Named, Key};
use iced::{
    alignment, Border, Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Theme,
    Vector,
};
use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

type P = iced::advanced::graphics::text::Paragraph;

/// Letter width at 96 dpi.
pub const PAGE_W: f32 = 816.0;
const GAP: f32 = 20.0;
const SCROLLBAR: f32 = 10.0;
const CELL_PAD: f32 = 6.0;
pub const PAPER: Color = Color::from_rgb(0.957, 0.945, 0.918);
pub const INK: Color = Color::from_rgb(0.12, 0.114, 0.10);
const LINK: Color = Color::from_rgb(0.12, 0.37, 0.75);
const CHIP: Color = Color::from_rgb(0.42, 0.40, 0.37);
const GRID: Color = Color::from_rgb(0.62, 0.60, 0.56);
const SELECT: Color = Color::from_rgba(0.40, 0.58, 0.95, 0.38);

/// What the widget asks `State` to do.
#[derive(Debug, Clone)]
pub enum Act {
    Select(Pos, Pos),
    Move(Motion, bool),
    Insert(String),
    Enter,
    LineBreak,
    Backspace(bool),
    Delete(bool),
    Tab(bool),
    SelectAll,
    Toggle(Attr),
    Undo,
    Redo,
    Cut,
}

// ------------------------------------------------------------------ layout

#[derive(Debug, Clone, Copy)]
struct Seg {
    /// model start/len, display start/len
    m: usize,
    ml: usize,
    d: usize,
    dl: usize,
    atom: bool,
}

struct Piece {
    para: P,
    x: f32,
    y: f32,
    /// display range covered
    d0: usize,
    d1: usize,
}

struct Deco {
    d: Range<usize>,
    underline: bool,
    strike: bool,
    color: Color,
}

/// One laid-out paragraph (coordinates relative to its top-left).
struct PL {
    rev: u64,
    width: f32,
    h: f32,
    /// text top (spacing before)
    top: f32,
    indent: f32,
    pieces: Vec<Piece>,
    segs: Vec<Seg>,
    deco: Vec<Deco>,
    dlen: usize,
    disp: String,
    mlen: usize,
    line_h: f32,
    image: Option<(iced::widget::image::Handle, Size)>,
    label: Option<(P, f32)>,
}

struct CellL {
    x: f32,
    w: f32,
    /// entry index → (y, para id or None for opaque)
    entries: Vec<(f32, Option<u64>)>,
}

struct RowL {
    y: f32,
    h: f32,
    cells: Vec<CellL>,
}

struct TL {
    rev: u64,
    width: f32,
    h: f32,
    rows: Vec<RowL>,
}

struct OL {
    width: f32,
    h: f32,
    para: P,
}

#[derive(Default)]
struct St {
    width: f32,
    margin: f32,
    view: Size,
    paras: HashMap<u64, PL>,
    tables: HashMap<u64, TL>,
    opaques: HashMap<u64, OL>,
    /// per body block: top (doc coords) and height
    ys: Vec<f32>,
    hs: Vec<f32>,
    total: f32,
    scroll: f32,
    seen_sel: u64,
    seen_focus: u64,
    focused: bool,
    drag: Option<Drag>,
    clicks: Option<(Instant, Point, u8)>,
    pref_x: Option<(Pos, f32)>,
    mods: keyboard::Modifiers,
}

#[derive(Clone, Copy)]
enum Drag {
    Select,
    Bar(f32),
}

pub struct Page<'a> {
    pub ed: &'a Model,
    pub images: &'a HashMap<String, iced::widget::image::Handle>,
    pub labels: &'a HashMap<u64, String>,
    pub editing: bool,
    pub focus_gen: u64,
}

fn px(pt: f32) -> f32 {
    pt * 96.0 / 72.0
}

fn twips(t: f32) -> f32 {
    t / 15.0
}

/// Per-column minimum width for autofit tables: the longest unbreakable
/// word in any single-span cell of that column (approximate glyph width at
/// the cell text's size), plus cell padding.
fn autofit_min(t: &super::model::Table, cols: usize) -> Vec<f32> {
    let mut min = vec![0.0f32; cols];
    for row in &t.rows {
        let mut col = 0;
        for cell in &row.cells {
            let span = usize::from(cell.span);
            if span == 1 && col < cols {
                for e in &cell.entries {
                    if let Block::Para(p) = &e.block {
                        let word = p
                            .text()
                            .split_whitespace()
                            .map(|w| w.chars().count())
                            .max()
                            .unwrap_or(0);
                        // ~0.62em per glyph covers bold body text
                        let w = word as f32 * px(11.0) * 0.62 + 2.0 * CELL_PAD + 2.0;
                        min[col] = min[col].max(w);
                    }
                }
            }
            col += span;
        }
    }
    min
}

fn font(b: bool, i: bool) -> Font {
    Font {
        weight: if b { Weight::Bold } else { Weight::Normal },
        style: if i {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..Font::DEFAULT
    }
}

fn rgb(c: [u8; 3]) -> Color {
    Color::from_rgb8(c[0], c[1], c[2])
}

/// cosmic-text splits lines on these; keep byte lengths so offsets map 1:1.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\r' | '\u{0B}' | '\u{0C}' | '\u{1C}' | '\u{1D}' | '\u{1E}' => ' ',
            '\u{85}' => '\u{A0}',
            '\u{2029}' => '\u{2003}',
            c => c,
        })
        .collect()
}

#[derive(Clone, Copy)]
struct SpanStyle {
    size: f32,
    font: Font,
    color: Color,
}

fn make(
    spans: &[(Range<usize>, SpanStyle)],
    disp: &str,
    r: Range<usize>,
    base: f32,
    width: f32,
    wrap: bool,
) -> P {
    let list: Vec<Span<'_, (), Font>> = spans
        .iter()
        .filter_map(|(sr, st)| {
            let (a, b) = (sr.start.max(r.start), sr.end.min(r.end));
            (a < b).then(|| {
                Span::new(&disp[a..b])
                    .size(st.size)
                    .font(st.font)
                    .color(st.color)
            })
        })
        .collect();
    P::with_spans(Text {
        content: list.as_slice(),
        bounds: Size::new(
            if wrap { width.max(10.0) } else { f32::INFINITY },
            f32::INFINITY,
        ),
        size: Pixels(base),
        line_height: iced::widget::text::LineHeight::Relative(1.25),
        font: Font::DEFAULT,
        horizontal_alignment: alignment::Horizontal::Left,
        vertical_alignment: alignment::Vertical::Top,
        shaping: iced::widget::text::Shaping::Advanced,
        wrapping: if wrap {
            iced::widget::text::Wrapping::WordOrGlyph
        } else {
            iced::widget::text::Wrapping::None
        },
    })
}

/// Visual lines of a single-hard-line paragraph: (byte start, byte end,
/// line top, line height, line width).
struct Run {
    s: usize,
    e: usize,
    top: f32,
    h: f32,
    base: f32,
}

fn runs(p: &P) -> Vec<(Run, Vec<(usize, usize, f32, f32)>)> {
    let mut out: Vec<(Run, Vec<(usize, usize, f32, f32)>)> = Vec::new();
    for r in p.buffer().layout_runs() {
        let glyphs: Vec<(usize, usize, f32, f32)> = r
            .glyphs
            .iter()
            .map(|g| (g.start, g.end, g.x, g.w))
            .collect();
        let prev_end = out.last().map(|o| o.0.e).unwrap_or(0);
        let s = glyphs.iter().map(|g| g.0).min().unwrap_or(prev_end);
        let e = glyphs.iter().map(|g| g.1).max().unwrap_or(s);
        out.push((
            Run {
                s,
                e,
                top: r.line_top,
                h: r.line_height,
                base: r.line_y,
            },
            glyphs,
        ));
    }
    if out.is_empty() {
        let h = p.min_bounds().height.max(1.0);
        out.push((
            Run {
                s: 0,
                e: 0,
                top: 0.0,
                h,
                base: h * 0.8,
            },
            Vec::new(),
        ));
    }
    out
}

fn x_at(glyphs: &[(usize, usize, f32, f32)], b: usize) -> f32 {
    let mut best: Option<f32> = None;
    for &(s, e, x, w) in glyphs {
        if s <= b && b < e {
            return x + w * (b - s) as f32 / (e - s).max(1) as f32;
        }
        if b >= e {
            best = Some(best.map_or(x + w, |v: f32| v.max(x + w)));
        }
    }
    best.unwrap_or_else(|| glyphs.first().map(|g| g.2).unwrap_or(0.0))
}

impl PL {
    fn m2d(&self, m: usize) -> usize {
        for s in &self.segs {
            if m <= s.m {
                return s.d;
            }
            if m < s.m + s.ml {
                return if s.atom { s.d } else { s.d + (m - s.m) };
            }
        }
        self.dlen
    }

    fn d2m(&self, d: usize) -> usize {
        for s in &self.segs {
            if d <= s.d {
                return s.m;
            }
            if d < s.d + s.dl {
                return if s.atom {
                    if (d - s.d) * 2 < s.dl {
                        s.m
                    } else {
                        s.m + s.ml
                    }
                } else {
                    s.m + (d - s.d)
                };
            }
        }
        self.mlen
    }

    /// Caret rectangle for model offset `m` (x, top, height).
    fn caret(&self, m: usize) -> (f32, f32, f32) {
        if let Some((_, sz)) = &self.image {
            let x = self.indent + if m == 0 { 0.0 } else { sz.width };
            return (x, self.top, sz.height);
        }
        let d = self.m2d(m);
        let n = self.pieces.len();
        for (k, pc) in self.pieces.iter().enumerate() {
            let last = k + 1 == n;
            let next_starts_here = !last && self.pieces[k + 1].d0 == d;
            if pc.d0 <= d && d <= pc.d1 && !(d == pc.d1 && next_starts_here) {
                let rs = runs(&pc.para);
                let b = d - pc.d0;
                let nr = rs.len();
                // a run owns [its start, next run's start)
                let (run, glyphs) = rs
                    .iter()
                    .enumerate()
                    .find(|(i, _)| *i + 1 == nr || b < rs[i + 1].0.s)
                    .map(|(_, r)| r)
                    .expect("one run");
                return (pc.x + x_at(glyphs, b), self.top + pc.y + run.top, run.h);
            }
        }
        (self.indent, self.top, self.line_h)
    }

    /// Model offset under a point (paragraph-relative).
    fn hit(&self, p: Point) -> usize {
        if let Some((_, sz)) = &self.image {
            return if p.x < self.indent + sz.width / 2.0 {
                0
            } else {
                self.mlen
            };
        }
        let y = p.y - self.top;
        // the piece whose rows contain y (else nearest)
        let mut best: Option<(
            f32,
            &Piece,
            Run,
            Vec<(usize, usize, f32, f32)>,
            Option<usize>,
        )> = None;
        for pc in &self.pieces {
            let rs = runs(&pc.para);
            let starts: Vec<usize> = rs.iter().map(|r| r.0.s).collect();
            for (i, (r, g)) in rs.into_iter().enumerate() {
                let top = pc.y + r.top;
                let dist = if y < top {
                    top - y
                } else if y > top + r.h {
                    y - top - r.h
                } else {
                    0.0
                };
                if best.as_ref().is_none_or(|b| dist < b.0) {
                    best = Some((dist, pc, r, g, starts.get(i + 1).copied()));
                }
            }
        }
        let Some((_, pc, run, glyphs, next)) = best else {
            return 0;
        };
        let x = p.x - pc.x;
        let text = &self.disp[pc.d0..pc.d1.min(self.disp.len())];
        let mut b = run.e;
        for &(s, e, gx, gw) in &glyphs {
            if x < gx + gw {
                // nearest char boundary inside the cluster (ligatures)
                let e = e.min(text.len());
                let marks: Vec<usize> = (s..=e).filter(|&i| text.is_char_boundary(i)).collect();
                let n = marks.len().saturating_sub(1).max(1);
                let k = (((x - gx) / gw.max(0.01)).clamp(0.0, 1.0) * n as f32).round() as usize;
                b = marks
                    .get(k.min(marks.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or(s);
                break;
            }
        }
        if let Some(ns) = next {
            // past the end of a wrapped line: stay on this line
            if b >= ns {
                b = text[..ns.min(text.len())]
                    .char_indices()
                    .next_back()
                    .map(|c| c.0)
                    .unwrap_or(0);
            }
        }
        self.d2m((pc.d0 + b).min(pc.d1))
    }

    /// Rectangles covering display range [a, b) (+ a stub when the range
    /// runs past the paragraph end).
    fn rects(&self, a: usize, b: usize, past_end: bool) -> Vec<Rectangle> {
        let mut out = Vec::new();
        if let Some((_, sz)) = &self.image {
            if a < b || past_end {
                out.push(Rectangle::new(Point::new(self.indent, self.top), *sz));
            }
            return out;
        }
        for pc in &self.pieces {
            for (r, g) in runs(&pc.para) {
                let (s, e) = (pc.d0 + r.s, pc.d0 + r.e);
                let (lo, hi) = (a.max(s), b.min(e));
                let tail = past_end && pc.d1 == self.dlen && e >= pc.d1;
                if lo < hi || (tail && a <= e) {
                    let x0 = x_at(&g, lo.min(e) - pc.d0);
                    let x1 = if lo < hi { x_at(&g, hi - pc.d0) } else { x0 };
                    let x1 = if tail { x1 + 6.0 } else { x1 };
                    out.push(Rectangle::new(
                        Point::new(pc.x + x0, self.top + pc.y + r.top),
                        Size::new((x1 - x0).max(1.0), r.h),
                    ));
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------- the widget

impl<'a> Page<'a> {
    fn para_layout(&self, p: &Para, width: f32) -> PL {
        let doc = &self.ed.doc;
        let res = doc.styles.resolve(p.props.style.as_deref());
        let heading = res.heading;
        let fallback_pt = if res.title {
            26.0
        } else {
            match heading {
                Some(1) => 20.0,
                Some(2) => 16.0,
                Some(3) => 14.0,
                Some(_) => 12.0,
                None => 11.0,
            }
        };
        let base_pt = res.look.size.unwrap_or(fallback_pt);
        let base = px(base_pt);
        let list = self.ed.list_of(p);
        let lvl_ind = list
            .as_ref()
            .and_then(|(id, l, _)| doc.numbering.level(id, *l).and_then(|lv| lv.ind_left));
        let indent = p
            .props
            .ind_left
            .or(lvl_ind)
            .or(res.ind_left)
            .map(twips)
            .unwrap_or(match &list {
                Some((_, l, _)) => 24.0 * f32::from(*l + 1),
                None => 0.0,
            })
            .clamp(0.0, width * 0.6);
        let avail = (width - indent).max(40.0);
        let before = p
            .props
            .before
            .or(res.before)
            .map(twips)
            .unwrap_or(match heading {
                Some(_) => 12.0,
                None => 0.0,
            });
        let after = p.props.after.or(res.after).map(twips).unwrap_or(8.0);
        let align = p.props.align.or(res.align).unwrap_or_default();

        // image-only paragraph: draw the picture
        let visible: Vec<_> = p.objects().collect();
        if !p.has_text() && visible.len() == 1 {
            if let ObjKind::Image {
                rid: Some(rid),
                w,
                h,
            } = &visible[0].kind
            {
                if let Some(hd) = self.images.get(rid) {
                    let (mut w, mut h) = if *w > 1.0 && *h > 1.0 {
                        (*w, *h)
                    } else {
                        (240.0, 180.0)
                    };
                    if w > avail {
                        h *= avail / w;
                        w = avail;
                    }
                    let x = match align {
                        Align::Center => (avail - w) / 2.0,
                        Align::Right => avail - w,
                        _ => 0.0,
                    };
                    return PL {
                        rev: p.rev,
                        width,
                        h: before + h + after,
                        top: before,
                        indent: indent + x,
                        pieces: Vec::new(),
                        segs: Vec::new(),
                        deco: Vec::new(),
                        dlen: 0,
                        disp: String::new(),
                        mlen: p.len(),
                        line_h: h,
                        image: Some((hd.clone(), Size::new(w, h))),
                        label: None,
                    };
                }
            }
        }

        // display text, spans and the model↔display map
        let mut disp = String::new();
        let mut spans: Vec<(Range<usize>, SpanStyle)> = Vec::new();
        let mut segs = Vec::new();
        let mut deco = Vec::new();
        let mut m = 0;
        for i in &p.inl {
            match i {
                Inline::Text(t) => {
                    let l = doc.run_look(p, t);
                    let st = SpanStyle {
                        size: px(l.size.unwrap_or(base_pt)),
                        font: font(l.b.unwrap_or(heading.is_some()), l.i.unwrap_or(false)),
                        color: l.color.map(rgb).unwrap_or(INK),
                    };
                    let d = disp.len();
                    disp.push_str(&sanitize(&t.text));
                    spans.push((d..disp.len(), st));
                    if l.u.unwrap_or(false) || l.s.unwrap_or(false) {
                        deco.push(Deco {
                            d: d..disp.len(),
                            underline: l.u.unwrap_or(false),
                            strike: l.s.unwrap_or(false),
                            color: st.color,
                        });
                    }
                    segs.push(Seg {
                        m,
                        ml: t.text.len(),
                        d,
                        dl: t.text.len(),
                        atom: false,
                    });
                    m += t.text.len();
                }
                Inline::Obj(o) if o.visible() => {
                    let (label, color, italic) = match &o.kind {
                        ObjKind::Image { .. } => (" [image] ".to_string(), CHIP, true),
                        ObjKind::PageBreak => (" ─── page break ─── ".to_string(), CHIP, true),
                        ObjKind::Link => (sanitize(&o.label).replace('\n', " "), LINK, false),
                        _ => (sanitize(&o.label).replace('\n', " "), INK, false),
                    };
                    let label = if label.is_empty() {
                        " ".to_string()
                    } else {
                        label
                    };
                    let d = disp.len();
                    disp.push_str(&label);
                    let st = SpanStyle {
                        size: base,
                        font: font(heading.is_some(), italic),
                        color,
                    };
                    spans.push((d..disp.len(), st));
                    if o.kind == ObjKind::Link {
                        deco.push(Deco {
                            d: d..disp.len(),
                            underline: true,
                            strike: false,
                            color: LINK,
                        });
                    }
                    segs.push(Seg {
                        m,
                        ml: super::model::OBJ_LEN,
                        d,
                        dl: label.len(),
                        atom: true,
                    });
                    m += super::model::OBJ_LEN;
                }
                Inline::Obj(_) => {}
            }
        }

        let mut pieces = Vec::new();
        let mut y = 0.0;
        let mut ls = 0;
        let mut line_h = base * 1.25;
        let hard: Vec<usize> = disp
            .char_indices()
            .filter(|(_, c)| *c == '\n')
            .map(|(i, _)| i)
            .chain(std::iter::once(disp.len()))
            .collect();
        for le in hard {
            let full = make(&spans, &disp, ls..le, base, avail, true);
            let rs = runs(&full);
            let h = full
                .min_bounds()
                .height
                .max(rs.iter().map(|r| r.0.top + r.0.h).fold(0.0, f32::max));
            let h = if h < 1.0 { base * 1.25 } else { h };
            line_h = rs.first().map(|r| r.0.h).unwrap_or(line_h);
            let k = match align {
                Align::Center => 0.5,
                Align::Right => 1.0,
                _ => 0.0,
            };
            if k > 0.0 && rs.len() > 1 {
                for (i, (r, _)) in rs.iter().enumerate() {
                    let s = ls + r.s.min(le - ls);
                    let e = rs.get(i + 1).map(|n| ls + n.0.s).unwrap_or(le).max(s);
                    let one = make(&spans, &disp, s..e, base, avail, false);
                    let w = one.min_bounds().width;
                    pieces.push(Piece {
                        para: one,
                        x: indent + (avail - w).max(0.0) * k,
                        y: y + r.top,
                        d0: s,
                        d1: e,
                    });
                }
            } else {
                let w = full.min_bounds().width;
                pieces.push(Piece {
                    para: full,
                    x: indent
                        + if k > 0.0 {
                            (avail - w).max(0.0) * k
                        } else {
                            0.0
                        },
                    y,
                    d0: ls,
                    d1: le,
                });
            }
            y += h;
            ls = le + 1;
        }
        let label = self.labels.get(&p.id).map(|l| {
            let st = SpanStyle {
                size: base,
                font: font(false, false),
                color: INK,
            };
            let lp = make(&[(0..l.len(), st)], l, 0..l.len(), base, 200.0, false);
            let w = lp.min_bounds().width;
            (lp, (indent - w - 8.0).max(0.0))
        });
        PL {
            rev: p.rev,
            width,
            h: before + y + after,
            top: before,
            indent,
            pieces,
            segs,
            deco,
            dlen: disp.len(),
            disp,
            mlen: m,
            line_h,
            image: None,
            label,
        }
    }

    fn ensure_para(&self, st: &mut St, p: &Para, width: f32) -> f32 {
        match st.paras.get(&p.id) {
            Some(l) if l.rev == p.rev && l.width == width => l.h,
            _ => {
                let l = self.para_layout(p, width);
                let h = l.h;
                st.paras.insert(p.id, l);
                h
            }
        }
    }

    fn opaque_layout(&self, label: &str, width: f32) -> OL {
        let text: String = sanitize(label).chars().take(4000).collect();
        let st = SpanStyle {
            size: 13.0,
            font: font(false, true),
            color: CHIP,
        };
        let para = make(
            &[(0..text.len(), st)],
            &text,
            0..text.len(),
            13.0,
            width - 16.0,
            true,
        );
        let h = para.min_bounds().height + 16.0;
        OL { width, h, para }
    }

    fn table_widths(&self, t: &super::model::Table, width: f32) -> Vec<f32> {
        let cols = t
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| usize::from(c.span)).sum::<usize>())
            .max()
            .unwrap_or(1)
            .max(1);
        let mut g: Vec<f32> = t.grid.iter().map(|w| twips(*w)).collect();
        if g.len() != cols || g.iter().any(|w| *w <= 1.0) {
            g = vec![width / cols as f32; cols];
        }
        if !t.fixed {
            // Word's default "autofit" layout: a column is never narrower
            // than its longest word, so words don't break mid-word.
            let min = autofit_min(t, cols);
            for (w, m) in g.iter_mut().zip(min) {
                *w = w.max(m);
            }
        }
        let sum: f32 = g.iter().sum();
        let scale = if sum > width { width / sum } else { 1.0 };
        g.iter().map(|w| w * scale).collect()
    }

    /// Lay out block `b` (if stale) and return its height.
    fn ensure_block(&self, st: &mut St, b: usize) -> f32 {
        let width = st.width;
        let Some(e) = self.ed.doc.body.get(b) else {
            return 0.0;
        };
        match &e.block {
            Block::Para(p) => self.ensure_para(st, p, width),
            Block::Opaque(o) if !o.visible => 0.0,
            Block::Opaque(o) => {
                if st.opaques.get(&o.id).is_none_or(|l| l.width != width) {
                    let l = self.opaque_layout(&o.label, width);
                    st.opaques.insert(o.id, l);
                }
                st.opaques[&o.id].h + 8.0
            }
            Block::Table(t) => {
                if let Some(l) = st.tables.get(&t.id) {
                    if l.rev == t.rev && l.width == width {
                        return l.h;
                    }
                }
                let cols = self.table_widths(t, width);
                let mut rows = Vec::new();
                let mut y = 0.0;
                for row in &t.rows {
                    let mut cells = Vec::new();
                    let mut col = 0;
                    let mut x = 0.0;
                    let mut row_h: f32 = 0.0;
                    for cell in &row.cells {
                        let span = usize::from(cell.span);
                        let w: f32 = cols.iter().skip(col).take(span).sum::<f32>().max(24.0);
                        col += span;
                        let inner = w - 2.0 * CELL_PAD;
                        let mut cy = CELL_PAD;
                        let mut entries = Vec::new();
                        for ce in &cell.entries {
                            match &ce.block {
                                Block::Para(p) => {
                                    let h = self.ensure_para(st, p, inner);
                                    entries.push((cy, Some(p.id)));
                                    cy += h;
                                }
                                Block::Opaque(o) if o.visible => {
                                    if st.opaques.get(&o.id).is_none_or(|l| l.width != inner) {
                                        let l = self.opaque_layout(&o.label, inner);
                                        st.opaques.insert(o.id, l);
                                    }
                                    entries.push((cy, None));
                                    cy += st.opaques[&o.id].h;
                                }
                                _ => entries.push((cy, None)),
                            }
                        }
                        row_h = row_h.max(cy + CELL_PAD - 6.0);
                        cells.push(CellL { x, w, entries });
                        x += w;
                    }
                    let row_h = row_h.max(22.0);
                    rows.push(RowL { y, h: row_h, cells });
                    y += row_h;
                }
                let h = y + 12.0;
                st.tables.insert(
                    t.id,
                    TL {
                        rev: t.rev,
                        width,
                        h,
                        rows,
                    },
                );
                h
            }
        }
    }

    fn estimate(&self, st: &St, b: usize) -> f32 {
        let width = st.width.max(100.0);
        let Some(e) = self.ed.doc.body.get(b) else {
            return 0.0;
        };
        match &e.block {
            Block::Para(p) => {
                if let Some(l) = st.paras.get(&p.id) {
                    if l.width == width {
                        return l.h;
                    }
                }
                let chars = p.len() as f32;
                let lines = (chars * 7.4 / width).ceil().max(1.0);
                lines * 19.0 + 10.0
            }
            Block::Opaque(o) if !o.visible => 0.0,
            Block::Opaque(o) => st.opaques.get(&o.id).map(|l| l.h + 8.0).unwrap_or(40.0),
            Block::Table(t) => st
                .tables
                .get(&t.id)
                .filter(|l| l.width == width)
                .map(|l| l.h)
                .unwrap_or(t.rows.len() as f32 * 28.0 + 12.0),
        }
    }

    fn is_fresh(&self, st: &St, b: usize) -> bool {
        let width = st.width;
        let Some(e) = self.ed.doc.body.get(b) else {
            return true;
        };
        match &e.block {
            Block::Para(p) => st
                .paras
                .get(&p.id)
                .is_some_and(|l| l.rev == p.rev && l.width == width),
            Block::Opaque(o) => {
                !o.visible || st.opaques.get(&o.id).is_some_and(|l| l.width == width)
            }
            Block::Table(t) => st
                .tables
                .get(&t.id)
                .is_some_and(|l| l.rev == t.rev && l.width == width),
        }
    }

    fn reflow(&self, st: &mut St) {
        let n = self.ed.doc.body.len();
        st.hs.clear();
        st.ys.clear();
        let mut y = 0.0;
        for b in 0..n {
            let h = self.estimate(st, b);
            st.ys.push(y);
            st.hs.push(h);
            y += h;
        }
        st.total = y;
    }

    fn content_h(&self, st: &St) -> f32 {
        GAP + 2.0 * st.margin + st.total + GAP
    }

    fn clamp_scroll(&self, st: &mut St) {
        let max = (self.content_h(st) - st.view.height).max(0.0);
        st.scroll = st.scroll.clamp(0.0, max);
    }

    /// Doc-y of the visible window.
    fn window(&self, st: &St) -> (f32, f32) {
        let top = st.scroll - GAP - st.margin;
        (top - 300.0, top + st.view.height + 300.0)
    }

    fn first_at(&self, st: &St, y: f32) -> usize {
        st.ys.partition_point(|&t| t <= y).saturating_sub(1)
    }

    /// Lay out the blocks in view, keeping what's on screen still when
    /// estimates above it turn into real heights.
    fn prepare(&self, st: &mut St) {
        self.reflow(st);
        for _ in 0..4 {
            let (lo, hi) = self.window(st);
            let mut changed = false;
            let mut b = self.first_at(st, lo);
            let view_top = st.scroll - GAP - st.margin;
            while b < st.ys.len() && st.ys[b] < hi {
                if !self.is_fresh(st, b) {
                    let old = st.hs[b];
                    let h = self.ensure_block(st, b);
                    if (h - old).abs() > 0.01 {
                        changed = true;
                        if st.ys[b] + old <= view_top + 1.0 {
                            st.scroll += h - old;
                        }
                    }
                }
                b += 1;
            }
            if !changed {
                break;
            }
            self.reflow(st);
        }
        self.clamp_scroll(st);
    }

    // --------------------------------------------------------- geometry

    /// Layout of the paragraph at `path` and its origin in doc coords.
    fn para_at<'s>(&self, st: &'s St, path: Path) -> Option<(Vector, &'s PL)> {
        let para = self.ed.doc.para(path)?;
        let l = st.paras.get(&para.id)?;
        let y0 = *st.ys.get(path.b)?;
        match path.cell {
            None => Some((Vector::new(0.0, y0), l)),
            Some((r, c, i)) => {
                let Block::Table(t) = &self.ed.doc.body.get(path.b)?.block else {
                    return None;
                };
                let tl = st.tables.get(&t.id)?;
                let row = tl.rows.get(r)?;
                let cell = row.cells.get(c)?;
                let (ey, _) = cell.entries.get(i)?;
                Some((Vector::new(cell.x + CELL_PAD, y0 + row.y + ey), l))
            }
        }
    }

    /// Caret rect in doc coords (lays out the block if needed).
    fn caret_rect(&self, st: &mut St, pos: Pos) -> Option<Rectangle> {
        if !self.is_fresh(st, pos.path.b) {
            let old = st.hs.get(pos.path.b).copied().unwrap_or(0.0);
            let h = self.ensure_block(st, pos.path.b);
            if (h - old).abs() > 0.01 {
                self.reflow(st);
            }
        }
        let (o, l) = self.para_at(st, pos.path)?;
        let (x, y, h) = l.caret(pos.off);
        Some(Rectangle::new(
            Point::new(o.x + x, o.y + y),
            Size::new(1.5, h),
        ))
    }

    /// Model position under a doc-coords point.
    fn hit(&self, st: &mut St, p: Point) -> Pos {
        let flow = &self.ed.flow;
        if flow.is_empty() {
            return Pos::default();
        }
        if p.y < 0.0 {
            return Pos::new(flow[0], 0);
        }
        let n = st.ys.len();
        if n == 0 {
            return Pos::default();
        }
        let mut b = self.first_at(st, p.y).min(n - 1);
        // skip blocks with nothing to put a caret in
        let caretable = |b: usize| match &self.ed.doc.body[b].block {
            Block::Para(_) | Block::Table(_) => true,
            Block::Opaque(_) => false,
        };
        if !caretable(b) {
            let next = (b..n).find(|&k| caretable(k));
            let prev = (0..b).rev().find(|&k| caretable(k));
            b = match (prev, next) {
                (_, Some(k)) if p.y >= st.ys[b] + st.hs[b] / 2.0 || prev.is_none() => k,
                (Some(k), _) => k,
                (None, Some(k)) => k,
                (None, None) => return Pos::new(flow[0], 0),
            };
        }
        if !self.is_fresh(st, b) {
            self.ensure_block(st, b);
            self.reflow(st);
        }
        let y0 = st.ys[b];
        match &self.ed.doc.body[b].block {
            Block::Para(para) => {
                let path = Path { b, cell: None };
                let off = st
                    .paras
                    .get(&para.id)
                    .map(|l| l.hit(Point::new(p.x, p.y - y0)))
                    .unwrap_or(0);
                Pos::new(path, off)
            }
            Block::Table(t) => {
                let Some(tl) = st.tables.get(&t.id) else {
                    return Pos::new(flow[0], 0);
                };
                let ry = p.y - y0;
                let r = tl
                    .rows
                    .iter()
                    .rposition(|row| row.y <= ry)
                    .unwrap_or(0)
                    .min(tl.rows.len().saturating_sub(1));
                let Some(row) = tl.rows.get(r) else {
                    return Pos::new(flow[0], 0);
                };
                let c = row
                    .cells
                    .iter()
                    .rposition(|cell| cell.x <= p.x)
                    .unwrap_or(0);
                let Some(cell) = row.cells.get(c) else {
                    return Pos::new(flow[0], 0);
                };
                let cy = ry - row.y;
                let paras: Vec<(usize, f32, u64)> = cell
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (y, id))| id.map(|id| (i, *y, id)))
                    .collect();
                let pick = paras
                    .iter()
                    .rev()
                    .find(|(_, y, _)| *y <= cy)
                    .or(paras.first());
                match pick {
                    Some(&(i, ey, id)) => {
                        let off = st
                            .paras
                            .get(&id)
                            .map(|l| l.hit(Point::new(p.x - cell.x - CELL_PAD, cy - ey)))
                            .unwrap_or(0);
                        Pos::new(
                            Path {
                                b,
                                cell: Some((r, c, i)),
                            },
                            off,
                        )
                    }
                    None => {
                        // a cell without paragraphs: nearest flow position
                        let i = flow
                            .partition_point(|q| q.b < b || (q.b == b && q.cell < Some((r, c, 0))));
                        Pos::new(flow[i.min(flow.len() - 1)], 0)
                    }
                }
            }
            Block::Opaque(_) => Pos::new(flow[0], 0),
        }
    }

    // --------------------------------------------------------- frame

    fn frame(&self, st: &St, bounds: Rectangle) -> (Rectangle, Point) {
        let page_w = PAGE_W.min(bounds.width - 2.0 * GAP - SCROLLBAR).max(200.0);
        let x = bounds.x + ((bounds.width - SCROLLBAR - page_w) / 2.0).max(0.0);
        let page = Rectangle::new(
            Point::new(x, bounds.y + GAP - st.scroll),
            Size::new(page_w, st.total + 2.0 * st.margin),
        );
        // doc-coords origin on screen
        let origin = Point::new(page.x + st.margin, page.y + st.margin);
        (page, origin)
    }

    fn setup(&self, st: &mut St, size: Size) {
        st.view = size;
        let page_w = PAGE_W.min(size.width - 2.0 * GAP - SCROLLBAR).max(200.0);
        st.margin = if page_w >= 640.0 {
            96.0
        } else {
            (page_w * 0.08).max(16.0)
        };
        let width = (page_w - 2.0 * st.margin).floor();
        if (width - st.width).abs() > 0.5 {
            st.width = width;
            st.paras.clear();
            st.tables.clear();
            st.opaques.clear();
        }
        if st.paras.len() > 20_000 {
            st.paras.clear();
            st.tables.clear();
        }
        if self.focus_gen != st.seen_focus {
            st.seen_focus = self.focus_gen;
            st.focused = self.editing;
        }
        self.prepare(st);
        if self.ed.sel_version != st.seen_sel {
            st.seen_sel = self.ed.sel_version;
            self.reveal(st);
        }
    }

    /// Scroll so the caret is visible.
    fn reveal(&self, st: &mut St) {
        // laying out the blocks that come into view can move the caret
        // again (estimates → real heights), so settle in a few rounds
        for _ in 0..4 {
            let Some(r) = self.caret_rect(st, self.ed.head) else {
                return;
            };
            let top = r.y + GAP + st.margin;
            let view = st.view.height;
            if top < st.scroll + 8.0 {
                st.scroll = top - 40.0;
            } else if top + r.height > st.scroll + view - 8.0 {
                st.scroll = top + r.height - view + 40.0;
            } else {
                return;
            }
            self.clamp_scroll(st);
            self.prepare(st);
        }
    }

    fn scroll_by(&self, st: &mut St, dy: f32) {
        st.scroll += dy;
        self.clamp_scroll(st);
        self.prepare(st);
    }

    /// Vertical caret move by visual lines (Up/Down/PageUp/PageDown).
    fn vertical(&self, st: &mut St, dir: f32, page: bool) -> Option<Pos> {
        let head = self.ed.head;
        let r = self.caret_rect(st, head)?;
        let x = match st.pref_x {
            Some((p, x)) if p == head => x,
            _ => r.x,
        };
        let step = if page { st.view.height * 0.9 } else { 0.0 };
        let mut y = if dir > 0.0 {
            r.y + r.height + 1.0 + step
        } else {
            r.y - 1.0 - step
        };
        if page {
            self.scroll_by(st, dir * step);
        }
        for _ in 0..200 {
            if y < 0.0 {
                return Some(self.ed.motion_target(Motion::DocStart, head));
            }
            if y > st.total {
                return Some(self.ed.motion_target(Motion::DocEnd, head));
            }
            let pos = self.hit(st, Point::new(x, y));
            let pr = self.caret_rect(st, pos)?;
            let moved = if dir > 0.0 {
                pr.y > r.y + 0.5
            } else {
                pr.y < r.y - 0.5
            };
            if moved {
                st.pref_x = Some((pos, x));
                return Some(pos);
            }
            y += dir * 4.0;
        }
        None
    }

    fn line_edge(&self, st: &mut St, end: bool) -> Option<Pos> {
        let r = self.caret_rect(st, self.ed.head)?;
        let pos = self.hit(
            st,
            Point::new(if end { 1.0e6 } else { -1.0e6 }, r.y + r.height / 2.0),
        );
        (pos.path == self.ed.head.path).then_some(pos)
    }

    fn key(
        &self,
        st: &mut St,
        key: &Key,
        mods: keyboard::Modifiers,
        text: Option<&str>,
        clip: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
    ) -> bool {
        let cmd = mods.command();
        let shift = mods.shift();
        let pub_ = |shell: &mut Shell<'_, Msg>, a: Act| shell.publish(Msg::Act(a));
        let ed = self.ed;
        if let Key::Character(c) = key.as_ref() {
            if cmd {
                match c.to_ascii_lowercase().as_str() {
                    "c" => {
                        if ed.has_sel() {
                            clip.write(clipboard::Kind::Standard, ed.selected_text());
                        }
                        return true;
                    }
                    "a" => {
                        pub_(shell, Act::SelectAll);
                        return true;
                    }
                    _ if !self.editing => return false,
                    "x" => {
                        if ed.has_sel() {
                            clip.write(clipboard::Kind::Standard, ed.selected_text());
                            pub_(shell, Act::Cut);
                        }
                        return true;
                    }
                    "v" => {
                        if let Some(t) = clip.read(clipboard::Kind::Standard) {
                            pub_(shell, Act::Insert(t));
                        }
                        return true;
                    }
                    "z" if shift => pub_(shell, Act::Redo),
                    "z" => pub_(shell, Act::Undo),
                    "y" => pub_(shell, Act::Redo),
                    "b" => pub_(shell, Act::Toggle(Attr::Bold)),
                    "i" => pub_(shell, Act::Toggle(Attr::Italic)),
                    "u" => pub_(shell, Act::Toggle(Attr::Underline)),
                    _ => return false,
                }
                return true;
            }
        }
        if !self.editing {
            // reading: keys scroll the page
            let dy = match key.as_ref() {
                Key::Named(Named::ArrowUp) => -48.0,
                Key::Named(Named::ArrowDown) => 48.0,
                Key::Named(Named::PageUp) => -st.view.height * 0.9,
                Key::Named(Named::PageDown) | Key::Named(Named::Space) => st.view.height * 0.9,
                Key::Named(Named::Home) => -1.0e9,
                Key::Named(Named::End) => 1.0e9,
                _ => return false,
            };
            self.scroll_by(st, dy);
            return true;
        }
        let keep_pref = matches!(
            key.as_ref(),
            Key::Named(Named::ArrowUp | Named::ArrowDown | Named::PageUp | Named::PageDown)
        );
        if !keep_pref {
            st.pref_x = None;
        }
        let select = |shell: &mut Shell<'_, Msg>, to: Option<Pos>| {
            if let Some(to) = to {
                let anchor = if shift { ed.anchor } else { to };
                shell.publish(Msg::Act(Act::Select(anchor, to)));
            }
        };
        match key.as_ref() {
            Key::Named(Named::ArrowLeft) => pub_(
                shell,
                Act::Move(if cmd { Motion::WordLeft } else { Motion::Left }, shift),
            ),
            Key::Named(Named::ArrowRight) => pub_(
                shell,
                Act::Move(
                    if cmd {
                        Motion::WordRight
                    } else {
                        Motion::Right
                    },
                    shift,
                ),
            ),
            Key::Named(Named::ArrowUp) => {
                let to = self.vertical(st, -1.0, false);
                select(shell, to)
            }
            Key::Named(Named::ArrowDown) => {
                let to = self.vertical(st, 1.0, false);
                select(shell, to)
            }
            Key::Named(Named::PageUp) => {
                let to = self.vertical(st, -1.0, true);
                select(shell, to)
            }
            Key::Named(Named::PageDown) => {
                let to = self.vertical(st, 1.0, true);
                select(shell, to)
            }
            Key::Named(Named::Home) if cmd => pub_(shell, Act::Move(Motion::DocStart, shift)),
            Key::Named(Named::End) if cmd => pub_(shell, Act::Move(Motion::DocEnd, shift)),
            Key::Named(Named::Home) => {
                let to = self
                    .line_edge(st, false)
                    .or(Some(ed.motion_target(Motion::LineStart, ed.head)));
                select(shell, to)
            }
            Key::Named(Named::End) => {
                let to = self
                    .line_edge(st, true)
                    .or(Some(ed.motion_target(Motion::LineEnd, ed.head)));
                select(shell, to)
            }
            Key::Named(Named::Enter) if shift => pub_(shell, Act::LineBreak),
            Key::Named(Named::Enter) => pub_(shell, Act::Enter),
            Key::Named(Named::Backspace) => pub_(shell, Act::Backspace(cmd)),
            Key::Named(Named::Delete) => pub_(shell, Act::Delete(cmd)),
            Key::Named(Named::Tab) => pub_(shell, Act::Tab(shift)),
            Key::Named(Named::Escape) => {
                st.focused = false;
                return false;
            }
            _ if cmd => return false,
            _ => {
                if let Some(t) = text.filter(|t| !t.chars().any(char::is_control)) {
                    pub_(shell, Act::Insert(t.to_string()));
                }
                // everything else is swallowed: F2/Delete/letters must not
                // reach the files pane while typing
            }
        }
        true
    }
}

impl Widget<Msg, Theme, iced::Renderer> for Page<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<St>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(St {
            seen_focus: self.focus_gen,
            focused: self.editing,
            seen_sel: u64::MAX,
            ..St::default()
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &self,
        tree: &mut Tree,
        _r: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.max();
        let st = tree.state.downcast_mut::<St>();
        self.setup(st, size);
        layout::Node::new(size)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        use iced::advanced::Renderer as _;
        let st = tree.state.downcast_ref::<St>();
        let bounds = layout.bounds();
        let (page, origin) = self.frame(st, bounds);
        renderer.with_layer(bounds, |r| {
            // soft edge instead of a Quad shadow (tiny-skia draws those
            // unclipped), and only the visible part of the sheet
            for (grow, dy, a) in [(6.0, 3.0, 0.10), (3.0, 2.0, 0.16), (1.0, 1.0, 0.30)] {
                let edge = Rectangle {
                    x: page.x - grow,
                    y: page.y - grow + dy,
                    width: page.width + 2.0 * grow,
                    height: page.height + 2.0 * grow,
                };
                if let Some(v) = edge.intersection(&bounds) {
                    r.fill_quad(
                        Quad {
                            bounds: v,
                            border: Border {
                                radius: (grow + 2.0).into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        Color::from_rgba(0.0, 0.0, 0.0, a),
                    );
                }
            }
            if let Some(v) = page.intersection(&bounds) {
                r.fill_quad(
                    Quad {
                        bounds: v,
                        ..Default::default()
                    },
                    PAPER,
                );
            }
            let (sa, sb) = self.ed.range();
            let show_caret = self.editing && st.focused;
            let view_top = bounds.y - origin.y;
            let view_bot = view_top + bounds.height;
            let first = self.first_at(st, view_top);
            let n = st.ys.len();
            let draw_para = |r: &mut iced::Renderer, path: Path, at: Point| {
                let Some(para) = self.ed.doc.para(path) else {
                    return;
                };
                let Some(l) = st.paras.get(&para.id) else {
                    return;
                };
                // selection
                if sa != sb && sa.path <= path && path <= sb.path {
                    let lo = if path == sa.path { sa.off } else { 0 };
                    let hi = if path == sb.path { sb.off } else { para.len() };
                    let past = path != sb.path;
                    for rc in l.rects(l.m2d(lo), l.m2d(hi), past) {
                        r.fill_quad(
                            Quad {
                                bounds: rc + Vector::new(at.x, at.y),
                                ..Default::default()
                            },
                            SELECT,
                        );
                    }
                }
                if let Some((h, sz)) = &l.image {
                    r.draw_image(
                        iced::advanced::image::Image {
                            handle: h.clone(),
                            filter_method: iced::advanced::image::FilterMethod::Linear,
                            rotation: iced::Radians(0.0),
                            opacity: 1.0,
                            snap: true,
                        },
                        Rectangle::new(Point::new(at.x + l.indent, at.y + l.top), *sz),
                    );
                }
                if let Some((lp, x)) = &l.label {
                    r.fill_paragraph(lp, Point::new(at.x + x, at.y + l.top), INK, bounds);
                }
                for pc in &l.pieces {
                    r.fill_paragraph(
                        &pc.para,
                        Point::new(at.x + pc.x, at.y + l.top + pc.y),
                        INK,
                        bounds,
                    );
                }
                for d in &l.deco {
                    for pc in &l.pieces {
                        if d.d.end <= pc.d0 || d.d.start >= pc.d1 {
                            continue;
                        }
                        for (run, g) in runs(&pc.para) {
                            let (s, e) =
                                ((d.d.start.max(pc.d0 + run.s)), d.d.end.min(pc.d0 + run.e));
                            if s >= e {
                                continue;
                            }
                            let x0 = x_at(&g, s - pc.d0);
                            let x1 = x_at(&g, e - pc.d0);
                            let base = at.y + l.top + pc.y + run.base;
                            let mut line = |y: f32| {
                                r.fill_quad(
                                    Quad {
                                        bounds: Rectangle::new(
                                            Point::new(at.x + pc.x + x0, y),
                                            Size::new((x1 - x0).max(1.0), 1.0),
                                        ),
                                        ..Default::default()
                                    },
                                    d.color,
                                );
                            };
                            if d.underline {
                                line(base + 2.0);
                            }
                            if d.strike {
                                line(base - run.h * 0.25);
                            }
                        }
                    }
                }
                if show_caret && self.ed.head.path == path {
                    let (x, y, h) = l.caret(self.ed.head.off);
                    r.fill_quad(
                        Quad {
                            bounds: Rectangle::new(
                                Point::new(at.x + x, at.y + y),
                                Size::new(1.6, h),
                            ),
                            ..Default::default()
                        },
                        INK,
                    );
                }
            };
            for b in first..n {
                let y = st.ys[b];
                if y > view_bot {
                    break;
                }
                let at = Point::new(origin.x, origin.y + y);
                match &self.ed.doc.body[b].block {
                    Block::Para(_) => draw_para(r, Path { b, cell: None }, at),
                    Block::Opaque(o) if o.visible => {
                        if let Some(l) = st.opaques.get(&o.id) {
                            r.fill_quad(
                                Quad {
                                    bounds: Rectangle::new(at, Size::new(st.width, l.h)),
                                    border: Border {
                                        radius: 4.0.into(),
                                        width: 1.0,
                                        color: Color { a: 0.5, ..GRID },
                                    },
                                    ..Default::default()
                                },
                                Color::from_rgba(0.0, 0.0, 0.0, 0.035),
                            );
                            r.fill_paragraph(
                                &l.para,
                                Point::new(at.x + 8.0, at.y + 8.0),
                                CHIP,
                                bounds,
                            );
                        }
                    }
                    Block::Opaque(_) => {}
                    Block::Table(t) => {
                        let Some(tl) = st.tables.get(&t.id) else {
                            continue;
                        };
                        for (ri, row) in tl.rows.iter().enumerate() {
                            for (ci, cell) in row.cells.iter().enumerate() {
                                r.fill_quad(
                                    Quad {
                                        bounds: Rectangle::new(
                                            Point::new(at.x + cell.x, at.y + row.y),
                                            Size::new(cell.w, row.h),
                                        ),
                                        border: Border {
                                            width: 1.0,
                                            color: GRID,
                                            radius: 0.0.into(),
                                        },
                                        ..Default::default()
                                    },
                                    Color::TRANSPARENT,
                                );
                                let Some(c) = t.rows.get(ri).and_then(|rw| rw.cells.get(ci)) else {
                                    continue;
                                };
                                for (ei, (ey, _)) in cell.entries.iter().enumerate() {
                                    let pt =
                                        Point::new(at.x + cell.x + CELL_PAD, at.y + row.y + ey);
                                    match c.entries.get(ei).map(|e| &e.block) {
                                        Some(Block::Para(_)) => draw_para(
                                            r,
                                            Path {
                                                b,
                                                cell: Some((ri, ci, ei)),
                                            },
                                            pt,
                                        ),
                                        Some(Block::Opaque(o)) if o.visible => {
                                            if let Some(l) = st.opaques.get(&o.id) {
                                                r.fill_paragraph(
                                                    &l.para,
                                                    Point::new(pt.x + 8.0, pt.y + 8.0),
                                                    CHIP,
                                                    bounds,
                                                );
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // scrollbar
            let content = self.content_h(st);
            if content > bounds.height + 1.0 {
                let track = bounds.height - 8.0;
                let h = (track * bounds.height / content).max(24.0);
                let y = bounds.y + 4.0 + (track - h) * st.scroll / (content - bounds.height);
                r.fill_quad(
                    Quad {
                        bounds: Rectangle::new(
                            Point::new(bounds.x + bounds.width - SCROLLBAR + 2.0, y),
                            Size::new(SCROLLBAR - 4.0, h),
                        ),
                        border: Border {
                            radius: 3.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    Color::from_rgba(0.6, 0.58, 0.55, 0.55),
                );
            }
        });
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        _viewport: &Rectangle,
    ) -> iced::event::Status {
        use iced::event::Status;
        let st = tree.state.downcast_mut::<St>();
        let bounds = layout.bounds();
        let to_doc = |st: &St, p: Point| {
            let (_, origin) = self.frame(st, bounds);
            Point::new(p.x - origin.x, p.y - origin.y)
        };
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                st.mods = m;
                Status::Ignored
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                if !cursor.is_over(bounds) {
                    return Status::Ignored;
                }
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -y * 60.0,
                    mouse::ScrollDelta::Pixels { y, .. } => -y,
                };
                self.scroll_by(st, dy);
                shell.invalidate_layout();
                Status::Captured
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(p) = cursor.position_over(bounds) else {
                    st.focused = false;
                    return Status::Ignored;
                };
                st.focused = true;
                if p.x >= bounds.x + bounds.width - SCROLLBAR - 2.0 {
                    let content = self.content_h(st);
                    let track = bounds.height - 8.0;
                    let h = (track * bounds.height / content.max(1.0)).max(24.0);
                    let y = bounds.y
                        + 4.0
                        + (track - h) * st.scroll / (content - bounds.height).max(1.0);
                    let grab = if p.y >= y && p.y <= y + h {
                        p.y - y
                    } else {
                        h / 2.0
                    };
                    st.drag = Some(Drag::Bar(grab));
                    return Status::Captured;
                }
                let pos = self.hit(st, to_doc(st, p));
                let now = Instant::now();
                let count = match st.clicks {
                    Some((t, q, n))
                        if now.duration_since(t).as_millis() < 450 && q.distance(p) < 5.0 =>
                    {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                st.clicks = Some((now, p, count));
                st.pref_x = None;
                let act = match count {
                    2 => {
                        let r = self
                            .ed
                            .doc
                            .para(pos.path)
                            .map(|pp| pp.word_at(pos.off))
                            .unwrap_or(pos.off..pos.off);
                        Act::Select(Pos::new(pos.path, r.start), Pos::new(pos.path, r.end))
                    }
                    3 => {
                        let len = self.ed.doc.para(pos.path).map(Para::len).unwrap_or(0);
                        Act::Select(Pos::new(pos.path, 0), Pos::new(pos.path, len))
                    }
                    _ if st.mods.shift() => Act::Select(self.ed.anchor, pos),
                    _ => Act::Select(pos, pos),
                };
                st.drag = Some(Drag::Select);
                shell.publish(Msg::Act(act));
                Status::Captured
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => match st.drag {
                Some(Drag::Select) => {
                    // auto-scroll past the edges
                    if position.y < bounds.y {
                        self.scroll_by(st, (position.y - bounds.y).max(-40.0));
                    } else if position.y > bounds.y + bounds.height {
                        self.scroll_by(st, (position.y - bounds.y - bounds.height).min(40.0));
                    }
                    let pos = self.hit(st, to_doc(st, position));
                    if pos != self.ed.head {
                        shell.publish(Msg::Act(Act::Select(self.ed.anchor, pos)));
                    }
                    shell.invalidate_layout();
                    Status::Captured
                }
                Some(Drag::Bar(grab)) => {
                    let content = self.content_h(st);
                    let track = bounds.height - 8.0;
                    let h = (track * bounds.height / content.max(1.0)).max(24.0);
                    let frac = (position.y - grab - bounds.y - 4.0) / (track - h).max(1.0);
                    st.scroll = frac.clamp(0.0, 1.0) * (content - bounds.height).max(0.0);
                    self.prepare(st);
                    shell.invalidate_layout();
                    Status::Captured
                }
                None => Status::Ignored,
            },
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if st.drag.take().is_some() {
                    Status::Captured
                } else {
                    Status::Ignored
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                text,
                ..
            }) if st.focused => {
                if self.key(st, &key, modifiers, text.as_deref(), clipboard, shell) {
                    shell.invalidate_layout();
                    Status::Captured
                } else {
                    Status::Ignored
                }
            }
            _ => Status::Ignored,
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<St>();
        let bounds = layout.bounds();
        match cursor.position_over(bounds) {
            Some(p) if p.x >= bounds.x + bounds.width - SCROLLBAR - 2.0 => mouse::Interaction::Idle,
            Some(p) => {
                let (page, _) = self.frame(st, bounds);
                if page.contains(p) {
                    mouse::Interaction::Text
                } else {
                    mouse::Interaction::Idle
                }
            }
            None => mouse::Interaction::Idle,
        }
    }
}

impl<'a> From<Page<'a>> for Element<'a, Msg> {
    fn from(p: Page<'a>) -> Self {
        Element::new(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewers::docx::{list_labels, parse, tests::build, tests::BODY};

    fn agree(body: &str, width: f32) {
        let pkg = parse::open_package(build(body, true)).unwrap();
        let ed = Model::new(pkg.doc);
        let images = HashMap::new();
        let labels = list_labels(&ed);
        let page = Page {
            ed: &ed,
            images: &images,
            labels: &labels,
            editing: true,
            focus_gen: 0,
        };
        let mut st = St::default();
        page.setup(&mut st, Size::new(width, 700.0));
        for &path in &ed.flow {
            let para = ed.doc.para(path).unwrap();
            let text = para.text();
            let offs = text.char_indices().map(|c| c.0).chain([text.len()]);
            for off in offs {
                let r = page.caret_rect(&mut st, Pos::new(path, off)).unwrap();
                let hit = page.hit(&mut st, Point::new(r.x + 0.5, r.y + r.height / 2.0));
                assert_eq!(hit, Pos::new(path, off), "{text:?} at {off}: {r:?}");
            }
        }
    }

    #[test]
    fn autofit_widens_narrow_columns_unless_fixed() {
        let tbl = |layout: &str| {
            format!(
                r#"<w:tbl><w:tblPr>{layout}</w:tblPr><w:tblGrid><w:gridCol w:w="400"/><w:gridCol w:w="400"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Q1</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
            )
        };
        let widths = |body: &str| {
            let pkg = parse::open_package(build(body, true)).unwrap();
            let ed = Model::new(pkg.doc);
            let (images, labels) = (HashMap::new(), HashMap::new());
            let page = Page {
                ed: &ed,
                images: &images,
                labels: &labels,
                editing: false,
                focus_gen: 0,
            };
            let t = ed
                .doc
                .body
                .iter()
                .find_map(|e| match &e.block {
                    Block::Table(t) => Some(t.clone()),
                    _ => None,
                })
                .unwrap();
            page.table_widths(&t, 800.0)
        };
        let auto = widths(&tbl(""));
        // "Region" needs more than the declared 400 twips (~27px)
        assert!(auto[0] > 60.0, "{auto:?}");
        assert!(auto[1] < auto[0]);
        let fixed = widths(&tbl(r#"<w:tblLayout w:type="fixed"/>"#));
        assert!((fixed[0] - twips(400.0)).abs() < 0.1, "{fixed:?}");
    }

    #[test]
    fn caret_and_hit_agree() {
        agree(BODY, 1000.0);
    }

    #[test]
    fn wrapped_and_centred_lines() {
        let long = "word ".repeat(120);
        let body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{long}</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t xml:space="preserve">{long}</w:t></w:r><w:r><w:br/><w:t>end</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="right"/></w:pPr><w:r><w:t>short</w:t></w:r></w:p>"#
        );
        agree(&body, 1000.0);
        agree(&body, 420.0);
        // Down walks visual lines, then into the next paragraph
        let pkg = parse::open_package(build(&body, true)).unwrap();
        let ed = Model::new(pkg.doc);
        let (images, labels) = (HashMap::new(), HashMap::new());
        let page = Page {
            ed: &ed,
            images: &images,
            labels: &labels,
            editing: true,
            focus_gen: 0,
        };
        let mut st = St::default();
        page.setup(&mut st, Size::new(1000.0, 700.0));
        let to = page.vertical(&mut st, 1.0, false).unwrap();
        assert_eq!(to.path, ed.head.path);
        assert!(to.off > 40 && to.off < 200, "{to:?}");
        // the centred paragraph's lines are each centred
        let p1 = ed.flow[1];
        let l = st.paras.get(&ed.doc.para(p1).unwrap().id).unwrap();
        assert!(l.pieces.len() > 3);
        let first = &l.pieces[0];
        let last = l.pieces.last().unwrap();
        // the last line is just "end" (after a line break): centred, it
        // starts far right of the full-width first line, whatever the font
        assert!(last.x > first.x + 20.0, "short last line is centred");
    }
}
