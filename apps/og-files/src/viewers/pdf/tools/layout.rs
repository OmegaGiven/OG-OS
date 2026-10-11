//! Page composition and stamping via Form XObjects.
//!
//! Composition tools (n-up, booklet, scaling, overlay, single page, split)
//! turn each source page into a Form XObject that shows the page's visible
//! box upright, then draw those onto new pages with `cm … Do`. Stamping
//! tools append a content stream to existing pages (prior content wrapped
//! in `q … Q`) using standard-14 fonts, ExtGState opacity and Image XObjects.
//!
//! Limits: pages rebuilt from XObjects lose their annotations (links, form
//! widgets) and the document's outline/structure tree; stamps use WinAnsi
//! text only (other characters become `?`).

use super::{Anchor, Paper, Rgb};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use std::collections::{BTreeSet, HashMap};

/// The 14 standard PDF fonts (no embedding needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Helvetica,
    HelveticaBold,
    TimesRoman,
    TimesBold,
    Courier,
    CourierBold,
}

impl Font {
    pub const ALL: [Font; 6] = [
        Font::Helvetica,
        Font::HelveticaBold,
        Font::TimesRoman,
        Font::TimesBold,
        Font::Courier,
        Font::CourierBold,
    ];
}

impl std::fmt::Display for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Font::Helvetica => "Helvetica",
            Font::HelveticaBold => "Helvetica Bold",
            Font::TimesRoman => "Times",
            Font::TimesBold => "Times Bold",
            Font::Courier => "Courier",
            Font::CourierBold => "Courier Bold",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    pub font: Font,
    pub size: f32,
    pub color: Rgb,
    /// 0.0 transparent – 1.0 opaque
    pub opacity: f32,
    /// degrees, counter-clockwise
    pub rotation: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: Font::Helvetica,
            size: 12.0,
            color: [0.0, 0.0, 0.0],
            opacity: 1.0,
            rotation: 0.0,
        }
    }
}

/// Where a stamp goes: an anchor on the page plus a margin (points) from
/// the edges, or an explicit offset added after anchoring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub anchor: Anchor,
    pub margin: f32,
    pub offset: (f32, f32),
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            anchor: Anchor::Bottom,
            margin: 24.0,
            offset: (0.0, 0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overlay {
    Foreground,
    Background,
}

/// Largest page side most viewers accept (200 in).
const MAX_SIDE: f32 = 14400.0;
/// Cap on decoded content per page, against decompression bombs.
const MAX_CONTENT: usize = 256 << 20;
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Sheets of `per_sheet` (2, 4, 6, 9 or 16) source pages each, scaled to
/// fit a grid on `paper` (orientation chosen to suit the grid), optional
/// thin border around each cell.
pub fn nup(pdf: &[u8], per_sheet: usize, paper: Paper, border: bool) -> Result<Vec<u8>, String> {
    let (cols, rows) = match per_sheet {
        2 => (2, 1),
        4 => (2, 2),
        6 => (3, 2),
        9 => (3, 3),
        16 => (4, 4),
        _ => return Err("pages per sheet must be 2, 4, 6, 9 or 16".into()),
    };
    let mut doc = load(pdf)?;
    let pages = page_ids(&doc);
    let forms = pages
        .iter()
        .map(|&p| form_xobject(&mut doc, p))
        .collect::<Result<Vec<_>, _>>()?;
    let (pw, ph) = paper.size();
    let (fw, fh) = forms[0].1;
    // try both grid and sheet orientations, keep the one with the biggest cells
    let mut best = (cols, rows, pw, ph, 0.0);
    for (c, r) in [(cols, rows), (rows, cols)] {
        for (w, h) in [(pw, ph), (ph, pw)] {
            let s = (w / c as f32 / fw).min(h / r as f32 / fh);
            if s > best.4 {
                best = (c, r, w, h, s);
            }
        }
    }
    let (c, r, w, h, _) = best;
    let (cw, ch) = (w / c as f32, h / r as f32);
    let mut kids = Vec::new();
    for chunk in forms.chunks(per_sheet) {
        let mut np = NewPage::new(w, h);
        for (i, &(xo, size)) in chunk.iter().enumerate() {
            let cell = [(i % c) as f32 * cw, h - ((i / c) + 1) as f32 * ch, cw, ch];
            np.draw(xo, fit(size, cell));
            if border {
                np.ops += &format!(
                    "q 0.5 w 0.6 G {} {} {} {} re S Q\n",
                    n(cell[0]),
                    n(cell[1]),
                    n(cw),
                    n(ch)
                );
            }
        }
        kids.push(Kid::New(np));
    }
    rebuild(doc, kids)
}

/// Saddle-stitch booklet: 2-up landscape sheets in imposition order, padded
/// with blank pages to a multiple of 4.
pub fn booklet(pdf: &[u8], paper: Paper) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let pages = page_ids(&doc);
    let forms = pages
        .iter()
        .map(|&p| form_xobject(&mut doc, p))
        .collect::<Result<Vec<_>, _>>()?;
    let total = forms.len().div_ceil(4) * 4;
    let (pw, ph) = paper.size();
    let (w, h) = (pw.max(ph), pw.min(ph));
    let mut kids = Vec::new();
    for i in 0..total / 4 {
        let front = [total - 1 - 2 * i, 2 * i];
        let back = [2 * i + 1, total - 2 - 2 * i];
        for side in [front, back] {
            let mut np = NewPage::new(w, h);
            for (half, idx) in side.into_iter().enumerate() {
                if let Some(&(xo, size)) = forms.get(idx) {
                    np.draw(xo, fit(size, [half as f32 * w / 2.0, 0.0, w / 2.0, h]));
                }
            }
            kids.push(Kid::New(np));
        }
    }
    rebuild(doc, kids)
}

/// Put `pages` onto `paper` sheets, content scaled to fit and centred.
pub fn scale_to_paper(pdf: &[u8], pages: &[usize], paper: Paper) -> Result<Vec<u8>, String> {
    let (pw, ph) = paper.size();
    transform_pages(pdf, pages, |(w, h)| {
        let (sw, sh) = if w > h { (ph, pw) } else { (pw, ph) };
        Ok(vec![((sw, sh), fit((w, h), [0.0, 0.0, sw, sh]))])
    })
}

/// Scale page content (and page size) of `pages` by `factor`.
pub fn scale_content(pdf: &[u8], pages: &[usize], factor: f32) -> Result<Vec<u8>, String> {
    if !factor.is_finite() || factor <= 0.0 {
        return Err("scale factor must be greater than 0".into());
    }
    transform_pages(pdf, pages, |(w, h)| {
        let size = (w * factor, h * factor);
        if size.0 < 1.0 || size.1 < 1.0 || size.0 > MAX_SIDE || size.1 > MAX_SIDE {
            return Err("scaled page size is out of range".into());
        }
        Ok(vec![(size, [factor, 0.0, 0.0, factor, 0.0, 0.0])])
    })
}

