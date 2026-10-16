//! OCR → searchable PDF, fully offline.
//!
//! Pages are rendered with hayro (150 dpi, long side capped), read by the
//! `ocrs` engine (pure Rust, `rten` runtime; detection + recognition models
//! embedded from `assets/ocr/`), and each recognised word is written back
//! as **invisible text** (render mode 3) positioned over its box. The
//! page's own content is left byte-for-byte untouched: the old content
//! streams are wrapped in `q … Q` and one new stream with the text layer is
//! appended.
//!
//! The text layer uses a small Type 3 font with empty glyphs, a uniform
//! 0.5 em advance and a `/ToUnicode` CMap, so any recognised character
//! extracts correctly; `Tz` stretches each word to its box width so text
//! selection lines up with the scan.
//!
//! Limits:
//!   * the bundled models read Latin script without diacritics: A–Z, a–z,
//!     0–9 and ASCII punctuation. Accented letters come out unaccented;
//!     other scripts aren't recognised ([`check_languages`]).
//!   * text is assumed horizontal; skewed lines get an axis-aligned box.
//!   * "force" OCRs pages that already have text but doesn't remove that
//!     text, so extraction may then return it twice.

use super::raster::{open_hayro, render_page, select};
use hayro::RenderCache;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use std::collections::BTreeMap;
use std::sync::OnceLock;

static DETECTION_MODEL: &[u8] = include_bytes!("../../../../assets/ocr/text-detection.rten");
static RECOGNITION_MODEL: &[u8] = include_bytes!("../../../../assets/ocr/text-recognition.rten");

/// Render resolution. The bundled models read best with body text ~20–40
/// px tall (10–14 pt at 150 dpi); 200–300 dpi measurably lowered accuracy
/// in testing (dropped letters on 14–30 pt text).
const OCR_DPI: f32 = 150.0;
/// Longest bitmap side handed to the detector (A4 at 150 dpi is 1754).
const MAX_LONG_SIDE: f32 = 4200.0;
/// Type 3 glyph advance in glyph space (FontMatrix 0.001).
const ADVANCE: f32 = 500.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// leave pages that already have a text layer alone
    Skip,
    /// OCR every requested page
    Force,
}

/// Language codes the bundled model can honestly serve: Latin-script
/// languages (Tesseract-style 3-letter or ISO 639-1 codes). Only English is
/// read completely; for the others accents are dropped.
const LATIN: &[&str] = &[
    "eng", "en", "deu", "de", "fra", "fr", "spa", "es", "ita", "it", "por", "pt", "nld", "nl",
    "dan", "da", "nor", "no", "nob", "nno", "swe", "sv", "fin", "fi", "isl", "is", "pol", "pl",
    "ces", "cs", "slk", "sk", "slv", "sl", "hrv", "hr", "bos", "bs", "srp_latn", "hun", "hu",
    "ron", "ro", "tur", "tr", "est", "et", "lav", "lv", "lit", "lt", "cat", "ca", "glg", "gl",
    "eus", "eu", "gle", "ga", "cym", "cy", "mlt", "mt", "sqi", "sq", "afr", "af", "ind", "id",
    "msa", "ms", "vie", "vi", "tgl", "tl", "swa", "sw", "lat", "la", "epo", "eo", "fil",
];

/// Validate the language field. Empty means English. Returns the codes
/// that were accepted; non-Latin scripts get a clear error.
pub fn check_languages(langs: &[String]) -> Result<Vec<String>, String> {
    let mut ok = Vec::new();
    for l in langs {
        let code = l.trim().to_ascii_lowercase();
        if code.is_empty() {
            continue;
        }
        if !LATIN.contains(&code.as_str()) {
            return Err(format!(
                "'{}' isn't supported: the built-in OCR reads Latin script only \
                 (English letters, digits and punctuation; accents are dropped)",
                l.trim()
            ));
        }
        ok.push(code);
    }
    if ok.is_empty() {
        ok.push("eng".into());
    }
    Ok(ok)
}

