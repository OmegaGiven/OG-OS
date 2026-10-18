//! The intermediate document every input is converted to, and its
//! translation to Typst source.
//!
//! All user text reaches Typst as string literals (`#"…"`), never as
//! markup, so nothing in a document can be interpreted as Typst code; the
//! only escaping needed is that of [`lit`].

use super::engine;
use std::collections::HashMap;
use std::fmt::Write as _;

pub type Rgb = [u8; 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Face {
    #[default]
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub sup: bool,
    pub sub: bool,
    /// absolute size, points
    pub size: Option<f32>,
    pub color: Option<Rgb>,
    pub highlight: Option<Rgb>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String, Style),
    Link(String, Vec<Inline>),
    Image(Img),
    Break,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Col {
    Auto,
    Pt(f32),
    Fr(f32),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cell {
    pub blocks: Vec<Block>,
    pub colspan: u16,
    pub rowspan: u16,
    pub fill: Option<Rgb>,
}

impl Cell {
    pub fn text(s: impl Into<String>, style: Style) -> Cell {
        Cell {
            blocks: vec![Block::para(vec![Inline::Text(s.into(), style)])],
            colspan: 1,
            rowspan: 1,
            fill: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    /// empty = as many `auto` columns as the widest row needs
    pub cols: Vec<Col>,
    /// leading rows repeated on every page
    pub header: usize,
    pub rows: Vec<Vec<Cell>>,
    /// text size inside the table, points
    pub size: Option<f32>,
    pub borders: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Para {
    pub inl: Vec<Inline>,
    pub align: Align,
    /// left indent, points
    pub indent: f32,
    /// space above / below, points (None = default paragraph spacing)
    pub above: Option<f32>,
    pub below: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(u8, Vec<Inline>),
    Para(Para),
    /// list paragraph with a precomputed marker ("•", "3.", "a)")
    Labeled {
        label: String,
        indent: f32,
        inl: Vec<Inline>,
    },
    List {
        ordered: bool,
        start: u32,
        items: Vec<Vec<Block>>,
    },
    /// preformatted text; `shaded` = code block look
    Code {
        text: String,
        shaded: bool,
    },
    Quote(Vec<Block>),
    Indent(f32, Vec<Block>),
    Table(Table),
    Image(Img, Align),
    Rule,
    PageBreak,
    /// muted italic line (skipped content, attachment list…)
    Note(String),
    /// absolutely positioned box on the current page (slides), points
    Place {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<Rgb>,
        /// vertically centre the content
        middle: bool,
        blocks: Vec<Block>,
    },
    /// a page of its own (slides): background colour and content
    Page {
        fill: Option<Rgb>,
        blocks: Vec<Block>,
    },
}

impl Block {
    pub fn para(inl: Vec<Inline>) -> Block {
        Block::Para(Para {
            inl,
            ..Default::default()
        })
    }

    pub fn plain(s: impl Into<String>) -> Block {
        Block::para(vec![Inline::Text(s.into(), Style::default())])
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Page {
    pub w: f32,
    pub h: f32,
    /// top, right, bottom, left (points)
    pub margin: [f32; 4],
}

impl Page {
    pub const A4: Page = Page {
        w: 595.28,
        h: 841.89,
        margin: [56.7, 56.7, 56.7, 56.7],
    };

    pub fn landscape(self) -> Page {
        Page {
            w: self.w.max(self.h),
            h: self.w.min(self.h),
            ..self
        }
    }

    pub fn body_w(&self) -> f32 {
        (self.w - self.margin[1] - self.margin[3]).max(36.0)
    }

    pub fn body_h(&self) -> f32 {
        (self.h - self.margin[0] - self.margin[2]).max(36.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Img {
    /// virtual path inside the Typst world ("/f/3.png")
    pub path: String,
    /// natural size, points (96 dpi for pixels)
    pub w: f32,
    pub h: f32,
}

/// Image files referenced by a document. Every image is validated (and
/// non-Typst formats re-encoded to PNG) before it gets a path, so a broken
/// image can never fail the whole conversion.
#[derive(Default)]
pub struct Files {
    list: Vec<(String, Vec<u8>)>,
    used: usize,
    seen: HashMap<u64, Img>,
}

/// total image bytes one document may embed
const IMAGE_BUDGET: usize = 512 * 1024 * 1024;
const MAX_PIXELS: u64 = 120_000_000;

fn hash(b: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

pub fn looks_like_svg(b: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&b[..b.len().min(4096)]).to_ascii_lowercase();
    head.contains("<svg")
}

/// Natural size of an SVG in points (user units are CSS pixels).
pub fn svg_size(b: &[u8]) -> Option<(f32, f32)> {
    let tree = usvg::Tree::from_data(b, &usvg::Options::default()).ok()?;
    let s = tree.size();
    Some((s.width() * 0.75, s.height() * 0.75))
}

impl Files {
    pub fn add(&mut self, bytes: Vec<u8>) -> Option<Img> {
        if bytes.is_empty() {
            return None;
        }
        let key = hash(&bytes);
        if let Some(img) = self.seen.get(&key) {
            return Some(img.clone());
        }
        let (ext, bytes, w, h) = if looks_like_svg(&bytes) {
            let (w, h) = svg_size(&bytes)?;
            ("svg", bytes, w, h)
        } else {
            use image::ImageFormat as F;
            let rd = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .ok()?;
            let fmt = rd.format()?;
            let (pw, ph) = rd.into_dimensions().ok()?;
            if pw == 0 || ph == 0 || u64::from(pw) * u64::from(ph) > MAX_PIXELS {
                return None;
            }
            let (w, h) = (pw as f32 * 0.75, ph as f32 * 0.75);
            match fmt {
                F::Png => ("png", bytes, w, h),
                F::Jpeg => ("jpg", bytes, w, h),
                F::Gif => ("gif", bytes, w, h),
                F::WebP => ("webp", bytes, w, h),
                _ => {
                    let img = image::load_from_memory(&bytes).ok()?;
                    let mut png = Vec::new();
                    img.write_to(&mut std::io::Cursor::new(&mut png), F::Png)
                        .ok()?;
                    ("png", png, w, h)
                }
            }
        };
        if self.used + bytes.len() > IMAGE_BUDGET {
            return None;
        }
        self.used += bytes.len();
        let path = format!("/f/{}.{ext}", self.list.len());
        self.list.push((path.clone(), bytes));
        let img = Img { path, w, h };
        self.seen.insert(key, img.clone());
        Some(img)
    }

    pub fn into_list(self) -> Vec<(String, Vec<u8>)> {
        self.list
    }
}

pub struct Doc {
    pub title: Option<String>,
    pub page: Page,
    pub face: Face,
    /// base text size, points
    pub size: f32,
    pub justify: bool,
    pub blocks: Vec<Block>,
    pub files: Files,
}

impl Doc {
    pub fn new(page: Page) -> Doc {
        Doc {
            title: None,
            page,
            face: Face::Sans,
            size: 11.0,
            justify: false,
            blocks: Vec::new(),
            files: Files::default(),
        }
    }

    pub fn to_pdf(self) -> Result<Vec<u8>, String> {
        let src = self.source();
        engine::compile(src, self.files.into_list())
    }

    /// The Typst source for this document.
    pub fn source(&self) -> String {
        let mut e = Emit {
            out: String::with_capacity(4096),
            max_h: self.page.body_h(),
        };
        let o = &mut e.out;
        if let Some(t) = self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            let _ = writeln!(o, "#set document(title: {})", lit(t));
        }
        let p = self.page;
        let _ = writeln!(
            o,
            "#set page(width: {}, height: {}, margin: (top: {}, right: {}, bottom: {}, left: {}))",
            pt(p.w),
            pt(p.h),
            pt(p.margin[0]),
            pt(p.margin[1]),
            pt(p.margin[2]),
            pt(p.margin[3])
        );
        let family = match self.face {
            Face::Sans => engine::SANS,
            Face::Serif => engine::SERIF,
            Face::Mono => engine::MONO,
        };
        let _ = writeln!(
            o,
            "#set text(font: ({}, {}, {}), size: {}, lang: \"en\")",
            lit(family),
            lit(engine::SANS),
            lit(engine::SERIF),
            pt(self.size)
        );
        let _ = writeln!(
            o,
            "#set par(justify: {}, leading: 0.55em, spacing: 0.9em)",
            self.justify
        );
        let _ = writeln!(
            o,
            "#show raw: set text(font: ({}, {}), size: 0.95em)",
            lit(engine::MONO),
            lit(engine::SANS)
        );
        o.push_str("#show heading: set block(above: 1.1em, below: 0.6em)\n");
        o.push_str("#show link: set text(fill: rgb(26, 95, 180))\n");
        o.push_str("#set table(inset: 4pt)\n");
        // image at natural width, shrunk to the container
        o.push_str(
            "#let ogimg(p, w) = layout(s => image(p, width: if s.width < w { s.width } else { w }))\n",
        );
        for b in &self.blocks {
            e.block(b);
        }
        e.out
    }
}

/// Typst string literal.
pub fn lit(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            '\t' => o.push_str("    "),
            c if c.is_control() => {
                let _ = write!(o, "\\u{{{:x}}}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn pt(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    format!("{:.2}pt", v)
}

fn rgb(c: Rgb) -> String {
    format!("rgb({}, {}, {})", c[0], c[1], c[2])
}

struct Emit {
    out: String,
    max_h: f32,
}

impl Emit {
    fn blocks(&mut self, bs: &[Block]) {
        for b in bs {
            self.block(b);
        }
    }

    /// `[…]` content block holding `bs`.
    fn content(&mut self, bs: &[Block]) {
        self.out.push('[');
        self.blocks(bs);
        self.out.push(']');
    }

    fn inline_content(&mut self, inl: &[Inline]) {
        self.out.push('[');
        self.inlines(inl);
        self.out.push(']');
    }

    fn block(&mut self, b: &Block) {
        match b {
            Block::Heading(level, inl) => {
                let _ = write!(self.out, "#heading(level: {})", (*level).clamp(1, 6));
                self.inline_content(inl);
                self.out.push_str("\n\n");
            }
            Block::Para(p) => self.para(p),
            Block::Labeled { label, indent, inl } => {
                let _ = write!(
                    self.out,
                    "#pad(left: {})[#grid(columns: (auto, 1fr), column-gutter: 0.45em, [#{}], [",
                    pt(*indent),
                    lit(label)
                );
                self.inlines(inl);
                self.out.push_str("])]\n\n");
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                if *ordered {
                    let _ = write!(self.out, "#enum(start: {}", (*start).max(1));
                } else {
                    self.out.push_str("#list(");
                }
                for (k, it) in items.iter().enumerate() {
                    if *ordered || k > 0 {
                        self.out.push_str(", ");
                    }
                    self.content(it);
                }
                self.out.push_str(")\n\n");
            }
            Block::Code { text, shaded } => {
                if *shaded {
                    self.out.push_str(
                        "#block(fill: luma(244), inset: 6pt, radius: 2pt, width: 100%)[#raw(block: true, ",
                    );
                    self.out.push_str(&lit(text));
                    self.out.push_str(")]\n\n");
                } else {
                    self.out.push_str("#raw(block: true, ");
                    self.out.push_str(&lit(text));
                    self.out.push_str(")\n\n");
                }
            }
            Block::Quote(bs) => {
                self.out.push_str(
                    "#block(inset: (left: 10pt, y: 2pt), stroke: (left: 2pt + luma(190)))",
                );
                self.content(bs);
                self.out.push_str("\n\n");
            }
            Block::Indent(by, bs) => {
                let _ = write!(self.out, "#pad(left: {})", pt(*by));
                self.content(bs);
                self.out.push_str("\n\n");
            }
            Block::Table(t) => self.table(t),
            Block::Image(img, align) => {
                let a = match align {
                    Align::Center => "center",
                    Align::Right => "right",
                    _ => "left",
                };
                let _ = write!(self.out, "#align({a})[#");
                self.image(img);
                self.out.push_str("]\n\n");
            }
            Block::Rule => {
                self.out
                    .push_str("#line(length: 100%, stroke: 0.6pt + luma(170))\n\n");
            }
            Block::PageBreak => self.out.push_str("#pagebreak(weak: true)\n\n"),
            Block::Note(s) => {
                let _ = write!(
                    self.out,
                    "#text(fill: luma(110), style: \"italic\")[#{}]\n\n",
                    lit(s)
                );
            }
            Block::Place {
                x,
                y,
                w,
                h,
                fill,
                middle,
                blocks,
            } => {
                let _ = write!(
                    self.out,
                    "#place(top + left, dx: {}, dy: {})[#block(width: {}, height: {}, inset: 3pt, clip: true",
                    pt(*x),
                    pt(*y),
                    pt(w.max(1.0)),
                    pt(h.max(1.0))
                );
                if let Some(c) = fill {
                    let _ = write!(self.out, ", fill: {}", rgb(*c));
                }
                self.out.push_str(")[#set par(spacing: 0.4em)\n");
                if *middle {
                    self.out.push_str("#align(horizon)");
                    self.content(blocks);
                } else {
                    self.blocks(blocks);
                }
                self.out.push_str("]]\n");
            }
            Block::Page { fill, blocks } => {
                match fill {
                    Some(c) => {
                        let _ = write!(self.out, "#page(fill: {})", rgb(*c));
                    }
                    None => self.out.push_str("#page"),
                }
                self.content(blocks);
                self.out.push_str("\n\n");
            }
        }
    }

    fn para(&mut self, p: &Para) {
        if p.inl.is_empty() {
            // empty paragraph = vertical gap
            self.out.push_str("#v(0.6em)\n\n");
            return;
        }
        let mut close = 0;
        // line height and spacing follow the paragraph's largest run (a
        // 24pt line in an 11pt paragraph would otherwise overlap its
        // neighbours)
        let biggest = p
            .inl
            .iter()
            .filter_map(|i| match i {
                Inline::Text(t, st) if !t.trim().is_empty() => st.size,
                _ => None,
            })
            .filter(|s| s.is_finite() && *s > 0.5)
            .fold(None, |m: Option<f32>, s| Some(m.map_or(s, |m| m.max(s))));
        if let Some(sz) = biggest {
            let _ = write!(self.out, "#[#set text(size: {})\n", pt(sz.min(400.0)));
            close += 1;
        }
        if p.above.is_some() || p.below.is_some() {
            self.out.push_str("#block(");
            let mut sep = "";
            if let Some(a) = p.above {
                let _ = write!(self.out, "above: {}", pt(a));
                sep = ", ";
            }
            if let Some(b) = p.below {
                let _ = write!(self.out, "{sep}below: {}", pt(b));
            }
            self.out.push_str(")[");
            close += 1;
        }
        if p.indent > 0.0 {
            let _ = write!(self.out, "#pad(left: {})[", pt(p.indent));
            close += 1;
        }
        match p.align {
            Align::Left => {}
            Align::Center => {
                self.out.push_str("#align(center)[");
                close += 1;
            }
            Align::Right => {
                self.out.push_str("#align(right)[");
                close += 1;
            }
            Align::Justify => {
                self.out.push_str("#par(justify: true)[");
                close += 1;
            }
        }
        self.inlines(&p.inl);
        for _ in 0..close {
            self.out.push(']');
        }
        self.out.push_str("\n\n");
    }

    /// code-mode expression (no leading `#`)
    fn image(&mut self, img: &Img) {
        // never taller than a page body
        let mut w = img.w;
        if img.h > self.max_h && img.h > 0.0 {
            w = img.w * self.max_h / img.h;
        }
        let _ = write!(self.out, "ogimg({}, {})", lit(&img.path), pt(w.max(1.0)));
    }

    fn inlines(&mut self, inl: &[Inline]) {
        for i in inl {
            match i {
                Inline::Text(s, st) => self.text(s, st),
                Inline::Break => self.out.push_str("#linebreak()"),
                Inline::Link(url, inner) => {
                    let _ = write!(self.out, "#link({})", lit(url));
                    if inner.is_empty() {
                        self.out.push(';');
                    } else {
                        self.inline_content(inner);
                    }
                }
                Inline::Image(img) => {
                    self.out.push_str("#box(");
                    self.image(img);
                    self.out.push(')');
                }
            }
        }
    }

    fn text(&mut self, s: &str, st: &Style) {
        if s.is_empty() {
            return;
        }
        let mut close = 0;
        let mut args = Vec::new();
        if st.bold {
            args.push("weight: \"bold\"".to_string());
        }
        if st.italic {
            args.push("style: \"italic\"".to_string());
        }
        if st.mono {
            args.push(format!(
                "font: ({}, {})",
                lit(engine::MONO),
                lit(engine::SANS)
            ));
        }
        if let Some(sz) = st.size.filter(|s| s.is_finite() && *s > 0.5) {
            args.push(format!("size: {}", pt(sz.min(400.0))));
        }
        if let Some(c) = st.color {
            args.push(format!("fill: {}", rgb(c)));
        }
        let wrap = |o: &mut String, f: &str| {
            o.push('#');
            o.push_str(f);
            o.push('[');
        };
        if st.underline {
            wrap(&mut self.out, "underline");
            close += 1;
        }
        if st.strike {
            wrap(&mut self.out, "strike");
            close += 1;
        }
        if let Some(c) = st.highlight {
            let _ = write!(self.out, "#highlight(fill: {})[", rgb(c));
            close += 1;
        }
        if st.sup {
            wrap(&mut self.out, "super");
            close += 1;
        } else if st.sub {
            wrap(&mut self.out, "sub");
            close += 1;
        }
        if !args.is_empty() {
            let _ = write!(self.out, "#text({})[", args.join(", "));
            close += 1;
        }
        let mut first = true;
        for line in s.split('\n') {
            if !first {
                self.out.push_str("#linebreak()");
            }
            first = false;
            if !line.is_empty() {
                self.out.push('#');
                self.out.push_str(&lit(line));
            }
        }
        for _ in 0..close {
            self.out.push(']');
        }
    }

    fn table(&mut self, t: &Table) {
        // columns needed by the widest row (spans counted)
        let width = |r: &Vec<Cell>| r.iter().map(|c| c.colspan.max(1) as usize).sum::<usize>();
        let ncols = if t.cols.is_empty() {
            t.rows.iter().map(width).max().unwrap_or(0)
        } else {
            t.cols.len()
        };
        if ncols == 0 {
            return;
        }
        let mut close = 0;
        if let Some(sz) = t.size {
            let _ = write!(self.out, "#[#set text(size: {})\n", pt(sz));
            close += 1;
        }
        self.out.push_str("#table(columns: (");
        for k in 0..ncols {
            let c = t.cols.get(k).unwrap_or(&Col::Auto);
            match c {
                Col::Auto => self.out.push_str("auto"),
                Col::Pt(v) => self.out.push_str(&pt(*v)),
                Col::Fr(v) => {
                    let _ = write!(self.out, "{:.3}fr", v.max(0.01));
                }
            }
            self.out.push_str(", ");
        }
        self.out.push(')');
        if t.borders {
            self.out.push_str(", stroke: 0.5pt + luma(150)");
        } else {
            self.out.push_str(", stroke: none");
        }
        // cells still covered by a rowspan from above, per column
        let mut covered = vec![0u16; ncols];
        for (ri, row) in t.rows.iter().enumerate() {
            if ri == 0 && t.header > 0 {
                self.out.push_str(", table.header(repeat: true");
            }
            let mut col = 0;
            let mut cells = row.iter();
            while col < ncols {
                if covered[col] > 0 {
                    covered[col] -= 1;
                    col += 1;
                    continue;
                }
                let Some(c) = cells.next() else {
                    // pad short rows so the next row starts on its own line
                    self.out.push_str(", []");
                    col += 1;
                    continue;
                };
                let span = (c.colspan.max(1) as usize).min(ncols - col);
                // a rowspan may not leave the header or the table
                let limit = if ri < t.header {
                    t.header
                } else {
                    t.rows.len()
                };
                let rs = c.rowspan.max(1).min(limit.saturating_sub(ri).max(1) as u16);
                if span > 1 || rs > 1 || c.fill.is_some() {
                    self.out.push_str(", table.cell(");
                    let mut sep = "";
                    if span > 1 {
                        let _ = write!(self.out, "colspan: {span}");
                        sep = ", ";
                    }
                    if rs > 1 {
                        let _ = write!(self.out, "{sep}rowspan: {rs}");
                        sep = ", ";
                    }
                    if let Some(f) = c.fill {
                        let _ = write!(self.out, "{sep}fill: {}", rgb(f));
                    }
                    self.out.push(')');
                } else {
                    self.out.push_str(", ");
                }
                self.content(&c.blocks);
                for k in col..col + span {
                    if rs > 1 {
                        covered[k] = covered[k].max(rs - 1);
                    }
                }
                col += span;
            }
            if ri + 1 == t.header.min(t.rows.len()) {
                self.out.push(')');
            }
        }
        self.out.push(')');
        for _ in 0..close {
            self.out.push(']');
        }
        self.out.push_str("\n\n");
    }
}