/// Draw pages of `other` over (or under) each page; `other` pages are
/// cycled when it is shorter.
pub fn overlay(pdf: &[u8], other: &[u8], mode: Overlay) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let mut src = load(other).map_err(|e| format!("overlay file: {e}"))?;
    src.renumber_objects_with(doc.max_id + 1);
    let src_pages = page_ids(&src);
    for &p in &src_pages {
        push_down_inherited(&mut src, p);
    }
    doc.max_id = doc.max_id.max(src.max_id);
    for (id, obj) in std::mem::take(&mut src.objects) {
        let ty = obj
            .as_dict()
            .ok()
            .and_then(|d| d.get_type().ok())
            .map(|t| t.to_vec());
        if !matches!(
            ty.as_deref(),
            Some(b"Catalog" | b"Pages" | b"ObjStm" | b"XRef")
        ) {
            doc.objects.insert(id, obj);
        }
    }
    let mut cache: HashMap<ObjectId, (ObjectId, (f32, f32))> = HashMap::new();
    for (i, page) in page_ids(&doc).into_iter().enumerate() {
        let sp = src_pages[i % src_pages.len()];
        let (xo, (_, oh)) = match cache.get(&sp) {
            Some(&v) => v,
            None => {
                let v = form_xobject(&mut doc, sp)?;
                cache.insert(sp, v);
                v
            }
        };
        let g = geom(&doc, page);
        let name = add_resource(&mut doc, page, b"XObject", "OgxOv", xo)?;
        // upright, natural size, aligned to the top-left corner
        let ops = format!(
            "q {} cm 1 0 0 1 0 {} cm /{name} Do Q\n",
            mat(g.from_upright()),
            n(g.size().1 - oh)
        );
        match mode {
            Overlay::Foreground => wrap_page(&mut doc, page, "", &ops),
            Overlay::Background => wrap_page(&mut doc, page, &ops, ""),
        }
    }
    doc.prune_objects();
    save(doc)
}

/// All pages stacked top-to-bottom onto one tall page.
pub fn single_page(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let pages = page_ids(&doc);
    let forms = pages
        .iter()
        .map(|&p| form_xobject(&mut doc, p))
        .collect::<Result<Vec<_>, _>>()?;
    let width = forms.iter().map(|f| f.1 .0).fold(0.0, f32::max);
    let height: f32 = forms.iter().map(|f| f.1 .1).sum();
    // shrink everything when the stack exceeds the maximum page size
    let s = (MAX_SIDE / height).min(MAX_SIDE / width).min(1.0);
    let mut np = NewPage::new(width * s, height * s);
    let mut y = height * s;
    for &(xo, (w, h)) in &forms {
        y -= h * s;
        np.draw(xo, [s, 0.0, 0.0, s, (width - w) * s / 2.0, y]);
    }
    rebuild(doc, vec![Kid::New(np)])
}

/// Cut each of `pages` into a cols×rows grid of separate pages (reading
/// order), e.g. 2×1 splits a two-page scan down the middle.
pub fn split_sections(
    pdf: &[u8],
    pages: &[usize],
    cols: usize,
    rows: usize,
) -> Result<Vec<u8>, String> {
    if cols == 0 || rows == 0 {
        return Err("columns and rows must be at least 1".into());
    }
    if cols * rows > 100 {
        return Err("at most 100 sections per page".into());
    }
    transform_pages(pdf, pages, |(w, h)| {
        let (cw, ch) = (w / cols as f32, h / rows as f32);
        let mut out = Vec::with_capacity(cols * rows);
        for r in 0..rows {
            for c in 0..cols {
                out.push((
                    (cw, ch),
                    [
                        1.0,
                        0.0,
                        0.0,
                        1.0,
                        -(c as f32) * cw,
                        -((rows - 1 - r) as f32) * ch,
                    ],
                ));
            }
        }
        Ok(out)
    })
}

/// Page numbers on `pages`. `format` placeholders: `{n}` number, `{total}`
/// page count of the document, `{filename}`. Numbering starts at `start`
/// on the first page of `pages`.
pub fn page_numbers(
    pdf: &[u8],
    pages: &[usize],
    format: &str,
    start: usize,
    filename: &str,
    style: &TextStyle,
    place: Placement,
) -> Result<Vec<u8>, String> {
    if format.trim().is_empty() {
        return Err("number format is empty".into());
    }
    check_style(style)?;
    let mut doc = load(pdf)?;
    let all = page_ids(&doc);
    let targets = targets(pages, all.len())?;
    let res = TextRes::new(&mut doc, style);
    for (k, &i) in targets.iter().enumerate() {
        let text = format
            .replace("{n}", &(start + k).to_string())
            .replace("{total}", &all.len().to_string())
            .replace("{filename}", filename);
        stamp_text(&mut doc, all[i], &res, &text, style, place)?;
    }
    save(doc)
}

/// A line of text stamped on `pages`.
pub fn text_stamp(
    pdf: &[u8],
    pages: &[usize],
    text: &str,
    style: &TextStyle,
    place: Placement,
) -> Result<Vec<u8>, String> {
    if text.trim().is_empty() {
        return Err("stamp text is empty".into());
    }
    check_style(style)?;
    let mut doc = load(pdf)?;
    let all = page_ids(&doc);
    let res = TextRes::new(&mut doc, style);
    for i in targets(pages, all.len())? {
        stamp_text(&mut doc, all[i], &res, text, style, place)?;
    }
    save(doc)
}

/// Text watermark on every page: centred and rotated, or `tiled` across the
/// page with `spacing` points between repeats.
pub fn watermark(
    pdf: &[u8],
    text: &str,
    style: &TextStyle,
    tiled: bool,
    spacing: f32,
) -> Result<Vec<u8>, String> {
    if text.trim().is_empty() {
        return Err("watermark text is empty".into());
    }
    check_style(style)?;
    if tiled && (!spacing.is_finite() || spacing < 0.0) {
        return Err("spacing must be 0 or more".into());
    }
    let mut doc = load(pdf)?;
    let res = TextRes::new(&mut doc, style);
    let bytes = win_ansi(text);
    let (bw, bh, ox, oy) = text_box(&bytes, style);
    for page in page_ids(&doc) {
        let (w, h) = geom(&doc, page).size();
        let mut ops = String::new();
        if tiled {
            let (sx, sy) = ((bw + spacing).max(1.0), (bh + spacing).max(1.0));
            let (nx, ny) = ((w / sx).ceil() as usize + 1, (h / sy).ceil() as usize + 1);
            if nx * ny > 5000 {
                return Err("watermark spacing too small for the text size".into());
            }
            for row in 0..ny {
                // stagger alternate rows by half a step
                let shift = if row % 2 == 1 { sx / 2.0 } else { 0.0 };
                for col in 0..nx {
                    let (cx, cy) = (col as f32 * sx + shift, row as f32 * sy + sy / 2.0);
                    ops += &res.text_op(&bytes, style, (cx - bw / 2.0 + ox, cy - bh / 2.0 + oy));
                }
            }
        } else {
            ops += &res.text_op(&bytes, style, ((w - bw) / 2.0 + ox, (h - bh) / 2.0 + oy));
        }
        let ops = res.wrap(&mut doc, page, &ops)?;
        stamp(&mut doc, page, &ops)?;
    }
    save(doc)
}

