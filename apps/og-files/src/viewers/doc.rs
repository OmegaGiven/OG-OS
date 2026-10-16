//! Documents: Markdown rendered preview (toggle to source; edit mode edits
//! the source, Save writes over), DOCX/ODT/RTF readable rich text with
//! headings, lists and tables, EPUB chapter reader.
//!
//! Everything is parsed off the UI thread into a small block model
//! (headings, paragraphs of bold/italic runs, tables, notes) and shown in
//! pages of `PAGE` blocks so huge documents stay smooth.

use super::Event;
use crate::{fsops, theme};
use iced::font::{Style as FontStyle, Weight};
use iced::highlighter;
use iced::widget::scrollable::{self, RelativeOffset};
use iced::widget::text::Span;
use iced::widget::{
    button, column, container, markdown, responsive, rich_text, row, span, text, text_editor, Space,
};
use iced::{Element, Font, Length, Task, Theme};
use quick_xml::events::{BytesRef, BytesStart, Event as X};
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// blocks (or top-level markdown items) per page
const PAGE: usize = 200;
/// cap on any single file read out of a zip container (zip-bomb guard)
const ENTRY_LIMIT: u64 = 64 * 1024 * 1024;
/// markdown sources up to this size re-render the preview on every keystroke
const LIVE_PREVIEW_LIMIT: usize = 256 * 1024;
/// below this width edit mode shows source or preview, not both
const SIDE_BY_SIDE_MIN: f32 = 900.0;
const CODE_THEME: highlighter::Theme = highlighter::Theme::Base16Mocha;
const EDIT_COLOR: iced::Color =
    iced::Color::from_rgb(1.0, 0xc8 as f32 / 255.0, 0x6b as f32 / 255.0);

fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    matches!(
        ext(path).as_str(),
        "md" | "markdown" | "docx" | "odt" | "rtf" | "epub"
    )
}

// ---------------------------------------------------------------- model

#[derive(Debug, Clone, Default, PartialEq)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Heading(u8, Vec<Run>),
    Para {
        runs: Vec<Run>,
        indent: u8,
    },
    Table(Vec<Vec<String>>),
    /// muted placeholder line (skipped image, missing chapter…)
    Note(String),
}

#[derive(Debug, Clone)]
struct Chapter {
    title: String,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
enum Doc {
    Markdown(String),
    Rich(Vec<Block>),
    Book(Vec<Chapter>),
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    doc: Doc,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    let p = path.clone();
    let doc = tokio::task::spawn_blocking(move || load(&p))
        .await
        .map_err(|e| e.to_string())??;
    Ok(Opened { path, doc })
}

/// Parsed document as a debug string (tests of the PDF converters check
/// that this viewer reads what they write).
#[cfg(test)]
pub(crate) fn debug_load(path: &Path) -> Result<String, String> {
    load(path).map(|d| format!("{d:?}"))
}

fn load(path: &Path) -> Result<Doc, String> {
    let file = || std::fs::File::open(path).map_err(|e| e.to_string());
    match ext(path).as_str() {
        "md" | "markdown" => {
            let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
            if len > super::TEXT_LIMIT {
                return Err("file too large to open as text".into());
            }
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            Ok(Doc::Markdown(match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            }))
        }
        "docx" => parse_docx(file()?).map(Doc::Rich),
        "odt" => parse_odt(file()?).map(Doc::Rich),
        "epub" => parse_epub(file()?).map(Doc::Book),
        "rtf" => {
            let mut bytes = Vec::new();
            file()?
                .take(ENTRY_LIMIT)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Ok(Doc::Rich(parse_rtf(&bytes)))
        }
        other => Err(format!("unsupported document type .{other}")),
    }
}

// ---------------------------------------------------------- block builder

#[derive(Default)]
struct TableB {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_cell: bool,
}

/// Accumulates runs into blocks. Format parsers set `heading` / `prefix` /
/// `indent` before calling `end_para`; text inside a table cell is folded
/// into that cell as plain text.
#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    runs: Vec<Run>,
    tables: Vec<TableB>,
    heading: Option<u8>,
    /// list marker ("• ", "3. ") for the next emitted paragraph
    prefix: Option<String>,
    indent: u8,
}

fn tidy(runs: &mut Vec<Run>) {
    runs.retain(|r| !r.text.is_empty());
    while let Some(r) = runs.first_mut() {
        let t = r.text.trim_start();
        if t.is_empty() {
            runs.remove(0);
        } else {
            r.text = t.to_string();
            break;
        }
    }
    while let Some(r) = runs.last_mut() {
        let t = r.text.trim_end();
        if t.is_empty() {
            runs.pop();
        } else {
            r.text.truncate(t.len());
            break;
        }
    }
}

fn plain(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

impl Builder {
    fn push(&mut self, s: &str, bold: bool, italic: bool) {
        if s.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(r) if r.bold == bold && r.italic == italic => r.text.push_str(s),
            _ => self.runs.push(Run {
                text: s.to_string(),
                bold,
                italic,
            }),
        }
    }

    /// HTML-style text: whitespace runs collapse to one space, leading
    /// whitespace at the start of a paragraph is dropped.
    fn push_collapsed(&mut self, s: &str, bold: bool, italic: bool) {
        let mut prev_space = self
            .runs
            .last()
            .is_none_or(|r| r.text.ends_with(|c: char| c.is_ascii_whitespace()));
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if c.is_ascii_whitespace() {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            } else {
                out.push(c);
                prev_space = false;
            }
        }
        self.push(&out, bold, italic);
    }

    fn end_para(&mut self) {
        let mut runs = std::mem::take(&mut self.runs);
        tidy(&mut runs);
        if runs.is_empty() {
            return;
        }
        if let Some(t) = self.tables.last_mut().filter(|t| t.in_cell) {
            if !t.cell.is_empty() {
                t.cell.push('\n');
            }
            if let Some(p) = self.prefix.take() {
                t.cell.push_str(&p);
            }
            t.cell.push_str(&plain(&runs));
            return;
        }
        let block = match self.heading {
            Some(l) => Block::Heading(l.clamp(1, 6), runs),
            None => {
                if let Some(p) = self.prefix.take() {
                    runs.insert(
                        0,
                        Run {
                            text: p,
                            ..Run::default()
                        },
                    );
                }
                Block::Para {
                    runs,
                    indent: self.indent,
                }
            }
        };
        self.blocks.push(block);
    }

    fn note(&mut self, s: String) {
        self.end_para();
        self.blocks.push(Block::Note(s));
    }

    fn table_start(&mut self) {
        self.end_para();
        self.tables.push(TableB::default());
    }

    fn row_start(&mut self) {
        if let Some(t) = self.tables.last_mut() {
            t.row.clear();
        }
    }

    fn cell_start(&mut self) {
        self.end_para();
        if let Some(t) = self.tables.last_mut() {
            t.in_cell = true;
            t.cell.clear();
        }
    }

    fn cell_end(&mut self) {
        self.end_para();
        if let Some(t) = self.tables.last_mut() {
            let c = std::mem::take(&mut t.cell);
            t.row.push(c);
            t.in_cell = false;
        }
    }

    fn row_end(&mut self) {
        if let Some(t) = self.tables.last_mut() {
            let mut r = std::mem::take(&mut t.row);
            while r.last().is_some_and(String::is_empty) {
                r.pop();
            }
            if !r.is_empty() {
                t.rows.push(r);
            }
        }
    }

    fn table_end(&mut self) {
        self.end_para();
        let Some(t) = self.tables.pop() else {
            return;
        };
        if t.rows.is_empty() {
            return;
        }
        // nested table: flatten into the enclosing cell
        if let Some(parent) = self.tables.last_mut().filter(|p| p.in_cell) {
            for r in t.rows {
                if !parent.cell.is_empty() {
                    parent.cell.push('\n');
                }
                parent.cell.push_str(&r.join(" | "));
            }
            return;
        }
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        let rows = t
            .rows
            .into_iter()
            .map(|mut r| {
                r.resize(cols, String::new());
                r
            })
            .collect();
        self.blocks.push(Block::Table(rows));
    }

    fn finish(mut self) -> Vec<Block> {
        self.end_para();
        while let Some(t) = self.tables.last() {
            if t.in_cell {
                self.cell_end();
            }
            self.row_end();
            self.table_end();
        }
        self.blocks
    }
}

