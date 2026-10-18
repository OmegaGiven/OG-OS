//! Layout-aware text extraction — the core under every "PDF → other format"
//! converter (`tools/convert`).
//!
//! **Capture.** Pages are interpreted by hayro (the same engine that renders
//! them) into a [`Device`] that records instead of painting:
//!   * every glyph with its Unicode text (ToUnicode CMaps, simple-font
//!     encodings + Differences, standard encodings, Identity-H CID fonts —
//!     all resolved by hayro), position in top-down page points, advance,
//!     font size, fill colour and whether it is invisible (OCR layers);
//!   * the font of each glyph by its resource dictionary (looked up by
//!     hayro's cache key): cleaned family name, bold / italic / monospace
//!     from the name, the descriptor flags, weight and italic angle;
//!   * raster images with their placement (JPEG passed through untouched
//!     when it is plain DeviceRGB/DeviceGray, everything else → PNG);
//!   * horizontal / vertical rules (thin filled rectangles, stroked lines,
//!     edges of non-white filled cells) for ruled-table detection.
//!
//! **Analysis** (per page, all heuristics relative to the font size):
//!   1. ruled tables: connected grids of rules → rows/columns (+ merged
//!      cells); glyphs inside are re-grouped per cell;
//!   2. lines: baseline clustering (superscripts/subscripts join their line),
//!      words from explicit spaces or gaps > 0.15 em, *segments* split at
//!      gaps > max(0.9 em, 2.5 × the line's word gap);
//!   3. unruled tables: runs of consecutive lines with ≥ 2 segments whose
//!      x-intervals fall into consistent column bands (prose-like bands —
//!      long cells — are excluded so two-column text isn't a table);
//!   4. reading order: recursive XY-cut over segments, tables and images,
//!      preferring vertical cuts (columns) and then horizontal bands;
//!   5. paragraphs: spacing, indentation, size/weight changes, short last
//!      lines, list markers; alignment from the column bounds;
//!   6. headings: paragraph size ≥ 1.15 × body size (levels by distinct
//!      size, largest = 1) or a short all-bold line; lists: bullets and
//!      `1.` `a)` `(iv)` markers, levels by indentation.
//!
//! Limits: right-to-left and vertical scripts come out in visual order;
//! rotated text is kept as separate lines; Type3 glyph widths are estimated;
//! text clipped away or hidden under images is still extracted.

use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, CacheKey, ClipPath, Context, Device, GlyphDrawMode, Image,
    InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask, TransformExt,
};
use hayro::hayro_syntax::object::{Array, Dict, Name, Stream};
use hayro::hayro_syntax::page::Page;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// ------------------------------------------------------------------ model