/// An image (PNG/JPEG/WebP… bytes) stamped on `pages`, `width` points wide
/// (height keeps aspect), with opacity.
pub fn image_stamp(
    pdf: &[u8],
    pages: &[usize],
    image: &[u8],
    width: f32,
    opacity: f32,
    place: Placement,
) -> Result<Vec<u8>, String> {
    if !width.is_finite() || width <= 0.0 || width > MAX_SIDE {
        return Err("image width must be greater than 0".into());
    }
    let mut doc = load(pdf)?;
    let all = page_ids(&doc);
    let targets = targets(pages, all.len())?;
    let img = image_xobject(&mut doc, image)?;
    let height = width * img.h as f32 / img.w as f32;
    let opacity = clamp01(opacity);
    let gs = (opacity < 1.0).then(|| opacity_gs(&mut doc, opacity));
    for i in targets {
        let page = all[i];
        let (x, y) = place_box(geom(&doc, page).size(), (width, height), place);
        let name = add_resource(&mut doc, page, b"XObject", "OgxIm", img.id)?;
        let mut ops = String::from("q ");
        if let Some(gs) = gs {
            ops += &format!(
                "/{} gs ",
                add_resource(&mut doc, page, b"ExtGState", "OgxGs", gs)?
            );
        }
        ops += &format!(
            "{} 0 0 {} {} {} cm /{name} Do Q\n",
            n(width),
            n(height),
            n(x),
            n(y)
        );
        stamp(&mut doc, page, &ops)?;
    }
    save(doc)
}

/// One page per image. `paper` None = page sized to the image (at 72 dpi
/// of its pixel size, capped to 200 in); Some = fit and centre with
/// `margin` points.
pub fn images_to_pdf(
    images: &[Vec<u8>],
    paper: Option<Paper>,
    margin: f32,
) -> Result<Vec<u8>, String> {
    if images.is_empty() {
        return Err("no images".into());
    }
    if !margin.is_finite() || margin < 0.0 {
        return Err("margin must be 0 or more".into());
    }
    let mut doc = Document::with_version("1.7");
    let mut kids = Vec::new();
    for (i, bytes) in images.iter().enumerate() {
        let img = image_xobject(&mut doc, bytes).map_err(|e| format!("image {}: {e}", i + 1))?;
        let (iw, ih) = (img.w as f32, img.h as f32);
        let (size, m) = match paper {
            None => {
                let s = (MAX_SIDE / iw).min(MAX_SIDE / ih).min(1.0);
                let (w, h) = ((iw * s).max(1.0), (ih * s).max(1.0));
                ((w, h), [w, 0.0, 0.0, h, 0.0, 0.0])
            }
            Some(p) => {
                let (pw, ph) = p.size();
                let (w, h) = if iw > ih { (ph, pw) } else { (pw, ph) };
                if margin * 2.0 >= w.min(h) {
                    return Err("margin is larger than the paper".into());
                }
                let cell = [margin, margin, w - 2.0 * margin, h - 2.0 * margin];
                let f = fit((iw, ih), cell);
                ((w, h), [iw * f[0], 0.0, 0.0, ih * f[3], f[4], f[5]])
            }
        };
        let mut np = NewPage::new(size.0, size.1);
        np.draw(img.id, m);
        kids.push(Kid::New(np));
    }
    rebuild(doc, kids)
}

/// A one-page blank document of the given size (points), for "insert blank
/// page".
pub fn blank_page(width: f32, height: f32) -> Result<Vec<u8>, String> {
    let ok = |v: f32| v.is_finite() && (1.0..=MAX_SIDE).contains(&v);
    if !ok(width) || !ok(height) {
        return Err("page size must be between 1 and 14400 points".into());
    }
    rebuild(
        Document::with_version("1.7"),
        vec![Kid::New(NewPage::new(width, height))],
    )
}

/// Vector signature: `strokes` are polylines in a unit box (0..1, y down)
/// drawn into `rect` (x, y, w, h in points, y from the bottom) on `page`.
pub fn draw_signature(
    pdf: &[u8],
    page: usize,
    strokes: &[Vec<(f32, f32)>],
    rect: (f32, f32, f32, f32),
    color: Rgb,
    line_width: f32,
) -> Result<Vec<u8>, String> {
    let (rx, ry, rw, rh) = rect;
    if ![rx, ry, rw, rh].iter().all(|v| v.is_finite()) || rw <= 0.0 || rh <= 0.0 {
        return Err("signature area is empty".into());
    }
    if !line_width.is_finite() || line_width <= 0.0 {
        return Err("line width must be greater than 0".into());
    }
    let mut path = String::new();
    for stroke in strokes {
        let pts: Vec<(f32, f32)> = stroke
            .iter()
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .map(|&(x, y)| (rx + x * rw, ry + (1.0 - y) * rh))
            .collect();
        let Some(&(x0, y0)) = pts.first() else {
            continue;
        };
        path += &format!("{} {} m\n", n(x0), n(y0));
        // a single tap still leaves a dot thanks to the round cap
        let rest = if pts.len() == 1 { &pts[..] } else { &pts[1..] };
        for &(x, y) in rest {
            path += &format!("{} {} l\n", n(x), n(y));
        }
    }
    if path.is_empty() {
        return Err("signature is empty".into());
    }
    let mut doc = load(pdf)?;
    let all = page_ids(&doc);
    let pid = *all
        .get(page)
        .ok_or_else(|| format!("page {} doesn't exist", page + 1))?;
    let ops = format!(
        "q {} RG {} w 1 J 1 j\n{path}S Q\n",
        rgb(color),
        n(line_width)
    );
    stamp(&mut doc, pid, &ops)?;
    save(doc)
}

// ---------------------------------------------------------------------------
// loading, saving, page tree

/// Loads (and decrypts, if it has an empty user password) a PDF.
fn load(bytes: &[u8]) -> Result<Document, String> {
    let mut doc = Document::load_mem(bytes).map_err(|_| "couldn't read the PDF".to_string())?;
    if doc.is_encrypted() {
        doc.decrypt("")
            .map_err(|_| "the PDF is password-protected".to_string())?;
    }
    if doc.get_pages().is_empty() {
        return Err("the PDF has no pages".into());
    }
    Ok(doc)
}

/// Compresses and serialises a document.
fn save(mut doc: Document) -> Result<Vec<u8>, String> {
    doc.compress();
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("couldn't write the PDF: {e}"))?;
    Ok(buf)
}

/// Page object ids in document order.
fn page_ids(doc: &Document) -> Vec<ObjectId> {
    doc.get_pages().into_values().collect()
}

/// Sorted, de-duplicated, range-checked page selection.
fn targets(pages: &[usize], count: usize) -> Result<Vec<usize>, String> {
    if pages.is_empty() {
        return Err("no pages selected".into());
    }
    let set: BTreeSet<usize> = pages.iter().copied().collect();
    if let Some(&bad) = set.iter().find(|&&p| p >= count) {
        return Err(format!("page {} doesn't exist", bad + 1));
    }
    Ok(set.into_iter().collect())
}

/// A page attribute, looked up through the page tree when inherited.
fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut node = doc.get_dictionary(page).ok()?;
    for _ in 0..64 {
        if let Ok(v) = node.get(key) {
            return Some(v);
        }
        let parent = node.get(b"Parent").ok()?.as_reference().ok()?;
        node = doc.get_dictionary(parent).ok()?;
    }
    None
}

/// Copies inherited attributes onto the page itself before it is re-parented.
fn push_down_inherited(doc: &mut Document, page: ObjectId) {
    let found: Vec<(&[u8], Object)> = INHERITABLE
        .iter()
        .filter_map(|&k| inherited(doc, page, k).map(|v| (k, v.clone())))
        .collect();
    if let Ok(dict) = doc.get_dictionary_mut(page) {
        for (k, v) in found {
            dict.set(k.to_vec(), v);
        }
    }
}