// ------------------------------------------------------------- xml walking

enum Ev<'a> {
    Start(&'a [u8], &'a BytesStart<'a>),
    End(&'a [u8]),
    Text(&'a str),
}

/// Lenient pull-parse: local names only (no namespace prefixes), empty
/// elements reported as Start+End, entities resolved, stops quietly at the
/// first syntax error keeping what came before.
fn walk(xml: &str, mut f: impl FnMut(Ev<'_>)) {
    let mut r = Reader::from_str(xml);
    let c = r.config_mut();
    c.check_end_names = false;
    c.allow_unmatched_ends = true;
    c.allow_dangling_amp = true;
    loop {
        match r.read_event() {
            Ok(X::Start(e)) => {
                let n = e.local_name();
                f(Ev::Start(n.as_ref(), &e));
            }
            Ok(X::Empty(e)) => {
                let n = e.local_name();
                f(Ev::Start(n.as_ref(), &e));
                f(Ev::End(n.as_ref()));
            }
            Ok(X::End(e)) => {
                let n = e.local_name();
                f(Ev::End(n.as_ref()));
            }
            Ok(X::Text(t)) => {
                if let Ok(s) = t.decode() {
                    f(Ev::Text(&s));
                }
            }
            Ok(X::CData(t)) => {
                if let Ok(s) = t.decode() {
                    f(Ev::Text(&s));
                }
            }
            Ok(X::GeneralRef(r)) => {
                if let Some(s) = entity(&r) {
                    f(Ev::Text(&s));
                }
            }
            Ok(X::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

fn entity(r: &BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    let name = r.decode().ok()?;
    let s = match name.as_ref() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "shy" => "",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "sbquo" => "‚",
        "ldquo" => "“",
        "rdquo" => "”",
        "bdquo" => "„",
        "laquo" => "«",
        "raquo" => "»",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "bull" => "•",
        "middot" => "·",
        "deg" => "°",
        "times" => "×",
        "eacute" => "é",
        "egrave" => "è",
        "agrave" => "à",
        "auml" => "ä",
        "ouml" => "ö",
        "uuml" => "ü",
        "szlig" => "ß",
        _ => return None,
    };
    Some(s.to_string())
}

/// Attribute by local name (prefix ignored), unescaped.
fn attr(e: &BytesStart<'_>, local: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// OOXML on/off property: present without val, or val not 0/false.
fn val_on(e: &BytesStart<'_>) -> bool {
    attr(e, b"val").is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off" | "none"))
}

fn zip_text<R: Read + Seek>(z: &mut zip::ZipArchive<R>, name: &str) -> Result<String, String> {
    let f = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut buf = Vec::new();
    f.take(ENTRY_LIMIT)
        .read_to_end(&mut buf)
        .map_err(|e| format!("{name}: {e}"))?;
    Ok(match String::from_utf8(buf) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

fn zip_open<R: Read + Seek>(r: R) -> Result<zip::ZipArchive<R>, String> {
    zip::ZipArchive::new(r).map_err(|e| format!("not a valid zip container: {e}"))
}

// ------------------------------------------------------------------- DOCX

#[derive(Default)]
struct Numbering {
    /// numId → abstractNumId
    abs_of: HashMap<String, String>,
    /// abstractNumId → level → numbered (false = bullet)
    fmts: HashMap<String, HashMap<u8, bool>>,
}

impl Numbering {
    fn parse(xml: &str) -> Self {
        let mut n = Numbering::default();
        let (mut abs, mut lvl, mut num) = (None::<String>, None::<u8>, None::<String>);
        walk(xml, |ev| match ev {
            Ev::Start(b"abstractNum", e) => abs = attr(e, b"abstractNumId"),
            Ev::Start(b"lvl", e) => lvl = attr(e, b"ilvl").and_then(|v| v.parse().ok()),
            Ev::Start(b"numFmt", e) => {
                if let (Some(a), Some(l)) = (&abs, lvl) {
                    let f = attr(e, b"val").unwrap_or_default();
                    n.fmts
                        .entry(a.clone())
                        .or_default()
                        .insert(l, !matches!(f.as_str(), "bullet" | "none" | ""));
                }
            }
            Ev::Start(b"num", e) => num = attr(e, b"numId"),
            Ev::Start(b"abstractNumId", e) => {
                if let (Some(id), Some(a)) = (&num, attr(e, b"val")) {
                    n.abs_of.insert(id.clone(), a);
                }
            }
            Ev::End(b"abstractNum") => abs = None,
            Ev::End(b"lvl") => lvl = None,
            Ev::End(b"num") => num = None,
            _ => {}
        });
        n
    }

    fn numbered(&self, id: &str, lvl: u8) -> bool {
        self.abs_of
            .get(id)
            .and_then(|a| self.fmts.get(a))
            .and_then(|m| m.get(&lvl))
            .copied()
            .unwrap_or(false)
    }
}

/// Heading level from a style id/name: Heading1..6, "heading 2", Title, Subtitle.
fn heading_level(name: &str) -> Option<u8> {
    let l = name.to_ascii_lowercase();
    match l.as_str() {
        "title" => return Some(1),
        "subtitle" => return Some(2),
        _ => {}
    }
    let rest = l.strip_prefix("heading")?.trim();
    rest.parse::<u8>()
        .ok()
        .filter(|n| (1..=9).contains(n))
        .map(|n| n.min(6))
}

/// styleId → heading level (via the style's display name or outline level,
/// so localized style ids still work).
fn docx_styles(xml: &str) -> HashMap<String, u8> {
    let mut map = HashMap::new();
    let mut cur: Option<String> = None;
    walk(xml, |ev| match ev {
        Ev::Start(b"style", e) => cur = attr(e, b"styleId"),
        Ev::Start(b"name", e) => {
            if let (Some(id), Some(l)) = (&cur, attr(e, b"val").as_deref().and_then(heading_level))
            {
                map.insert(id.clone(), l);
            }
        }
        Ev::Start(b"outlineLvl", e) => {
            if let (Some(id), Some(l)) = (&cur, attr(e, b"val").and_then(|v| v.parse::<u8>().ok()))
            {
                if l < 9 {
                    map.entry(id.clone()).or_insert((l + 1).min(6));
                }
            }
        }
        Ev::End(b"style") => cur = None,
        _ => {}
    });
    map
}

fn parse_docx<R: Read + Seek>(r: R) -> Result<Vec<Block>, String> {
    let mut z = zip_open(r)?;
    let doc = zip_text(&mut z, "word/document.xml")?;
    let numbering = zip_text(&mut z, "word/numbering.xml")
        .map(|s| Numbering::parse(&s))
        .unwrap_or_default();
    let styles = zip_text(&mut z, "word/styles.xml")
        .map(|s| docx_styles(&s))
        .unwrap_or_default();
    Ok(docx_blocks(&doc, &numbering, &styles))
}

fn docx_blocks(doc: &str, num: &Numbering, styles: &HashMap<String, u8>) -> Vec<Block> {
    let mut b = Builder::default();
    let (mut in_r, mut in_t, mut bold, mut italic) = (false, false, false, false);
    let mut heading: Option<u8> = None;
    let (mut num_id, mut ilvl): (Option<String>, u8) = (None, 0);
    let mut counters: HashMap<String, [u32; 9]> = HashMap::new();
    let mut skip = 0usize;
    walk(doc, |ev| {
        if skip > 0 {
            match ev {
                Ev::Start(..) => skip += 1,
                Ev::End(_) => skip -= 1,
                Ev::Text(_) => {}
            }
            return;
        }
        match ev {
            Ev::Start(n, e) => match n {
                // duplicate fallback renderings, text boxes, field codes
                b"Fallback" | b"txbxContent" | b"instrText" | b"delText" => skip = 1,
                b"p" => {
                    heading = None;
                    num_id = None;
                    ilvl = 0;
                }
                b"pStyle" => {
                    heading = attr(e, b"val")
                        .and_then(|v| styles.get(&v).copied().or_else(|| heading_level(&v)));
                }
                b"numId" => num_id = attr(e, b"val").filter(|v| v != "0"),
                b"ilvl" => ilvl = attr(e, b"val").and_then(|v| v.parse().ok()).unwrap_or(0),
                b"r" => {
                    in_r = true;
                    bold = false;
                    italic = false;
                }
                b"b" if in_r => bold = val_on(e),
                b"i" if in_r => italic = val_on(e),
                b"t" if in_r => in_t = true,
                b"tab" if in_r => b.push("    ", bold, italic),
                b"br" | b"cr" if in_r => b.push("\n", bold, italic),
                b"drawing" | b"pict" | b"object" => {
                    b.push(" [image] ", false, true);
                    skip = 1;
                }
                b"tbl" => b.table_start(),
                b"tr" => b.row_start(),
                b"tc" => b.cell_start(),
                _ => {}
            },
            Ev::End(n) => match n {
                b"r" => in_r = false,
                b"t" => in_t = false,
                b"p" => {
                    b.heading = heading;
                    if let Some(id) = num_id.as_ref().filter(|_| heading.is_none()) {
                        let lvl = usize::from(ilvl.min(8));
                        let c = counters.entry(id.clone()).or_insert([0; 9]);
                        c[lvl] += 1;
                        c[lvl + 1..].fill(0);
                        b.prefix = Some(if num.numbered(id, lvl as u8) {
                            format!("{}. ", c[lvl])
                        } else {
                            "• ".into()
                        });
                        b.indent = lvl as u8 + 1;
                    }
                    b.end_para();
                    b.heading = None;
                    b.prefix = None;
                    b.indent = 0;
                }
                b"tc" => b.cell_end(),
                b"tr" => b.row_end(),
                b"tbl" => b.table_end(),
                _ => {}
            },
            Ev::Text(s) if in_t => b.push(s, bold, italic),
            Ev::Text(_) => {}
        }
    });
    b.finish()
}

// -------------------------------------------------------------------- ODT

#[derive(Default)]
struct OdtStyles {
    /// style name → (bold, italic)
    text: HashMap<String, (bool, bool)>,
    /// list style name → level → numbered
    lists: HashMap<String, HashMap<u8, bool>>,
}

impl OdtStyles {
    fn collect(&mut self, xml: &str) {
        let (mut cur, mut list) = (None::<String>, None::<String>);
        walk(xml, |ev| match ev {
            Ev::Start(b"style", e) => cur = attr(e, b"name"),
            Ev::Start(b"text-properties", e) => {
                if let Some(name) = &cur {
                    let bold = attr(e, b"font-weight")
                        .is_some_and(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
                    let italic =
                        attr(e, b"font-style").is_some_and(|s| s == "italic" || s == "oblique");
                    if bold || italic {
                        self.text.insert(name.clone(), (bold, italic));
                    }
                }
            }
            Ev::Start(b"list-style", e) => list = attr(e, b"name"),
            Ev::Start(n @ (b"list-level-style-number" | b"list-level-style-bullet"), e) => {
                if let Some(l) = &list {
                    let level = attr(e, b"level").and_then(|v| v.parse().ok()).unwrap_or(1);
                    self.lists
                        .entry(l.clone())
                        .or_default()
                        .insert(level, n == b"list-level-style-number");
                }
            }
            Ev::End(b"style") => cur = None,
            Ev::End(b"list-style") => list = None,
            _ => {}
        });
    }

    fn style(&self, name: Option<String>) -> (bool, bool) {
        name.and_then(|n| self.text.get(&n).copied())
            .unwrap_or((false, false))
    }
}

fn parse_odt<R: Read + Seek>(r: R) -> Result<Vec<Block>, String> {
    let mut z = zip_open(r)?;
    let content = zip_text(&mut z, "content.xml")?;
    let styles = zip_text(&mut z, "styles.xml").ok();
    Ok(odt_blocks(styles.as_deref(), &content))
}

fn odt_blocks(styles_xml: Option<&str>, content: &str) -> Vec<Block> {
    let mut st = OdtStyles::default();
    if let Some(s) = styles_xml {
        st.collect(s);
    }
    st.collect(content);

    let mut b = Builder::default();
    let mut para = 0usize;
    let mut spans: Vec<(bool, bool)> = Vec::new();
    // (list style, items so far)
    let mut lists: Vec<(Option<String>, u32)> = Vec::new();
    let (mut skip, mut frame_img) = (0usize, false);
    walk(content, |ev| {
        if skip > 0 {
            match ev {
                Ev::Start(n, _) => {
                    frame_img |= n == b"image";
                    skip += 1;
                }
                Ev::End(_) => {
                    skip -= 1;
                    if skip == 0 && frame_img {
                        if para > 0 {
                            b.push(" [image] ", false, true);
                        } else {
                            b.note("[image]".into());
                        }
                    }
                }
                Ev::Text(_) => {}
            }
            return;
        }
        let cur = spans.last().copied().unwrap_or((false, false));
        match ev {
            Ev::Start(n, e) => match n {
                b"note" | b"annotation" | b"tracked-changes" | b"sequence-decls" | b"forms"
                | b"automatic-styles" | b"font-face-decls" | b"scripts" => skip = 1,
                b"frame" => {
                    skip = 1;
                    frame_img = false;
                }
                b"h" | b"p" => {
                    para += 1;
                    if para == 1 {
                        b.heading = (n == b"h").then(|| {
                            attr(e, b"outline-level")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1)
                        });
                        b.indent = lists.len() as u8;
                        spans.clear();
                        spans.push(st.style(attr(e, b"style-name")));
                    }
                }
                b"span" => {
                    let (sb, si) = st.style(attr(e, b"style-name"));
                    spans.push((cur.0 || sb, cur.1 || si));
                }
                b"s" if para > 0 => {
                    let c = attr(e, b"c").and_then(|v| v.parse().ok()).unwrap_or(1usize);
                    b.push(&" ".repeat(c.min(64)), cur.0, cur.1);
                }
                b"tab" if para > 0 => b.push("    ", cur.0, cur.1),
                b"line-break" if para > 0 => b.push("\n", cur.0, cur.1),
                b"list" => {
                    let style =
                        attr(e, b"style-name").or_else(|| lists.last().and_then(|l| l.0.clone()));
                    lists.push((style, 0));
                }
                b"list-item" => {
                    let depth = lists.len() as u8;
                    if let Some((style, count)) = lists.last_mut() {
                        *count += 1;
                        let numbered = style
                            .as_ref()
                            .and_then(|s| st.lists.get(s))
                            .and_then(|m| m.get(&depth))
                            .copied()
                            .unwrap_or(false);
                        b.prefix = Some(if numbered {
                            format!("{count}. ")
                        } else {
                            "• ".into()
                        });
                    }
                }
                b"list-header" => b.prefix = None,
                b"table" => b.table_start(),
                b"table-row" => b.row_start(),
                b"table-cell" | b"covered-table-cell" => b.cell_start(),
                _ => {}
            },
            Ev::End(n) => match n {
                b"h" | b"p" => {
                    para = para.saturating_sub(1);
                    if para == 0 {
                        b.end_para();
                        b.heading = None;
                        spans.clear();
                    }
                }
                b"span" => {
                    if spans.len() > 1 {
                        spans.pop();
                    }
                }
                b"list" => {
                    lists.pop();
                }
                b"list-item" => b.prefix = None,
                b"table-cell" | b"covered-table-cell" => b.cell_end(),
                b"table-row" => b.row_end(),
                b"table" => b.table_end(),
                _ => {}
            },
            Ev::Text(s) if para > 0 => b.push(s, cur.0, cur.1),
            Ev::Text(_) => {}
        }
    });
    b.finish()
}

// -------------------------------------------------------------------- RTF

/// Destination groups whose content is never document text.
const RTF_SKIP: &[&[u8]] = &[
    b"fonttbl",
    b"colortbl",
    b"stylesheet",
    b"info",
    b"pict",
    b"object",
    b"objdata",
    b"header",
    b"headerl",
    b"headerr",
    b"headerf",
    b"footer",
    b"footerl",
    b"footerr",
    b"footerf",
    b"fldinst",
    b"themedata",
    b"colorschememapping",
    b"latentstyles",
    b"datastore",
    b"listtable",
    b"listoverridetable",
    b"rsidtbl",
    b"generator",
    b"xmlnstbl",
    b"mmathPr",
    b"filetbl",
    b"revtbl",
    b"pgdsctbl",
    b"footnote",
    b"annotation",
    b"atnid",
    b"atnauthor",
    b"nonshppict",
    b"sp",
    b"userprops",
    b"wgrffmtfilter",
    b"xe",
    b"tc",
    b"falt",
    b"panose",
    b"bkmkstart",
    b"bkmkend",
    b"listtext",
    b"pntext",
    b"pntxta",
    b"pntxtb",
    b"template",
    b"operator",
];

/// Windows-1252 (RTF's usual \ansicpg) to char.
fn cp1252(b: u8) -> char {
    const HI: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9f => HI[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

#[derive(Clone, Copy)]
struct RtfGroup {
    bold: bool,
    italic: bool,
    skip: bool,
    /// fallback chars after \uN
    uc: usize,
}

fn rtf_emit(b: &mut Builder, g: RtfGroup, pending: &mut usize, s: &str) {
    for c in s.chars() {
        if *pending > 0 {
            *pending -= 1;
        } else if !g.skip {
            let mut buf = [0u8; 4];
            b.push(c.encode_utf8(&mut buf), g.bold, g.italic);
        }
    }
}

/// Minimal RTF reader: text with bold/italic and paragraph breaks; control
/// words otherwise dropped, non-text destinations skipped.
fn parse_rtf(src: &[u8]) -> Vec<Block> {
    let mut b = Builder::default();
    let mut stack: Vec<RtfGroup> = Vec::new();
    let mut g = RtfGroup {
        bold: false,
        italic: false,
        skip: false,
        uc: 1,
    };
    let mut pending = 0usize;
    let mut i = 0usize;
    while i < src.len() {
        let c = src[i];
        i += 1;
        match c {
            b'{' => {
                stack.push(g);
                pending = 0;
            }
            b'}' => {
                if let Some(p) = stack.pop() {
                    g = p;
                }
                pending = 0;
            }
            b'\r' | b'\n' => {}
            b'\\' => {
                let Some(&n) = src.get(i) else { break };
                if n.is_ascii_alphabetic() {
                    let s = i;
                    while i < src.len() && src[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word = &src[s..i];
                    let ps = i;
                    if src.get(i) == Some(&b'-') {
                        i += 1;
                    }
                    while i < src.len() && src[i].is_ascii_digit() {
                        i += 1;
                    }
                    let param: Option<i32> = std::str::from_utf8(&src[ps..i])
                        .ok()
                        .and_then(|p| p.parse().ok());
                    if src.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    let on = param != Some(0);
                    let emit = |b: &mut Builder, pending: &mut usize, s: &str| {
                        rtf_emit(b, g, pending, s);
                    };
                    match word {
                        b"bin" => i += param.unwrap_or(0).max(0) as usize,
                        b"par" | b"sect" | b"page" | b"row" => {
                            if !g.skip {
                                b.end_para();
                            }
                        }
                        b"line" => emit(&mut b, &mut pending, "\n"),
                        b"tab" => emit(&mut b, &mut pending, "    "),
                        b"cell" | b"nestcell" => emit(&mut b, &mut pending, "  |  "),
                        b"b" => g.bold = on,
                        b"i" => g.italic = on,
                        b"plain" => {
                            g.bold = false;
                            g.italic = false;
                        }
                        b"u" => {
                            if let Some(p) = param {
                                let cp = if p < 0 { p + 65536 } else { p } as u32;
                                let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                                emit(&mut b, &mut pending, ch.encode_utf8(&mut [0; 4]));
                                pending = g.uc;
                            }
                        }
                        b"uc" => g.uc = param.unwrap_or(1).max(0) as usize,
                        b"emdash" => emit(&mut b, &mut pending, "—"),
                        b"endash" => emit(&mut b, &mut pending, "–"),
                        b"lquote" => emit(&mut b, &mut pending, "‘"),
                        b"rquote" => emit(&mut b, &mut pending, "’"),
                        b"ldblquote" => emit(&mut b, &mut pending, "“"),
                        b"rdblquote" => emit(&mut b, &mut pending, "”"),
                        b"bullet" => emit(&mut b, &mut pending, "•"),
                        b"emspace" | b"enspace" | b"qmspace" => emit(&mut b, &mut pending, " "),
                        w if RTF_SKIP.contains(&w) => g.skip = true,
                        _ => {}
                    }
                } else {
                    i += 1;
                    match n {
                        b'\'' => {
                            let byte = src
                                .get(i..i + 2)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u8::from_str_radix(h, 16).ok());
                            if let Some(byte) = byte {
                                i += 2;
                                rtf_emit(
                                    &mut b,
                                    g,
                                    &mut pending,
                                    cp1252(byte).encode_utf8(&mut [0; 4]),
                                );
                            }
                        }
                        b'*' => g.skip = true,
                        b'\\' | b'{' | b'}' => {
                            rtf_emit(
                                &mut b,
                                g,
                                &mut pending,
                                char::from(n).encode_utf8(&mut [0; 4]),
                            );
                        }
                        b'~' => rtf_emit(&mut b, g, &mut pending, "\u{a0}"),
                        b'_' => rtf_emit(&mut b, g, &mut pending, "-"),
                        b'\n' | b'\r' if !g.skip => b.end_para(),
                        _ => {}
                    }
                }
            }
            _ => rtf_emit(&mut b, g, &mut pending, cp1252(c).encode_utf8(&mut [0; 4])),
        }
    }
    b.finish()
}

// ------------------------------------------------------------------- EPUB

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Zip path of `href` relative to the directory `base` ("" = root).
fn resolve(base: &str, href: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn parse_epub<R: Read + Seek>(r: R) -> Result<Vec<Chapter>, String> {
    let mut z = zip_open(r)?;
    let container = zip_text(&mut z, "META-INF/container.xml")?;
    let mut opf_path = None;
    walk(&container, |ev| {
        if let Ev::Start(b"rootfile", e) = ev {
            if opf_path.is_none() {
                opf_path = attr(e, b"full-path");
            }
        }
    });
    let opf_path = opf_path.ok_or("EPUB container names no package file")?;
    let opf = zip_text(&mut z, &opf_path)?;
    let base = opf_path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    let mut items: HashMap<String, String> = HashMap::new();
    let mut spine: Vec<String> = Vec::new();
    walk(&opf, |ev| match ev {
        Ev::Start(b"item", e) => {
            if let (Some(id), Some(href)) = (attr(e, b"id"), attr(e, b"href")) {
                items.insert(id, href);
            }
        }
        Ev::Start(b"itemref", e) => spine.extend(attr(e, b"idref")),
        _ => {}
    });
    let mut chapters = Vec::new();
    for id in &spine {
        let Some(href) = items.get(id) else { continue };
        let href = href.split('#').next().unwrap_or_default();
        let path = resolve(&base, &percent_decode(href));
        let (title, blocks) = match zip_text(&mut z, &path) {
            Ok(x) => html_blocks(&x),
            Err(e) => (None, vec![Block::Note(format!("[missing chapter: {e}]"))]),
        };
        let heading = blocks.iter().find_map(|b| match b {
            Block::Heading(_, runs) => Some(plain(runs).trim().to_string()),
            _ => None,
        });
        let title = heading
            .filter(|t| !t.is_empty())
            .or(title)
            .unwrap_or_else(|| format!("Chapter {}", chapters.len() + 1));
        chapters.push(Chapter { title, blocks });
    }
    if chapters.is_empty() {
        return Err("EPUB has no readable chapters".into());
    }
    Ok(chapters)
}

#[derive(Default)]
struct Html {
    b: Builder,
    skip: usize,
    bold: u32,
    italic: u32,
    pre: u32,
    quote: u8,
    in_title: bool,
    title: String,
    /// open lists: Some(counter) = ordered
    lists: Vec<Option<u32>>,
}

fn is_block(tag: &[u8]) -> bool {
    matches!(
        tag,
        b"p" | b"div"
            | b"section"
            | b"article"
            | b"header"
            | b"footer"
            | b"aside"
            | b"nav"
            | b"main"
            | b"figure"
            | b"figcaption"
            | b"dd"
            | b"dt"
            | b"dl"
            | b"address"
            | b"center"
            | b"body"
            | b"caption"
            | b"hgroup"
            | b"hr"
    )
}

impl Html {
    fn flush(&mut self) {
        self.b.indent = self.lists.len() as u8 + self.quote;
        self.b.end_para();
    }

    fn ev(&mut self, ev: Ev<'_>) {
        if self.skip > 0 {
            match ev {
                Ev::Start(..) => self.skip += 1,
                Ev::End(_) => self.skip -= 1,
                Ev::Text(_) => {}
            }
            return;
        }
        match ev {
            Ev::Start(n, e) => {
                let n = n.to_ascii_lowercase();
                match n.as_slice() {
                    b"script" | b"style" | b"noscript" | b"template" => self.skip = 1,
                    b"title" => self.in_title = true,
                    b"svg" => {
                        self.b.push(" [image] ", false, true);
                        self.skip = 1;
                    }
                    b"img" | b"image" => {
                        let label = match attr(e, b"alt").filter(|a| !a.trim().is_empty()) {
                            Some(a) => format!(" [image: {}] ", a.trim()),
                            None => " [image] ".into(),
                        };
                        self.b.push(&label, false, true);
                    }
                    b"br" => self.b.push("\n", self.bold > 0, self.italic > 0),
                    b"b" | b"strong" => self.bold += 1,
                    b"i" | b"em" | b"cite" | b"var" | b"dfn" => self.italic += 1,
                    b"pre" => {
                        self.flush();
                        self.pre += 1;
                    }
                    [b'h', d @ b'1'..=b'6'] => {
                        self.flush();
                        self.b.heading = Some(d - b'0');
                    }
                    b"ul" => {
                        self.flush();
                        self.lists.push(None);
                    }
                    b"ol" => {
                        self.flush();
                        let start = attr(e, b"start").and_then(|v| v.parse::<u32>().ok());
                        self.lists.push(Some(start.unwrap_or(1).saturating_sub(1)));
                    }
                    b"li" => {
                        self.flush();
                        self.b.prefix = Some(match self.lists.last_mut() {
                            Some(Some(c)) => {
                                *c += 1;
                                format!("{c}. ")
                            }
                            _ => "• ".into(),
                        });
                    }
                    b"blockquote" => {
                        self.flush();
                        self.quote += 1;
                    }
                    b"table" => {
                        self.flush();
                        self.b.table_start();
                    }
                    b"tr" => self.b.row_start(),
                    b"td" | b"th" => self.b.cell_start(),
                    t if is_block(t) => self.flush(),
                    _ => {}
                }
            }
            Ev::End(n) => {
                let n = n.to_ascii_lowercase();
                match n.as_slice() {
                    b"title" => self.in_title = false,
                    b"b" | b"strong" => self.bold = self.bold.saturating_sub(1),
                    b"i" | b"em" | b"cite" | b"var" | b"dfn" => {
                        self.italic = self.italic.saturating_sub(1);
                    }
                    b"pre" => {
                        self.flush();
                        self.pre = self.pre.saturating_sub(1);
                    }
                    [b'h', b'1'..=b'6'] => {
                        self.flush();
                        self.b.heading = None;
                    }
                    b"ul" | b"ol" => {
                        self.flush();
                        self.lists.pop();
                    }
                    b"li" => {
                        self.flush();
                        self.b.prefix = None;
                    }
                    b"blockquote" => {
                        self.flush();
                        self.quote = self.quote.saturating_sub(1);
                    }
                    b"td" | b"th" => self.b.cell_end(),
                    b"tr" => self.b.row_end(),
                    b"table" => self.b.table_end(),
                    t if is_block(t) => self.flush(),
                    _ => {}
                }
            }
            Ev::Text(s) => {
                if self.in_title {
                    self.title.push_str(s);
                } else if self.pre > 0 {
                    self.b.push(s, self.bold > 0, self.italic > 0);
                } else {
                    self.b.push_collapsed(s, self.bold > 0, self.italic > 0);
                }
            }
        }
    }
}

/// XHTML chapter → (document <title>, blocks).
fn html_blocks(xml: &str) -> (Option<String>, Vec<Block>) {
    let mut h = Html::default();
    walk(xml, |ev| h.ev(ev));
    let title = Some(h.title.trim().to_string()).filter(|t| !t.is_empty());
    (title, h.b.finish())
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Copy, Default)]
struct Pos {
    chapter: usize,
    page: usize,
    y: f32,
}

/// Reading position per file for this session (reopening a book resumes).
static POSITIONS: LazyLock<Mutex<HashMap<PathBuf, Pos>>> = LazyLock::new(Default::default);

struct Md {
    source: text_editor::Content,
    items: Vec<markdown::Item>,
    show_source: bool,
    editing: bool,
    dirty: bool,
    saving: bool,
    /// preview lags the source (big file: re-rendered on save / leaving edit)
    stale: bool,
}

impl Md {
    fn reparse(&mut self) {
        self.items = markdown::parse(&self.source.text()).collect();
        self.stale = false;
    }
}

enum Body {
    Markdown(Md),
    Rich(Vec<Block>),
    Book {
        chapters: Vec<Chapter>,
        current: usize,
    },
}

pub struct State {
    path: PathBuf,
    body: Body,
    page: usize,
    offset: f32,
    scroll: scrollable::Id,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Page(usize),
    Chapter(usize),
    Scrolled(f32),
    Link(markdown::Url),
    ShowSource(bool),
    Edit(text_editor::Action),
    ToggleEdit,
    Save,
    Saved(Result<(), String>),
    Discard,
    Reloaded(Result<String, String>),
}

fn pages(len: usize) -> usize {
    len.div_ceil(PAGE).max(1)
}

fn font(bold: bool, italic: bool) -> Font {
    Font {
        weight: if bold { Weight::Bold } else { Weight::Normal },
        style: if italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
        ..Font::DEFAULT
    }
}

fn spans(runs: &[Run], bold: bool) -> Vec<Span<'_, Msg, Font>> {
    runs.iter()
        .map(|r| span(r.text.as_str()).font(font(r.bold || bold, r.italic)))
        .collect()
}

fn note(s: &str) -> Element<'_, Msg> {
    text(s)
        .size(13)
        .font(font(false, true))
        .color(theme::MUTED)
        .into()
}

fn block_view(b: &Block) -> Element<'_, Msg> {
    match b {
        Block::Heading(l, runs) => {
            let size = [28.0, 24.0, 20.0, 18.0, 16.0, 15.0][usize::from(l.clamp(&1, &6) - 1)];
            container(rich_text(spans(runs, true)).size(size))
                .padding(iced::padding::top(6))
                .into()
        }
        Block::Para { runs, indent } => container(rich_text(spans(runs, false)).size(15))
            .padding(iced::padding::left(f32::from(*indent) * 22.0))
            .into(),
        Block::Note(s) => note(s),
        Block::Table(rows) => column(rows.iter().enumerate().map(|(ri, r)| {
            iced::widget::Row::with_children(r.iter().map(|c| {
                let header = ri == 0;
                container(text(c.as_str()).size(13).font(font(header, false)))
                    .padding([3, 6])
                    .width(Length::Fill)
                    .style(move |t: &Theme| {
                        let p = t.extended_palette();
                        container::Style {
                            background: header
                                .then(|| p.background.weak.color.scale_alpha(0.4).into()),
                            border: iced::Border {
                                width: 1.0,
                                color: p.background.strong.color,
                                radius: 0.0.into(),
                            },
                            ..Default::default()
                        }
                    })
                    .into()
            }))
            .into()
        }))
        .into(),
    }
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let body = match o.doc {
            Doc::Markdown(s) => Body::Markdown(Md {
                items: markdown::parse(&s).collect(),
                source: text_editor::Content::with_text(&s),
                show_source: false,
                editing: false,
                dirty: false,
                saving: false,
                stale: false,
            }),
            Doc::Rich(b) => Body::Rich(b),
            Doc::Book(chapters) => Body::Book {
                chapters,
                current: 0,
            },
        };
        let mut st = Self {
            path: o.path,
            body,
            page: 0,
            offset: 0.0,
            scroll: scrollable::Id::unique(),
        };
        let saved = POSITIONS.lock().ok().and_then(|m| m.get(&st.path).copied());
        let task = match saved {
            Some(pos) => {
                if let Body::Book { chapters, current } = &mut st.body {
                    *current = pos.chapter.min(chapters.len().saturating_sub(1));
                }
                st.page = pos.page;
                st.offset = pos.y;
                scrollable::snap_to(st.scroll.clone(), RelativeOffset { x: 0.0, y: pos.y })
            }
            None => Task::none(),
        };
        (st, task)
    }

    fn remember(&self) {
        let chapter = match &self.body {
            Body::Book { current, .. } => *current,
            _ => 0,
        };
        if let Ok(mut m) = POSITIONS.lock() {
            m.insert(
                self.path.clone(),
                Pos {
                    chapter,
                    page: self.page,
                    y: self.offset,
                },
            );
        }
    }

    fn scroll_top(&mut self) -> Task<Msg> {
        self.offset = 0.0;
        self.remember();
        scrollable::snap_to(self.scroll.clone(), RelativeOffset::START)
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Page(p) => {
                self.page = p;
                (self.scroll_top(), None)
            }
            Msg::Chapter(i) => {
                if let Body::Book { chapters, current } = &mut self.body {
                    *current = i.min(chapters.len().saturating_sub(1));
                }
                self.page = 0;
                (self.scroll_top(), None)
            }
            Msg::Scrolled(y) => {
                self.offset = y;
                self.remember();
                (Task::none(), None)
            }
            Msg::Link(url) => (Task::none(), Some(Event::Status(format!("Link: {url}")))),
            Msg::Save => (self.save(), None),
            msg => {
                let Body::Markdown(md) = &mut self.body else {
                    return (Task::none(), None);
                };
                let path = &self.path;
                match msg {
                    Msg::ShowSource(s) => md.show_source = s,
                    Msg::Edit(a) => {
                        // read-only: selection and cursor moves only
                        if md.editing || !a.is_edit() {
                            let edit = a.is_edit();
                            md.source.perform(a);
                            if edit {
                                md.dirty = true;
                                let src = md.source.text();
                                if src.len() < LIVE_PREVIEW_LIMIT {
                                    md.items = markdown::parse(&src).collect();
                                } else {
                                    md.stale = true;
                                }
                            }
                        }
                    }
                    Msg::ToggleEdit => {
                        if md.editing && md.dirty {
                            return (
                                Task::none(),
                                Some(Event::Status(
                                    "Unsaved changes: Save or Discard first".into(),
                                )),
                            );
                        }
                        md.editing = !md.editing;
                        md.show_source = md.editing;
                        if md.stale {
                            md.reparse();
                        }
                    }
                    Msg::Saved(r) => {
                        md.saving = false;
                        return match r {
                            Ok(()) => {
                                md.dirty = false;
                                if md.stale {
                                    md.reparse();
                                }
                                (
                                    Task::none(),
                                    Some(Event::Status(format!("Saved over {}", path.display()))),
                                )
                            }
                            Err(e) => (
                                Task::none(),
                                Some(Event::Status(format!("Save failed: {e}"))),
                            ),
                        };
                    }
                    Msg::Discard => {
                        return (
                            Task::perform(
                                fsops::read_text(path.clone(), super::TEXT_LIMIT),
                                Msg::Reloaded,
                            ),
                            None,
                        );
                    }
                    Msg::Reloaded(r) => {
                        return match r {
                            Ok(s) => {
                                md.source = text_editor::Content::with_text(&s);
                                md.reparse();
                                md.dirty = false;
                                md.editing = false;
                                md.show_source = false;
                                (
                                    Task::none(),
                                    Some(Event::Status("Changes discarded".into())),
                                )
                            }
                            Err(e) => (
                                Task::none(),
                                Some(Event::Status(format!("Reload failed: {e}"))),
                            ),
                        };
                    }
                    _ => {}
                }
                (Task::none(), None)
            }
        }
    }

    fn pager(&self, len: usize) -> Option<Element<'_, Msg>> {
        let n = pages(len);
        if n <= 1 {
            return None;
        }
        let p = self.page.min(n - 1);
        Some(
            row![
                button(text("Prev").size(12))
                    .on_press_maybe((p > 0).then(|| Msg::Page(p - 1)))
                    .style(button::secondary),
                text(format!("Page {} / {n}", p + 1))
                    .size(12)
                    .color(theme::MUTED),
                button(text("Next").size(12))
                    .on_press_maybe((p + 1 < n).then(|| Msg::Page(p + 1)))
                    .style(button::secondary),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
        )
    }

    /// Scrollable, centred reading column with the page bar under it.
    fn reader<'a>(&'a self, body: Element<'a, Msg>, len: usize) -> Element<'a, Msg> {
        let scroller = iced::widget::scrollable(
            container(container(body).max_width(880.0))
                .center_x(Length::Fill)
                .padding(16),
        )
        .id(self.scroll.clone())
        .on_scroll(|v| Msg::Scrolled(v.relative_offset().y))
        .width(Length::Fill)
        .height(Length::Fill);
        let mut col = column![scroller].spacing(6);
        if let Some(p) = self.pager(len) {
            col = col.push(container(p).center_x(Length::Fill));
        }
        col.into()
    }

    fn page_range(&self, len: usize) -> std::ops::Range<usize> {
        let p = self.page.min(pages(len) - 1);
        p * PAGE..((p + 1) * PAGE).min(len)
    }

    fn blocks_view<'a>(&'a self, blocks: &'a [Block]) -> Element<'a, Msg> {
        let body: Element<'a, Msg> = if blocks.is_empty() {
            note("(no text found)")
        } else {
            column(blocks[self.page_range(blocks.len())].iter().map(block_view))
                .spacing(10)
                .into()
        };
        self.reader(body, blocks.len())
    }

    fn md_preview<'a>(&'a self, md: &'a Md) -> Element<'a, Msg> {
        let items = &md.items[self.page_range(md.items.len())];
        let body = markdown::view(
            items,
            markdown::Settings::with_text_size(15),
            markdown::Style::from_palette(theme::theme().palette()),
        )
        .map(Msg::Link);
        let body: Element<'a, Msg> = if md.stale {
            column![note("(preview updates when you save)"), body]
                .spacing(8)
                .into()
        } else {
            body
        };
        self.reader(body, md.items.len())
    }

    fn md_view<'a>(&'a self, md: &'a Md, width: f32) -> Element<'a, Msg> {
        let wide = md.editing && width >= SIDE_BY_SIDE_MIN;
        let toggle = |label: &'static str, src: bool| {
            button(text(label).size(12))
                .on_press(Msg::ShowSource(src))
                .style(if md.show_source == src {
                    button::primary
                } else {
                    button::secondary
                })
        };
        let mut bar = row![].spacing(6).align_y(iced::Alignment::Center);
        bar = if md.editing {
            bar.push(
                text(if md.dirty {
                    "EDITING  * modified"
                } else {
                    "EDITING"
                })
                .size(12)
                .color(EDIT_COLOR),
            )
        } else {
            bar.push(text("Markdown · read-only").size(12).color(theme::MUTED))
        };
        if !wide {
            bar = bar
                .push(toggle("Preview", false))
                .push(toggle("Source", true));
        }
        bar = bar.push(Space::with_width(Length::Fill));
        bar = if md.editing {
            bar.push(
                button(text("Discard").size(12))
                    .on_press_maybe(md.dirty.then_some(Msg::Discard))
                    .style(button::secondary),
            )
            .push(
                button(text("Save  Ctrl+S").size(12)).on_press_maybe(md.dirty.then_some(Msg::Save)),
            )
            .push(
                button(text("Done").size(12))
                    .on_press_maybe((!md.dirty).then_some(Msg::ToggleEdit))
                    .style(button::secondary),
            )
        } else {
            bar.push(button(text("Edit  Ctrl+E").size(12)).on_press(Msg::ToggleEdit))
        };
        // always interactive so read-only source can be selected/copied;
        // `update` drops edit actions unless editing
        let editor = || -> Element<'a, Msg> {
            text_editor(&md.source)
                .font(Font::MONOSPACE)
                .height(Length::Fill)
                .on_action(Msg::Edit)
                .highlight("md", CODE_THEME)
                .into()
        };
        let body: Element<'a, Msg> = if wide {
            row![
                container(editor()).width(Length::FillPortion(1)),
                container(self.md_preview(md)).width(Length::FillPortion(1)),
            ]
            .spacing(8)
            .into()
        } else if md.show_source {
            editor()
        } else {
            self.md_preview(md)
        };
        column![bar, body].spacing(6).into()
    }

    fn book_view<'a>(&'a self, chapters: &'a [Chapter], current: usize) -> Element<'a, Msg> {
        let list = column(chapters.iter().enumerate().map(|(i, c)| {
            button(text(c.title.as_str()).size(12))
                .width(Length::Fill)
                .on_press(Msg::Chapter(i))
                .style(if i == current {
                    button::primary
                } else {
                    button::text
                })
                .into()
        }))
        .spacing(2)
        .padding(4);
        let sidebar = container(iced::widget::scrollable(list).height(Length::Fill))
            .width(220)
            .height(Length::Fill)
            .style(theme::panel);
        let ch = &chapters[current];
        let n = chapters.len();
        let bar = row![
            button(text("Prev chapter").size(12))
                .on_press_maybe((current > 0).then(|| Msg::Chapter(current - 1)))
                .style(button::secondary),
            text(format!("{} / {n}  ·  {}", current + 1, ch.title))
                .size(12)
                .color(theme::MUTED),
            Space::with_width(Length::Fill),
            button(text("Next chapter").size(12))
                .on_press_maybe((current + 1 < n).then(|| Msg::Chapter(current + 1)))
                .style(button::secondary),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        row![
            sidebar,
            column![bar, self.blocks_view(&ch.blocks)].spacing(6)
        ]
        .spacing(8)
        .into()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        match &self.body {
            Body::Markdown(md) => responsive(move |size| self.md_view(md, size.width)).into(),
            Body::Rich(blocks) => {
                let label = match ext(&self.path).as_str() {
                    "docx" => "Word document · read-only",
                    "odt" => "OpenDocument text · read-only",
                    "rtf" => "Rich Text · read-only",
                    _ => "Document · read-only",
                };
                column![
                    text(label).size(12).color(theme::MUTED),
                    self.blocks_view(blocks)
                ]
                .spacing(6)
                .into()
            }
            Body::Book { chapters, current } => self.book_view(chapters, *current),
        }
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        matches!(&self.body, Body::Markdown(md) if md.dirty)
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        match &self.body {
            Body::Markdown(md) => Some(md.editing),
            _ => None,
        }
    }

    /// Ctrl+E. Leaving edit mode with unsaved changes must be refused (return
    /// an Event::Status explaining) — see text.rs.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        match &self.body {
            // route through update so the refusal surfaces as Event::Status
            Body::Markdown(md) if md.editing && md.dirty => Task::done(Msg::ToggleEdit),
            Body::Markdown(_) => self.update(Msg::ToggleEdit).0,
            _ => Task::none(),
        }
    }

    /// Ctrl+S: write over the original (atomically). No-op if not dirty.
    pub fn save(&mut self) -> Task<Msg> {
        let Body::Markdown(md) = &mut self.body else {
            return Task::none();
        };
        if md.saving || !md.dirty {
            return Task::none();
        }
        md.saving = true;
        Task::perform(
            fsops::write_text(self.path.clone(), md.source.text()),
            Msg::Saved,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    fn tmp(name: &str) -> PathBuf {
        let d = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("ogx-p2-doc-tests")
            .join(format!("{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, body) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    fn text_of(b: &Block) -> String {
        match b {
            Block::Heading(_, r) | Block::Para { runs: r, .. } => plain(r),
            Block::Note(s) => s.clone(),
            Block::Table(_) => "<table>".into(),
        }
    }

    fn has_bold(blocks: &[Block], word: &str) -> bool {
        blocks.iter().any(|b| match b {
            Block::Para { runs, .. } => runs.iter().any(|r| r.bold && r.text.contains(word)),
            _ => false,
        })
    }

    fn table(blocks: &[Block]) -> Vec<Vec<String>> {
        blocks
            .iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t.clone()),
                _ => None,
            })
            .expect("a table")
    }

    fn grid(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn supported_extensions() {
        for f in ["a.md", "b.MARKDOWN", "c.docx", "d.odt", "e.rtf", "f.epub"] {
            assert!(supported(Path::new(f)), "{f}");
        }
        for f in ["a.txt", "b.pdf", "c.doc", "noext"] {
            assert!(!supported(Path::new(f)), "{f}");
        }
    }

    #[tokio::test]
    async fn docx_extraction() {
        let dir = tmp("docx");
        let path = dir.join("t.docx");
        let doc = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {W}><w:body>
<w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>My Title</w:t></w:r></w:p>
<w:p><w:pPr><w:pStyle w:val="Berschrift2"/></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Plain </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:rPr><w:i/><w:b w:val="0"/></w:rPr><w:t xml:space="preserve"> it &amp; more</w:t></w:r><w:r><w:br/><w:t>line2</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>first</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>second</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>dot</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:t>After</w:t></w:r></w:p>
</w:body></w:document>"#
        );
        let numbering = format!(
            r#"<w:numbering {W}>
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#
        );
        let styles = format!(
            r#"<w:styles {W}><w:style w:type="paragraph" w:styleId="Berschrift2"><w:name w:val="heading 2"/></w:style></w:styles>"#
        );
        make_zip(
            &path,
            &[
                ("[Content_Types].xml", "<Types/>"),
                ("word/document.xml", &doc),
                ("word/numbering.xml", &numbering),
                ("word/styles.xml", &styles),
            ],
        );
        let o = open(path).await.unwrap();
        let Doc::Rich(blocks) = o.doc else {
            panic!("not rich")
        };
        assert_eq!(
            blocks[0],
            Block::Heading(
                1,
                vec![Run {
                    text: "My Title".into(),
                    ..Run::default()
                }]
            )
        );
        assert!(matches!(&blocks[1], Block::Heading(2, r) if plain(r) == "Intro"));
        assert_eq!(text_of(&blocks[2]), "Plain bold it & more\nline2");
        assert!(has_bold(&blocks, "bold"));
        assert!(!has_bold(&blocks, "more"));
        assert_eq!(text_of(&blocks[3]), "1. first");
        assert_eq!(text_of(&blocks[4]), "2. second");
        assert_eq!(text_of(&blocks[5]), "• dot");
        assert!(matches!(blocks[5], Block::Para { indent: 1, .. }));
        assert_eq!(table(&blocks), grid(&[&["A1", "B1"], &["A2", "B2"]]));
        assert_eq!(text_of(blocks.last().unwrap()), "After");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn odt_extraction() {
        let dir = tmp("odt");
        let path = dir.join("t.odt");
        let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
<office:automatic-styles>
  <style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
  <text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style>
</office:automatic-styles>
<office:body><office:text>
  <text:sequence-decls><text:sequence-decl text:name="Table"/></text:sequence-decls>
  <text:h text:outline-level="2">Chapter One</text:h>
  <text:p>Some <text:span text:style-name="T1">strong</text:span> text<text:s text:c="2"/>here<text:line-break/>next</text:p>
  <text:list text:style-name="L1"><text:list-item><text:p>alpha</text:p></text:list-item><text:list-item><text:p>beta</text:p></text:list-item></text:list>
  <text:list><text:list-item><text:p>bullet</text:p></text:list-item></text:list>
  <table:table><table:table-row><table:table-cell><text:p>x</text:p></table:table-cell><table:table-cell><text:p>y</text:p></table:table-cell></table:table-row>
  <table:table-row><table:table-cell><text:p>1</text:p></table:table-cell><table:table-cell><text:p>2</text:p></table:table-cell></table:table-row></table:table>
  <text:p>Note<text:note><text:note-body><text:p>footnote</text:p></text:note-body></text:note> end</text:p>
</office:text></office:body></office:document-content>"#;
        make_zip(
            &path,
            &[
                ("mimetype", "application/vnd.oasis.opendocument.text"),
                ("content.xml", content),
            ],
        );
        let blocks = parse_odt(std::fs::File::open(&path).unwrap()).unwrap();
        assert!(matches!(&blocks[0], Block::Heading(2, r) if plain(r) == "Chapter One"));
        assert_eq!(text_of(&blocks[1]), "Some strong text  here\nnext");
        assert!(has_bold(&blocks, "strong"));
        assert_eq!(text_of(&blocks[2]), "1. alpha");
        assert_eq!(text_of(&blocks[3]), "2. beta");
        assert_eq!(text_of(&blocks[4]), "• bullet");
        assert_eq!(table(&blocks), grid(&[&["x", "y"], &["1", "2"]]));
        assert_eq!(text_of(blocks.last().unwrap()), "Note end");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn epub_extraction() {
        let dir = tmp("epub");
        let path = dir.join("t.epub");
        let container = r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
        let opf = r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><manifest>
<item id="c1" href="text/ch%201.xhtml" media-type="application/xhtml+xml"/>
<item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
<item id="css" href="style.css" media-type="text/css"/></manifest>
<spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#;
        let ch1 = r#"<?xml version="1.0" encoding="utf-8"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>T1</title><style>p { color: red }</style></head>
<body><h1>The Start</h1><p>It was a <em>dark</em> and
   <strong>stormy</strong>&nbsp;night.</p><script>alert(1)</script><p><img src="a.png" alt="map"/></p>
<table><tr><th>k</th><th>v</th></tr><tr><td>a</td><td>1</td></tr></table>
<ol><li>one</li><li>two</li></ol></body></html>"#;
        let ch2 = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Second</title></head><body><div><p>Only text.</p></div></body></html>"#;
        make_zip(
            &path,
            &[
                ("mimetype", "application/epub+zip"),
                ("META-INF/container.xml", container),
                ("OEBPS/content.opf", opf),
                ("OEBPS/text/ch 1.xhtml", ch1),
                ("OEBPS/text/ch2.xhtml", ch2),
            ],
        );
        let chapters = parse_epub(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "The Start");
        assert_eq!(chapters[1].title, "Second");
        let b = &chapters[0].blocks;
        assert!(matches!(&b[0], Block::Heading(1, r) if plain(r) == "The Start"));
        assert_eq!(text_of(&b[1]), "It was a dark and stormy\u{a0}night.");
        assert!(has_bold(b, "stormy"));
        assert!(
            matches!(&b[1], Block::Para { runs, .. } if runs.iter().any(|r| r.italic && r.text == "dark"))
        );
        assert_eq!(text_of(&b[2]), "[image: map]");
        assert_eq!(table(b), grid(&[&["k", "v"], &["a", "1"]]));
        assert_eq!(text_of(&b[4]), "1. one");
        assert_eq!(text_of(&b[5]), "2. two");
        let all: String = b.iter().map(text_of).collect();
        assert!(!all.contains("alert") && !all.contains("color"));
        assert_eq!(text_of(&chapters[1].blocks[0]), "Only text.");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rtf_strip() {
        let src = br"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0 Times New Roman;}}{\colortbl;\red0\green0\blue0;}
{\*\generator Riched20;}\pard\f0\fs24 Hello {\b bold} caf\'e9\par
Second \u8212? dash\par
{\pict\wmetafile8 0102abcdef}\b Last\b0  line\par}";
        let blocks = parse_rtf(src);
        let texts: Vec<String> = blocks.iter().map(text_of).collect();
        assert_eq!(texts, ["Hello bold café", "Second — dash", "Last line"]);
        assert!(has_bold(&blocks, "bold"));
        assert!(has_bold(&blocks, "Last"));
        assert!(!has_bold(&blocks, "line"));
    }

    #[test]
    fn markdown_parses_items() {
        let items: Vec<markdown::Item> =
            markdown::parse("# Title\n\nSome *text*.\n\n- a\n- b\n\n```rust\nfn x() {}\n```\n")
                .collect();
        assert_eq!(items.len(), 4);
        assert!(matches!(items[0], markdown::Item::Heading(..)));
        assert!(matches!(items[2], markdown::Item::List { start: None, .. }));
    }

    #[test]
    fn markdown_edit_save_cycle() {
        let dir = tmp("md");
        let path = dir.join("n.md");
        std::fs::write(&path, "# Hi\n").unwrap();
        let (mut st, _) = State::new(Opened {
            path: path.clone(),
            doc: load(&path).unwrap(),
        });
        let insert = || Msg::Edit(text_editor::Action::Edit(text_editor::Edit::Insert('x')));
        assert_eq!(st.is_editing(), Some(false));
        // read-only: edits are dropped
        let _ = st.update(insert());
        assert!(!st.is_dirty());
        let _ = st.toggle_edit();
        assert_eq!(st.is_editing(), Some(true));
        let _ = st.update(insert());
        assert!(st.is_dirty());
        // leaving edit mode while dirty is refused with a status message
        let (_, ev) = st.update(Msg::ToggleEdit);
        assert!(matches!(ev, Some(Event::Status(_))));
        assert_eq!(st.is_editing(), Some(true));
        let _ = st.update(Msg::Saved(Ok(())));
        assert!(!st.is_dirty());
        let _ = st.toggle_edit();
        assert_eq!(st.is_editing(), Some(false));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn views_build_and_paginate() {
        let many: Vec<Block> = (0..450)
            .map(|i| Block::Para {
                runs: vec![Run {
                    text: format!("p{i}"),
                    ..Run::default()
                }],
                indent: 0,
            })
            .collect();
        let (mut st, _) = State::new(Opened {
            path: "big.docx".into(),
            doc: Doc::Rich(many),
        });
        assert_eq!(st.page_range(450), 0..200);
        let _ = st.update(Msg::Page(2));
        assert_eq!(st.page_range(450), 400..450);
        let _ = st.view();

        let ch = |t: &str| Chapter {
            title: t.into(),
            blocks: vec![
                Block::Heading(
                    1,
                    vec![Run {
                        text: t.into(),
                        bold: false,
                        italic: false,
                    }],
                ),
                Block::Table(vec![vec!["a".into(), "b".into()]]),
                Block::Note("[image]".into()),
            ],
        };
        let path = PathBuf::from("/nonexistent/book-views.epub");
        let (mut st, _) = State::new(Opened {
            path: path.clone(),
            doc: Doc::Book(vec![ch("one"), ch("two")]),
        });
        let _ = st.update(Msg::Chapter(1));
        let _ = st.view();
        // position is remembered for the session
        let (st2, _) = State::new(Opened {
            path,
            doc: Doc::Book(vec![ch("one"), ch("two")]),
        });
        assert!(matches!(st2.body, Body::Book { current: 1, .. }));

        let (mut st, _) = State::new(Opened {
            path: "x.md".into(),
            doc: Doc::Markdown("# A\n\ntext [link](https://example.com)\n".into()),
        });
        let _ = st.view();
        for editing in [false, true] {
            if let Body::Markdown(md) = &mut st.body {
                md.editing = editing;
            }
            let Body::Markdown(md) = &st.body else {
                unreachable!()
            };
            let _ = st.md_view(md, 1200.0);
            let _ = st.md_view(md, 500.0);
        }
        let url = markdown::Url::parse("https://example.com").unwrap();
        let (_, ev) = st.update(Msg::Link(url));
        assert!(matches!(ev, Some(Event::Status(s)) if s.contains("example.com")));
    }

    #[test]
    fn rich_docs_have_no_edit_mode() {
        let (st, _) = State::new(Opened {
            path: "x.rtf".into(),
            doc: Doc::Rich(parse_rtf(br"{\rtf1 hi\par}")),
        });
        assert_eq!(st.is_editing(), None);
        assert!(!st.is_dirty());
    }
}