/// Axis-aligned box in page points, origin top-left, y down.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BBox {
    pub fn w(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn h(&self) -> f32 {
        self.y1 - self.y0
    }
    fn union(&self, o: &BBox) -> BBox {
        BBox {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }
    fn cx(&self) -> f32 {
        (self.x0 + self.x1) / 2.0
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

/// A run of text with one look.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    /// cleaned family name ("Times New Roman", "Arial", "CMR10")
    pub font: Arc<str>,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub color: [u8; 3],
    /// drawn with text render mode 3 (OCR layer)
    pub invisible: bool,
    pub bbox: BBox,
}

impl Span {
    fn same_look(&self, o: &Span) -> bool {
        self.font == o.font
            && (self.size - o.size).abs() < 0.6
            && self.bold == o.bold
            && self.italic == o.italic
            && self.mono == o.mono
            && self.color == o.color
            && self.invisible == o.invisible
    }
}

/// One visual line (possibly several segments joined) or a cell line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
    pub bbox: BBox,
    pub baseline: f32,
    /// dominant font size
    pub size: f32,
}

impl Line {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
    fn all_mono(&self) -> bool {
        self.spans.iter().any(|s| !s.text.trim().is_empty())
            && self
                .spans
                .iter()
                .all(|s| s.mono || s.text.trim().is_empty())
    }
    fn all_bold(&self) -> bool {
        let t: usize = self.spans.iter().map(|s| s.text.trim().len()).sum();
        t > 0
            && self
                .spans
                .iter()
                .all(|s| s.bold || s.text.trim().is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Body,
    /// 1 (largest) … 3
    Heading(u8),
    /// `marker` is the original bullet/number, already removed from the text
    List {
        ordered: bool,
        marker: String,
        level: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Para {
    pub lines: Vec<Line>,
    pub kind: Kind,
    pub align: Align,
    pub bbox: BBox,
}

impl Para {
    /// Text runs of the whole paragraph: lines joined with a space (or
    /// de-hyphenated), adjacent runs with the same look merged.
    pub fn runs(&self) -> Vec<Span> {
        join_lines(&self.lines)
    }
    pub fn text(&self) -> String {
        self.runs().iter().map(|s| s.text.as_str()).collect()
    }
    /// dominant font size (by characters)
    pub fn size(&self) -> f32 {
        dominant_size(self.lines.iter().flat_map(|l| l.spans.iter()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub lines: Vec<Line>,
    pub colspan: u16,
    pub rowspan: u16,
    /// part of a merged cell whose origin is above/left
    pub covered: bool,
}

impl Cell {
    fn new(lines: Vec<Line>) -> Cell {
        Cell {
            lines,
            colspan: 1,
            rowspan: 1,
            covered: false,
        }
    }
    pub fn runs(&self) -> Vec<Span> {
        join_lines(&self.lines)
    }
    pub fn text(&self) -> String {
        self.runs()
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>()
            .trim()
            .to_string()
    }
    pub fn bold(&self) -> bool {
        !self.lines.is_empty() && self.lines.iter().all(|l| l.all_bold())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// rectangular: every row has the same number of cells
    pub rows: Vec<Vec<Cell>>,
    /// column widths in points (one per column)
    pub widths: Vec<f32>,
    pub bbox: BBox,
    /// found from drawn rules (vs. aligned text)
    pub ruled: bool,
}

impl Table {
    pub fn cols(&self) -> usize {
        self.rows.first().map_or(0, |r| r.len())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    /// unique per distinct image in the document (same image drawn twice
    /// shares the id and the bytes)
    pub id: usize,
    pub bbox: BBox,
    pub data: Arc<Vec<u8>>,
    /// "png" or "jpg"
    pub ext: &'static str,
    pub px: (u32, u32),
}

impl Picture {
    pub fn mime(&self) -> &'static str {
        if self.ext == "jpg" {
            "image/jpeg"
        } else {
            "image/png"
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Para(Para),
    Table(Table),
    Picture(Picture),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageLayout {
    /// 1-based
    pub number: usize,
    pub width: f32,
    pub height: f32,
    /// reading order
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub pages: Vec<PageLayout>,
    /// most common text size in the document
    pub body_size: f32,
}

impl Layout {
    pub fn has_text(&self) -> bool {
        self.pages.iter().any(|p| {
            p.items.iter().any(|i| match i {
                Item::Para(p) => p.lines.iter().any(|l| !l.text().trim().is_empty()),
                Item::Table(t) => t.rows.iter().flatten().any(|c| !c.text().is_empty()),
                Item::Picture(_) => false,
            })
        })
    }
    pub fn tables(&self) -> impl Iterator<Item = (usize, &Table)> {
        self.pages.iter().flat_map(|p| {
            p.items.iter().filter_map(move |i| match i {
                Item::Table(t) => Some((p.number, t)),
                _ => None,
            })
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// decode and keep images (txt/csv/xlsx don't need them)
    pub images: bool,
}

// -------------------------------------------------------------- capture

const MAX_GLYPHS: usize = 400_000;
const MAX_RULES: usize = 20_000;
const MAX_IMAGES: usize = 300;
const MAX_IMAGE_PIXELS: u64 = 30_000_000;

#[derive(Clone)]
struct FontInfo {
    family: Arc<str>,
    bold: bool,
    italic: bool,
    mono: bool,
}

#[derive(Debug, Clone)]
struct G {
    text: String,
    /// left edge / right edge along the baseline (upright text) or bbox
    x0: f32,
    x1: f32,
    /// baseline (y down)
    y: f32,
    size: f32,
    font: usize,
    color: [u8; 3],
    invisible: bool,
    /// not upright horizontal: kept in content order as separate lines
    rotated: bool,
    /// for rotated glyphs: their bbox top/bottom
    top: f32,
    bottom: f32,
    seq: usize,
}

impl G {
    fn is_space(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }
    fn top(&self) -> f32 {
        if self.rotated {
            self.top
        } else {
            self.y - 0.78 * self.size
        }
    }
    fn bottom(&self) -> f32 {
        if self.rotated {
            self.bottom
        } else {
            self.y + 0.22 * self.size
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Rule {
    horizontal: bool,
    /// y for horizontal, x for vertical
    pos: f32,
    a: f32,
    b: f32,
}

struct RawImage {
    key: u128,
    bbox: BBox,
}

struct Collector<'f> {
    fonts: &'f HashMap<u128, FontInfo>,
    font_list: Vec<FontInfo>,
    font_index: HashMap<u128, usize>,
    glyphs: Vec<G>,
    rules: Vec<Rule>,
    images: Vec<RawImage>,
    /// decoded image data by hayro cache key (shared across pages)
    decoded: &'f mut HashMap<u128, Option<Decoded>>,
    want_images: bool,
    page: BBox,
    seq: usize,
}

#[derive(Clone)]
struct Decoded {
    data: Arc<Vec<u8>>,
    ext: &'static str,
    px: (u32, u32),
    id: usize,
}

fn p32(p: Point) -> (f32, f32) {
    (p.x as f32, p.y as f32)
}

impl<'f> Collector<'f> {
    fn font_for(&mut self, glyph: &Glyph<'_>) -> usize {
        let key = match glyph {
            Glyph::Outline(g) => g.font_cache_key(),
            Glyph::Type3(_) => 0,
        };
        if let Some(&i) = self.font_index.get(&key) {
            return i;
        }
        let info = match (self.fonts.get(&key), glyph) {
            (Some(f), _) => f.clone(),
            (None, Glyph::Outline(g)) => match g.font_data() {
                Some(d) => {
                    let name = d.postscript_name.unwrap_or_default();
                    let mut f = font_from_name(&name);
                    f.bold |= d.weight.is_some_and(|w| w >= 600);
                    f.italic |= d.is_italic;
                    f.mono |= d.is_monospace;
                    f
                }
                None => font_from_name(""),
            },
            (None, Glyph::Type3(_)) => font_from_name(""),
        };
        self.font_list.push(info);
        let i = self.font_list.len() - 1;
        self.font_index.insert(key, i);
        i
    }

    fn rule_from_box(&mut self, b: Rect) {
        let (w, h) = (b.width() as f32, b.height() as f32);
        let (x0, y0, x1, y1) = (b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32);
        if h <= 2.5 && w >= 4.0 {
            self.push_rule(Rule {
                horizontal: true,
                pos: (y0 + y1) / 2.0,
                a: x0,
                b: x1,
            });
        } else if w <= 2.5 && h >= 4.0 {
            self.push_rule(Rule {
                horizontal: false,
                pos: (x0 + x1) / 2.0,
                a: y0,
                b: y1,
            });
        }
    }

    fn push_rule(&mut self, r: Rule) {
        if self.rules.len() < MAX_RULES && r.a.is_finite() && r.b.is_finite() && r.pos.is_finite() {
            self.rules.push(r);
        }
    }
}

impl<'a> Device<'a> for Collector<'_> {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_path(
        &mut self,
        path: &BezPath,
        transform: Affine,
        paint: &Paint<'a>,
        mode: &PathDrawMode,
    ) {
        if self.rules.len() >= MAX_RULES {
            return;
        }
        let p = transform * path.clone();
        match mode {
            PathDrawMode::Fill(_) => {
                let b = p.bounding_box();
                let (w, h) = (b.width() as f32, b.height() as f32);
                if (h <= 2.5 && w >= 4.0) || (w <= 2.5 && h >= 4.0) {
                    self.rule_from_box(b);
                    return;
                }
                // edges of filled (non-white) cell rectangles
                let light = match paint {
                    Paint::Color(c) => {
                        let [r, g, b, a] = c.to_rgba().to_rgba8();
                        a < 30 || (r as u32 + g as u32 + b as u32) > 3 * 242
                    }
                    Paint::Pattern(_) => true,
                };
                let page_area = self.page.w() * self.page.h();
                if !light && is_rect(&p) && w * h < 0.5 * page_area && w >= 4.0 && h >= 4.0 {
                    let (x0, y0, x1, y1) = (b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32);
                    for (horizontal, pos, a, bb) in [
                        (true, y0, x0, x1),
                        (true, y1, x0, x1),
                        (false, x0, y0, y1),
                        (false, x1, y0, y1),
                    ] {
                        self.push_rule(Rule {
                            horizontal,
                            pos,
                            a,
                            b: bb,
                        });
                    }
                }
            }
            PathDrawMode::Stroke(_) => {
                let mut start = Point::ZERO;
                let mut last = Point::ZERO;
                for el in p.elements() {
                    let seg = match *el {
                        PathEl::MoveTo(q) => {
                            start = q;
                            last = q;
                            None
                        }
                        PathEl::LineTo(q) => {
                            let s = Some((last, q));
                            last = q;
                            s
                        }
                        PathEl::ClosePath => {
                            let s = Some((last, start));
                            last = start;
                            s
                        }
                        PathEl::QuadTo(_, q) | PathEl::CurveTo(_, _, q) => {
                            last = q;
                            None
                        }
                    };
                    if let Some((a, b)) = seg {
                        let ((ax, ay), (bx, by)) = (p32(a), p32(b));
                        if (ay - by).abs() < 1.0 && (ax - bx).abs() >= 4.0 {
                            self.push_rule(Rule {
                                horizontal: true,
                                pos: (ay + by) / 2.0,
                                a: ax.min(bx),
                                b: ax.max(bx),
                            });
                        } else if (ax - bx).abs() < 1.0 && (ay - by).abs() >= 4.0 {
                            self.push_rule(Rule {
                                horizontal: false,
                                pos: (ax + bx) / 2.0,
                                a: ay.min(by),
                                b: ay.max(by),
                            });
                        }
                    }
                }
            }
        }
    }

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        paint: &Paint<'a>,
        mode: &GlyphDrawMode,
    ) {
        if self.glyphs.len() >= MAX_GLYPHS {
            return;
        }
        let text = match glyph.as_unicode() {
            Some(hayro::hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
            Some(hayro::hayro_interpret::hayro_cmap::BfString::String(s)) => s,
            None => return,
        };
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '\u{fb00}' => out.push_str("ff"),
                '\u{fb01}' => out.push_str("fi"),
                '\u{fb02}' => out.push_str("fl"),
                '\u{fb03}' => out.push_str("ffi"),
                '\u{fb04}' => out.push_str("ffl"),
                '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
                '\u{a0}' | '\t' => out.push(' '),
                c if c.is_control() => {}
                c => out.push(c),
            }
        }
        let text = out;
        if text.is_empty() {
            return;
        }
        let t = transform * glyph_transform;
        let adv = match glyph {
            Glyph::Outline(g) => g.advance_width().filter(|w| *w > 0.0).unwrap_or(500.0),
            Glyph::Type3(_) => 500.0,
        } as f64;
        let o = t * Point::ZERO;
        let e = t * Point::new(adv, 0.0);
        let up = t * Point::new(0.0, 1000.0);
        let size = ((up.x - o.x).powi(2) + (up.y - o.y).powi(2)).sqrt() as f32;
        if !size.is_finite() || size < 0.5 || size > 2000.0 {
            return;
        }
        let (dx, dy) = ((e.x - o.x) as f32, (e.y - o.y) as f32);
        // upright: advancing to the right, "up" pointing up the page
        let rotated = !(dx >= 0.0 && dy.abs() <= 0.1 * dx.abs().max(0.01) && up.y < o.y);
        let color = match paint {
            Paint::Color(c) => {
                let [r, g, b, _] = c.to_rgba().to_rgba8();
                [r, g, b]
            }
            Paint::Pattern(_) => [0, 0, 0],
        };
        let font = self.font_for(glyph);
        let (x0, x1, top, bottom) = if rotated {
            let pts = [o, e, up, t * Point::new(adv, 1000.0)];
            let xs = pts.iter().map(|p| p.x as f32);
            let ys = pts.iter().map(|p| p.y as f32);
            (
                xs.clone().fold(f32::MAX, f32::min),
                xs.fold(f32::MIN, f32::max),
                ys.clone().fold(f32::MAX, f32::min),
                ys.fold(f32::MIN, f32::max),
            )
        } else {
            (o.x as f32, (o.x as f32 + dx).max(o.x as f32), 0.0, 0.0)
        };
        self.seq += 1;
        self.glyphs.push(G {
            text,
            x0,
            x1,
            y: o.y as f32,
            size,
            font,
            color,
            invisible: matches!(mode, GlyphDrawMode::Invisible),
            rotated,
            top,
            bottom,
            seq: self.seq,
        });
    }

    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        let (iw, ih) = (image.width().max(1) as f64, image.height().max(1) as f64);
        let Image::Raster(r) = image else { return };
        let b = (transform * Rect::new(0.0, 0.0, iw, ih).to_path(0.1)).bounding_box();
        let bbox = BBox {
            x0: b.x0 as f32,
            y0: b.y0 as f32,
            x1: b.x1 as f32,
            y1: b.y1 as f32,
        };
        if bbox.w() < 6.0 || bbox.h() < 6.0 || self.images.len() >= MAX_IMAGES {
            return;
        }
        let key = r.cache_key();
        if self.want_images && !self.decoded.contains_key(&key) {
            let id = self.decoded.len();
            let d = decode_image(&r, id);
            self.decoded.insert(key, d);
        }
        if self.images.iter().any(|i| i.key == key && i.bbox == bbox) {
            return;
        }
        self.images.push(RawImage { key, bbox });
    }
}

fn is_rect(p: &BezPath) -> bool {
    let els = p.elements();
    let lines = els
        .iter()
        .filter(|e| matches!(e, PathEl::LineTo(_)))
        .count();
    let moves = els
        .iter()
        .filter(|e| matches!(e, PathEl::MoveTo(_)))
        .count();
    let curves = els
        .iter()
        .any(|e| matches!(e, PathEl::QuadTo(..) | PathEl::CurveTo(..)));
    if moves != 1 || curves || !(3..=4).contains(&lines) {
        return false;
    }
    // axis aligned: every point on the bbox edge
    let b = p.bounding_box();
    els.iter().all(|e| match e {
        PathEl::MoveTo(q) | PathEl::LineTo(q) => {
            ((q.x - b.x0).abs() < 0.5 || (q.x - b.x1).abs() < 0.5)
                && ((q.y - b.y0).abs() < 0.5 || (q.y - b.y1).abs() < 0.5)
        }
        _ => true,
    })
}

fn decode_image(r: &hayro::hayro_interpret::RasterImage<'_>, id: usize) -> Option<Decoded> {
    let stream = r.stream();
    let dict = stream.dict();
    let dct_only = match dict.get::<Name<'_>>(b"Filter".as_slice()) {
        Some(n) => n.as_str() == "DCTDecode",
        None => dict
            .get::<Array<'_>>(b"Filter".as_slice())
            .map(|a| {
                a.iter::<Name<'_>>()
                    .map(|n| n.as_str().to_string())
                    .collect::<Vec<_>>()
            })
            .is_some_and(|v| v.len() == 1 && v[0] == "DCTDecode"),
    };
    let cs = dict
        .get::<Name<'_>>(b"ColorSpace".as_slice())
        .map(|n| n.as_str().to_string());
    let plain = !dict.contains_key(b"SMask".as_slice())
        && !dict.contains_key(b"Mask".as_slice())
        && !dict.contains_key(b"Decode".as_slice());
    if dct_only && matches!(cs.as_deref(), Some("DeviceRGB") | Some("DeviceGray")) && plain {
        let raw = stream.raw_data().to_vec();
        if raw.starts_with(&[0xFF, 0xD8]) {
            if let Ok(dim) = image::ImageReader::with_format(
                std::io::Cursor::new(&raw),
                image::ImageFormat::Jpeg,
            )
            .into_dimensions()
            {
                return Some(Decoded {
                    data: Arc::new(raw),
                    ext: "jpg",
                    px: dim,
                    id,
                });
            }
        }
    }
    let (w, h) = (r.width() as u64, r.height() as u64);
    if w == 0 || h == 0 || w * h > MAX_IMAGE_PIXELS {
        return None;
    }
    let mut out = None;
    r.with_rgba(
        |data, alpha| {
            out = encode_png(data, alpha);
        },
        None,
    );
    out.map(|(bytes, px)| Decoded {
        data: Arc::new(bytes),
        ext: "png",
        px,
        id,
    })
}

fn encode_png(
    data: hayro::hayro_interpret::ImageData,
    alpha: Option<hayro::hayro_interpret::LumaData>,
) -> Option<(Vec<u8>, (u32, u32))> {
    use hayro::hayro_interpret::ImageData;
    use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
    let (w, h) = (data.width(), data.height());
    let alpha = alpha.filter(|a| a.width == w && a.height == h && a.data.iter().any(|&v| v < 255));
    let img = match (data, alpha) {
        (ImageData::Rgb(d), None) => DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, d.data)?),
        (ImageData::Luma(d), None) => DynamicImage::ImageLuma8(GrayImage::from_raw(w, h, d.data)?),
        (d, Some(a)) => {
            let mut px = Vec::with_capacity((w * h * 4) as usize);
            match d {
                ImageData::Rgb(d) => {
                    for (c, al) in d.data.chunks_exact(3).zip(&a.data) {
                        px.extend_from_slice(&[c[0], c[1], c[2], *al]);
                    }
                }
                ImageData::Luma(d) => {
                    for (c, al) in d.data.iter().zip(&a.data) {
                        px.extend_from_slice(&[*c, *c, *c, *al]);
                    }
                }
            }
            DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, px)?)
        }
    };
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    Some((buf, (w, h)))
}

// ------------------------------------------------------------- fonts

fn font_from_name(raw: &str) -> FontInfo {
    let name = match raw.split_once('+') {
        Some((p, rest)) if p.len() == 6 && p.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => raw,
    };
    let lower = name.to_ascii_lowercase();
    let (fam, style) = match name.find(['-', ',']) {
        Some(i) => (&name[..i], name[i + 1..].to_ascii_lowercase()),
        None => (name, String::new()),
    };
    let bold_words = [
        "bold",
        "black",
        "heavy",
        "semibold",
        "demi",
        "extrabold",
        "ultrabold",
    ];
    let mut bold = bold_words.iter().any(|w| lower.contains(w))
        || style == "bd"
        || style.starts_with("bd")
        || lower.starts_with("cmbx")
        || lower.starts_with("cmb10");
    let mut italic = lower.contains("italic")
        || lower.contains("oblique")
        || (style.contains("it") && !style.contains("lite"))
        || lower.starts_with("cmti")
        || lower.starts_with("cmsl")
        || lower.starts_with("cmmi")
        || lower.starts_with("cmbxti");
    let mono = ["courier", "mono", "consol", "cmtt", "menlo", "typewriter"]
        .iter()
        .any(|w| lower.contains(w));
    // "Arial,Bold" style suffixes inside the family part
    let mut fam = fam.to_string();
    for suf in ["PSMT", "MT", "PS"] {
        if fam.len() > suf.len() + 2 && fam.ends_with(suf) {
            fam.truncate(fam.len() - suf.len());
            break;
        }
    }
    for (suf, b, i) in [
        ("BoldItalic", true, true),
        ("BoldOblique", true, true),
        ("Bold", true, false),
        ("Italic", false, true),
        ("Oblique", false, true),
    ] {
        if fam.len() > suf.len() + 2 && fam.ends_with(suf) {
            fam.truncate(fam.len() - suf.len());
            bold |= b;
            italic |= i;
            break;
        }
    }
    let family = match fam.as_str() {
        "" => "Times New Roman".to_string(),
        "Helvetica" | "Arial" | "ArialMT" => "Arial".into(),
        "Times" | "TimesNewRoman" | "Times New Roman" => "Times New Roman".into(),
        "Courier" | "CourierNew" | "CourierNewPS" => "Courier New".into(),
        "Symbol" => "Symbol".into(),
        "ZapfDingbats" => "ZapfDingbats".into(),
        f => split_camel(f),
    };
    FontInfo {
        family: family.into(),
        bold,
        italic,
        mono,
    }
}

/// "TimesNewRoman" → "Times New Roman"; leaves "CMR10", "DejaVuSans" sane.
fn split_camel(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in cs.iter().enumerate() {
        if i > 0 && c.is_uppercase() && cs[i - 1].is_lowercase() {
            // keep "DejaVu"-style and "McX" short pieces together
            let prev_word_len = out
                .rsplit(' ')
                .next()
                .map(|w| w.chars().count())
                .unwrap_or(0);
            if prev_word_len >= 3 {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

fn font_from_dict(d: &Dict<'_>) -> FontInfo {
    let name = d
        .get::<Name<'_>>(b"BaseFont".as_slice())
        .map(|n| n.as_str().to_string())
        .unwrap_or_default();
    let mut f = font_from_name(&name);
    let desc = d.get::<Dict<'_>>(b"FontDescriptor".as_slice()).or_else(|| {
        d.get::<Array<'_>>(b"DescendantFonts".as_slice())
            .and_then(|a| a.iter::<Dict<'_>>().next())
            .and_then(|df| df.get::<Dict<'_>>(b"FontDescriptor".as_slice()))
    });
    if let Some(desc) = desc {
        let flags = desc.get::<i64>(b"Flags".as_slice()).unwrap_or(0);
        f.mono |= flags & 1 != 0;
        f.italic |= flags & 64 != 0;
        f.bold |= flags & (1 << 18) != 0;
        if let Some(w) = desc.get::<f32>(b"FontWeight".as_slice()) {
            f.bold |= w >= 600.0;
        }
        if let Some(a) = desc.get::<f32>(b"ItalicAngle".as_slice()) {
            f.italic |= a.abs() > 3.0;
        }
    }
    f
}

/// Font dictionaries reachable from a page (its resources, form XObjects,
/// annotation appearances), keyed like hayro keys its loaded fonts.
fn collect_fonts(page: &Page<'_>, out: &mut HashMap<u128, FontInfo>) {
    let mut seen = HashSet::new();
    let mut res = Some(page.resources());
    while let Some(r) = res {
        walk_fonts(&r.fonts, &r.x_objects, out, &mut seen, 0);
        res = r.parent();
    }
    if let Some(annots) = page.raw().get::<Array<'_>>(b"Annots".as_slice()) {
        for a in annots.iter::<Dict<'_>>().take(500) {
            if let Some(n) = a
                .get::<Dict<'_>>(b"AP".as_slice())
                .and_then(|ap| ap.get::<Stream<'_>>(b"N".as_slice()))
            {
                walk_form(&n, out, &mut seen, 1);
            }
        }
    }
}

fn walk_fonts(
    fonts: &Dict<'_>,
    xobjects: &Dict<'_>,
    out: &mut HashMap<u128, FontInfo>,
    seen: &mut HashSet<u128>,
    depth: u32,
) {
    for k in fonts.keys() {
        if let Some(f) = fonts.get::<Dict<'_>>(&*k) {
            out.entry(f.cache_key())
                .or_insert_with(|| font_from_dict(&f));
        }
    }
    if depth > 8 {
        return;
    }
    for k in xobjects.keys() {
        if let Some(s) = xobjects.get::<Stream<'_>>(&*k) {
            walk_form(&s, out, seen, depth + 1);
        }
    }
}

fn walk_form(
    s: &Stream<'_>,
    out: &mut HashMap<u128, FontInfo>,
    seen: &mut HashSet<u128>,
    depth: u32,
) {
    let d = s.dict();
    if d.get::<Name<'_>>(b"Subtype".as_slice())
        .map(|n| n.as_str() == "Form")
        != Some(true)
    {
        return;
    }
    if !seen.insert(d.cache_key()) {
        return;
    }
    if let Some(r) = d.get::<Dict<'_>>(b"Resources".as_slice()) {
        let fonts = r.get::<Dict<'_>>(b"Font".as_slice()).unwrap_or_default();
        let xo = r.get::<Dict<'_>>(b"XObject".as_slice()).unwrap_or_default();
        walk_fonts(&fonts, &xo, out, seen, depth);
    }
}

// ------------------------------------------------------------ entry point

/// Extract the layout of every page.
pub fn extract(pdf: &[u8], opts: Options) -> Result<Layout, String> {
    let data = Arc::new(pdf.to_vec());
    let doc = match std::panic::catch_unwind(|| Pdf::new(data)) {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return Err(format!("can't read PDF ({e:?})")),
        Err(_) => return Err("can't read PDF".into()),
    };
    let cache = InterpreterCache::new();
    let settings = InterpreterSettings::default();
    let mut decoded: HashMap<u128, Option<Decoded>> = HashMap::new();
    let mut raw_pages = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capture_page(page, &cache, &settings, &mut decoded, opts.images)
        }));
        let (w, h) = page.render_dimensions();
        match captured {
            Ok(c) => raw_pages.push((i, w, h, c)),
            // a page hayro can't interpret comes out empty
            Err(_) => raw_pages.push((
                i,
                w,
                h,
                Captured {
                    glyphs: vec![],
                    fonts: vec![],
                    rules: vec![],
                    images: vec![],
                },
            )),
        }
    }
    let body_size = {
        let mut hist: HashMap<i32, usize> = HashMap::new();
        for (_, _, _, c) in &raw_pages {
            for g in &c.glyphs {
                if !g.is_space() {
                    *hist.entry((g.size * 2.0).round() as i32).or_default() += 1;
                }
            }
        }
        hist.into_iter()
            .max_by_key(|&(s, n)| (n, -s))
            .map(|(s, _)| s as f32 / 2.0)
            .unwrap_or(11.0)
    };
    let mut pages = Vec::new();
    for (i, w, h, c) in raw_pages {
        let pics: Vec<Picture> = c
            .images
            .iter()
            .filter_map(|ri| {
                let d = decoded.get(&ri.key)?.as_ref()?;
                Some(Picture {
                    id: d.id,
                    bbox: ri.bbox,
                    data: d.data.clone(),
                    ext: d.ext,
                    px: d.px,
                })
            })
            .collect();
        pages.push(analyze_page(i + 1, w, h, c, pics, body_size));
    }
    assign_headings(&mut pages, body_size);
    Ok(Layout { pages, body_size })
}

struct Captured {
    glyphs: Vec<G>,
    fonts: Vec<FontInfo>,
    rules: Vec<Rule>,
    images: Vec<RawImage>,
}

fn capture_page<'a>(
    page: &'a Page<'a>,
    cache: &InterpreterCache<'a>,
    settings: &InterpreterSettings,
    decoded: &mut HashMap<u128, Option<Decoded>>,
    want_images: bool,
) -> Captured {
    let mut fonts = HashMap::new();
    collect_fonts(page, &mut fonts);
    let (w, h) = page.render_dimensions();
    let transform = page.initial_transform(true);
    let mut ctx = Context::new(
        transform.to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64),
        cache,
        page.xref(),
        settings.clone(),
    );
    let mut dev = Collector {
        fonts: &fonts,
        font_list: Vec::new(),
        font_index: HashMap::new(),
        glyphs: Vec::new(),
        rules: Vec::new(),
        images: Vec::new(),
        decoded,
        want_images,
        page: BBox {
            x0: 0.0,
            y0: 0.0,
            x1: w,
            y1: h,
        },
        seq: 0,
    };
    interpret_page(page, &mut ctx, &mut dev);
    Captured {
        glyphs: dev.glyphs,
        fonts: dev.font_list,
        rules: dev.rules,
        images: dev.images,
    }
}

// ------------------------------------------------------------ analysis

/// A horizontal piece of a line: glyphs on one baseline without a large gap.
#[derive(Debug, Clone)]
struct Seg {
    line: Line,
    rotated: bool,
}

impl Seg {
    fn bbox(&self) -> BBox {
        self.line.bbox
    }
    fn chars(&self) -> usize {
        self.line.text().trim().chars().count()
    }
}

fn span_of(g: &G, fonts: &[FontInfo]) -> Span {
    let f = fonts
        .get(g.font)
        .cloned()
        .unwrap_or_else(|| font_from_name(""));
    Span {
        text: String::new(),
        font: f.family,
        size: (g.size * 2.0).round() / 2.0,
        bold: f.bold,
        italic: f.italic,
        mono: f.mono,
        color: g.color,
        invisible: g.invisible,
        bbox: BBox {
            x0: g.x0,
            y0: g.top(),
            x1: g.x1,
            y1: g.bottom(),
        },
    }
}

/// Remove duplicates (fill+stroke, fake bold by overprinting).
fn dedupe(glyphs: &mut Vec<G>) {
    let mut seen = HashSet::new();
    glyphs.retain(|g| {
        let k = (
            (g.x0 * 2.0).round() as i64,
            (g.y * 2.0).round() as i64,
            g.text.clone(),
        );
        seen.insert(k)
    });
}

/// Glyphs → lines → segments. `split` = false keeps every line whole
/// (inside table cells).
fn build_segments(mut glyphs: Vec<G>, fonts: &[FontInfo], split: bool) -> Vec<Seg> {
    let mut segs = Vec::new();
    // rotated text: consecutive (content order) runs
    let (rot, mut up): (Vec<G>, Vec<G>) = glyphs.drain(..).partition(|g| g.rotated);
    let mut run: Vec<G> = Vec::new();
    let flush_rot = |run: &mut Vec<G>, segs: &mut Vec<Seg>| {
        if run.is_empty() {
            return;
        }
        let mut span = span_of(&run[0], fonts);
        let mut bbox = span.bbox;
        for g in run.iter() {
            span.text.push_str(&g.text);
            bbox = bbox.union(&BBox {
                x0: g.x0,
                y0: g.top,
                x1: g.x1,
                y1: g.bottom,
            });
        }
        span.text = span.text.split_whitespace().collect::<Vec<_>>().join(" ");
        span.bbox = bbox;
        if !span.text.is_empty() {
            let size = span.size;
            segs.push(Seg {
                line: Line {
                    spans: vec![span],
                    bbox,
                    baseline: bbox.y1,
                    size,
                },
                rotated: true,
            });
        }
        run.clear();
    };
    for g in rot {
        if let Some(p) = run.last() {
            let near = (g.x0 - p.x1).abs() < 2.0 * g.size
                || (g.top - p.bottom).abs() < 2.0 * g.size
                || (p.top - g.bottom).abs() < 2.0 * g.size;
            if g.seq != p.seq + 1 && !near {
                flush_rot(&mut run, &mut segs);
            }
        }
        run.push(g);
    }
    flush_rot(&mut run, &mut segs);

    // upright: cluster baselines
    up.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x0.total_cmp(&b.x0)));
    let mut lines: Vec<Vec<G>> = Vec::new();
    let mut ref_y = f32::MIN;
    let mut ref_size = 0.0f32;
    for g in up {
        let tol = 0.45 * ref_size.max(g.size);
        if lines.is_empty() || (g.y - ref_y).abs() > tol {
            ref_y = g.y;
            ref_size = g.size;
            lines.push(vec![g]);
        } else {
            if g.size > ref_size * 1.05 && !g.is_space() {
                ref_y = g.y;
                ref_size = g.size;
            }
            lines.last_mut().unwrap().push(g);
        }
    }
    for mut lg in lines {
        lg.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.seq.cmp(&b.seq)));
        // overlapping glyphs of different text drawn at nearly the same spot
        // (e.g. accents) are kept; spaces become gap markers
        let size = dominant_size_g(&lg);
        // word gaps for this line
        let mut gaps = Vec::new();
        let mut prev_x1: Option<f32> = None;
        for g in &lg {
            if g.is_space() {
                continue;
            }
            if let Some(p) = prev_x1 {
                let gap = g.x0 - p;
                if gap > 0.15 * size {
                    gaps.push(gap);
                }
            }
            prev_x1 = Some(g.x1);
        }
        gaps.sort_by(f32::total_cmp);
        let word_gap = gaps.get(gaps.len() / 2).copied().unwrap_or(0.3 * size);
        let seg_gap = (0.9 * size).max(2.5 * word_gap.min(0.6 * size));

        let mut cur: Vec<(G, bool)> = Vec::new(); // (glyph, space before)
        let mut space_pending = false;
        let mut last_x1: Option<f32> = None;
        let mut pieces: Vec<Vec<(G, bool)>> = Vec::new();
        for g in lg {
            if g.is_space() {
                space_pending = true;
                continue;
            }
            let gap = last_x1.map(|x| g.x0 - x);
            if let Some(gap) = gap {
                let leader = cur.len() >= 3
                    && cur[cur.len() - 3..]
                        .iter()
                        .all(|(p, _)| p.text == "." || p.text == "\u{2026}");
                if split && gap > seg_gap && !cur.is_empty() && !leader {
                    pieces.push(std::mem::take(&mut cur));
                    space_pending = false;
                }
            }
            let sp = !cur.is_empty()
                && (space_pending || gap.is_some_and(|gp| gp > 0.15 * g.size.min(size)));
            last_x1 = Some(last_x1.map_or(g.x1, |x| x.max(g.x1)));
            cur.push((g, sp));
            space_pending = false;
        }
        if !cur.is_empty() {
            pieces.push(cur);
        }
        for p in pieces {
            segs.push(Seg {
                line: make_line(&p, fonts),
                rotated: false,
            });
        }
    }
    segs
}