/// A page to emit: an existing page kept as is, or a new composed one.
enum Kid {
    Keep(ObjectId),
    New(NewPage),
}

/// A new page under construction: size, content and the XObjects it draws.
struct NewPage {
    size: (f32, f32),
    ops: String,
    xobjects: Vec<ObjectId>,
}

impl NewPage {
    fn new(w: f32, h: f32) -> Self {
        Self {
            size: (w, h),
            ops: String::new(),
            xobjects: Vec::new(),
        }
    }

    /// Draws XObject `xo` with matrix `m`.
    fn draw(&mut self, xo: ObjectId, m: [f32; 6]) {
        let idx = match self.xobjects.iter().position(|&x| x == xo) {
            Some(i) => i,
            None => {
                self.xobjects.push(xo);
                self.xobjects.len() - 1
            }
        };
        self.ops += &format!("q {} cm /X{idx} Do Q\n", mat(m));
    }
}

/// Replaces the document's page tree with `kids` under a fresh catalog,
/// drops everything no longer referenced and serialises.
fn rebuild(mut doc: Document, kids: Vec<Kid>) -> Result<Vec<u8>, String> {
    let pages_id = doc.new_object_id();
    let mut refs = Vec::with_capacity(kids.len());
    for kid in kids {
        let id = match kid {
            Kid::Keep(id) => {
                push_down_inherited(&mut doc, id);
                id
            }
            Kid::New(np) => {
                let mut xobjects = Dictionary::new();
                for (i, xo) in np.xobjects.iter().enumerate() {
                    xobjects.set(format!("X{i}"), *xo);
                }
                let contents = doc.add_object(Stream::new(Dictionary::new(), np.ops.into_bytes()));
                doc.add_object(dictionary! {
                    "Type" => "Page",
                    "MediaBox" => reals([0.0, 0.0, np.size.0, np.size.1]),
                    "Resources" => dictionary! { "XObject" => xobjects },
                    "Contents" => contents,
                })
            }
        };
        doc.get_dictionary_mut(id)
            .map_err(|_| "broken page object")?
            .set("Parent", pages_id);
        refs.push(Object::Reference(id));
    }
    let count = refs.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => refs, "Count" => count }),
    );
    // outlines, named destinations and the structure tree point at the old
    // pages, so only harmless catalog entries survive
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if let Ok(old) = doc.catalog() {
        for key in [&b"Metadata"[..], b"Lang", b"ViewerPreferences"] {
            if let Ok(v) = old.get(key) {
                catalog.set(key.to_vec(), v.clone());
            }
        }
    }
    let catalog = doc.add_object(catalog);
    doc.trailer.set("Root", catalog);
    doc.prune_objects();
    save(doc)
}

/// Rebuilds the document replacing each selected page with the pages `f`
/// returns for its upright size: (new page size, matrix for the page XObject).
fn transform_pages(
    pdf: &[u8],
    pages: &[usize],
    f: impl Fn((f32, f32)) -> Result<Vec<((f32, f32), [f32; 6])>, String>,
) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let all = page_ids(&doc);
    let selected = targets(pages, all.len())?;
    let mut kids = Vec::new();
    for (i, &page) in all.iter().enumerate() {
        if selected.binary_search(&i).is_err() {
            kids.push(Kid::Keep(page));
            continue;
        }
        let (xo, size) = form_xobject(&mut doc, page)?;
        for ((w, h), m) in f(size)? {
            let mut np = NewPage::new(w, h);
            np.draw(xo, m);
            kids.push(Kid::New(np));
        }
    }
    rebuild(doc, kids)
}

// ---------------------------------------------------------------------------
// geometry

/// A page's visible box (CropBox ∩ MediaBox) and its /Rotate.
struct Geom {
    b: [f32; 4],
    rot: i64,
}

impl Geom {
    /// Size as the reader sees it (after rotation).
    fn size(&self) -> (f32, f32) {
        let (w, h) = (self.b[2] - self.b[0], self.b[3] - self.b[1]);
        if self.rot % 180 == 0 {
            (w, h)
        } else {
            (h, w)
        }
    }

    /// Maps page user space to upright space with the origin at the
    /// bottom-left of the visible box.
    fn to_upright(&self) -> [f32; 6] {
        let [x0, y0, x1, y1] = self.b;
        match self.rot {
            90 => [0.0, -1.0, 1.0, 0.0, -y0, x1],
            180 => [-1.0, 0.0, 0.0, -1.0, x1, y1],
            270 => [0.0, 1.0, -1.0, 0.0, y1, -x0],
            _ => [1.0, 0.0, 0.0, 1.0, -x0, -y0],
        }
    }

    /// Inverse of [`Geom::to_upright`].
    fn from_upright(&self) -> [f32; 6] {
        let [x0, y0, x1, y1] = self.b;
        match self.rot {
            90 => [0.0, 1.0, -1.0, 0.0, x1, y0],
            180 => [-1.0, 0.0, 0.0, -1.0, x1, y1],
            270 => [0.0, -1.0, 1.0, 0.0, x0, y1],
            _ => [1.0, 0.0, 0.0, 1.0, x0, y0],
        }
    }
}

/// A number object (direct or referenced) as f32.
fn num(doc: &Document, o: &Object) -> Option<f32> {
    match doc.dereference(o).ok()?.1 {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) if r.is_finite() => Some(*r),
        _ => None,
    }
}

/// A rectangle array, normalised; None when malformed or empty.
fn rect(doc: &Document, o: &Object) -> Option<[f32; 4]> {
    let arr = doc.dereference(o).ok()?.1.as_array().ok()?;
    let v: Vec<f32> = arr
        .iter()
        .take(4)
        .map(|o| num(doc, o))
        .collect::<Option<_>>()?;
    let r = [
        v.first()?.min(*v.get(2)?),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ];
    (r[2] - r[0] >= 1.0 && r[3] - r[1] >= 1.0).then_some(r)
}

fn geom(doc: &Document, page: ObjectId) -> Geom {
    let media = inherited(doc, page, b"MediaBox")
        .and_then(|o| rect(doc, o))
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = inherited(doc, page, b"CropBox").and_then(|o| rect(doc, o));
    let b = crop
        .map(|c| {
            [
                c[0].max(media[0]),
                c[1].max(media[1]),
                c[2].min(media[2]),
                c[3].min(media[3]),
            ]
        })
        .filter(|b| b[2] - b[0] >= 1.0 && b[3] - b[1] >= 1.0)
        .unwrap_or(media);
    let rot = inherited(doc, page, b"Rotate")
        .and_then(|o| num(doc, o))
        .unwrap_or(0.0);
    let rot = ((rot / 90.0).round() as i64 * 90).rem_euclid(360);
    Geom { b, rot }
}

/// Matrix scaling `size` uniformly to fit inside `cell` (x, y, w, h), centred.
fn fit(size: (f32, f32), cell: [f32; 4]) -> [f32; 6] {
    let s = (cell[2] / size.0).min(cell[3] / size.1);
    [
        s,
        0.0,
        0.0,
        s,
        cell[0] + (cell[2] - size.0 * s) / 2.0,
        cell[1] + (cell[3] - size.1 * s) / 2.0,
    ]
}

