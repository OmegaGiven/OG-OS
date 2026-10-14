//! The editable document: blocks (paragraphs, tables, opaque XML) over the
//! original document.xml text. Anything never edited keeps a span into that
//! text and is written back byte for byte; an edited paragraph drops its
//! span and is regenerated from its runs (see write.rs).
//!
//! Positions are byte offsets into a paragraph's *model text*: text runs
//! contribute their text, each visible object (image, hyperlink, field…)
//! one U+FFFC, invisible markers (bookmarks, field codes…) nothing.

use super::styles::{Kind, Look, Numbering, Styles};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const OBJ: char = '\u{FFFC}';
pub const OBJ_LEN: usize = 3;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Process-unique id / revision stamp.
pub fn next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    Bold,
    Italic,
    Underline,
    Strike,
}

impl Attr {
    pub const ALL: [Attr; 4] = [Attr::Bold, Attr::Italic, Attr::Underline, Attr::Strike];
}

/// Direct run formatting; None = not set on the run (inherited).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fmt {
    pub b: Option<bool>,
    pub i: Option<bool>,
    pub u: Option<bool>,
    pub s: Option<bool>,
}

impl Fmt {
    pub fn get(&self, a: Attr) -> Option<bool> {
        match a {
            Attr::Bold => self.b,
            Attr::Italic => self.i,
            Attr::Underline => self.u,
            Attr::Strike => self.s,
        }
    }