fn make_line(glyphs: &[(G, bool)], fonts: &[FontInfo]) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    for (g, sp) in glyphs {
        let mut s = span_of(g, fonts);
        if *sp {
            if let Some(last) = spans.last_mut() {
                last.text.push(' ');
            }
        }
        match spans.last_mut() {
            Some(last) if last.same_look(&s) => {
                last.text.push_str(&g.text);
                last.bbox = last.bbox.union(&s.bbox);
            }
            _ => {
                s.text.push_str(&g.text);
                spans.push(s);
            }
        }
    }
    let bbox = spans
        .iter()
        .map(|s| s.bbox)
        .reduce(|a, b| a.union(&b))
        .unwrap_or_default();
    let size = dominant_size(spans.iter());
    // baseline of the dominant-size glyphs
    let baseline = glyphs
        .iter()
        .filter(|(g, _)| (g.size - size).abs() < 0.6)
        .map(|(g, _)| g.y)
        .next()
        .unwrap_or(bbox.y1);
    Line {
        spans,
        bbox,
        baseline,
        size,
    }
}

fn dominant_size_g(gs: &[G]) -> f32 {
    let mut hist: HashMap<i32, usize> = HashMap::new();
    for g in gs {
        *hist.entry((g.size * 2.0).round() as i32).or_default() += g.text.len();
    }
    hist.into_iter()
        .max_by_key(|&(s, n)| (n, s))
        .map(|(s, _)| s as f32 / 2.0)
        .unwrap_or(10.0)
}