/// Bottom-left corner of a `bx`×`by` box anchored on a `w`×`h` page.
fn place_box((w, h): (f32, f32), (bw, bh): (f32, f32), place: Placement) -> (f32, f32) {
    use Anchor::*;
    let m = if place.margin.is_finite() {
        place.margin
    } else {
        0.0
    };
    let x = match place.anchor {
        TopLeft | Left | BottomLeft => m,
        Top | Center | Bottom => (w - bw) / 2.0,
        TopRight | Right | BottomRight => w - m - bw,
    };
    let y = match place.anchor {
        BottomLeft | Bottom | BottomRight => m,
        Left | Center | Right => (h - bh) / 2.0,
        TopLeft | Top | TopRight => h - m - bh,
    };
    let off = |v: f32| if v.is_finite() { v } else { 0.0 };
    (x + off(place.offset.0), y + off(place.offset.1))
}

/// Compact PDF number.
fn n(v: f32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

/// Six numbers of a `cm` operand list.
fn mat(m: [f32; 6]) -> String {
    m.iter().map(|&v| n(v)).collect::<Vec<_>>().join(" ")
}

fn reals<const N: usize>(v: [f32; N]) -> Vec<Object> {
    v.iter().map(|&x| Object::Real(x)).collect()
}

fn clamp01(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Colour components as operands.
fn rgb(c: Rgb) -> String {
    c.iter()
        .map(|&v| n(clamp01(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// XObjects and page editing

/// Turns a page into a Form XObject that shows its visible box upright at
/// the origin; returns the id and the upright size.
fn form_xobject(doc: &mut Document, page: ObjectId) -> Result<(ObjectId, (f32, f32)), String> {
    let g = geom(doc, page);
    let res = inherited(doc, page, b"Resources")
        .cloned()
        .unwrap_or_else(|| Object::Dictionary(Dictionary::new()));
    let content = doc
        .get_page_content_with_limit(page, MAX_CONTENT)
        .map_err(|_| "page content is too large".to_string())?;
    let dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "BBox" => reals(g.b),
        "Matrix" => reals(g.to_upright()),
        "Resources" => res,
    };
    let mut stream = Stream::new(dict, content);
    let _ = stream.compress();
    Ok((doc.add_object(stream), g.size()))
}

/// Registers `id` in the page's `category` resources under a fresh name.
/// Inherited/referenced resource dictionaries are copied onto the page
/// first, so other pages are unaffected.
fn add_resource(
    doc: &mut Document,
    page: ObjectId,
    category: &[u8],
    prefix: &str,
    id: ObjectId,
) -> Result<String, String> {
    let deref = |o: &Object| {
        doc.dereference(o)
            .ok()
            .and_then(|(_, o)| o.as_dict().ok().cloned())
    };
    let mut res = inherited(doc, page, b"Resources")
        .and_then(deref)
        .unwrap_or_default();
    let mut cat = res.get(category).ok().and_then(deref).unwrap_or_default();
    let name = (1..)
        .map(|i| format!("{prefix}{i}"))
        .find(|k| !cat.has(k.as_bytes()))
        .unwrap_or_default();
    cat.set(name.clone(), id);
    res.set(category.to_vec(), cat);
    doc.get_dictionary_mut(page)
        .map_err(|_| "broken page object")?
        .set("Resources", res);
    Ok(name)
}

/// Rewrites the page's contents as `before` + `q` + existing + `Q` + `after`,
/// so the old content's graphics state can't leak into ours.
fn wrap_page(doc: &mut Document, page: ObjectId, before: &str, after: &str) {
    let existing = doc.get_page_contents(page);
    let head = doc.add_object(Stream::new(
        Dictionary::new(),
        format!("{before}q\n").into_bytes(),
    ));
    let tail = doc.add_object(Stream::new(
        Dictionary::new(),
        format!("\nQ\n{after}").into_bytes(),
    ));
    let mut contents = vec![Object::Reference(head)];
    contents.extend(existing.into_iter().map(Object::Reference));
    contents.push(Object::Reference(tail));
    if let Ok(dict) = doc.get_dictionary_mut(page) {
        dict.set("Contents", contents);
    }
}

/// Appends `ops`, written in upright page coordinates, to the page.
fn stamp(doc: &mut Document, page: ObjectId, ops: &str) -> Result<(), String> {
    let m = geom(doc, page).from_upright();
    doc.get_dictionary(page).map_err(|_| "broken page object")?;
    wrap_page(doc, page, "", &format!("q {} cm\n{ops}Q\n", mat(m)));
    Ok(())
}

/// ExtGState object for fill and stroke opacity.
fn opacity_gs(doc: &mut Document, opacity: f32) -> ObjectId {
    doc.add_object(dictionary! { "Type" => "ExtGState", "ca" => Object::Real(opacity), "CA" => Object::Real(opacity) })
}

/// A decoded image added as an Image XObject.
struct Img {
    id: ObjectId,
    w: u32,
    h: u32,
}

/// Width, height and component count of a baseline/progressive 8-bit JPEG.
fn jpeg_info(b: &[u8]) -> Option<(u32, u32, u8)> {
    if b.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let s = b.get(i + 4..i + 10)?;
            let (h, w) = (
                u16::from_be_bytes([s[1], s[2]]),
                u16::from_be_bytes([s[3], s[4]]),
            );
            return (s[0] == 8 && w > 0 && h > 0).then_some((w as u32, h as u32, s[5]));
        }
        i += 2 + len;
    }
    None
}

/// Adds image bytes as an Image XObject: grey/RGB JPEGs pass through as
/// DCTDecode, everything else is decoded to RGB (+ /SMask for alpha).
fn image_xobject(doc: &mut Document, bytes: &[u8]) -> Result<Img, String> {
    if let Some((w, h, comps @ (1 | 3))) = jpeg_info(bytes) {
        // make sure it actually decodes before passing it through
        image::load_from_memory(bytes).map_err(|_| "damaged JPEG image".to_string())?;
        let cs = if comps == 1 {
            "DeviceGray"
        } else {
            "DeviceRGB"
        };
        let dict = dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
            "ColorSpace" => cs, "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        };
        let id = doc.add_object(Stream::new(dict, bytes.to_vec()).with_compression(false));
        return Ok(Img { id, w, h });
    }
    let img = image::load_from_memory(bytes)
        .map_err(|_| "unsupported or damaged image".to_string())?
        .to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err("image is empty".into());
    }
    let px = img.into_raw();
    let rgb: Vec<u8> = px
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let alpha: Vec<u8> = px.chunks_exact(4).map(|p| p[3]).collect();
    let image_dict = |cs: &str| {
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
            "ColorSpace" => cs, "BitsPerComponent" => 8,
        }
    };
    let mut dict = image_dict("DeviceRGB");
    if alpha.iter().any(|&a| a < 255) {
        let mut s = Stream::new(image_dict("DeviceGray"), alpha);
        let _ = s.compress();
        dict.set("SMask", doc.add_object(s));
    }
    let mut s = Stream::new(dict, rgb);
    let _ = s.compress();
    Ok(Img {
        id: doc.add_object(s),
        w,
        h,
    })
}

// ---------------------------------------------------------------------------
// text

/// Validates a text style.
fn check_style(style: &TextStyle) -> Result<(), String> {
    if !style.size.is_finite() || style.size <= 0.0 || style.size > 1000.0 {
        return Err("font size must be between 0 and 1000".into());
    }
    if !style.rotation.is_finite() {
        return Err("invalid rotation".into());
    }
    Ok(())
}

/// Shared font / opacity objects for one stamping run.
struct TextRes {
    font: ObjectId,
    gs: Option<ObjectId>,
    color: String,
}

impl TextRes {
    fn new(doc: &mut Document, style: &TextStyle) -> Self {
        let base = match style.font {
            Font::Helvetica => "Helvetica",
            Font::HelveticaBold => "Helvetica-Bold",
            Font::TimesRoman => "Times-Roman",
            Font::TimesBold => "Times-Bold",
            Font::Courier => "Courier",
            Font::CourierBold => "Courier-Bold",
        };
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => base, "Encoding" => "WinAnsiEncoding",
        });
        let opacity = clamp01(style.opacity);
        let gs = (opacity < 1.0).then(|| opacity_gs(doc, opacity));
        Self {
            font,
            gs,
            color: rgb(style.color),
        }
    }

    /// One text-showing block with its baseline origin at `(x, y)`; uses the
    /// placeholder font name `/OGXFONT`, replaced by [`TextRes::wrap`].
    fn text_op(&self, bytes: &[u8], style: &TextStyle, (x, y): (f32, f32)) -> String {
        let (sin, cos) = style.rotation.to_radians().sin_cos();
        let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        format!(
            "BT /OGXFONT {} Tf {} {} {} {} {} {} Tm <{hex}> Tj ET\n",
            n(style.size),
            n(cos),
            n(sin),
            n(-sin),
            n(cos),
            n(x),
            n(y)
        )
    }

    /// Registers the font (and opacity) on the page and wraps text ops with
    /// colour and graphics state.
    fn wrap(&self, doc: &mut Document, page: ObjectId, ops: &str) -> Result<String, String> {
        let f = add_resource(doc, page, b"Font", "OgxF", self.font)?;
        let gs = match self.gs {
            Some(gs) => format!(
                "/{} gs ",
                add_resource(doc, page, b"ExtGState", "OgxGs", gs)?
            ),
            None => String::new(),
        };
        Ok(format!(
            "q {gs}{} rg\n{}Q\n",
            self.color,
            ops.replace("/OGXFONT ", &format!("/{f} "))
        ))
    }
}