/// Add an invisible, searchable text layer to `pages` (0-based). Pages
/// whose text layer already has text are skipped in [`Mode::Skip`].
pub fn ocr_pdf(pdf: &[u8], pages: &[usize], mode: Mode) -> Result<Vec<u8>, String> {
    let hdoc = open_hayro(pdf)?;
    let count = hdoc.pages().len();
    let wanted = select(pages, count)?;
    let mut doc = Document::load_mem(pdf).map_err(|e| format!("can't read PDF ({e})"))?;
    if doc.is_encrypted() {
        return Err("remove the password first".into());
    }
    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    if page_ids.len() != count {
        return Err("the page tree is damaged — run Repair first".into());
    }

    let todo: Vec<usize> = match mode {
        Mode::Force => wanted,
        Mode::Skip => wanted
            .into_iter()
            .filter(|&i| {
                doc.extract_text(&[i as u32 + 1])
                    .map(|t| t.trim().is_empty())
                    .unwrap_or(true)
            })
            .collect(),
    };
    if todo.is_empty() {
        return Err("every selected page already has text — tick \
                    “Redo pages that already have text” to OCR them anyway"
            .into());
    }

    let engine = engine()?;
    let cache = RenderCache::new();
    let mut found: Vec<(usize, Vec<Line>, [f64; 6])> = Vec::new();
    for &i in &todo {
        let page = &hdoc.pages()[i];
        let (w, h) = page.render_dimensions();
        let dpi = OCR_DPI.min(MAX_LONG_SIDE * 72.0 / w.max(h).max(1.0));
        let (img, (pw, ph)) = render_page(&hdoc, &cache, i, dpi)?;
        let lines = recognise(
            engine,
            &img,
            pw / img.width() as f32,
            ph / img.height() as f32,
        )
        .map_err(|e| format!("page {}: {e}", i + 1))?;
        if lines.is_empty() {
            continue;
        }
        found.push((i, lines, display_to_user(page, ph)));
    }
    if found.is_empty() {
        return Err("no text was recognised on the selected pages".into());
    }

    // one font for the whole document, codes assigned per character
    let mut codes = Codes::default();
    for (_, lines, _) in &found {
        for l in lines {
            for w in &l.words {
                for c in w.text.chars() {
                    codes.code(c);
                }
            }
        }
    }
    let font = add_font(&mut doc, &codes);
    for (i, lines, m) in &found {
        let id = page_ids[*i];
        let name = add_font_resource(&mut doc, id, font)?;
        let layer = text_layer(lines, &name, *m, &mut codes);
        append_layer(&mut doc, id, layer)?;
    }
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| format!("can't write PDF ({e})"))?;
    Ok(out)
}

// ---------------------------------------------------------------- engine