fn dominant_size<'s>(spans: impl Iterator<Item = &'s Span>) -> f32 {
    let mut hist: HashMap<i32, usize> = HashMap::new();
    for s in spans {
        let n = s.text.trim().chars().count();
        if n > 0 {
            *hist.entry((s.size * 2.0).round() as i32).or_default() += n;
        }
    }
    hist.into_iter()
        .max_by_key(|&(s, n)| (n, s))
        .map(|(s, _)| s as f32 / 2.0)
        .unwrap_or(10.0)
}

/// Lines → runs: join with a space, or drop a trailing hyphen when the next
/// line continues the word; merge neighbours with the same look.
fn join_lines(lines: &[Line]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        let mut spans = l.spans.clone();
        // trim line edges
        if let Some(f) = spans.first_mut() {
            f.text = f.text.trim_start().to_string();
        }
        if let Some(f) = spans.last_mut() {
            f.text = f.text.trim_end().to_string();
        }
        if li > 0 {
            let next_lower = spans
                .iter()
                .flat_map(|s| s.text.chars())
                .next()
                .is_some_and(|c| c.is_lowercase());
            let hyph = out.last().is_some_and(|s| {
                s.text.ends_with('-')
                    && s.text
                        .chars()
                        .rev()
                        .nth(1)
                        .is_some_and(|c| c.is_alphabetic())
            });
            if hyph && next_lower {
                if let Some(s) = out.last_mut() {
                    s.text.pop();
                }
            } else if let Some(s) = out.last_mut() {
                if !s.text.ends_with(' ') && !s.text.is_empty() {
                    s.text.push(' ');
                }
            }
        }
        for s in spans {
            if s.text.is_empty() {
                continue;
            }
            match out.last_mut() {
                Some(last) if last.same_look(&s) => {
                    last.text.push_str(&s.text);
                    last.bbox = last.bbox.union(&s.bbox);
                }
                _ => out.push(s),
            }
        }
    }
    if let Some(l) = out.last_mut() {
        let t = l.text.trim_end().to_string();
        l.text = t;
    }
    out.retain(|s| !s.text.is_empty());
    out
}

// --------------------------------------------------------- ruled tables

struct Grid {
    xs: Vec<f32>,
    ys: Vec<f32>,
    bbox: BBox,
    /// merged_right[r][c]: no rule between (r,c) and (r,c+1)
    merged_right: Vec<Vec<bool>>,
    merged_down: Vec<Vec<bool>>,
}

fn cluster_vals(mut v: Vec<f32>, tol: f32) -> Vec<f32> {
    v.sort_by(f32::total_cmp);
    let mut out: Vec<(f32, usize)> = Vec::new();
    for x in v {
        match out.last_mut() {
            Some((m, n)) if (x - *m / *n as f32).abs() <= tol => {
                *m += x;
                *n += 1;
            }
            _ => out.push((x, 1)),
        }
    }
    out.into_iter().map(|(m, n)| m / n as f32).collect()
}

fn merge_rules(mut rules: Vec<Rule>) -> Vec<Rule> {
    rules.sort_by(|a, b| a.pos.total_cmp(&b.pos).then(a.a.total_cmp(&b.a)));
    let mut out: Vec<Rule> = Vec::new();
    for r in rules {
        if let Some(m) = out
            .iter_mut()
            .rev()
            .take(64)
            .find(|m| (m.pos - r.pos).abs() <= 1.5 && r.a <= m.b + 1.5 && r.b >= m.a - 1.5)
        {
            m.a = m.a.min(r.a);
            m.b = m.b.max(r.b);
        } else {
            out.push(r);
        }
    }
    out
}