/// Stamps one anchored line of text on a page.
fn stamp_text(
    doc: &mut Document,
    page: ObjectId,
    res: &TextRes,
    text: &str,
    style: &TextStyle,
    place: Placement,
) -> Result<(), String> {
    let bytes = win_ansi(text);
    let (bw, bh, ox, oy) = text_box(&bytes, style);
    let (x, y) = place_box(geom(doc, page).size(), (bw, bh), place);
    let ops = res.wrap(doc, page, &res.text_op(&bytes, style, (x + ox, y + oy)))?;
    stamp(doc, page, &ops)
}

/// Size of the rotated text's bounding box and the offset from the box's
/// bottom-left corner to the text origin.
fn text_box(bytes: &[u8], style: &TextStyle) -> (f32, f32, f32, f32) {
    let w = bytes
        .iter()
        .map(|&b| char_width(style.font, b) as f32)
        .sum::<f32>()
        * style.size
        / 1000.0;
    let (desc, asc) = (-0.22 * style.size, 0.72 * style.size);
    let (sin, cos) = style.rotation.to_radians().sin_cos();
    let pts = [(0.0, desc), (w, desc), (0.0, asc), (w, asc)]
        .map(|(x, y)| (x * cos - y * sin, x * sin + y * cos));
    let min_x = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    let max_x = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let min_y = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let max_y = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    (max_x - min_x, max_y - min_y, -min_x, -min_y)
}

/// Encodes text as WinAnsi bytes; unencodable characters become `?` and
/// control characters spaces.
fn win_ansi(text: &str) -> Vec<u8> {
    const HIGH: [(char, u8); 27] = [
        ('€', 0x80),
        ('‚', 0x82),
        ('ƒ', 0x83),
        ('„', 0x84),
        ('…', 0x85),
        ('†', 0x86),
        ('‡', 0x87),
        ('ˆ', 0x88),
        ('‰', 0x89),
        ('Š', 0x8A),
        ('‹', 0x8B),
        ('Œ', 0x8C),
        ('Ž', 0x8E),
        ('\u{2018}', 0x91),
        ('\u{2019}', 0x92),
        ('\u{201C}', 0x93),
        ('\u{201D}', 0x94),
        ('•', 0x95),
        ('–', 0x96),
        ('—', 0x97),
        ('˜', 0x98),
        ('™', 0x99),
        ('š', 0x9A),
        ('›', 0x9B),
        ('œ', 0x9C),
        ('ž', 0x9E),
        ('Ÿ', 0x9F),
    ];
    text.chars()
        .map(|c| match c as u32 {
            0..=0x1F | 0x7F => b' ',
            0x20..=0x7E | 0xA0..=0xFF => c as u8,
            _ => HIGH.iter().find(|(h, _)| *h == c).map_or(b'?', |&(_, b)| b),
        })
        .collect()
}

/// AFM advance width (1/1000 em) of a WinAnsi byte; non-ASCII bytes use an
/// average width.
fn char_width(font: Font, b: u8) -> u16 {
    let (table, fallback) = match font {
        Font::Courier | Font::CourierBold => return 600,
        Font::Helvetica => (&HELVETICA, 556),
        Font::HelveticaBold => (&HELVETICA_BOLD, 611),
        Font::TimesRoman => (&TIMES, 500),
        Font::TimesBold => (&TIMES_BOLD, 500),
    };
    match b {
        32..=126 => table[(b - 32) as usize],
        _ => fallback,
    }
}