    pub fn set(&mut self, a: Attr, v: Option<bool>) {
        match a {
            Attr::Bold => self.b = v,
            Attr::Italic => self.i = v,
            Attr::Underline => self.u = v,
            Attr::Strike => self.s = v,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// '\t' = w:tab, '\n' = w:br
    pub text: String,
    pub fmt: Fmt,
    /// formatting as parsed from `rpr` (write.rs only rewrites what differs)
    pub orig: Fmt,
    /// original `<w:rPr>…</w:rPr>`
    pub rpr: Option<Arc<str>>,
    /// display only
    pub size: Option<f32>,
    pub color: Option<[u8; 3]>,
    pub rstyle: Option<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObjKind {
    /// bookmarks, field codes, deletions… (zero width)
    Hidden,
    Image {
        rid: Option<String>,
        w: f32,
        h: f32,
    },
    Link,
    /// other text-bearing element shown as-is (field result, insertion…)
    Text,
    PageBreak,
}

/// An inline element kept verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct Obj {
    pub xml: Arc<str>,
    pub kind: ObjKind,
    pub label: Arc<str>,
}

impl Obj {
    pub fn visible(&self) -> bool {
        self.kind != ObjKind::Hidden
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(TextRun),
    Obj(Obj),
}

impl Inline {
    pub fn len(&self) -> usize {
        match self {
            Inline::Text(t) => t.text.len(),
            Inline::Obj(o) if o.visible() => OBJ_LEN,
            Inline::Obj(_) => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

impl Align {
    pub fn val(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "both",
        }
    }
}

pub use super::styles::PprBits as PProps;

#[derive(Debug, Clone, PartialEq)]
pub struct Para {
    pub id: u64,
    /// changes whenever the content does (layout cache key)
    pub rev: u64,
    /// span in the source while unedited
    pub orig: Option<Range<usize>>,
    /// original `<w:p …>` start tag
    pub open: Option<Arc<str>>,
    pub ppr: Option<String>,
    pub props: PProps,
    pub inl: Vec<Inline>,
}

#[derive(Debug, Clone)]
pub struct Opaque {
    pub id: u64,
    pub span: Range<usize>,
    pub label: String,
    pub visible: bool,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub span: u16,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub id: u64,
    pub rev: u64,
    pub orig: Option<Range<usize>>,
    /// raw XML around the cell contents: pieces[k] precedes cell k's
    /// blocks (row-major), the last one closes the table
    pub pieces: Vec<Range<usize>>,
    /// column widths, twips
    pub grid: Vec<f32>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone)]
pub enum Block {
    Para(Para),
    Table(Table),
    Opaque(Opaque),
}

impl Block {
    pub fn id(&self) -> u64 {
        match self {
            Block::Para(p) => p.id,
            Block::Table(t) => t.id,
            Block::Opaque(o) => o.id,
        }
    }

    pub fn rev(&self) -> u64 {
        match self {
            Block::Para(p) => p.rev,
            Block::Table(t) => t.rev,
            Block::Opaque(o) => o.id,
        }
    }
}

/// A block and the raw text (whitespace) before it in the source.
#[derive(Debug, Clone)]
pub struct Entry {
    pub gap: Range<usize>,
    pub block: Block,
}

impl Entry {
    pub fn new(block: Block) -> Self {
        Entry { gap: 0..0, block }
    }
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub src: Arc<str>,
    /// up to the first body child
    pub prefix: Range<usize>,
    /// after the last body child
    pub suffix: Range<usize>,
    pub body: Vec<Arc<Entry>>,
    /// namespace prefix of WordprocessingML ("w")
    pub ns: String,
    pub styles: Arc<Styles>,
    pub numbering: Arc<Numbering>,
}

/// Where a paragraph lives: a body block, or a paragraph inside a table
/// cell (row, column, entry). Derived Ord = document order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Path {
    pub b: usize,
    pub cell: Option<(usize, usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub path: Path,
    pub off: usize,
}

impl Pos {
    pub fn new(path: Path, off: usize) -> Pos {
        Pos { path, off }
    }
}

// --------------------------------------------------------------- para ops

impl Para {
    pub fn empty(ppr: Option<String>, template: TextRun) -> Para {
        let mut p = Para {
            id: next_id(),
            rev: next_id(),
            orig: None,
            open: None,
            ppr: None,
            props: PProps::default(),
            inl: vec![Inline::Text(TextRun {
                text: String::new(),
                ..template
            })],
        };
        p.set_ppr(ppr);
        p
    }

    /// Replace the pPr (and re-derive the properties shown/used).
    pub fn set_ppr(&mut self, ppr: Option<String>) {
        self.props = match ppr.as_deref() {
            Some(x) => super::parse::tree(x)
                .ok()
                .and_then(|t| t.into_iter().next())
                .map(|n| super::styles::ppr_bits(x, &n))
                .unwrap_or_default(),
            None => PProps::default(),
        };
        self.ppr = ppr;
        self.touch();
    }

    pub fn touch(&mut self) {
        self.orig = None;
        self.rev = next_id();
    }

    pub fn len(&self) -> usize {
        self.inl.iter().map(Inline::len).sum()
    }

    /// Model text (objects as U+FFFC).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for i in &self.inl {
            match i {
                Inline::Text(t) => s.push_str(&t.text),
                Inline::Obj(o) if o.visible() => s.push(OBJ),
                Inline::Obj(_) => {}
            }
        }
        s
    }

    /// Plain text of a model range (objects as their label, images dropped).
    pub fn plain(&self, r: Range<usize>) -> String {
        let mut s = String::new();
        let mut acc = 0;
        for i in &self.inl {
            let len = i.len();
            let (a, b) = (r.start.max(acc), r.end.min(acc + len));
            if a < b {
                match i {
                    Inline::Text(t) => s.push_str(&t.text[a - acc..b - acc]),
                    Inline::Obj(o) => match o.kind {
                        ObjKind::Link | ObjKind::Text => s.push_str(&o.label),
                        ObjKind::PageBreak => s.push('\n'),
                        _ => {}
                    },
                }
            }
            acc += len;
        }
        s
    }

    /// Make a run boundary at `off`; returns the index of the first inline
    /// at or after it (zero-width markers at `off` stay on the left).
    fn cut(&mut self, off: usize) -> usize {
        let mut acc = 0;
        let mut i = 0;
        while i < self.inl.len() {
            let len = self.inl[i].len();
            if acc + len <= off {
                acc += len;
                i += 1;
                continue;
            }
            if acc < off {
                if let Inline::Text(t) = &mut self.inl[i] {
                    let right = t.text.split_off(off - acc);
                    let mut r = t.clone();
                    r.text = right;
                    self.inl.insert(i + 1, Inline::Text(r));
                    return i + 1;
                }
            }
            return i;
        }
        i
    }

    fn template_near(&self, idx: usize) -> Option<TextRun> {
        let left = self.inl[..idx.min(self.inl.len())].iter().rev();
        let right = self.inl[idx.min(self.inl.len())..].iter();
        left.chain(right).find_map(|i| match i {
            Inline::Text(t) => Some(TextRun {
                text: String::new(),
                ..t.clone()
            }),
            _ => None,
        })
    }

    /// Paragraph-mark run properties (pPr/rPr), as a template.
    fn mark_template(&self) -> TextRun {
        let rpr = self.ppr.as_deref().and_then(|p| {
            let t = super::parse::tree(p).ok()?;
            let r = t.first()?.child("rPr")?;
            Some(r.raw(p).to_string())
        });
        super::parse::run_template(rpr.as_deref())
    }

    /// The text run whose formatting applies at `off` (the one before it).
    pub fn run_at(&self, off: usize) -> TextRun {
        let mut acc = 0;
        let mut right = None;
        for i in &self.inl {
            let len = i.len();
            if let Inline::Text(t) = i {
                if acc < off && off <= acc + len {
                    return t.clone();
                }
                if right.is_none() && acc >= off {
                    right = Some(t.clone());
                }
            }
            acc += len;
        }
        right
            .or_else(|| self.template_near(self.inl.len()))
            .unwrap_or_else(|| self.mark_template())
    }

    /// Insert plain text (no paragraph breaks) at `off`; `fmt` forces the
    /// formatting of the new text.
    pub fn insert(&mut self, off: usize, s: &str, fmt: Option<Fmt>) {
        if s.is_empty() {
            return;
        }
        self.touch();
        let mut acc = 0;
        let mut hit = None;
        for (i, inl) in self.inl.iter().enumerate() {
            let len = inl.len();
            if let Inline::Text(_) = inl {
                if acc <= off && off <= acc + len {
                    hit = Some((i, off - acc));
                    // prefer the run that ends here (left affinity)
                    if off > acc || len == 0 {
                        break;
                    }
                }
            }
            if acc > off {
                break;
            }
            acc += len;
        }
        if let Some((i, lo)) = hit {
            let Inline::Text(t) = &mut self.inl[i] else {
                unreachable!()
            };
            match fmt {
                Some(f) if f != t.fmt => {
                    let right = t.text.split_off(lo);
                    let mut mid = t.clone();
                    mid.text = s.to_string();
                    mid.fmt = f;
                    let mut r = t.clone();
                    r.text = right;
                    let left_empty = t.text.is_empty();
                    self.inl.insert(i + 1, Inline::Text(mid));
                    if !r.text.is_empty() {
                        self.inl.insert(i + 2, Inline::Text(r));
                    }
                    if left_empty {
                        self.inl.remove(i);
                    }
                }
                _ => t.text.insert_str(lo, s),
            }
            return;
        }
        let idx = self.cut(off);
        let mut t = self
            .template_near(idx)
            .unwrap_or_else(|| self.mark_template());
        t.text = s.to_string();
        if let Some(f) = fmt {
            t.fmt = f;
        }
        self.inl.insert(idx, Inline::Text(t));
    }

    /// Remove a model range. Zero-width markers inside it survive.
    pub fn delete(&mut self, r: Range<usize>) {
        if r.start >= r.end {
            return;
        }
        self.touch();
        let i = self.cut(r.start);
        let j = self.cut(r.end);
        let mut template = None;
        let (mut k, mut end) = (i, j);
        while k < end {
            match &self.inl[k] {
                Inline::Obj(o) if !o.visible() => k += 1,
                other => {
                    if template.is_none() {
                        if let Inline::Text(t) = other {
                            template = Some(TextRun {
                                text: String::new(),
                                ..t.clone()
                            });
                        }
                    }
                    self.inl.remove(k);
                    end -= 1;
                }
            }
        }
        self.tidy(template, i);
    }

    /// Drop empty text runs, but keep one text run so the paragraph keeps
    /// its formatting for typing.
    fn tidy(&mut self, template: Option<TextRun>, at: usize) {
        let has_text = |inl: &[Inline]| inl.iter().any(|i| matches!(i, Inline::Text(_)));
        let before = has_text(&self.inl) || template.is_some();
        self.inl
            .retain(|i| !matches!(i, Inline::Text(t) if t.text.is_empty()));
        if before && !self.inl.iter().any(|i| matches!(i, Inline::Text(_))) {
            let t = template
                .or_else(|| self.template_near(at))
                .unwrap_or_else(|| self.mark_template());
            self.inl.insert(at.min(self.inl.len()), Inline::Text(t));
        }
    }

    /// Split at `off`: self keeps the left part, the right part is returned.
    pub fn split_off(&mut self, off: usize) -> Vec<Inline> {
        self.touch();
        let i = self.cut(off);
        let right = self.inl.split_off(i);
        right
    }

    /// Apply `f` to the text runs covering `r`.
    pub fn apply(&mut self, r: Range<usize>, mut f: impl FnMut(&mut TextRun)) {
        if r.start >= r.end {
            return;
        }
        self.touch();
        let i = self.cut(r.start);
        let j = self.cut(r.end);
        for k in i..j {
            if let Inline::Text(t) = &mut self.inl[k] {
                if !t.text.is_empty() {
                    f(t);
                }
            }
        }
    }

    /// Text runs (with their model ranges) overlapping `r`.
    pub fn runs_in(&self, r: Range<usize>) -> Vec<&TextRun> {
        let mut acc = 0;
        let mut out = Vec::new();
        for i in &self.inl {
            let len = i.len();
            if let Inline::Text(t) = i {
                if acc < r.end && r.start < acc + len {
                    out.push(t);
                }
            }
            acc += len;
        }
        out
    }

    /// Previous/next char boundary in model text.
    pub fn prev_char(&self, off: usize) -> usize {
        let t = self.text();
        t[..off.min(t.len())]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    pub fn next_char(&self, off: usize) -> usize {
        let t = self.text();
        match t[off.min(t.len())..].chars().next() {
            Some(c) => off + c.len_utf8(),
            None => t.len(),
        }
    }

    pub fn word_left(&self, off: usize) -> usize {
        let t = self.text();
        let chars: Vec<(usize, char)> = t[..off.min(t.len())].char_indices().collect();
        let mut k = chars.len();
        while k > 0 && !is_word(chars[k - 1].1) {
            k -= 1;
        }
        while k > 0 && is_word(chars[k - 1].1) {
            k -= 1;
        }
        chars
            .get(k)
            .map(|c| c.0)
            .unwrap_or(if k == 0 { 0 } else { off })
    }

    pub fn word_right(&self, off: usize) -> usize {
        let t = self.text();
        let mut it = t[off.min(t.len())..].char_indices().peekable();
        let mut end = t.len() - off.min(t.len());
        while let Some(&(_, c)) = it.peek() {
            if is_word(c) {
                break;
            }
            it.next();
        }
        while let Some(&(i, c)) = it.peek() {
            if !is_word(c) {
                end = i;
                break;
            }
            it.next();
        }
        off.min(t.len()) + end
    }

    /// The word around `off` (for double-click).
    pub fn word_at(&self, off: usize) -> Range<usize> {
        let t = self.text();
        let off = off.min(t.len());
        let next_is_word = t[off..].chars().next().is_some_and(is_word);
        let prev_is_word = t[..off].chars().next_back().is_some_and(is_word);
        if !next_is_word && !prev_is_word {
            return off..self.next_char(off);
        }
        let mut a = off;
        for (i, c) in t[..off].char_indices().rev() {
            if !is_word(c) {
                break;
            }
            a = i;
        }
        let mut b = off;
        for (i, c) in t[off..].char_indices() {
            if !is_word(c) {
                break;
            }
            b = off + i + c.len_utf8();
        }
        a..b
    }

    /// Visible objects only (for image-only paragraphs).
    pub fn objects(&self) -> impl Iterator<Item = &Obj> {
        self.inl.iter().filter_map(|i| match i {
            Inline::Obj(o) if o.visible() => Some(o),
            _ => None,
        })
    }

    pub fn has_text(&self) -> bool {
        self.inl
            .iter()
            .any(|i| matches!(i, Inline::Text(t) if !t.text.trim().is_empty()))
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

// ---------------------------------------------------------------- doc ops

impl Doc {
    pub fn entry(&self, p: Path) -> Option<&Entry> {
        let e = self.body.get(p.b)?;
        match p.cell {
            None => Some(e),
            Some((r, c, i)) => match &e.block {
                Block::Table(t) => t.rows.get(r)?.cells.get(c)?.entries.get(i),
                _ => None,
            },
        }
    }

    pub fn para(&self, p: Path) -> Option<&Para> {
        match &self.entry(p)?.block {
            Block::Para(para) => Some(para),
            _ => None,
        }
    }

    /// The container (body or cell entries) a path lives in, mutable.
    fn container_mut(&mut self, p: Path) -> Option<ContainerMut<'_>> {
        match p.cell {
            None => Some(ContainerMut::Body(&mut self.body)),
            Some((r, c, _)) => {
                let e = Arc::make_mut(self.body.get_mut(p.b)?);
                match &mut e.block {
                    Block::Table(t) => {
                        t.orig = None;
                        t.rev = next_id();
                        Some(ContainerMut::Cell(
                            &mut t.rows.get_mut(r)?.cells.get_mut(c)?.entries,
                        ))
                    }
                    _ => None,
                }
            }
        }
    }

    pub fn para_mut(&mut self, p: Path) -> Option<&mut Para> {
        let e = Arc::make_mut(self.body.get_mut(p.b)?);
        let blk = match p.cell {
            None => &mut e.block,
            Some((r, c, i)) => match &mut e.block {
                Block::Table(t) => {
                    t.orig = None;
                    t.rev = next_id();
                    &mut t
                        .rows
                        .get_mut(r)?
                        .cells
                        .get_mut(c)?
                        .entries
                        .get_mut(i)?
                        .block
                }
                _ => return None,
            },
        };
        match blk {
            Block::Para(para) => Some(para),
            _ => None,
        }
    }

    /// Every editable paragraph in document order.
    pub fn flow(&self) -> Vec<Path> {
        let mut out = Vec::new();
        for (b, e) in self.body.iter().enumerate() {
            match &e.block {
                Block::Para(_) => out.push(Path { b, cell: None }),
                Block::Table(t) => {
                    for (r, row) in t.rows.iter().enumerate() {
                        for (c, cell) in row.cells.iter().enumerate() {
                            for (i, ce) in cell.entries.iter().enumerate() {
                                if matches!(ce.block, Block::Para(_)) {
                                    out.push(Path {
                                        b,
                                        cell: Some((r, c, i)),
                                    });
                                }
                            }
                        }
                    }
                }
                Block::Opaque(_) => {}
            }
        }
        out
    }

    /// Insert `para` right after `p` in the same container; returns its path.
    fn insert_after(&mut self, p: Path, para: Para) -> Path {
        let entry = Entry::new(Block::Para(para));
        match self.container_mut(p) {
            Some(ContainerMut::Body(v)) => {
                v.insert(p.b + 1, Arc::new(entry));
                Path {
                    b: p.b + 1,
                    cell: None,
                }
            }
            Some(ContainerMut::Cell(v)) => {
                let (r, c, i) = p.cell.unwrap_or_default();
                v.insert(i + 1, entry);
                Path {
                    b: p.b,
                    cell: Some((r, c, i + 1)),
                }
            }
            None => p,
        }
    }

    fn remove(&mut self, p: Path) {
        match self.container_mut(p) {
            Some(ContainerMut::Body(v)) => {
                v.remove(p.b);
            }
            Some(ContainerMut::Cell(v)) => {
                v.remove(p.cell.unwrap_or_default().2);
            }
            None => {}
        }
    }

    /// The paragraph just before `p` in the same container, if it is one.
    fn prev_sibling(&self, p: Path) -> Option<Path> {
        let q = match p.cell {
            None => Path {
                b: p.b.checked_sub(1)?,
                cell: None,
            },
            Some((r, c, i)) => Path {
                b: p.b,
                cell: Some((r, c, i.checked_sub(1)?)),
            },
        };
        self.para(q).map(|_| q)
    }

    fn next_sibling(&self, p: Path) -> Option<Path> {
        let q = match p.cell {
            None => Path {
                b: p.b + 1,
                cell: None,
            },
            Some((r, c, i)) => Path {
                b: p.b,
                cell: Some((r, c, i + 1)),
            },
        };
        self.para(q).map(|_| q)
    }

    /// Look of a run (style chain applied) and of its paragraph.
    pub fn run_look(&self, para: &Para, t: &TextRun) -> Look {
        let st = &self.styles;
        let base = st.resolve(para.props.style.as_deref()).look;
        let cs = t
            .rstyle
            .as_deref()
            .map(|r| st.char_look(r))
            .unwrap_or_default();
        Look {
            b: t.fmt.b,
            i: t.fmt.i,
            u: t.fmt.u,
            s: t.fmt.s,
            size: t.size,
            color: t.color,
        }
        .over(cs)
        .over(base)
    }

    /// Value of `a` the run would have without direct formatting.
    fn inherited(&self, para: &Para, t: &TextRun, a: Attr) -> bool {
        let mut bare = t.clone();
        bare.fmt.set(a, None);
        let l = self.run_look(para, &bare);
        match a {
            Attr::Bold => l.b,
            Attr::Italic => l.i,
            Attr::Underline => l.u,
            Attr::Strike => l.s,
        }
        .unwrap_or(false)
    }

    pub fn effective(&self, para: &Para, t: &TextRun, a: Attr) -> bool {
        t.fmt.get(a).unwrap_or_else(|| self.inherited(para, t, a))
    }
}

enum ContainerMut<'a> {
    Body(&'a mut Vec<Arc<Entry>>),
    Cell(&'a mut Vec<Entry>),
}

// ---------------------------------------------------------------- editor

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    DocStart,
    DocEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Typing,
    Other,
}

#[derive(Debug, Clone)]
struct Snap {
    body: Vec<Arc<Entry>>,
    anchor: Pos,
    head: Pos,
    version: u64,
}

/// Document + selection + undo history.
#[derive(Debug, Clone)]
pub struct Editor {
    pub doc: Doc,
    pub flow: Vec<Path>,
    pub anchor: Pos,
    pub head: Pos,
    /// formatting for the next typed text (toggled with no selection)
    pub pending: Option<Fmt>,
    /// content version (undo restores it: dirty = version != saved)
    pub version: u64,
    /// bumps on every selection change (the view scrolls the caret into view)
    pub sel_version: u64,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    last: Option<EditKind>,
}

const UNDO_LIMIT: usize = 300;

impl Editor {
    pub fn new(doc: Doc) -> Editor {
        let flow = doc.flow();
        let start = Pos::new(flow.first().copied().unwrap_or_default(), 0);
        Editor {
            doc,
            flow,
            anchor: start,
            head: start,
            pending: None,
            version: next_id(),
            sel_version: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last: None,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn has_sel(&self) -> bool {
        self.anchor != self.head
    }

    pub fn range(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn fi(&self, p: Path) -> usize {
        self.flow
            .binary_search(&p)
            .unwrap_or_else(|i| i.min(self.flow.len().saturating_sub(1)))
    }

    /// Clamp a position to an existing paragraph and offset.
    pub fn clamp(&self, mut p: Pos) -> Pos {
        if self.doc.para(p.path).is_none() {
            match self.flow.get(self.fi(p.path)) {
                Some(&q) => p = Pos::new(q, 0),
                None => return Pos::default(),
            }
        }
        let para = self.doc.para(p.path).expect("clamped path");
        let t = para.text();
        p.off = p.off.min(t.len());
        while !t.is_char_boundary(p.off) {
            p.off -= 1;
        }
        p
    }

    pub fn set_sel(&mut self, anchor: Pos, head: Pos) {
        self.anchor = self.clamp(anchor);
        self.head = self.clamp(head);
        self.pending = None;
        self.last = None;
        self.sel_version += 1;
    }

    fn caret_to(&mut self, p: Pos) {
        let p = self.clamp(p);
        self.anchor = p;
        self.head = p;
        self.sel_version += 1;
    }

    pub fn select_all(&mut self) {
        let (Some(&a), Some(&b)) = (self.flow.first(), self.flow.last()) else {
            return;
        };
        let end = self.doc.para(b).map(Para::len).unwrap_or(0);
        self.set_sel(Pos::new(a, 0), Pos::new(b, end));
    }

    fn snap(&self) -> Snap {
        Snap {
            body: self.doc.body.clone(),
            anchor: self.anchor,
            head: self.head,
            version: self.version,
        }
    }

    fn begin(&mut self, kind: EditKind) {
        if !(kind == EditKind::Typing && self.last == Some(EditKind::Typing)) {
            self.undo.push(self.snap());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = Some(kind);
    }

    fn end(&mut self) {
        self.version = next_id();
        self.flow = self.doc.flow();
        self.sel_version += 1;
    }

    fn restore(&mut self, s: Snap) {
        self.doc.body = s.body;
        self.version = s.version;
        self.flow = self.doc.flow();
        self.anchor = self.clamp(s.anchor);
        self.head = self.clamp(s.head);
        self.pending = None;
        self.last = None;
        self.sel_version += 1;
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snap());
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snap());
        self.restore(s);
        true
    }

    // ------------------------------------------------------------ motion

    pub fn motion_target(&self, m: Motion, from: Pos) -> Pos {
        let Some(para) = self.doc.para(from.path) else {
            return from;
        };
        let fi = self.fi(from.path);
        let prev_end = || {
            fi.checked_sub(1)
                .and_then(|i| self.flow.get(i))
                .map(|&q| Pos::new(q, self.doc.para(q).map(Para::len).unwrap_or(0)))
        };
        let next_start = || self.flow.get(fi + 1).map(|&q| Pos::new(q, 0));
        match m {
            Motion::Left if from.off == 0 => prev_end().unwrap_or(from),
            Motion::Left => Pos::new(from.path, para.prev_char(from.off)),
            Motion::Right if from.off >= para.len() => next_start().unwrap_or(from),
            Motion::Right => Pos::new(from.path, para.next_char(from.off)),
            Motion::WordLeft if from.off == 0 => prev_end().unwrap_or(from),
            Motion::WordLeft => Pos::new(from.path, para.word_left(from.off)),
            Motion::WordRight if from.off >= para.len() => next_start().unwrap_or(from),
            Motion::WordRight => Pos::new(from.path, para.word_right(from.off)),
            Motion::LineStart => Pos::new(from.path, 0),
            Motion::LineEnd => Pos::new(from.path, para.len()),
            Motion::DocStart => Pos::new(self.flow.first().copied().unwrap_or_default(), 0),
            Motion::DocEnd => {
                let q = self.flow.last().copied().unwrap_or_default();
                Pos::new(q, self.doc.para(q).map(Para::len).unwrap_or(0))
            }
        }
    }

    pub fn motion(&mut self, m: Motion, extend: bool) {
        // collapsing a selection with plain Left/Right goes to its edge
        if !extend && self.has_sel() && matches!(m, Motion::Left | Motion::Right) {
            let (a, b) = self.range();
            let p = if m == Motion::Left { a } else { b };
            self.set_sel(p, p);
            return;
        }
        let to = self.motion_target(m, self.head);
        let anchor = if extend { self.anchor } else { to };
        self.set_sel(anchor, to);
    }

    /// Next/previous table cell (Tab / Shift+Tab). False if not in a table.
    pub fn cell_step(&mut self, back: bool) -> bool {
        let Some((r, c, _)) = self.head.path.cell else {
            return false;
        };
        let fi = self.fi(self.head.path);
        let same = |p: &Path| p.b == self.head.path.b && p.cell.map(|x| (x.0, x.1)) == Some((r, c));
        let target = if back {
            self.flow[..fi]
                .iter()
                .rev()
                .find(|p| !same(p) && p.b == self.head.path.b)
                .map(|p| {
                    // start of that cell
                    let (pr, pc, _) = p.cell.unwrap_or_default();
                    Path {
                        b: p.b,
                        cell: Some((pr, pc, 0)),
                    }
                })
        } else {
            self.flow[fi..]
                .iter()
                .find(|p| !same(p) && p.b == self.head.path.b)
                .copied()
        };
        if let Some(p) = target {
            let p = self.clamp(Pos::new(p, 0));
            let end = self.doc.para(p.path).map(Para::len).unwrap_or(0);
            // select the cell's text like Word
            self.set_sel(p, Pos::new(p.path, end));
        }
        true
    }

    // ------------------------------------------------------------- edits

    /// Delete [a, b) (a <= b). Returns where the caret ends up.
    fn delete_range(&mut self, a: Pos, b: Pos) -> Pos {
        if a >= b {
            return a;
        }
        if a.path == b.path {
            if let Some(p) = self.doc.para_mut(a.path) {
                p.delete(a.off..b.off);
            }
            return a;
        }
        let same_container = match (a.path.cell, b.path.cell) {
            (None, None) => true,
            (Some((r1, c1, _)), Some((r2, c2, _))) => a.path.b == b.path.b && (r1, c1) == (r2, c2),
            _ => false,
        };
        if same_container {
            // keep a's head + b's tail in one paragraph, drop what's between
            let tail = match self.doc.para_mut(b.path) {
                Some(p) => p.split_off(b.off),
                None => Vec::new(),
            };
            if let Some(p) = self.doc.para_mut(a.path) {
                let len = p.len();
                p.delete(a.off..len);
                p.inl.extend(tail);
                p.tidy(None, 0);
                p.touch();
            }
            // remove the blocks in between (invisible markers stay)
            let (lo, hi) = match (a.path.cell, b.path.cell) {
                (None, None) => (a.path.b + 1, b.path.b),
                (Some((_, _, i)), Some((_, _, j))) => (i + 1, j),
                _ => unreachable!(),
            };
            for k in (lo..=hi).rev() {
                let p = match a.path.cell {
                    None => Path { b: k, cell: None },
                    Some((r, c, _)) => Path {
                        b: a.path.b,
                        cell: Some((r, c, k)),
                    },
                };
                let hidden = matches!(
                    self.doc.entry(p).map(|e| &e.block),
                    Some(Block::Opaque(o)) if !o.visible
                );
                // b's paragraph was merged into a; invisible markers stay
                if k == hi || !hidden {
                    self.doc.remove(p);
                }
            }
            return a;
        }
        // across table structure: clear the covered text, keep the structure
        let (fa, fb) = (self.fi(a.path), self.fi(b.path));
        for k in fa..=fb {
            let p = self.flow[k];
            let Some(para) = self.doc.para_mut(p) else {
                continue;
            };
            let len = para.len();
            let s = if k == fa { a.off } else { 0 };
            let e = if k == fb { b.off } else { len };
            para.delete(s..e);
        }
        a
    }

    fn delete_selection(&mut self) -> bool {
        if !self.has_sel() {
            return false;
        }
        let (a, b) = self.range();
        let at = self.delete_range(a, b);
        self.flow = self.doc.flow();
        self.caret_to(at);
        true
    }

    /// Typed or pasted text; '\n' starts a new paragraph.
    pub fn insert(&mut self, s: &str) {
        if self.flow.is_empty() || s.is_empty() {
            return;
        }
        let typing = s.chars().count() == 1 && !s.contains('\n');
        self.begin(if typing && !self.has_sel() {
            EditKind::Typing
        } else {
            EditKind::Other
        });
        let pending = self.pending.take();
        self.delete_selection();
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        for (i, line) in s.split('\n').enumerate() {
            if i > 0 {
                self.split_here();
            }
            self.insert_inline(line, pending);
        }
        self.end();
    }

    fn insert_inline(&mut self, s: &str, fmt: Option<Fmt>) {
        let h = self.head;
        if let Some(p) = self.doc.para_mut(h.path) {
            p.insert(h.off, s, fmt);
            self.caret_to(Pos::new(h.path, h.off + s.len()));
        }
    }

    /// Shift+Enter: a line break inside the paragraph.
    pub fn line_break(&mut self) {
        if self.flow.is_empty() {
            return;
        }
        self.begin(EditKind::Other);
        let pending = self.pending.take();
        self.delete_selection();
        self.insert_inline("\n", pending);
        self.end();
    }

    fn split_here(&mut self) {
        let h = self.head;
        let styles = self.doc.styles.clone();
        let ns = self.doc.ns.clone();
        let Some(p) = self.doc.para_mut(h.path) else {
            return;
        };
        let at_end = h.off >= p.len();
        let template = p.run_at(h.off);
        let right_inl = p.split_off(h.off);
        p.tidy(Some(template.clone()), p.inl.len());
        let mut ppr = p.ppr.clone();
        // Enter at the end of a heading/title continues with body text
        let kind = styles.kind(p.props.style.as_deref());
        if at_end && !matches!(kind, Kind::Normal | Kind::Other) {
            ppr = super::write::set_prop(ppr.as_deref(), "pPr", "pStyle", None, &ns);
        }
        let mut right = Para::empty(ppr, template.clone());
        right.inl = right_inl;
        right.tidy(
            Some(TextRun {
                text: String::new(),
                ..template
            }),
            0,
        );
        let path = self.doc.insert_after(h.path, right);
        self.flow = self.doc.flow();
        self.caret_to(Pos::new(path, 0));
    }

    pub fn enter(&mut self) {
        if self.flow.is_empty() {
            return;
        }
        self.begin(EditKind::Other);
        self.delete_selection();
        let h = self.head;
        // Enter on an empty list item ends the list
        let empty_item = self
            .doc
            .para(h.path)
            .is_some_and(|p| p.len() == 0 && p.props.num.is_some());
        if empty_item {
            self.set_num(h.path, None);
        } else {
            self.split_here();
        }
        self.end();
    }

    pub fn backspace(&mut self, word: bool) {
        if self.flow.is_empty() {
            return;
        }
        self.begin(EditKind::Other);
        if !self.delete_selection() {
            let h = self.head;
            let Some(para) = self.doc.para(h.path) else {
                return;
            };
            if h.off > 0 {
                let from = if word {
                    para.word_left(h.off)
                } else {
                    para.prev_char(h.off)
                };
                if let Some(p) = self.doc.para_mut(h.path) {
                    p.delete(from..h.off);
                }
                self.caret_to(Pos::new(h.path, from));
            } else if para.props.num.is_some() {
                self.set_num(h.path, None);
            } else if let Some(prev) = self.doc.prev_sibling(h.path) {
                self.merge(prev, h.path);
            }
        }
        self.end();
    }

    pub fn delete(&mut self, word: bool) {
        if self.flow.is_empty() {
            return;
        }
        self.begin(EditKind::Other);
        if !self.delete_selection() {
            let h = self.head;
            let Some(para) = self.doc.para(h.path) else {
                return;
            };
            if h.off < para.len() {
                let to = if word {
                    para.word_right(h.off)
                } else {
                    para.next_char(h.off)
                };
                if let Some(p) = self.doc.para_mut(h.path) {
                    p.delete(h.off..to);
                }
            } else if let Some(next) = self.doc.next_sibling(h.path) {
                self.merge(h.path, next);
            }
        }
        self.end();
    }

    /// Append paragraph `b` to `a` (its sibling) and remove `b`.
    fn merge(&mut self, a: Path, b: Path) {
        let Some(tail) = self.doc.para(b).map(|p| p.inl.clone()) else {
            return;
        };
        let Some(pa) = self.doc.para_mut(a) else {
            return;
        };
        let len = pa.len();
        pa.inl.extend(tail);
        pa.tidy(None, 0);
        pa.touch();
        self.doc.remove(b);
        self.flow = self.doc.flow();
        self.caret_to(Pos::new(a, len));
    }

    // ------------------------------------------------------- formatting

    /// Paragraph paths covered by the selection (or the caret's).
    pub fn sel_paths(&self) -> Vec<Path> {
        let (a, b) = self.range();
        if self.flow.is_empty() {
            return Vec::new();
        }
        let (i, j) = (self.fi(a.path), self.fi(b.path));
        self.flow[i..=j.max(i)].to_vec()
    }

    /// Is `a` on at the caret / over the whole selection?
    pub fn attr_on(&self, a: Attr) -> bool {
        if !self.has_sel() {
            if let Some(f) = self.pending {
                if let Some(v) = f.get(a) {
                    return v;
                }
            }
            let Some(p) = self.doc.para(self.head.path) else {
                return false;
            };
            let t = p.run_at(self.head.off);
            return self.doc.effective(p, &t, a);
        }
        let (s, e) = self.range();
        let mut any = false;
        for path in self.sel_paths() {
            let Some(p) = self.doc.para(path) else {
                continue;
            };
            let lo = if path == s.path { s.off } else { 0 };
            let hi = if path == e.path { e.off } else { p.len() };
            for t in p.runs_in(lo..hi) {
                if t.text.is_empty() {
                    continue;
                }
                any = true;
                if !self.doc.effective(p, t, a) {
                    return false;
                }
            }
        }
        any
    }

    pub fn toggle(&mut self, a: Attr) {
        let v = !self.attr_on(a);
        if !self.has_sel() {
            let Some(p) = self.doc.para(self.head.path) else {
                return;
            };
            let mut f = self.pending.unwrap_or(p.run_at(self.head.off).fmt);
            f.set(a, Some(v));
            self.pending = Some(f);
            self.sel_version += 1;
            return;
        }
        self.begin(EditKind::Other);
        let (s, e) = self.range();
        let doc = self.doc.clone();
        for path in self.sel_paths() {
            let Some(orig_para) = doc.para(path) else {
                continue;
            };
            let lo = if path == s.path { s.off } else { 0 };
            let hi = if path == e.path {
                e.off
            } else {
                orig_para.len()
            };
            let Some(p) = self.doc.para_mut(path) else {
                continue;
            };
            p.apply(lo..hi, |t| {
                let inherited = doc.inherited(orig_para, t, a);
                let new = if v == inherited && t.orig.get(a) != Some(v) {
                    None
                } else {
                    Some(v)
                };
                t.fmt.set(a, new);
            });
        }
        self.end();
    }

    fn edit_ppr(&mut self, path: Path, name: &str, xml: Option<String>) {
        let ns = self.doc.ns.clone();
        if let Some(p) = self.doc.para_mut(path) {
            let ppr = super::write::set_prop(p.ppr.as_deref(), "pPr", name, xml.as_deref(), &ns);
            p.set_ppr(ppr);
        }
    }

    fn set_num(&mut self, path: Path, num: Option<(String, u8)>) {
        let w = self.doc.ns.clone();
        let xml = num.map(|(id, lvl)| {
            format!(
                r#"<{w}:numPr><{w}:ilvl {w}:val="{lvl}"/><{w}:numId {w}:val="{id}"/></{w}:numPr>"#
            )
        });
        // a list style (e.g. "List Bullet") carries its own numbering: turn
        // it off explicitly with numId 0
        let style_num = self
            .doc
            .styles
            .resolve(self.doc.para(path).and_then(|p| p.props.style.as_deref()))
            .num
            .is_some();
        let xml = match xml {
            None if style_num => Some(format!(
                r#"<{w}:numPr><{w}:ilvl {w}:val="0"/><{w}:numId {w}:val="0"/></{w}:numPr>"#
            )),
            x => x,
        };
        self.edit_ppr(path, "numPr", xml);
    }

    pub fn set_kind(&mut self, kind: Kind) {
        if self.flow.is_empty() {
            return;
        }
        self.begin(EditKind::Other);
        let w = self.doc.ns.clone();
        let id = self.doc.styles.id_for(kind);
        let xml = id.map(|id| format!(r#"<{w}:pStyle {w}:val="{}"/>"#, attr_escape(&id)));
        for p in self.sel_paths() {
            self.edit_ppr(p, "pStyle", xml.clone());
        }
        self.end();
    }

    pub fn kind_at(&self) -> Kind {
        self.doc
            .para(self.head.path)
            .map(|p| self.doc.styles.kind(p.props.style.as_deref()))
            .unwrap_or(Kind::Normal)
    }

    pub fn set_align(&mut self, a: Align) {
        if self.flow.is_empty() {
            return;
        }
        self.begin(EditKind::Other);
        let w = self.doc.ns.clone();
        for path in self.sel_paths() {
            let style_align = self
                .doc
                .styles
                .resolve(self.doc.para(path).and_then(|p| p.props.style.as_deref()))
                .align
                .unwrap_or_default();
            let xml = if a == Align::Left && style_align == Align::Left {
                None
            } else {
                Some(format!(r#"<{w}:jc {w}:val="{}"/>"#, a.val()))
            };
            self.edit_ppr(path, "jc", xml);
        }
        self.end();
    }

    pub fn align_at(&self) -> Align {
        self.doc
            .para(self.head.path)
            .map(|p| {
                p.props.align.unwrap_or_else(|| {
                    self.doc
                        .styles
                        .resolve(p.props.style.as_deref())
                        .align
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default()
    }

    /// List state of a paragraph: Some(bullet?) when it is a list item.
    pub fn list_of(&self, p: &Para) -> Option<(String, u8, bool)> {
        let (id, lvl) = p
            .props
            .num
            .clone()
            .or_else(|| self.doc.styles.resolve(p.props.style.as_deref()).num)?;
        if id == "0" {
            return None;
        }
        let bullet = self.doc.numbering.is_bullet(&id, lvl);
        Some((id, lvl, bullet))
    }

    pub fn list_at(&self) -> Option<bool> {
        let p = self.doc.para(self.head.path)?;
        self.list_of(p).map(|(_, _, b)| b)
    }

    /// Toggle bullets (or numbers) on the selected paragraphs, using
    /// `num_id` for paragraphs that aren't in such a list yet.
    pub fn toggle_list(&mut self, bullet: bool, num_id: &str) {
        if self.flow.is_empty() {
            return;
        }
        let paths = self.sel_paths();
        let all = paths.iter().all(|&p| {
            self.doc
                .para(p)
                .and_then(|p| self.list_of(p))
                .is_some_and(|(_, _, b)| b == bullet)
        });
        self.begin(EditKind::Other);
        for p in paths {
            if all {
                self.set_num(p, None);
            } else {
                // continue the list above when it's the same kind
                let prev = self
                    .doc
                    .prev_sibling(p)
                    .and_then(|q| self.doc.para(q))
                    .and_then(|q| self.list_of(q))
                    .filter(|(_, _, b)| *b == bullet)
                    .map(|(id, _, _)| id);
                let lvl = self
                    .doc
                    .para(p)
                    .and_then(|q| q.props.num.as_ref().map(|n| n.1))
                    .unwrap_or(0);
                let id = prev.unwrap_or_else(|| num_id.to_string());
                self.set_num(p, Some((id, lvl)));
            }
        }
        self.end();
    }

    /// The numbering definitions changed (a list definition was added).
    pub fn set_numbering(&mut self, n: Numbering) {
        self.doc.numbering = Arc::new(n);
    }

    // --------------------------------------------------------- queries

    pub fn selected_text(&self) -> String {
        let (a, b) = self.range();
        let mut out = String::new();
        for (k, path) in self.sel_paths().into_iter().enumerate() {
            let Some(p) = self.doc.para(path) else {
                continue;
            };
            if k > 0 {
                out.push('\n');
            }
            let lo = if path == a.path { a.off } else { 0 };
            let hi = if path == b.path { b.off } else { p.len() };
            out.push_str(&p.plain(lo..hi));
        }
        out
    }

    pub fn word_count(&self) -> usize {
        let mut n = 0;
        for &p in &self.flow {
            if let Some(p) = self.doc.para(p) {
                n += p
                    .plain(0..p.len())
                    .split_whitespace()
                    .filter(|w| w.chars().any(char::is_alphanumeric))
                    .count();
            }
        }
        n
    }
}

pub fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}