fn find_grids(rules: &[Rule]) -> Vec<Grid> {
    let (h, v): (Vec<Rule>, Vec<Rule>) = rules.iter().partition(|r| r.horizontal);
    let h = merge_rules(h);
    let v = merge_rules(v);
    if h.len() < 2 || v.len() < 2 || h.len() * v.len() > 4_000_000 {
        return vec![];
    }
    let n = h.len() + v.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    const T: f32 = 2.0;
    for (i, hr) in h.iter().enumerate() {
        for (j, vr) in v.iter().enumerate() {
            if vr.pos >= hr.a - T && vr.pos <= hr.b + T && hr.pos >= vr.a - T && hr.pos <= vr.b + T
            {
                let (a, b) = (find(&mut parent, i), find(&mut parent, h.len() + j));
                parent[a] = b;
            }
        }
    }
    let mut comps: HashMap<usize, (Vec<Rule>, Vec<Rule>)> = HashMap::new();
    for (i, r) in h.iter().enumerate() {
        let root = find(&mut parent, i);
        comps.entry(root).or_default().0.push(*r);
    }
    for (j, r) in v.iter().enumerate() {
        let root = find(&mut parent, h.len() + j);
        comps.entry(root).or_default().1.push(*r);
    }
    let mut grids = Vec::new();
    for (_, (hs, vs)) in comps {
        if hs.len() < 2 || vs.len() < 2 {
            continue;
        }
        let ys = cluster_vals(hs.iter().map(|r| r.pos).collect(), 2.0);
        let xs = cluster_vals(vs.iter().map(|r| r.pos).collect(), 2.0);
        if xs.len() < 2 || ys.len() < 2 || xs.len() > 101 || ys.len() > 501 {
            continue;
        }
        let (nr, nc) = (ys.len() - 1, xs.len() - 1);
        if nr * nc < 2 {
            continue;
        }
        let v_at = |x: f32, y: f32| {
            vs.iter()
                .any(|r| (r.pos - x).abs() <= 2.0 && y >= r.a - 1.0 && y <= r.b + 1.0)
        };
        let h_at = |y: f32, x: f32| {
            hs.iter()
                .any(|r| (r.pos - y).abs() <= 2.0 && x >= r.a - 1.0 && x <= r.b + 1.0)
        };
        let merged_right = (0..nr)
            .map(|r| {
                let ym = (ys[r] + ys[r + 1]) / 2.0;
                (0..nc)
                    .map(|c| c + 1 < nc && !v_at(xs[c + 1], ym))
                    .collect()
            })
            .collect();
        let merged_down = (0..nr)
            .map(|r| {
                (0..nc)
                    .map(|c| {
                        let xm = (xs[c] + xs[c + 1]) / 2.0;
                        r + 1 < nr && !h_at(ys[r + 1], xm)
                    })
                    .collect()
            })
            .collect();
        grids.push(Grid {
            bbox: BBox {
                x0: xs[0],
                y0: ys[0],
                x1: xs[nc],
                y1: ys[nr],
            },
            xs,
            ys,
            merged_right,
            merged_down,
        });
    }
    // nested grids (a table inside a frame): keep the inner ones
    let boxes: Vec<BBox> = grids.iter().map(|g| g.bbox).collect();
    grids.retain(|g| {
        !boxes.iter().any(|b| {
            *b != g.bbox
                && b.x0 >= g.bbox.x0 - 1.0
                && b.x1 <= g.bbox.x1 + 1.0
                && b.y0 >= g.bbox.y0 - 1.0
                && b.y1 <= g.bbox.y1 + 1.0
        })
    });
    grids.sort_by(|a, b| a.bbox.y0.total_cmp(&b.bbox.y0));
    grids
}

fn grid_table(grid: &Grid, glyphs: Vec<G>, fonts: &[FontInfo]) -> Option<Table> {
    let (nr, nc) = (grid.ys.len() - 1, grid.xs.len() - 1);
    // cell origin for every grid position (merged cells)
    let mut origin: Vec<Vec<(usize, usize)>> = vec![vec![(0, 0); nc]; nr];
    for r in 0..nr {
        for c in 0..nc {
            origin[r][c] = (r, c);
            if c > 0 && grid.merged_right[r][c - 1] {
                origin[r][c] = origin[r][c - 1];
            } else if r > 0 && grid.merged_down[r - 1][c] {
                origin[r][c] = origin[r - 1][c];
            }
        }
    }
    let mut per_cell: HashMap<(usize, usize), Vec<G>> = HashMap::new();
    for g in glyphs {
        let (cx, cy) = ((g.x0 + g.x1) / 2.0, (g.top() + g.bottom()) / 2.0);
        let c = grid.xs.windows(2).position(|w| cx >= w[0] && cx < w[1]);
        let r = grid.ys.windows(2).position(|w| cy >= w[0] && cy < w[1]);
        if let (Some(r), Some(c)) = (r, c) {
            per_cell.entry(origin[r][c]).or_default().push(g);
        }
    }
    let filled = per_cell
        .values()
        .filter(|v| v.iter().any(|g| !g.is_space()))
        .count();
    if filled < 2 {
        return None;
    }
    let mut rows: Vec<Vec<Cell>> = Vec::with_capacity(nr);
    for r in 0..nr {
        let mut row = Vec::with_capacity(nc);
        for c in 0..nc {
            if origin[r][c] != (r, c) {
                row.push(Cell {
                    lines: vec![],
                    colspan: 1,
                    rowspan: 1,
                    covered: true,
                });
                continue;
            }
            let gl = per_cell.remove(&(r, c)).unwrap_or_default();
            let mut cell = Cell::new(cell_lines(gl, fonts));
            cell.colspan = (c..nc).take_while(|&cc| origin[r][cc] == (r, c)).count() as u16;
            cell.rowspan = (r..nr).take_while(|&rr| origin[rr][c] == (r, c)).count() as u16;
            row.push(cell);
        }
        rows.push(row);
    }
    // drop rows that are entirely empty (decorative double rules)
    rows.retain(|row| {
        row.iter()
            .any(|c| !c.lines.is_empty() || c.covered || c.rowspan > 1)
    });
    if rows.is_empty() {
        return None;
    }
    Some(Table {
        rows,
        widths: grid.xs.windows(2).map(|w| w[1] - w[0]).collect(),
        bbox: grid.bbox,
        ruled: true,
    })
}

fn cell_lines(glyphs: Vec<G>, fonts: &[FontInfo]) -> Vec<Line> {
    let mut segs = build_segments(glyphs, fonts, false);
    segs.sort_by(|a, b| a.line.baseline.total_cmp(&b.line.baseline));
    segs.into_iter()
        .map(|s| s.line)
        .filter(|l| !l.text().trim().is_empty())
        .collect()
}

// ------------------------------------------------------- unruled tables

/// Rows of segments (indices into `segs`) on the same baseline, top→bottom.
fn rows_of(segs: &[Seg], ids: &[usize]) -> Vec<Vec<usize>> {
    let mut ids: Vec<usize> = ids.iter().copied().filter(|&i| !segs[i].rotated).collect();
    ids.sort_by(|&a, &b| {
        segs[a]
            .line
            .baseline
            .total_cmp(&segs[b].line.baseline)
            .then(segs[a].bbox().x0.total_cmp(&segs[b].bbox().x0))
    });
    let mut rows: Vec<Vec<usize>> = Vec::new();
    for i in ids {
        let s = &segs[i];
        match rows.last_mut() {
            Some(r)
                if (segs[r[0]].line.baseline - s.line.baseline).abs()
                    < 0.35 * s.line.size.max(segs[r[0]].line.size) =>
            {
                r.push(i)
            }
            _ => rows.push(vec![i]),
        }
    }
    for r in &mut rows {
        r.sort_by(|&a, &b| segs[a].bbox().x0.total_cmp(&segs[b].bbox().x0));
    }
    rows
}

/// Column bands: merged x-intervals of the given segments.
fn bands(segs: &[Seg], ids: impl Iterator<Item = usize>) -> Vec<(f32, f32)> {
    let mut iv: Vec<(f32, f32)> = ids
        .map(|i| (segs[i].bbox().x0, segs[i].bbox().x1))
        .collect();
    iv.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f32, f32)> = Vec::new();
    for (a, b) in iv {
        match out.last_mut() {
            Some(l) if a <= l.1 + 1.0 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

fn band_of(b: &[(f32, f32)], s: &Seg) -> Option<usize> {
    let c = s.bbox().cx();
    b.iter().position(|&(a, z)| c >= a - 1.0 && c <= z + 1.0)
}

/// Find text tables; returns (table, member segment ids).
fn find_text_tables(segs: &[Seg], free: &[usize]) -> Vec<(Table, Vec<usize>)> {
    let rows = rows_of(segs, free);
    let mut out = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].len() < 2 {
            i += 1;
            continue;
        }
        // maximal run of multi-segment rows, allowing single rows that are
        // sandwiched (wrapped cell text)
        let mut j = i + 1;
        while j < rows.len() {
            let prev = &segs[rows[j - 1][0]].line;
            let cur = &segs[rows[j][0]].line;
            if cur.baseline - prev.baseline > 3.0 * cur.size.max(prev.size) {
                break;
            }
            if rows[j].len() >= 2 {
                j += 1;
                continue;
            }
            // single-segment rows (wrapped cell text) followed by more rows
            let mut k = j;
            while k < rows.len() && rows[k].len() < 2 && k - j < 6 {
                let a = &segs[rows[k - 1][0]].line;
                let b = &segs[rows[k][0]].line;
                if b.baseline - a.baseline > 2.0 * a.size.max(b.size) {
                    break;
                }
                k += 1;
            }
            if k < rows.len()
                && k > j
                && rows[k].len() >= 2
                && segs[rows[k][0]].line.baseline - segs[rows[k - 1][0]].line.baseline
                    < 2.0 * segs[rows[k][0]].line.size
            {
                j = k + 1;
            } else {
                break;
            }
        }
        let run: Vec<Vec<usize>> = rows[i..j].to_vec();
        i = j;
        out.extend(table_from_run(segs, run));
    }
    out
}

fn is_prose(segs: &[Seg], ids: &[usize]) -> bool {
    let mut n: Vec<usize> = ids.iter().map(|&i| segs[i].chars()).collect();
    n.sort();
    n.get(n.len() / 2).is_some_and(|&m| m > 28)
}

