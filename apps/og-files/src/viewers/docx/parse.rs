//! Reading a .docx: a span tree over document.xml (every element knows its
//! exact byte range, so untouched XML can be re-emitted verbatim), then the
//! editor model on top of it.

use super::model::{
    next_id, Block, Cell, Doc, Entry, Fmt, Inline, Obj, ObjKind, Opaque, Para, Row, Table, TextRun,
};
use super::styles::{self, Numbering, Styles};
use quick_xml::events::Event as X;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::ops::Range;
use std::sync::Arc;

/// An element and where it sits in the source.
#[derive(Debug, Clone)]
pub struct Node {
    /// local name (prefix dropped)
    pub name: Box<str>,
    pub start: usize,
    /// end of the start tag
    pub tag_end: usize,
    /// content between the tags (empty for `<x/>`)
    pub inner: Range<usize>,
    pub end: usize,
    pub kids: Vec<Node>,
}

impl Node {
    pub fn raw<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }

    pub fn self_closing(&self) -> bool {
        self.tag_end == self.end && self.inner.is_empty()
    }

    /// Attribute by local name, unescaped.
    pub fn attr(&self, src: &str, local: &str) -> Option<String> {
        let tag = &src[self.start..self.tag_end];
        let mut r = Reader::from_str(tag);
        let e = match r.read_event() {
            Ok(X::Start(e)) | Ok(X::Empty(e)) => e,
            _ => return None,
        };
        e.attributes()
            .flatten()
            .find(|a| a.key.local_name().as_ref() == local.as_bytes())
            .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.kids.iter().find(|k| &*k.name == name)
    }

    /// First descendant (depth first) named `name`.
    pub fn find(&self, name: &str) -> Option<&Node> {
        for k in &self.kids {
            if &*k.name == name {
                return Some(k);
            }
            if let Some(f) = k.find(name) {
                return Some(f);
            }
        }
        None
    }

    /// Visible text of all `t` descendants (tabs as tabs).
    pub fn text(&self, src: &str) -> String {
        let mut s = String::new();
        self.text_into(src, &mut s);
        s
    }

    fn text_into(&self, src: &str, out: &mut String) {
        for k in &self.kids {
            match &*k.name {
                "t" => out.push_str(&unescape(&src[k.inner.clone()])),
                "tab" => out.push('\t'),
                "br" | "cr" => out.push(' '),
                "instrText" | "delText" | "Fallback" => {}
                "p" => {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    k.text_into(src, out);
                }
                _ => k.text_into(src, out),
            }
        }
    }
}

pub fn unescape(s: &str) -> String {
    match quick_xml::escape::unescape(s) {
        Ok(c) => c.into_owned(),
        Err(_) => s.to_string(),
    }
}