fn engine() -> Result<&'static OcrEngine, String> {
    static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let load = |bytes: &'static [u8]| {
                rten::Model::load_static_slice(bytes)
                    .map_err(|e| format!("OCR model failed to load ({e})"))
            };
            OcrEngine::new(OcrEngineParams {
                detection_model: Some(load(DETECTION_MODEL)?),
                recognition_model: Some(load(RECOGNITION_MODEL)?),
                ..Default::default()
            })
            .map_err(|e| format!("OCR engine failed to start ({e})"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// A recognised word, in display points (origin bottom-left, y up).
#[derive(Debug, Clone)]
struct Word {
    text: String,
    x0: f32,
    x1: f32,
}

#[derive(Debug, Clone)]
struct Line {
    /// bottom and top of the line box, display points, y up
    y0: f32,
    y1: f32,
    words: Vec<Word>,
}

/// Run detection + recognition on a rendered page. `sx`/`sy` convert pixels
/// to points.
fn recognise(
    engine: &OcrEngine,
    img: &image::RgbaImage,
    sx: f32,
    sy: f32,
) -> Result<Vec<Line>, String> {
    let rgb = super::raster::rgba_to_rgb(img);
    let src = ImageSource::from_bytes(rgb.as_raw(), rgb.dimensions())
        .map_err(|e| format!("bad bitmap ({e:?})"))?;
    let run = || -> Result<Vec<Option<ocrs::TextLine>>, String> {
        let input = engine.prepare_input(src).map_err(|e| e.to_string())?;
        let words = engine.detect_words(&input).map_err(|e| e.to_string())?;
        let lines = engine.find_text_lines(&input, &words);
        engine
            .recognize_text(&input, &lines)
            .map_err(|e| e.to_string())
    };
    let lines = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .map_err(|_| "the OCR engine crashed".to_string())?
        .map_err(|e| format!("OCR failed ({e})"))?;
    let height = img.height() as f32 * sy;
    let mut out = Vec::new();
    for line in lines.into_iter().flatten() {
        let r = line.bounding_rect();
        let (top, bottom) = (r.top() as f32 * sy, r.bottom() as f32 * sy);
        if bottom <= top {
            continue;
        }
        let words: Vec<Word> = line
            .words()
            .map(|w| {
                let wr = w.bounding_rect();
                Word {
                    text: w.to_string(),
                    x0: wr.left() as f32 * sx,
                    x1: (wr.right() as f32 * sx).max(wr.left() as f32 * sx + 0.5),
                }
            })
            .filter(|w| !w.text.trim().is_empty())
            .collect();
        if words.is_empty() {
            continue;
        }
        out.push(Line {
            y0: height - bottom,
            y1: height - top,
            words,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------- geometry

/// Affine `[a b c d e f]` mapping display points (origin bottom-left of the
/// page as shown, y up, `/Rotate` applied) to the page's user space — the
/// inverse of hayro's page transform, so the text layer lands exactly on
/// what was rendered.
fn display_to_user(page: &hayro::hayro_syntax::page::Page<'_>, height: f32) -> [f64; 6] {
    let t = page.initial_transform(true).as_coeffs();
    let inv = invert(t).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let flip = [1.0, 0.0, 0.0, -1.0, 0.0, height as f64];
    compose(inv, flip)
}

/// `p ∘ q` (apply `q` first).
fn compose(p: [f64; 6], q: [f64; 6]) -> [f64; 6] {
    [
        p[0] * q[0] + p[2] * q[1],
        p[1] * q[0] + p[3] * q[1],
        p[0] * q[2] + p[2] * q[3],
        p[1] * q[2] + p[3] * q[3],
        p[0] * q[4] + p[2] * q[5] + p[4],
        p[1] * q[4] + p[3] * q[5] + p[5],
    ]
}

fn invert(m: [f64; 6]) -> Option<[f64; 6]> {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])])
}

// ---------------------------------------------------------------- font

/// Single-byte codes for the Type 3 font. Printable ASCII keeps its own
/// code (so even readers that ignore /ToUnicode get it right); other
/// characters take free codes; past ~160 distinct extras they become '?'.
#[derive(Default)]
struct Codes {
    extra: BTreeMap<char, u8>,
}

impl Codes {
    fn code(&mut self, c: char) -> u8 {
        if c == ' ' || ('!'..='~').contains(&c) {
            return c as u8;
        }
        if let Some(&b) = self.extra.get(&c) {
            return b;
        }
        let free = (0x80..=0xFFu8).chain(0x01..=0x1Fu8);
        match free.clone().nth(self.extra.len()) {
            Some(b) => {
                self.extra.insert(c, b);
                b
            }
            None => b'?',
        }
    }
}

/// Adobe glyph names for printable ASCII, 0x20…0x7E.
const ASCII_NAMES: [&str; 95] = [
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
];

/// Type 3 font with empty glyphs (nothing is ever painted: render mode 3),
/// a 0.5 em advance for every code and a ToUnicode CMap.
fn add_font(doc: &mut Document, codes: &Codes) -> ObjectId {
    let glyph = doc.add_object(Stream::new(dictionary! {}, b"500 0 d0".to_vec()));
    let mut procs = Dictionary::new();
    let mut diffs: Vec<Object> = vec![Object::Integer(0x20)];
    for name in ASCII_NAMES {
        diffs.push(Object::Name(name.as_bytes().to_vec()));
        procs.set(name, glyph);
    }
    let mut map: Vec<(u8, char)> = (0x20u8..=0x7E).map(|b| (b, b as char)).collect();
    for (&c, &b) in &codes.extra {
        let name = format!("uni{:04X}", c as u32 & 0xFFFF);
        diffs.push(Object::Integer(b as i64));
        diffs.push(Object::Name(name.clone().into_bytes()));
        procs.set(name, glyph);
        map.push((b, c));
    }
    let widths: Vec<Object> = (1..=255).map(|_| Object::Integer(ADVANCE as i64)).collect();
    let cmap = doc.add_object(Stream::new(dictionary! {}, to_unicode_cmap(&map)));
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "Name" => "OCRText",
        "FontBBox" => vec![0.into(), Object::Integer(-200), 500.into(), 800.into()],
        "FontMatrix" => vec![0.001.into(), 0.into(), 0.into(), 0.001.into(), 0.into(), 0.into()],
        "CharProcs" => procs,
        "Encoding" => dictionary! { "Type" => "Encoding", "Differences" => diffs },
        "FirstChar" => 1,
        "LastChar" => 255,
        "Widths" => widths,
        "Resources" => dictionary! {},
        "ToUnicode" => cmap,
    })
}