fn table_from_run(segs: &[Seg], run: Vec<Vec<usize>>) -> Vec<(Table, Vec<usize>)> {
    if run.len() < 2 {
        return vec![];
    }
    let all: Vec<usize> = run.iter().flatten().copied().collect();
    let b = bands(segs, all.iter().copied());
    // prose bands (long text in every row) are columns of running text
    let prose: Vec<bool> = (0..b.len())
        .map(|k| {
            let ids: Vec<usize> = all
                .iter()
                .copied()
                .filter(|&i| band_of(&b, &segs[i]) == Some(k))
                .collect();
            let (a, z) = b[k];
            let mut fill: Vec<f32> = ids
                .iter()
                .map(|&i| segs[i].bbox().w() / (z - a).max(1.0))
                .collect();
            fill.sort_by(f32::total_cmp);
            let fill_med = fill.get(fill.len() / 2).copied().unwrap_or(0.0);
            is_prose(segs, &ids) && fill_med > 0.8
        })
        .collect();
    // a long-text description column on the right that every row uses
    // belongs to the table
    let mut prose = prose;
    if let Some(last) = prose.len().checked_sub(1) {
        let multi: Vec<&Vec<usize>> = run.iter().filter(|r| r.len() >= 2).collect();
        let dense = multi
            .iter()
            .filter(|r| r.iter().any(|&i| band_of(&b, &segs[i]) == Some(last)))
            .count();
        if prose[last]
            && prose[..last].iter().all(|p| !p)
            && last >= 1
            && dense * 10 >= multi.len() * 8
        {
            prose[last] = false;
        }
    }
    if prose.iter().any(|&p| p) {
        // drop prose bands, retry on the rest
        let kept: Vec<Vec<usize>> = run
            .iter()
            .map(|r| {
                r.iter()
                    .copied()
                    .filter(|&i| band_of(&b, &segs[i]).is_some_and(|k| !prose[k]))
                    .collect()
            })
            .collect();
        if kept.iter().flatten().count() == all.len() {
            return vec![];
        }
        let mut out = Vec::new();
        let mut cur = Vec::new();
        for r in kept {
            if r.len() >= 2 {
                cur.push(r);
            } else if !cur.is_empty() {
                out.extend(table_from_run(segs, std::mem::take(&mut cur)));
            }
        }
        out.extend(table_from_run(segs, cur));
        return out;
    }
    let ncols = b.len();
    let multi = run.iter().filter(|r| r.len() >= 2).count();
    if ncols < 2 || multi < 2 || (ncols == 2 && multi < 3) {
        return vec![];
    }
    // every row: distinct bands
    for r in &run {
        let mut used = HashSet::new();
        for &i in r {
            match band_of(&b, &segs[i]) {
                Some(k) if used.insert(k) => {}
                _ => return vec![],
            }
        }
    }
    // a "table" whose rows are just one long line split by a gap is prose
    let avg_fill: f32 = run
        .iter()
        .flatten()
        .map(|&i| segs[i].bbox().w())
        .sum::<f32>()
        / (b.iter().map(|(a, z)| z - a).sum::<f32>() * run.len() as f32).max(1.0);
    if ncols == 2 && avg_fill > 0.85 && is_prose(segs, &all) {
        return vec![];
    }
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for r in &run {
        let mut cells: Vec<Option<Line>> = vec![None; ncols];
        for &i in r {
            if let Some(k) = band_of(&b, &segs[i]) {
                cells[k] = Some(segs[i].line.clone());
            }
        }
        // a single-segment row inside the run continues the cell above
        if r.len() == 1 && !rows.is_empty() {
            let k = band_of(&b, &segs[r[0]]).unwrap_or(0);
            let last = rows.last_mut().unwrap();
            if let Some(l) = cells[k].take() {
                last[k].lines.push(l);
            }
            continue;
        }
        rows.push(
            cells
                .into_iter()
                .map(|l| Cell::new(l.into_iter().collect()))
                .collect(),
        );
    }
    // mostly empty grids are scattered text (formulas, labels), not tables
    let cells = rows.len() * ncols;
    let filled = rows
        .iter()
        .flatten()
        .filter(|c| !c.lines.is_empty())
        .count();
    if filled * 10 < cells * 6 {
        return vec![];
    }
    let bbox = all
        .iter()
        .map(|&i| segs[i].bbox())
        .reduce(|a, b| a.union(&b))
        .unwrap_or_default();
    vec![(
        Table {
            rows,
            widths: (0..b.len())
                .map(|k| match b.get(k + 1) {
                    Some(n) => n.0 - b[k].0,
                    None => b[k].1 - b[k].0,
                })
                .collect(),
            bbox,
            ruled: false,
        },
        all,
    )]
}

// ------------------------------------------------------------ ordering

#[derive(Clone, Copy, Debug)]
enum Node {
    Seg(usize),
    Table(usize),
    Pic(usize),
}

/// Recursive XY-cut; emits (node, column bounds) in reading order.
fn xycut(
    items: Vec<(BBox, Node)>,
    col: (f32, f32),
    min_vgap: f32,
    out: &mut Vec<(Node, (f32, f32))>,
    depth: u32,
) {
    if items.len() <= 1 || depth > 400 {
        let mut items = items;
        items.sort_by(|a, b| a.0.y0.total_cmp(&b.0.y0).then(a.0.x0.total_cmp(&b.0.x0)));
        out.extend(items.into_iter().map(|(_, n)| (n, col)));
        return;
    }
    // widest vertical gap (column gutter)
    let mut xs: Vec<(f32, f32)> = items.iter().map(|(b, _)| (b.x0, b.x1)).collect();
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut vbest: Option<(f32, f32)> = None; // (gap width, cut x)
    let mut reach = xs[0].1;
    for &(a, z) in &xs[1..] {
        if a - reach >= min_vgap && vbest.is_none_or(|(w, _)| a - reach > w) {
            vbest = Some((a - reach, (a + reach) / 2.0));
        }
        reach = reach.max(z);
    }
    // horizontal gaps between bands (tall glyph boxes of adjacent lines may
    // touch: small overlaps still separate)
    let mut items = items;
    items.sort_by(|a, b| a.0.y0.total_cmp(&b.0.y0));
    let mut hgaps: Vec<(usize, f32)> = Vec::new(); // (index starting a band, gap)
    let mut reach = items[0].0.y1;
    for (k, it) in items.iter().enumerate().skip(1) {
        let tol = 0.15 * it.0.h().min(20.0);
        if it.0.y0 > reach - tol {
            hgaps.push((k, it.0.y0 - reach));
        }
        reach = reach.max(it.0.y1);
    }
    let hmax = hgaps.iter().map(|g| g.1).fold(f32::MIN, f32::max);
    if let Some((w, cut)) = vbest {
        if hgaps.is_empty() || w > hmax {
            let (l, r): (Vec<_>, Vec<_>) = items.into_iter().partition(|(b, _)| b.cx() < cut);
            for part in [l, r] {
                let x0 = part.iter().map(|(b, _)| b.x0).fold(f32::MAX, f32::min);
                let x1 = part.iter().map(|(b, _)| b.x1).fold(f32::MIN, f32::max);
                xycut(part, (x0, x1), min_vgap, out, depth + 1);
            }
            return;
        }
    }
    if hgaps.is_empty() {
        items.sort_by(|a, c| a.0.y0.total_cmp(&c.0.y0).then(a.0.x0.total_cmp(&c.0.x0)));
        out.extend(items.into_iter().map(|(_, n)| (n, col)));
        return;
    }
    // cut at every horizontal gap wider than the best gutter; without a
    // gutter only at the wide gaps (narrower ones are cut further down,
    // after a gutter inside a band had its chance)
    let thresh = vbest.map_or(hmax * 0.5, |(w, _)| w).min(hmax);
    let cuts: Vec<usize> = hgaps
        .iter()
        .filter(|g| g.1 >= thresh)
        .map(|g| g.0)
        .collect();
    let mut rest = items;
    let mut parts = Vec::new();
    for &c in cuts.iter().rev() {
        parts.push(rest.split_off(c));
    }
    parts.push(rest);
    parts.reverse();
    for p in parts {
        xycut(p, col, min_vgap, out, depth + 1);
    }
}

// ----------------------------------------------------------- paragraphs

fn list_marker(text: &str) -> Option<(bool, String, usize)> {
    let t = text.trim_start();
    let lead = text.len() - t.len();
    let mut chars = t.char_indices();
    let (_, c0) = chars.next()?;
    const BULLETS: &[char] = &[
        '•', '●', '◦', '○', '▪', '■', '□', '‣', '⁃', '–', '—', '-', '*', '·', '➢', '►', '✓', '✔',
        '\u{f0b7}', '\u{f0a7}', '❖',
    ];
    if BULLETS.contains(&c0) {
        let rest = &t[c0.len_utf8()..];
        if rest.starts_with(' ') || (c0 != '-' && c0 != '*' && !rest.is_empty()) {
            let skip = rest.len() - rest.trim_start().len();
            if !rest.trim().is_empty() {
                return Some((false, c0.to_string(), lead + c0.len_utf8() + skip));
            }
        }
        return None;
    }
    // 1.  1)  (1)  a.  a)  (a)  iv.  (iv)
    let paren = t.starts_with('(');
    let body = if paren { &t[1..] } else { t };
    let end = body.find(|c: char| !c.is_alphanumeric())?;
    let tok = &body[..end];
    let after = &body[end..];
    let close = after.chars().next()?;
    let ok_tok = (tok.len() <= 3 && tok.chars().all(|c| c.is_ascii_digit()))
        || (tok.len() == 1 && tok.chars().all(|c| c.is_ascii_lowercase()))
        || (!tok.is_empty() && tok.len() <= 4 && tok.chars().all(|c| "ivxlc".contains(c)));
    let ok_close = if paren {
        close == ')'
    } else {
        close == '.' || close == ')'
    };
    if !ok_tok || !ok_close {
        return None;
    }
    let rest = &after[1..];
    if !rest.starts_with(' ') || rest.trim().is_empty() {
        return None;
    }
    // "3.5 million" / "1. 2. 3." tables aren't lists
    if rest.trim_start().starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let marker_len = (if paren { 1 } else { 0 }) + end + 1;
    let skip = rest.len() - rest.trim_start().len();
    Some((true, t[..marker_len].to_string(), lead + marker_len + skip))
}

/// Strip `n` bytes of text from the start of a line's spans.
fn strip_prefix(line: &mut Line, mut n: usize) {
    for s in &mut line.spans {
        if n == 0 {
            break;
        }
        let k = n.min(s.text.len());
        let k = (0..=k)
            .rev()
            .find(|&i| s.text.is_char_boundary(i))
            .unwrap_or(0);
        s.text.drain(..k);
        n -= k;
        if n > 0 && k == 0 {
            break;
        }
    }
    line.spans.retain(|s| !s.text.is_empty());
}