#[rustfmt::skip]
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];
#[rustfmt::skip]
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611,
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556,
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611,
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];
#[rustfmt::skip]
const TIMES: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444,
    921, 722, 667, 667, 722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722,
    556, 722, 667, 556, 611, 722, 722, 944, 722, 722, 611, 333, 278, 333, 469, 500,
    333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500, 278, 778, 500, 500,
    500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
];
#[rustfmt::skip]
const TIMES_BOLD: [u16; 95] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278,
    500, 500, 500, 500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500,
    930, 722, 667, 722, 722, 667, 611, 778, 778, 389, 500, 778, 667, 944, 722, 778,
    611, 778, 722, 556, 667, 722, 722, 1000, 722, 722, 667, 333, 278, 333, 581, 500,
    333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556, 278, 833, 556, 500,
    556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
];

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};

    /// n-page Letter PDF with MediaBox/Resources inherited from the Pages
    /// node; page 2 has /Rotate 90 and page 3 its own smaller CropBox.
    fn sample(n: usize) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
        );
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        for i in 0..n {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new(
                        "Tj",
                        vec![Object::string_literal(format!("page {}", i + 1))],
                    ),
                    Operation::new("ET", vec![]),
                ],
            };
            let c = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let mut page = dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c };
            if i == 1 {
                page.set("Rotate", 90);
            }
            if i == 2 {
                page.set("CropBox", vec![0.into(), 0.into(), 500.into(), 792.into()]);
            }
            kids.push(doc.add_object(page).into());
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => n as i64,
                "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    /// Reloads with lopdf, parses and renders every page with hayro, and
    /// returns the lopdf document.
    fn check(bytes: &[u8], pages: usize) -> Document {
        let doc = Document::load_mem(bytes).unwrap();
        assert_eq!(doc.get_pages().len(), pages);
        let pdf = hayro::hayro_syntax::Pdf::new(bytes.to_vec()).unwrap();
        assert_eq!(pdf.pages().len(), pages);
        let cache = hayro::RenderCache::new();
        let settings = hayro::hayro_interpret::InterpreterSettings::default();
        for page in pdf.pages().iter() {
            let pm = hayro::render(page, &cache, &settings, &hayro::RenderSettings::default());
            assert!(pm.width() > 0 && pm.height() > 0);
        }
        doc
    }

    fn media(doc: &Document, page: usize) -> (f32, f32) {
        let id = page_ids(doc)[page];
        let g = geom(doc, id);
        (g.b[2] - g.b[0], g.b[3] - g.b[1])
    }

    fn text(doc: &Document, page: usize) -> String {
        doc.extract_text(&[page as u32 + 1]).unwrap()
    }

    /// Source page numbers drawn on an output page, in draw order, with the
    /// x translation of each `cm` before the `Do`.
    fn drawn(doc: &Document, page: usize) -> Vec<(usize, f32)> {
        let id = page_ids(doc)[page];
        let content = Content::decode(&doc.get_page_content(id)).unwrap();
        let xobjects = doc
            .get_dictionary(id)
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"XObject")
            .unwrap()
            .as_dict()
            .unwrap();
        let mut last_x = 0.0;
        let mut out = Vec::new();
        for op in content.operations {
            match op.operator.as_str() {
                "cm" => last_x = op.operands[4].as_float().unwrap(),
                "Do" => {
                    let xo = xobjects
                        .get(op.operands[0].as_name().unwrap())
                        .unwrap()
                        .as_reference()
                        .unwrap();
                    let s = doc.get_object(xo).unwrap().as_stream().unwrap();
                    let body = String::from_utf8_lossy(
                        &s.decompressed_content().unwrap_or(s.content.clone()),
                    )
                    .to_string();
                    let num: String = body
                        .split("page ")
                        .nth(1)
                        .unwrap()
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    out.push((num.parse().unwrap(), last_x));
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn nup_groups_pages() {
        let out = nup(&sample(9), 4, Paper::A4, true).unwrap();
        let doc = check(&out, 3);
        assert_eq!(media(&doc, 0), (595.28, 841.89));
        assert_eq!(
            drawn(&doc, 0).iter().map(|d| d.0).collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert_eq!(drawn(&doc, 2).iter().map(|d| d.0).collect::<Vec<_>>(), [9]);
        // 2-up of portrait pages goes landscape
        let doc = check(&nup(&sample(3), 2, Paper::A4, false).unwrap(), 2);
        assert_eq!(media(&doc, 0), (841.89, 595.28));
        assert!(nup(&sample(3), 3, Paper::A4, false).is_err());
        assert!(nup(b"not a pdf", 4, Paper::A4, false).is_err());
    }

    #[test]
    fn booklet_imposition() {
        let doc = check(&booklet(&sample(8), Paper::A4).unwrap(), 4);
        assert_eq!(media(&doc, 0), (841.89, 595.28));
        let order: Vec<Vec<usize>> = (0..4)
            .map(|p| drawn(&doc, p).iter().map(|d| d.0).collect())
            .collect();
        assert_eq!(order, [vec![8, 1], vec![2, 7], vec![6, 3], vec![4, 5]]);
        // 5 pages pad to 8: the first side only has page 1, on the right half
        let doc = check(&booklet(&sample(5), Paper::A4).unwrap(), 4);
        let first = drawn(&doc, 0);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0, 1);
        assert!(first[0].1 >= 841.89 / 2.0 - 0.01);
    }

    #[test]
    fn scale_to_paper_sizes() {
        let doc = check(&scale_to_paper(&sample(3), &[0, 1], Paper::A4).unwrap(), 3);
        assert_eq!(media(&doc, 0), (595.28, 841.89));
        // rotated page is landscape to the reader, so gets landscape A4
        assert_eq!(media(&doc, 1), (841.89, 595.28));
        assert!(doc
            .get_dictionary(page_ids(&doc)[1])
            .unwrap()
            .get(b"Rotate")
            .is_err());
        // untouched page keeps its size and text
        assert_eq!(media(&doc, 2), (500.0, 792.0));
        assert!(text(&doc, 2).contains("page 3"));
        assert!(scale_to_paper(&sample(3), &[5], Paper::A4).is_err());
        assert!(scale_to_paper(&sample(3), &[], Paper::A4).is_err());
    }

    #[test]
    fn scale_content_sizes() {
        let doc = check(&scale_content(&sample(2), &[0], 0.5).unwrap(), 2);
        assert_eq!(media(&doc, 0), (306.0, 396.0));
        assert_eq!(media(&doc, 1), (612.0, 792.0));
        assert!(scale_content(&sample(2), &[0], 0.0).is_err());
        assert!(scale_content(&sample(2), &[0], f32::NAN).is_err());
        assert!(scale_content(&sample(2), &[0], 100.0).is_err());
    }

    #[test]
    fn overlay_cycles_other() {
        let out = overlay(&sample(3), &sample(1), Overlay::Foreground).unwrap();
        let doc = check(&out, 3);
        for p in 0..3 {
            assert_eq!(drawn(&doc, p).iter().map(|d| d.0).collect::<Vec<_>>(), [1]);
            assert!(text(&doc, p).contains(&format!("page {}", p + 1)));
        }
        let doc = check(
            &overlay(&sample(3), &sample(2), Overlay::Background).unwrap(),
            3,
        );
        assert_eq!(drawn(&doc, 1)[0].0, 2);
        assert_eq!(drawn(&doc, 2)[0].0, 1);
        assert!(overlay(&sample(1), b"junk", Overlay::Background).is_err());
    }

    #[test]
    fn single_page_stacks() {
        let doc = check(&single_page(&sample(3)).unwrap(), 1);
        // 792 + 612 (rotated) + 792; widest page is the rotated one (792)
        let (w, h) = media(&doc, 0);
        assert!(
            (w - 792.0).abs() < 0.01 && (h - 2196.0).abs() < 0.01,
            "{w}x{h}"
        );
        assert_eq!(
            drawn(&doc, 0).iter().map(|d| d.0).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn split_sections_grid() {
        let doc = check(&split_sections(&sample(2), &[0], 2, 1).unwrap(), 3);
        assert_eq!(media(&doc, 0), (306.0, 792.0));
        assert_eq!(media(&doc, 1), (306.0, 792.0));
        assert_eq!(media(&doc, 2), (612.0, 792.0));
        let doc = check(&split_sections(&sample(1), &[0], 2, 3).unwrap(), 6);
        assert_eq!(media(&doc, 5), (306.0, 264.0));
        assert!(split_sections(&sample(1), &[0], 0, 1).is_err());
    }

    #[test]
    fn page_numbers_stamped() {
        let style = TextStyle::default();
        let out = page_numbers(
            &sample(3),
            &[0, 1, 2],
            "Page {n} of {total} {filename}",
            1,
            "doc.pdf",
            &style,
            Placement::default(),
        )
        .unwrap();
        let doc = check(&out, 3);
        assert!(text(&doc, 0).contains("Page 1 of 3 doc.pdf"));
        assert!(text(&doc, 1).contains("Page 2 of 3"));
        assert!(text(&doc, 2).contains("page 3"), "original text kept");
        let out =
            page_numbers(&sample(3), &[2], "{n}", 7, "", &style, Placement::default()).unwrap();
        let doc = check(&out, 3);
        assert!(text(&doc, 2).contains('7'));
        assert!(page_numbers(&sample(1), &[0], " ", 1, "", &style, Placement::default()).is_err());
    }

    #[test]
    fn text_stamp_all_anchors_and_fonts() {
        for (i, anchor) in Anchor::ALL.into_iter().enumerate() {
            let style = TextStyle {
                font: Font::ALL[i % 6],
                rotation: 30.0,
                opacity: 0.5,
                ..Default::default()
            };
            let place = Placement {
                anchor,
                margin: 10.0,
                offset: (2.0, -3.0),
            };
            let out = text_stamp(&sample(2), &[1], "Approved – ok", &style, place).unwrap();
            let doc = check(&out, 2);
            assert!(text(&doc, 1).contains("Approved"));
            assert!(!text(&doc, 0).contains("Approved"));
        }
        assert!(text_stamp(
            &sample(1),
            &[0],
            "",
            &TextStyle::default(),
            Placement::default()
        )
        .is_err());
        let bad = TextStyle {
            size: -1.0,
            ..Default::default()
        };
        assert!(text_stamp(&sample(1), &[0], "x", &bad, Placement::default()).is_err());
    }

    #[test]
    fn watermark_has_opacity() {
        let style = TextStyle {
            size: 48.0,
            opacity: 0.3,
            rotation: 45.0,
            color: [1.0, 0.0, 0.0],
            ..Default::default()
        };
        for tiled in [false, true] {
            let doc = check(
                &watermark(&sample(2), "DRAFT", &style, tiled, 40.0).unwrap(),
                2,
            );
            assert!(text(&doc, 1).contains("DRAFT"));
            let has_gs = doc.objects.values().any(|o| {
                o.as_dict().is_ok_and(|d| {
                    d.has_type(b"ExtGState")
                        && d.get(b"ca")
                            .and_then(Object::as_float)
                            .is_ok_and(|v| (v - 0.3).abs() < 1e-4)
                })
            });
            assert!(has_gs);
        }
        assert!(watermark(&sample(1), "x", &style, true, -1.0).is_err());
    }

    fn png_with_alpha() -> Vec<u8> {
        let img = image::RgbaImage::from_fn(4, 2, |x, _| {
            image::Rgba([255, 0, 0, if x < 2 { 128 } else { 255 }])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 4, image::Rgb([0, 128, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    fn images(doc: &Document) -> Vec<&Stream> {
        doc.objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .filter(|s| {
                s.dict
                    .get(b"Subtype")
                    .and_then(Object::as_name)
                    .is_ok_and(|n| n == b"Image")
                    && !s.dict.has(b"Decode")
            })
            .collect()
    }

    #[test]
    fn image_stamp_adds_image() {
        let place = Placement {
            anchor: Anchor::TopRight,
            ..Default::default()
        };
        let doc = check(
            &image_stamp(&sample(2), &[0, 1], &png_with_alpha(), 100.0, 0.5, place).unwrap(),
            2,
        );
        let imgs = images(&doc);
        assert!(imgs.iter().any(|s| s.dict.has(b"SMask")));
        assert!(imgs.iter().all(|s| s
            .dict
            .get(b"Filter")
            .and_then(Object::as_name)
            .map_or(true, |f| f != b"DCTDecode")));
        let doc = check(
            &image_stamp(&sample(1), &[0], &jpeg(), 50.0, 1.0, place).unwrap(),
            1,
        );
        assert!(images(&doc).iter().any(|s| s
            .dict
            .get(b"Filter")
            .and_then(Object::as_name)
            .is_ok_and(|f| f == b"DCTDecode")));
        assert!(image_stamp(&sample(1), &[0], b"nope", 50.0, 1.0, place).is_err());
        assert!(image_stamp(&sample(1), &[0], &jpeg(), 0.0, 1.0, place).is_err());
    }

    #[test]
    fn images_to_pdf_pages() {
        let imgs = vec![png_with_alpha(), jpeg()];
        let doc = check(&images_to_pdf(&imgs, None, 0.0).unwrap(), 2);
        assert_eq!(media(&doc, 0), (4.0, 2.0));
        assert_eq!(media(&doc, 1), (8.0, 4.0));
        let doc = check(&images_to_pdf(&imgs, Some(Paper::A4), 36.0).unwrap(), 2);
        assert_eq!(media(&doc, 0), (841.89, 595.28)); // landscape image
        assert!(images_to_pdf(&[], None, 0.0).is_err());
        assert!(images_to_pdf(&imgs, Some(Paper::A4), 400.0).is_err());
        assert!(images_to_pdf(&[b"x".to_vec()], None, 0.0).is_err());
    }

    #[test]
    fn blank_page_size() {
        let doc = check(&blank_page(300.0, 400.0).unwrap(), 1);
        assert_eq!(media(&doc, 0), (300.0, 400.0));
        assert!(blank_page(0.0, 10.0).is_err());
        assert!(blank_page(f32::INFINITY, 10.0).is_err());
    }

    #[test]
    fn draw_signature_strokes() {
        let strokes = vec![vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)], vec![(0.2, 0.2)]];
        let out = draw_signature(
            &sample(2),
            1,
            &strokes,
            (100.0, 100.0, 200.0, 60.0),
            [0.0, 0.0, 0.5],
            1.5,
        )
        .unwrap();
        let doc = check(&out, 2);
        let id = page_ids(&doc)[1];
        let ops: Vec<String> = Content::decode(&doc.get_page_content(id))
            .unwrap()
            .operations
            .into_iter()
            .map(|o| o.operator)
            .collect();
        for op in ["m", "l", "S", "J", "j", "RG"] {
            assert!(ops.iter().any(|o| o == op), "missing {op}");
        }
        assert!(draw_signature(
            &sample(1),
            3,
            &strokes,
            (0.0, 0.0, 10.0, 10.0),
            [0.0; 3],
            1.0
        )
        .is_err());
        assert!(draw_signature(&sample(1), 0, &[], (0.0, 0.0, 10.0, 10.0), [0.0; 3], 1.0).is_err());
        assert!(draw_signature(
            &sample(1),
            0,
            &strokes,
            (0.0, 0.0, 0.0, 10.0),
            [0.0; 3],
            1.0
        )
        .is_err());
    }

    #[test]
    fn rotation_matrices_invert() {
        for rot in [0, 90, 180, 270] {
            let g = Geom {
                b: [10.0, 20.0, 110.0, 220.0],
                rot,
            };
            let (a, b) = (g.to_upright(), g.from_upright());
            let apply = |m: [f32; 6], (x, y): (f32, f32)| {
                (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
            };
            let (w, h) = g.size();
            for p in [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)] {
                let user = apply(b, p);
                assert!(
                    user.0 >= 10.0 - 1e-3
                        && user.0 <= 110.0 + 1e-3
                        && user.1 >= 20.0 - 1e-3
                        && user.1 <= 220.0 + 1e-3
                );
                let back = apply(a, user);
                assert!((back.0 - p.0).abs() < 1e-3 && (back.1 - p.1).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn win_ansi_encoding() {
        assert_eq!(
            win_ansi("A€é\u{4e2d}\n"),
            vec![b'A', 0x80, 0xE9, b'?', b' ']
        );
    }
}