fn to_unicode_cmap(map: &[(u8, char)]) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /OGFiles-OCR-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for chunk in map.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for &(b, c) in chunk {
            let mut units = [0u16; 2];
            let hex: String = c
                .encode_utf16(&mut units)
                .iter()
                .map(|u| format!("{u:04X}"))
                .collect();
            s.push_str(&format!("<{b:02X}> <{hex}>\n"));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

// ---------------------------------------------------------------- page edits

/// Resolve `obj` (direct or by reference) to an owned dictionary.
fn dict_of(doc: &Document, obj: &Object) -> Option<Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        _ => None,
    }
}

/// Give the page its own (direct) /Resources with the OCR font added under
/// a free name; returns that name. Inherited and shared resource
/// dictionaries are copied, never modified, so other pages are untouched.
fn add_font_resource(doc: &mut Document, page: ObjectId, font: ObjectId) -> Result<String, String> {
    let mut res = None;
    let mut node = Some(page);
    for _ in 0..64 {
        let Some(id) = node else { break };
        let Ok(d) = doc.get_dictionary(id) else { break };
        if let Ok(r) = d.get(b"Resources") {
            res = dict_of(doc, r);
            break;
        }
        node = d.get(b"Parent").and_then(Object::as_reference).ok();
    }
    let mut res = res.unwrap_or_default();
    let mut fonts = res
        .get(b"Font")
        .ok()
        .and_then(|f| dict_of(doc, f))
        .unwrap_or_default();
    let mut name = "OCRText".to_string();
    let mut n = 1;
    while fonts.has(name.as_bytes()) {
        n += 1;
        name = format!("OCRText{n}");
    }
    fonts.set(name.as_bytes(), font);
    res.set("Font", fonts);
    doc.get_dictionary_mut(page)
        .map_err(|_| "page object is damaged".to_string())?
        .set("Resources", res);
    Ok(name)
}

fn num(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.into()
    }
}

/// Content stream for one page's invisible text.
fn text_layer(lines: &[Line], font: &str, m: [f64; 6], codes: &mut Codes) -> Vec<u8> {
    let mut s = format!(
        "q {} {} {} {} {} {} cm\n",
        num(m[0]),
        num(m[1]),
        num(m[2]),
        num(m[3]),
        num(m[4]),
        num(m[5])
    );
    for line in lines {
        let size = (line.y1 - line.y0).max(1.0);
        // glyph box is -0.2…0.8 em: put the baseline 20 % above the bottom
        let base = line.y0 + 0.2 * size;
        s.push_str(&format!("BT 3 Tr /{font} {} Tf\n", num(size as f64)));
        let last = line.words.len().saturating_sub(1);
        for (k, w) in line.words.iter().enumerate() {
            let n = w.text.chars().count().max(1) as f32;
            let natural = n * ADVANCE / 1000.0 * size;
            let tz = (100.0 * (w.x1 - w.x0) / natural).clamp(1.0, 1000.0);
            let mut bytes: Vec<u8> = w.text.chars().map(|c| codes.code(c)).collect();
            if k < last {
                bytes.push(b' ');
            }
            let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
            s.push_str(&format!(
                "{} Tz 1 0 0 1 {} {} Tm <{hex}> Tj\n",
                num(tz as f64),
                num(w.x0 as f64),
                num(base as f64)
            ));
        }
        s.push_str("ET\n");
    }
    s.push_str("Q\n");
    s.into_bytes()
}