fn group_paragraphs(lines: Vec<(Line, (f32, f32))>) -> Vec<Para> {
    let mut paras: Vec<(Vec<Line>, (f32, f32), Option<(bool, String, f32)>)> = Vec::new();
    for (line, col) in lines {
        let text = line.text();
        let marker = list_marker(&text);
        let start_new = match paras.last() {
            None => true,
            Some((pl, pcol, item)) => {
                let prev = pl.last().unwrap();
                let pitch = line.baseline - prev.baseline;
                let size = line.size.max(prev.size);
                let colw = (pcol.1 - pcol.0).max(1.0);
                let prev_short = prev.bbox.x1 < pcol.1 - (4.0 * size).max(0.25 * colw)
                    && prev.text().trim_end().ends_with(['.', '!', '?', ':', ';']);
                let indented = line.bbox.x0 - col.0 > 0.8 * size
                    && (prev.bbox.x0 - pcol.0) < 0.3 * size
                    && item.is_none();
                let continues_item = item
                    .as_ref()
                    .is_some_and(|(_, _, tx)| (line.bbox.x0 - tx).abs() < 0.8 * size);
                *pcol != col
                    || (prev.all_mono() && line.all_mono())
                    || is_leader(&text)
                    || is_leader(&prev.text())
                    || pitch < 0.0
                    || pitch > 1.75 * size
                    || (line.size - prev.size).abs() > 0.12 * size
                    || prev.all_bold() != line.all_bold()
                    || marker.is_some()
                    || (item.is_some() && !continues_item)
                    || prev_short
                    || indented
            }
        };
        if start_new {
            let item = marker.as_ref().map(|(o, m, n)| {
                // x where the item text starts (for continuation lines)
                let mut probe = line.clone();
                strip_prefix(&mut probe, *n);
                let tx = probe.spans.first().map_or(line.bbox.x0, |s| s.bbox.x0);
                (*o, m.clone(), tx)
            });
            let mut l = line;
            if let Some((_, _, n)) = &marker {
                strip_prefix(&mut l, *n);
            }
            paras.push((vec![l], col, item));
        } else {
            paras.last_mut().unwrap().0.push(line);
        }
    }
    // list levels by indentation within each run of consecutive items
    let mut level_of = vec![0u8; paras.len()];
    let mut k = 0;
    while k < paras.len() {
        if paras[k].2.is_none() {
            k += 1;
            continue;
        }
        let start = k;
        while k < paras.len() && paras[k].2.is_some() {
            k += 1;
        }
        let xs: Vec<f32> = paras[start..k].iter().map(|p| p.0[0].bbox.x0).collect();
        let levels = cluster_vals(xs.clone(), 4.0);
        for (j, x) in xs.iter().enumerate() {
            level_of[start + j] = levels
                .iter()
                .position(|&lx| (lx - x).abs() <= 4.0)
                .unwrap_or(0)
                .min(3) as u8;
        }
    }
    paras
        .into_iter()
        .enumerate()
        .map(|(pi, (lines, col, item))| {
            let bbox = lines
                .iter()
                .map(|l| l.bbox)
                .reduce(|a, b| a.union(&b))
                .unwrap_or_default();
            let kind = match item {
                Some((ordered, marker, _)) => Kind::List {
                    ordered,
                    marker,
                    level: level_of[pi],
                },
                None => Kind::Body,
            };
            let align = alignment(&lines, col);
            Para {
                lines,
                kind,
                align,
                bbox,
            }
        })
        .collect()
}

/// table-of-contents line: "Title . . . . . 12"
fn is_leader(t: &str) -> bool {
    t.contains(". . . .") || t.contains("....") || t.contains("\u{2026}\u{2026}")
}

fn alignment(lines: &[Line], col: (f32, f32)) -> Align {
    let size = lines.iter().map(|l| l.size).fold(0.0f32, f32::max).max(1.0);
    let tol = 0.6 * size;
    let colw = col.1 - col.0;
    if lines.len() == 1 {
        let l = &lines[0];
        let left = l.bbox.x0 - col.0;
        let right = col.1 - l.bbox.x1;
        if l.bbox.w() < 0.85 * colw && left > 2.0 * size && (left - right).abs() < 1.5 * size {
            return Align::Center;
        }
        if left > 0.3 * colw && right < tol {
            return Align::Right;
        }
        return Align::Left;
    }
    let lefts_eq = lines
        .iter()
        .all(|l| (l.bbox.x0 - lines[0].bbox.x0).abs() < tol);
    let body = &lines[..lines.len() - 1];
    let rights_eq = body
        .iter()
        .all(|l| (l.bbox.x1 - body[0].bbox.x1).abs() < tol)
        && (lines.len() > 2 || (lines[0].bbox.x1 - col.1).abs() < tol);
    let centers_eq = lines
        .iter()
        .all(|l| (l.bbox.cx() - lines[0].bbox.cx()).abs() < tol);
    if lefts_eq && rights_eq && lines.len() >= 3 {
        Align::Justify
    } else if !lefts_eq && centers_eq {
        Align::Center
    } else if !lefts_eq
        && lines
            .iter()
            .all(|l| (l.bbox.x1 - lines[0].bbox.x1).abs() < tol)
    {
        Align::Right
    } else {
        Align::Left
    }
}

fn analyze_page(
    number: usize,
    width: f32,
    height: f32,
    c: Captured,
    pics: Vec<Picture>,
    body_size: f32,
) -> PageLayout {
    let Captured {
        mut glyphs,
        fonts,
        rules,
        ..
    } = c;
    dedupe(&mut glyphs);
    // 1. ruled tables
    let mut tables: Vec<Table> = Vec::new();
    for grid in find_grids(&rules) {
        let (inside, rest): (Vec<G>, Vec<G>) = glyphs.into_iter().partition(|g| {
            !g.rotated
                && grid
                    .bbox
                    .contains((g.x0 + g.x1) / 2.0, (g.top() + g.bottom()) / 2.0)
        });
        glyphs = rest;
        match grid_table(&grid, inside.clone(), &fonts) {
            Some(t) => tables.push(t),
            None => glyphs.extend(inside),
        }
    }
    // 2. segments
    let segs = build_segments(glyphs, &fonts, true);
    let all: Vec<usize> = (0..segs.len()).collect();
    // 3. text tables
    let mut used = HashSet::new();
    for (t, members) in find_text_tables(&segs, &all) {
        used.extend(members);
        tables.push(t);
    }
    // 4. reading order
    let mut items: Vec<(BBox, Node)> = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        if !used.contains(&i) {
            items.push((s.bbox(), Node::Seg(i)));
        }
    }
    for (i, t) in tables.iter().enumerate() {
        items.push((t.bbox, Node::Table(i)));
    }
    for (i, p) in pics.iter().enumerate() {
        // images under text (backgrounds, scans) don't take part in cutting
        items.push((p.bbox, Node::Pic(i)));
    }
    let full = (0.0, width);
    let mut order = Vec::new();
    let (bg, fg): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|(b, n)| matches!(n, Node::Pic(_)) && b.w() * b.h() > 0.6 * width * height);
    let min_vgap = (0.8 * body_size).max(6.0);
    let col_all = fg
        .iter()
        .map(|(b, _)| (b.x0, b.x1))
        .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
        .unwrap_or(full);
    for (_, n) in bg {
        order.push((n, full));
    }
    xycut(fg, col_all, min_vgap, &mut order, 0);

    // 5. lines → paragraphs, interleaved with tables and pictures
    let mut out: Vec<Item> = Vec::new();
    let mut pending: Vec<(Line, (f32, f32))> = Vec::new();
    let mut tables: Vec<Option<Table>> = tables.into_iter().map(Some).collect();
    let mut pics: Vec<Option<Picture>> = pics.into_iter().map(Some).collect();
    let flush = |pending: &mut Vec<(Line, (f32, f32))>, out: &mut Vec<Item>| {
        if !pending.is_empty() {
            out.extend(
                group_paragraphs(std::mem::take(pending))
                    .into_iter()
                    .map(Item::Para),
            );
        }
    };
    for (node, col) in order {
        match node {
            Node::Seg(i) => {
                let s = &segs[i];
                if s.rotated {
                    flush(&mut pending, &mut out);
                    pending.push((s.line.clone(), (s.bbox().x0, s.bbox().x1)));
                    flush(&mut pending, &mut out);
                    continue;
                }
                // same visual line as the previous segment → join
                if let Some((prev, pcol)) = pending.last_mut() {
                    if *pcol == col
                        && (prev.baseline - s.line.baseline).abs()
                            < 0.35 * prev.size.max(s.line.size)
                        && s.bbox().x0 >= prev.bbox.x1 - 1.0
                    {
                        if let Some(l) = prev.spans.last_mut() {
                            l.text.push(' ');
                        }
                        prev.spans.extend(s.line.spans.iter().cloned());
                        prev.bbox = prev.bbox.union(&s.bbox());
                        prev.size = dominant_size(prev.spans.iter());
                        continue;
                    }
                }
                pending.push((s.line.clone(), col));
            }
            Node::Table(i) => {
                flush(&mut pending, &mut out);
                if let Some(t) = tables[i].take() {
                    out.push(Item::Table(t));
                }
            }
            Node::Pic(i) => {
                flush(&mut pending, &mut out);
                if let Some(p) = pics[i].take() {
                    out.push(Item::Picture(p));
                }
            }
        }
    }
    flush(&mut pending, &mut out);
    out.retain(|i| match i {
        Item::Para(p) => !p.text().trim().is_empty(),
        _ => true,
    });
    PageLayout {
        number,
        width,
        height,
        items: out,
    }
}

/// Headings across the whole document: larger-than-body short paragraphs
/// (levels by distinct size) and short all-bold lines.
fn assign_headings(pages: &mut [PageLayout], body: f32) {
    let is_candidate = |p: &Para| {
        let t = p.text();
        let n = t.chars().count();
        n > 0 && n <= 200 && p.lines.len() <= 3 && t.chars().any(char::is_alphabetic)
    };
    let mut sizes: Vec<f32> = Vec::new();
    for pg in pages.iter() {
        for it in &pg.items {
            if let Item::Para(p) = it {
                let s = p.size();
                if s >= body * 1.15 && is_candidate(p) {
                    sizes.push(s);
                }
            }
        }
    }
    let mut levels = cluster_vals(sizes, 0.6);
    levels.reverse(); // largest first
    let size_levels = levels.len().min(3);
    for pg in pages.iter_mut() {
        let n = pg.items.len();
        for k in 0..n {
            let next_is_body = match pg.items.get(k + 1) {
                Some(Item::Para(q)) => !q.lines.iter().all(|l| l.all_bold()),
                _ => false,
            };
            let Item::Para(p) = &mut pg.items[k] else {
                continue;
            };
            if matches!(p.kind, Kind::List { .. }) && p.size() < body * 1.15 {
                continue;
            }
            if !is_candidate(p) {
                continue;
            }
            let s = p.size();
            if s >= body * 1.15 {
                let idx = levels
                    .iter()
                    .position(|&l| (l - s).abs() <= 0.6)
                    .unwrap_or(levels.len().saturating_sub(1));
                restore_marker(p);
                p.kind = Kind::Heading((idx + 1).min(3) as u8);
            } else if p.lines.len() == 1
                && p.lines[0].all_bold()
                && (s - body).abs() < 0.12 * body + 0.5
                && next_is_body
                && !p.text().trim_end().ends_with(['.', ',', ';'])
                && p.text().chars().count() <= 80
            {
                restore_marker(p);
                p.kind = Kind::Heading(((size_levels + 1).min(3)) as u8);
            }
        }
    }
}