fn local(qname: &[u8]) -> Box<str> {
    let l = match qname.iter().position(|&b| b == b':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    };
    String::from_utf8_lossy(l).into()
}

/// Parse XML into element nodes with byte spans (top-level elements).
pub fn tree(src: &str) -> Result<Vec<Node>, String> {
    let mut r = Reader::from_str(src);
    let mut stack: Vec<Node> = Vec::new();
    let mut top: Vec<Node> = Vec::new();
    let leaf = |name: &[u8], start: usize, end: usize| Node {
        name: local(name),
        start,
        tag_end: end,
        inner: end..end,
        end,
        kids: Vec::new(),
    };
    loop {
        let before = r.buffer_position() as usize;
        let ev = r
            .read_event()
            .map_err(|e| format!("XML error at {before}: {e}"))?;
        let after = r.buffer_position() as usize;
        match ev {
            X::Start(e) => {
                let mut n = leaf(e.name().as_ref(), before, after);
                n.end = usize::MAX;
                stack.push(n);
            }
            X::Empty(e) => {
                let n = leaf(e.name().as_ref(), before, after);
                match stack.last_mut() {
                    Some(p) => p.kids.push(n),
                    None => top.push(n),
                }
            }
            X::End(_) => {
                let mut n = stack.pop().ok_or("unbalanced XML")?;
                n.inner = n.tag_end..before;
                n.end = after;
                match stack.last_mut() {
                    Some(p) => p.kids.push(n),
                    None => top.push(n),
                }
            }
            X::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("truncated XML".into());
    }
    Ok(top)
}

// ------------------------------------------------------------- document

/// Run formatting, display size/colour and char style from an rPr node.
fn run_props(
    src: &str,
    rpr: Option<&Node>,
) -> (Fmt, Option<f32>, Option<[u8; 3]>, Option<Arc<str>>) {
    let Some(rpr) = rpr else {
        return (Fmt::default(), None, None, None);
    };
    let l = styles::look_of(src, rpr);
    let rstyle = rpr
        .child("rStyle")
        .and_then(|n| n.attr(src, "val"))
        .map(Arc::from);
    (
        Fmt {
            b: l.b,
            i: l.i,
            u: l.u,
            s: l.s,
        },
        l.size,
        l.color,
        rstyle,
    )
}

/// A text run from a standalone rPr string (the paragraph mark's, for
/// typing into an empty paragraph).
pub fn run_template(rpr: Option<&str>) -> TextRun {
    let parsed = rpr.and_then(|r| tree(r).ok()).and_then(|mut t| t.pop());
    let (fmt, size, color, rstyle) = match (&parsed, rpr) {
        (Some(n), Some(r)) => run_props(r, Some(n)),
        _ => run_props("", None),
    };
    TextRun {
        text: String::new(),
        fmt,
        orig: fmt,
        rpr: rpr.map(Arc::from),
        size,
        color,
        rstyle,
    }
}

fn run_from(src: &str, n: &Node) -> Inline {
    let mut rpr = None;
    let mut text = String::new();
    let mut simple = true;
    let mut page = false;
    for k in &n.kids {
        match &*k.name {
            "rPr" => rpr = Some(k),
            "t" => text.push_str(&unescape(&src[k.inner.clone()])),
            "tab" => text.push('\t'),
            "br" => match k.attr(src, "type").as_deref() {
                Some("page") | Some("column") => {
                    simple = false;
                    page = true;
                }
                _ => text.push('\n'),
            },
            "cr" => text.push('\n'),
            "noBreakHyphen" => text.push('\u{2011}'),
            "softHyphen" => text.push('\u{AD}'),
            "lastRenderedPageBreak" => {}
            _ => simple = false,
        }
    }
    if simple {
        let (fmt, size, color, rstyle) = run_props(src, rpr);
        return Inline::Text(TextRun {
            text,
            fmt,
            orig: fmt,
            rpr: rpr.map(|r| Arc::from(r.raw(src))),
            size,
            color,
            rstyle,
        });
    }
    let xml: Arc<str> = Arc::from(n.raw(src));
    let pic = ["drawing", "pict", "object"]
        .iter()
        .any(|t| n.find(t).is_some());
    let (kind, label) = if pic {
        let rid = n
            .find("blip")
            .and_then(|b| b.attr(src, "embed"))
            .or_else(|| n.find("imagedata").and_then(|i| i.attr(src, "id")));
        let (w, h) = n
            .find("extent")
            .map(|e| {
                let emu = |a| {
                    e.attr(src, a)
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(0.0)
                };
                (emu("cx") / 9525.0, emu("cy") / 9525.0)
            })
            .unwrap_or((0.0, 0.0));
        (ObjKind::Image { rid, w, h }, "[image]".to_string())
    } else if page {
        (ObjKind::PageBreak, "page break".to_string())
    } else if !text.is_empty() {
        (ObjKind::Text, text)
    } else if n.find("footnoteReference").is_some() || n.find("endnoteReference").is_some() {
        (ObjKind::Text, "*".to_string())
    } else if n.find("sym").is_some() {
        (ObjKind::Text, "•".to_string())
    } else {
        (ObjKind::Hidden, String::new())
    };
    Inline::Obj(Obj {
        xml,
        kind,
        label: Arc::from(label),
    })
}

pub fn para_from(src: &str, n: &Node) -> Para {
    let mut ppr = None;
    let mut inl = Vec::new();
    for k in &n.kids {
        match &*k.name {
            "pPr" => ppr = Some(k.raw(src).to_string()),
            "r" => inl.push(run_from(src, k)),
            name => {
                let label = k.text(src);
                let kind = if name == "hyperlink" && !label.is_empty() {
                    ObjKind::Link
                } else if label.is_empty() || matches!(name, "del" | "moveFrom") {
                    ObjKind::Hidden
                } else {
                    ObjKind::Text
                };
                inl.push(Inline::Obj(Obj {
                    xml: Arc::from(k.raw(src)),
                    kind,
                    label: Arc::from(label.replace('\n', " ")),
                }))
            }
        }
    }
    let mut p = Para {
        id: next_id(),
        rev: next_id(),
        orig: Some(n.start..n.end),
        open: (!n.self_closing()).then(|| Arc::from(&src[n.start..n.tag_end])),
        ppr: None,
        props: Default::default(),
        inl,
    };
    p.set_ppr(ppr);
    p.orig = Some(n.start..n.end);
    p
}

fn opaque(src: &str, n: &Node, visible_label: Option<&str>) -> Block {
    let text = n.text(src);
    let label = match visible_label {
        Some(l) if text.trim().is_empty() => l.to_string(),
        Some(l) => format!("{l}\n{}", text.trim()),
        None => text.trim().to_string(),
    };
    Block::Opaque(Opaque {
        id: next_id(),
        span: n.start..n.end,
        visible: visible_label.is_some() || !label.is_empty(),
        label,
    })
}

fn block_from(src: &str, n: &Node, in_cell: bool) -> Block {
    match &*n.name {
        "p" => Block::Para(para_from(src, n)),
        "tbl" if !in_cell => Block::Table(table_from(src, n)),
        "tbl" => opaque(src, n, Some("[nested table]")),
        "sdt" => opaque(src, n, Some("[content control]")),
        "sectPr" | "bookmarkStart" | "bookmarkEnd" | "proofErr" | "permStart" | "permEnd"
        | "commentRangeStart" | "commentRangeEnd" => Block::Opaque(Opaque {
            id: next_id(),
            span: n.start..n.end,
            label: String::new(),
            visible: false,
        }),
        _ => opaque(src, n, None),
    }
}

fn entries(src: &str, kids: &[&Node], in_cell: bool) -> Vec<Entry> {
    let mut prev = kids.first().map(|k| k.start).unwrap_or(0);
    kids.iter()
        .map(|k| {
            let e = Entry {
                gap: prev..k.start,
                block: block_from(src, k, in_cell),
            };
            prev = k.end;
            e
        })
        .collect()
}

fn table_from(src: &str, n: &Node) -> Table {
    let grid = n
        .child("tblGrid")
        .map(|g| {
            g.kids
                .iter()
                .filter(|c| &*c.name == "gridCol")
                .map(|c| c.attr(src, "w").and_then(|v| v.parse().ok()).unwrap_or(0.0))
                .collect()
        })
        .unwrap_or_default();
    let mut pieces = Vec::new();
    let mut last = n.start;
    let mut rows = Vec::new();
    for tr in n.kids.iter().filter(|k| &*k.name == "tr") {
        let mut cells = Vec::new();
        for tc in tr.kids.iter().filter(|k| &*k.name == "tc") {
            let span = tc
                .child("tcPr")
                .and_then(|p| p.child("gridSpan"))
                .and_then(|g| g.attr(src, "val"))
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            let content: Vec<&Node> = tc.kids.iter().filter(|k| &*k.name != "tcPr").collect();
            let first = content.first().map(|k| k.start).unwrap_or(tc.inner.end);
            pieces.push(last..first);
            last = content.last().map(|k| k.end).unwrap_or(first);
            cells.push(Cell {
                span,
                entries: entries(src, &content, true),
            });
        }
        rows.push(Row { cells });
    }
    pieces.push(last..n.end);
    Table {
        id: next_id(),
        rev: next_id(),
        orig: Some(n.start..n.end),
        pieces,
        grid,
        fixed: n
            .child("tblPr")
            .and_then(|p| p.child("tblLayout"))
            .and_then(|l| l.attr(src, "type"))
            .is_some_and(|v| v == "fixed"),
        rows,
    }
}

/// Model of a document.xml. Fails on documents the editor can't represent
/// safely (the caller falls back to the read-only viewer).
pub fn document(
    src: Arc<str>,
    styles: Arc<Styles>,
    numbering: Arc<Numbering>,
) -> Result<Doc, String> {
    let nodes = tree(&src)?;
    let root = nodes
        .iter()
        .find(|n| &*n.name == "document")
        .ok_or("no w:document element")?;
    let tag = &src[root.start + 1..root.tag_end];
    let ns = match tag.split([' ', '>', '/', '\n', '\r', '\t']).next() {
        Some(q) if q.contains(':') => q.split(':').next().unwrap_or("w").to_string(),
        _ => return Err("unprefixed WordprocessingML is not editable".into()),
    };
    let body = root.child("body").ok_or("no w:body")?;
    let kids: Vec<&Node> = body.kids.iter().collect();
    let first = kids.first().map(|k| k.start).unwrap_or(body.inner.end);
    let last = kids.last().map(|k| k.end).unwrap_or(first);
    let body_entries = entries(&src, &kids, false);
    Ok(Doc {
        prefix: 0..first,
        suffix: last..src.len(),
        body: body_entries.into_iter().map(Arc::new).collect(),
        src,
        ns,
        styles,
        numbering,
    })
}

// -------------------------------------------------------------- package

/// cap on the whole file and on any single part (zip-bomb guard)
pub const FILE_LIMIT: u64 = 256 * 1024 * 1024;
const PART_LIMIT: u64 = 64 * 1024 * 1024;
const IMAGE_BUDGET: usize = 96 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Package {
    pub bytes: Arc<Vec<u8>>,
    /// main document part, e.g. "word/document.xml"
    pub main: String,
    pub doc: Doc,
    /// rId → image bytes
    pub images: HashMap<String, Arc<Vec<u8>>>,
    pub numbering_part: Option<String>,
    pub numbering_xml: Option<String>,
}

pub fn part_text(z: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let f = z.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.take(PART_LIMIT).read_to_end(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn part_bytes(z: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<Vec<u8>> {
    let f = z.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.take(PART_LIMIT).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Relationship Id → (Type, Target) of a .rels part.
pub fn rels(xml: &str) -> Vec<(String, String, String)> {
    let Ok(nodes) = tree(xml) else {
        return Vec::new();
    };
    let Some(root) = nodes.first() else {
        return Vec::new();
    };
    root.kids
        .iter()
        .filter(|k| &*k.name == "Relationship")
        .filter_map(|k| {
            Some((
                k.attr(xml, "Id")?,
                k.attr(xml, "Type").unwrap_or_default(),
                k.attr(xml, "Target")?,
            ))
        })
        .collect()
}

/// Resolve a relationship target against the source part's folder.
pub fn resolve(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

pub fn dir_of(part: &str) -> &str {
    part.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

pub fn open_package(bytes: Vec<u8>) -> Result<Package, String> {
    let bytes = Arc::new(bytes);
    let mut z = zip::ZipArchive::new(Cursor::new(bytes.as_slice()))
        .map_err(|e| format!("not a valid .docx (zip): {e}"))?;
    let main = part_text(&mut z, "_rels/.rels")
        .and_then(|r| {
            rels(&r)
                .into_iter()
                .find(|(_, t, _)| t.ends_with("/officeDocument"))
                .map(|(_, _, target)| resolve("", &target))
        })
        .unwrap_or_else(|| "word/document.xml".into());
    let src = part_text(&mut z, &main).ok_or_else(|| format!("{main} missing or not UTF-8"))?;
    let dir = dir_of(&main).to_string();
    let doc_rels = part_text(&mut z, &rels_path(&main))
        .map(|r| rels(&r))
        .unwrap_or_default();
    let target = |kind: &str| {
        doc_rels
            .iter()
            .find(|(_, t, _)| t.ends_with(kind))
            .map(|(_, _, target)| resolve(&dir, target))
    };
    let styles = target("/styles")
        .and_then(|p| part_text(&mut z, &p))
        .map(|s| Styles::parse(&s))
        .unwrap_or_default();
    let numbering_part = target("/numbering");
    let numbering_xml = numbering_part.as_ref().and_then(|p| part_text(&mut z, p));
    let numbering = numbering_xml
        .as_deref()
        .map(Numbering::parse)
        .unwrap_or_default();
    let mut images = HashMap::new();
    let mut budget = IMAGE_BUDGET;
    for (id, t, target) in &doc_rels {
        if !t.ends_with("/image") {
            continue;
        }
        let ext = target.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff"
        ) {
            continue;
        }
        if let Some(b) = part_bytes(&mut z, &resolve(&dir, target)) {
            if b.len() <= budget {
                budget -= b.len();
                images.insert(id.clone(), Arc::new(b));
            }
        }
    }
    drop(z);
    let doc = document(Arc::from(src), Arc::new(styles), Arc::new(numbering))?;
    Ok(Package {
        bytes,
        main,
        doc,
        images,
        numbering_part,
        numbering_xml,
    })
}