/// Wrap the page's existing content in `q … Q` and append `layer`.
fn append_layer(doc: &mut Document, page: ObjectId, layer: Vec<u8>) -> Result<(), String> {
    let old: Vec<Object> = {
        let d = doc
            .get_dictionary(page)
            .map_err(|_| "page object is damaged".to_string())?;
        match d.get(b"Contents") {
            Ok(Object::Reference(id)) => match doc.get_object(*id) {
                Ok(Object::Array(a)) => a.clone(),
                Ok(_) => vec![Object::Reference(*id)],
                Err(_) => vec![],
            },
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![],
        }
    };
    let q = doc.add_object(Stream::new(dictionary! {}, b"q\n".to_vec()));
    let qq = doc.add_object(Stream::new(dictionary! {}, b"\nQ\n".to_vec()));
    let mut text = Stream::new(dictionary! {}, layer);
    let _ = text.compress();
    let t = doc.add_object(text);
    let mut contents = vec![Object::Reference(q)];
    contents.extend(old);
    contents.push(Object::Reference(qq));
    contents.push(Object::Reference(t));
    doc.get_dictionary_mut(page)
        .map_err(|_| "page object is damaged".to_string())?
        .set("Contents", contents);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};

    /// One page per entry, lines of Helvetica 14 pt, flattened (300 dpi JPEG)
    /// to an image-only PDF so no text layer remains — a typical scan.
    pub(crate) fn scanned_pdf(pages: &[&[&str]]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        for lines in pages {
            let mut ops = vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 14.into()]),
                Operation::new("TL", vec![22.into()]),
                Operation::new("Td", vec![60.into(), 700.into()]),
            ];
            for l in *lines {
                ops.push(Operation::new("Tj", vec![Object::string_literal(*l)]));
                ops.push(Operation::new("T*", vec![]));
            }
            ops.push(Operation::new("ET", vec![]));
            let c = doc.add_object(Stream::new(
                dictionary! {},
                Content { operations: ops }.encode().unwrap(),
            ));
            kids.push(
                doc.add_object(dictionary! {
                    "Type" => "Page", "Parent" => pages_id, "Contents" => c,
                })
                .into(),
            );
        }
        let n = kids.len() as i64;
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => n,
                "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        let all: Vec<usize> = (0..pages.len()).collect();
        super::super::raster::rasterize(&buf, &all, 300.0, 90).unwrap()
    }

    fn texts(pdf: &[u8]) -> Vec<String> {
        super::super::structure::page_texts(pdf).unwrap()
    }

    #[test]
    fn ocr_adds_searchable_text_and_keeps_visible_content() {
        let pdf = scanned_pdf(&[
            &["Quarterly report 2026", "Invoice number 4711"],
            &["Hello world from page two"],
        ]);
        assert!(texts(&pdf).iter().all(|t| t.trim().is_empty()));
        let out = ocr_pdf(&pdf, &[0, 1], Mode::Skip).unwrap();
        let t = texts(&out);
        let p1 = t[0].to_lowercase();
        let p2 = t[1].to_lowercase();
        for w in ["quarterly", "report", "invoice", "4711"] {
            assert!(p1.contains(w), "page 1 missing {w:?}: {p1:?}");
        }
        for w in ["hello", "world", "page"] {
            assert!(p2.contains(w), "page 2 missing {w:?}: {p2:?}");
        }
        // the old content stream is still there, wrapped in q … Q
        let doc = Document::load_mem(&out).unwrap();
        let page = *doc.get_pages().get(&1).unwrap();
        let contents = doc.get_page_content(page);
        let s = String::from_utf8_lossy(&contents);
        assert!(s.starts_with("q\n"), "{}", &s[..40.min(s.len())]);
        assert!(s.contains("Do"), "image draw kept");
        assert!(s.contains("3 Tr"));
        // and it renders exactly as before: the text layer is invisible
        let (a, b) = (open_hayro(&pdf).unwrap(), open_hayro(&out).unwrap());
        let (ca, cb) = (RenderCache::new(), RenderCache::new());
        for i in 0..2 {
            let ra = render_page(&a, &ca, i, 60.0).unwrap().0;
            let rb = render_page(&b, &cb, i, 60.0).unwrap().0;
            assert!(ra == rb, "page {} looks different after OCR", i + 1);
        }

        // skip mode leaves the now-searchable pages alone
        assert!(ocr_pdf(&out, &[0, 1], Mode::Skip)
            .unwrap_err()
            .contains("already has text"));
    }

    /// A scan stored sideways and shown upright via /Rotate 90: the image
    /// is drawn turned 90° counter-clockwise on a landscape MediaBox.
    fn sideways(pdf: &[u8]) -> Vec<u8> {
        let mut doc = Document::load_mem(pdf).unwrap();
        let first = *doc.get_pages().get(&1).unwrap();
        let c = doc.add_object(Stream::new(
            dictionary! {},
            b"q 0 612 -792 0 792 0 cm /Im0 Do Q".to_vec(),
        ));
        let page = doc.get_dictionary_mut(first).unwrap();
        page.set("MediaBox", vec![0.into(), 0.into(), 792.into(), 612.into()]);
        page.set("Rotate", 90);
        page.set("Contents", c);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    #[test]
    fn ocr_rotated_page_and_only_selected_pages() {
        let pdf = sideways(&scanned_pdf(&[
            &["Rotated text works"],
            &["Untouched page"],
        ]));
        let out = ocr_pdf(&pdf, &[0], Mode::Force).unwrap();
        let t = texts(&out);
        assert!(t[0].to_lowercase().contains("rotated"), "{:?}", t[0]);
        assert!(t[1].trim().is_empty());
    }

    /// `OGX_OCR_DUMP=/tmp/dir cargo test -p og-files ocr_dump` writes a
    /// scanned sample and its OCR result for checking in other readers.
    #[test]
    fn ocr_dump() {
        let Some(dir) = std::env::var_os("OGX_OCR_DUMP") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = sideways(&scanned_pdf(&[
            &["Quarterly report 2026", "Invoice number 4711"],
            &["Hello world from page two"],
        ]));
        std::fs::write(dir.join("scan.pdf"), &pdf).unwrap();
        let out = ocr_pdf(&pdf, &[0, 1], Mode::Skip).unwrap();
        std::fs::write(dir.join("scan-ocr.pdf"), out).unwrap();
    }

    /// OCR, auto-split and photo splitting driven through the tool panel
    /// like the UI does, with no server configured.
    #[test]
    fn vision_tools_run_from_the_panel_without_a_server() {
        use super::super::super::toolpanel::{job, Form, FormMsg, JobInput, JobOut, Tool};
        use super::super::Output;
        use std::sync::Arc;
        let input = || JobInput {
            page_count: 1,
            current: 0,
            selected: vec![],
            file_name: "scan.pdf".into(),
            server: None,
        };
        for t in [Tool::Ocr, Tool::AutoSplit, Tool::ExtractScans] {
            assert!(!t.needs_server(), "{}", t.label());
        }
        let scan = Arc::new(scanned_pdf(&[&["Searchable now"]]));
        let mut form = Form::default();
        form.update(FormMsg::Text("langs", "eng, fra".into()));
        match job(Tool::Ocr, &form, input()).unwrap()(scan.clone()) {
            Ok(JobOut::Out(Output::Pdf(b))) => {
                assert!(texts(&b)[0].to_lowercase().contains("searchable"))
            }
            other => panic!("ocr: {:?}", other.err()),
        }
        form.update(FormMsg::Text("langs", "jpn".into()));
        assert!(job(Tool::Ocr, &form, input()).is_err());
        // no QR in a text scan → clear error, not a crash
        let e = job(Tool::AutoSplit, &Form::default(), input()).unwrap()(scan.clone());
        assert!(e.err().unwrap().contains("separator"));
        let e = job(Tool::ExtractScans, &Form::default(), input()).unwrap()(scan);
        assert!(e.is_ok() || e.err().unwrap().contains("no photos"));
        let mut bad = Form::default();
        bad.update(FormMsg::Text("tolerance", "999".into()));
        assert!(job(Tool::ExtractScans, &bad, input()).is_err());
    }

    #[test]
    fn languages_are_checked() {
        assert_eq!(check_languages(&[]).unwrap(), vec!["eng".to_string()]);
        assert!(check_languages(&["eng".into(), "DEU".into()]).is_ok());
        let e = check_languages(&["rus".into()]).unwrap_err();
        assert!(e.contains("Latin script"), "{e}");
        assert!(check_languages(&["chi_sim".into()]).is_err());
    }

    #[test]
    fn transforms_and_codes() {
        let m = [0.0, 1.0, -1.0, 0.0, 612.0, 0.0];
        let i = invert(m).unwrap();
        let id = compose(m, i);
        for (a, b) in id.iter().zip([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]) {
            assert!((a - b).abs() < 1e-9);
        }
        let mut c = Codes::default();
        assert_eq!(c.code('A'), b'A');
        assert_eq!(c.code('é'), 0x80);
        assert_eq!(c.code('é'), 0x80);
        assert_eq!(c.code('ß'), 0x81);
        assert!(String::from_utf8(to_unicode_cmap(&[(0x80, 'é')]))
            .unwrap()
            .contains("<80> <00E9>"));
    }
}