/// A heading that looked like a numbered list item ("1. Introduction")
/// keeps its number.
fn restore_marker(p: &mut Para) {
    if let Kind::List { marker, .. } = &p.kind {
        let m = marker.clone();
        if let Some(s) = p.lines.first_mut().and_then(|l| l.spans.first_mut()) {
            s.text = format!("{m} {}", s.text);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn font_names() {
        let f = font_from_name("ABCDEF+TimesNewRomanPS-BoldItalicMT");
        assert_eq!(&*f.family, "Times New Roman");
        assert!(f.bold && f.italic);
        let f = font_from_name("Helvetica-Oblique");
        assert_eq!(&*f.family, "Arial");
        assert!(!f.bold && f.italic);
        let f = font_from_name("Arial,Bold");
        assert!(f.bold);
        let f = font_from_name("Courier");
        assert!(f.mono);
        assert_eq!(&*font_from_name("CMR10").family, "CMR10");
        assert!(font_from_name("CMBX12").bold);
        assert!(!font_from_name("Lato-Light").italic);
        assert_eq!(&*font_from_name("Calibri").family, "Calibri");
    }

    #[test]
    fn markers() {
        assert_eq!(list_marker("• Item").map(|m| m.0), Some(false));
        assert_eq!(list_marker("1. First").map(|m| m.1), Some("1.".into()));
        assert_eq!(list_marker("(iv) roman").map(|m| m.1), Some("(iv)".into()));
        assert_eq!(list_marker("a) letter").map(|m| m.0), Some(true));
        assert!(list_marker("3.5 million").is_none());
        assert!(list_marker("-5 degrees").is_none());
        assert!(list_marker("Hello").is_none());
        let (_, _, n) = list_marker("1. First").unwrap();
        assert_eq!(&"1. First"[n..], "First");
    }

    #[test]
    fn hyphen_join() {
        let mk = |t: &str| Line {
            spans: vec![Span {
                text: t.into(),
                font: "Arial".into(),
                size: 10.0,
                bold: false,
                italic: false,
                mono: false,
                color: [0; 3],
                invisible: false,
                bbox: BBox::default(),
            }],
            bbox: BBox::default(),
            baseline: 0.0,
            size: 10.0,
        };
        let r = join_lines(&[mk("extrac-"), mk("tion works "), mk("well")]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "extraction works well");
        let r = join_lines(&[mk("well-"), mk("Known")]);
        assert_eq!(r[0].text, "well- Known");
    }

    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream as LStream};

    fn op(o: &str, args: Vec<Object>) -> Operation {
        Operation::new(o, args)
    }
    fn n(v: f32) -> Object {
        Object::Real(v)
    }
    fn txt(font: &str, size: f32, x: f32, y: f32, s: &str) -> Vec<Operation> {
        vec![
            op("BT", vec![]),
            op("Tf", vec![Object::Name(font.as_bytes().to_vec()), n(size)]),
            op("Td", vec![n(x), n(y)]),
            op(
                "Tj",
                vec![Object::String(
                    s.as_bytes().to_vec(),
                    lopdf::StringFormat::Literal,
                )],
            ),
            op("ET", vec![]),
        ]
    }
    fn line(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Operation> {
        vec![
            op("m", vec![n(x0), n(y0)]),
            op("l", vec![n(x1), n(y1)]),
            op("S", vec![]),
        ]
    }

    /// Three pages: (1) heading, paragraphs, bullet list, ruled table,
    /// image; (2) two text columns + an unruled table; (3) a CID font
    /// (Identity-H + ToUnicode, incl. a ligature).
    pub(crate) fn fixture() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let f1 = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Times-Roman", "Encoding" => "WinAnsiEncoding" });
        let f2 = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold", "Encoding" => "WinAnsiEncoding" });
        let f4 = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
        let cmap = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n4 beginbfchar\n<0001> <0048>\n<0002> <0069>\n<0003> <00660069>\n<0004> <0020>\nendbfchar\nendcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n".to_vec();
        let tu = doc.add_object(LStream::new(dictionary! {}, cmap));
        let desc = doc.add_object(dictionary! { "Type" => "FontDescriptor", "FontName" => "Arial-BoldMT", "Flags" => 32, "FontBBox" => vec![0.into(), (-200).into(), 1000.into(), 900.into()], "ItalicAngle" => 0, "Ascent" => 900, "Descent" => -200, "CapHeight" => 700, "StemV" => 80 });
        let cid = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "Arial-BoldMT", "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 }, "FontDescriptor" => desc, "DW" => 500, "W" => vec![1.into(), vec![722.into(), 278.into(), 556.into(), 278.into()].into()] });
        let f3 = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "Arial-BoldMT", "Encoding" => "Identity-H", "DescendantFonts" => vec![cid.into()], "ToUnicode" => tu });
        // 16×16 RGB gradient, Flate
        let mut raw = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                raw.extend_from_slice(&[x * 16, y * 16, 128]);
            }
        }
        let mut img = LStream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 16, "Height" => 16, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 },
            raw,
        );
        img.compress().unwrap();
        let im = doc.add_object(img);
        let resources = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => f1, "F2" => f2, "F3" => f3, "F4" => f4 },
            "XObject" => dictionary! { "Im1" => im },
        });

        // page 1
        let mut p1 = Vec::new();
        p1.extend(txt("F2", 20.0, 72.0, 720.0, "Quarterly Report"));
        let para1 = [
            "The first paragraph of the report explains the",
            "results of the quarter in some detail so that",
            "the extraction sees a multi-line paragraph.",
        ];
        for (k, l) in para1.iter().enumerate() {
            p1.extend(txt("F1", 11.0, 72.0, 690.0 - 14.0 * k as f32, l));
        }
        p1.extend(txt(
            "F1",
            11.0,
            72.0,
            640.0,
            "A second paragraph follows after a gap.",
        ));
        for (k, l) in ["\u{95} First item", "\u{95} Second item"]
            .iter()
            .enumerate()
        {
            let bytes: Vec<u8> = l.chars().map(|c| c as u32 as u8).collect();
            let s = String::from_utf8_lossy(&bytes).to_string();
            let _ = s;
            p1.push(op("BT", vec![]));
            p1.push(op("Tf", vec!["F1".into(), n(11.0)]));
            p1.push(op("Td", vec![n(72.0), n(612.0 - 14.0 * k as f32)]));
            p1.push(op(
                "Tj",
                vec![Object::String(bytes, lopdf::StringFormat::Literal)],
            ));
            p1.push(op("ET", vec![]));
        }
        // ruled table 3 cols × 4 rows: x 72,172,272,372; y 560..480
        p1.push(op("w", vec![n(0.5)]));
        for k in 0..5 {
            let y = 560.0 - 20.0 * k as f32;
            p1.extend(line(72.0, y, 372.0, y));
        }
        for x in [72.0, 172.0, 272.0, 372.0] {
            p1.extend(line(x, 560.0, x, 480.0));
        }
        let cells = [
            ["Item", "Qty", "Price"],
            ["Apples", "3", "1.20"],
            ["Pears", "12", "0.80"],
            ["Plums", "7", "2.50"],
        ];
        for (r, row) in cells.iter().enumerate() {
            for (c, t) in row.iter().enumerate() {
                let f = if r == 0 { "F2" } else { "F1" };
                p1.extend(txt(
                    f,
                    10.0,
                    76.0 + 100.0 * c as f32,
                    546.0 - 20.0 * r as f32,
                    t,
                ));
            }
        }
        // image at (400, 600) 100×100
        p1.push(op("q", vec![]));
        p1.push(op(
            "cm",
            vec![n(100.0), n(0.0), n(0.0), n(100.0), n(400.0), n(600.0)],
        ));
        p1.push(op("Do", vec!["Im1".into()]));
        p1.push(op("Q", vec![]));

        // page 2: two columns, then an unruled table
        let mut p2 = Vec::new();
        p2.extend(txt("F2", 16.0, 72.0, 740.0, "Two Columns"));
        for k in 0..8 {
            let y = 700.0 - 13.0 * k as f32;
            p2.extend(txt(
                "F1",
                10.0,
                72.0,
                y,
                &format!("Left column sentence number {k} goes here"),
            ));
            p2.extend(txt(
                "F1",
                10.0,
                320.0,
                y,
                &format!("Right column sentence number {k} goes here"),
            ));
        }
        let rows = [
            ["Name", "Count", "Total"],
            ["Alpha", "1", "10.00"],
            ["Beta", "22", "220.00"],
            ["Gamma", "333", "3,330.00"],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, t) in row.iter().enumerate() {
                p2.extend(txt(
                    "F4",
                    10.0,
                    72.0 + 130.0 * c as f32,
                    520.0 - 14.0 * r as f32,
                    t,
                ));
            }
        }

        // page 3: CID text "Hi fi" via Identity-H
        let p3 = vec![
            op("BT", vec![]),
            op("Tf", vec!["F3".into(), n(14.0)]),
            op("Td", vec![n(72.0), n(700.0)]),
            op(
                "Tj",
                vec![Object::String(
                    vec![0, 1, 0, 2, 0, 4, 0, 3],
                    lopdf::StringFormat::Hexadecimal,
                )],
            ),
            op("ET", vec![]),
        ];

        let mut kids: Vec<Object> = Vec::new();
        for ops in [p1, p2, p3] {
            let c = doc.add_object(LStream::new(
                dictionary! {},
                Content { operations: ops }.encode().unwrap(),
            ));
            kids.push(doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c, "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] }).into());
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 3 }),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    pub(crate) fn dump(l: &Layout) -> String {
        let mut s = String::new();
        for p in &l.pages {
            s += &format!("== page {}\n", p.number);
            for it in &p.items {
                match it {
                    Item::Para(pa) => {
                        s += &format!("{:?} {:?}: {}\n", pa.kind, pa.align, pa.text())
                    }
                    Item::Table(t) => {
                        s += &format!("TABLE ruled={}\n", t.ruled);
                        for r in &t.rows {
                            s += &format!(
                                "  | {}\n",
                                r.iter().map(|c| c.text()).collect::<Vec<_>>().join(" | ")
                            );
                        }
                    }
                    Item::Picture(pi) => {
                        s += &format!("PICTURE {} {:?} {:?}\n", pi.ext, pi.px, pi.bbox)
                    }
                }
            }
        }
        s
    }

    #[test]
    fn fixture_layout() {
        let l = extract(&fixture(), Options { images: true }).unwrap();
        let d = dump(&l);
        eprintln!("{d}");
        if let Ok(p) = std::env::var("OGX_PDF") {
            let l = extract(&std::fs::read(p).unwrap(), Options { images: true }).unwrap();
            eprintln!("{}", dump(&l));
        }
    }
}
