//! Writing a .docx back. document.xml is re-assembled from the model:
//! untouched blocks are copied from the original text byte for byte, edited
//! paragraphs are regenerated (original `<w:p>` tag, pPr and each run's rPr
//! reused, only toggled b/i/u/strike merged in). Every other zip entry is
//! copied raw (same compressed bytes) unless the editor replaced it.

use super::model::{Attr, Block, Doc, Inline, Para, TextRun};
use super::parse::tree;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{Cursor, Write};

/// CT_PPr child order (ECMA-376 §17.3.1.26); Word rejects out-of-order pPr.
const PPR_ORDER: &[&str] = &[
    "pStyle",
    "keepNext",
    "keepLines",
    "pageBreakBefore",
    "framePr",
    "widowControl",
    "numPr",
    "suppressLineNumbers",
    "pBdr",
    "shd",
    "tabs",
    "suppressAutoHyphens",
    "kinsoku",
    "wordWrap",
    "overflowPunct",
    "topLinePunct",
    "autoSpaceDE",
    "autoSpaceDN",
    "bidi",
    "adjustRightInd",
    "snapToGrid",
    "spacing",
    "ind",
    "contextualSpacing",
    "mirrorIndents",
    "suppressOverlap",
    "jc",
    "textDirection",
    "textAlignment",
    "textboxTightWrap",
    "outlineLvl",
    "divId",
    "cnfStyle",
    "rPr",
    "sectPr",
    "pPrChange",
];

/// CT_RPr child order (§17.3.2.28).
const RPR_ORDER: &[&str] = &[
    "rStyle",
    "rFonts",
    "b",
    "bCs",
    "i",
    "iCs",
    "caps",
    "smallCaps",
    "strike",
    "dstrike",
    "outline",
    "shadow",
    "emboss",
    "imprint",
    "noProof",
    "snapToGrid",
    "vanish",
    "webHidden",
    "color",
    "spacing",
    "w",
    "kern",
    "position",
    "sz",
    "szCs",
    "highlight",
    "u",
    "effect",
    "bdr",
    "shd",
    "fitText",
    "vertAlign",
    "rtl",
    "cs",
    "em",
    "lang",
    "eastAsianLayout",
    "specVanish",
    "oMath",
    "rPrChange",
];

/// Set (`Some(xml)`) or remove (`None`) child `name` of a property element
/// (`tag` = "pPr" / "rPr"), keeping the schema order. Returns None when
/// the element ends up empty.
pub fn set_prop(
    props: Option<&str>,
    tag: &str,
    name: &str,
    new: Option<&str>,
    w: &str,
) -> Option<String> {
    let order = if tag == "rPr" { RPR_ORDER } else { PPR_ORDER };
    let bare_open = format!("<{w}:{tag}>");
    let (open, mut kids): (String, Vec<(String, String)>) = match props {
        Some(x) => {
            let Some(n) = tree(x).ok().and_then(|mut t| t.pop()) else {
                return Some(x.to_string());
            };
            let open = if n.self_closing() {
                let t = &x[n.start..n.tag_end];
                format!(
                    "{}>",
                    t.trim_end_matches('>').trim_end_matches('/').trim_end()
                )
            } else {
                x[n.start..n.tag_end].to_string()
            };
            let kids = n
                .kids
                .iter()
                .map(|k| (k.name.to_string(), k.raw(x).to_string()))
                .collect();
            (open, kids)
        }
        None => (bare_open.clone(), Vec::new()),
    };
    kids.retain(|(n, _)| n != name);
    if let Some(new) = new {
        let rank = |n: &str| order.iter().position(|o| *o == n);
        let mine = rank(name).unwrap_or(usize::MAX);
        let at = kids
            .iter()
            .position(|(n, _)| rank(n).is_some_and(|r| r > mine))
            .unwrap_or(kids.len());
        kids.insert(at, (name.to_string(), new.to_string()));
    }
    if kids.is_empty() && open == bare_open {
        return None;
    }
    let mut out = open;
    for (_, k) in kids {
        out.push_str(&k);
    }
    let _ = write!(out, "</{w}:{tag}>");
    Some(out)
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn attr_xml(w: &str, a: Attr, v: bool) -> String {
    match (a, v) {
        (Attr::Bold, true) => format!("<{w}:b/>"),
        (Attr::Bold, false) => format!(r#"<{w}:b {w}:val="0"/>"#),
        (Attr::Italic, true) => format!("<{w}:i/>"),
        (Attr::Italic, false) => format!(r#"<{w}:i {w}:val="0"/>"#),
        (Attr::Strike, true) => format!("<{w}:strike/>"),
        (Attr::Strike, false) => format!(r#"<{w}:strike {w}:val="0"/>"#),
        (Attr::Underline, true) => format!(r#"<{w}:u {w}:val="single"/>"#),
        (Attr::Underline, false) => format!(r#"<{w}:u {w}:val="none"/>"#),
    }
}

fn attr_name(a: Attr) -> &'static str {
    match a {
        Attr::Bold => "b",
        Attr::Italic => "i",
        Attr::Underline => "u",
        Attr::Strike => "strike",
    }
}

/// The run's rPr with its changed toggles merged in.
pub fn run_rpr(w: &str, t: &TextRun) -> Option<String> {
    let mut r = t.rpr.as_deref().map(str::to_string);
    for a in Attr::ALL {
        if t.fmt.get(a) != t.orig.get(a) {
            let x = t.fmt.get(a).map(|v| attr_xml(w, a, v));
            r = set_prop(r.as_deref(), "rPr", attr_name(a), x.as_deref(), w);
        }
    }
    r
}

fn run_xml(w: &str, t: &TextRun, out: &mut String) {
    let _ = write!(out, "<{w}:r>");
    if let Some(r) = run_rpr(w, t) {
        out.push_str(&r);
    }
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut String| {
        if buf.is_empty() {
            return;
        }
        let keep = buf.starts_with(char::is_whitespace)
            || buf.ends_with(char::is_whitespace)
            || buf.contains("  ");
        if keep {
            let _ = write!(out, r#"<{w}:t xml:space="preserve">"#);
        } else {
            let _ = write!(out, "<{w}:t>");
        }
        escape_text(buf, out);
        let _ = write!(out, "</{w}:t>");
        buf.clear();
    };
    for c in t.text.chars() {
        let el = match c {
            '\t' => "tab",
            '\n' => "br",
            '\u{2011}' => "noBreakHyphen",
            '\u{AD}' => "softHyphen",
            c => {
                buf.push(c);
                continue;
            }
        };
        flush(&mut buf, out);
        let _ = write!(out, "<{w}:{el}/>");
    }
    flush(&mut buf, out);
    let _ = write!(out, "</{w}:r>");
}

pub fn para_xml(doc: &Doc, p: &Para, out: &mut String) {
    if let Some(r) = &p.orig {
        out.push_str(&doc.src[r.clone()]);
        return;
    }
    let w = &doc.ns;
    match &p.open {
        Some(o) => out.push_str(o),
        None => {
            let _ = write!(out, "<{w}:p>");
        }
    }
    if let Some(ppr) = &p.ppr {
        out.push_str(ppr);
    }
    for i in &p.inl {
        match i {
            Inline::Text(t) if t.text.is_empty() => {}
            Inline::Text(t) => run_xml(w, t, out),
            Inline::Obj(o) => out.push_str(&o.xml),
        }
    }
    let _ = write!(out, "</{w}:p>");
}

fn block_xml(doc: &Doc, b: &Block, out: &mut String) {
    let src = &doc.src;
    match b {
        Block::Para(p) => para_xml(doc, p, out),
        Block::Opaque(o) => out.push_str(&src[o.span.clone()]),
        Block::Table(t) => {
            if let Some(r) = &t.orig {
                out.push_str(&src[r.clone()]);
                return;
            }
            let mut k = 0;
            for row in &t.rows {
                for cell in &row.cells {
                    if let Some(p) = t.pieces.get(k) {
                        out.push_str(&src[p.clone()]);
                    }
                    k += 1;
                    for e in &cell.entries {
                        out.push_str(&src[e.gap.clone()]);
                        block_xml(doc, &e.block, out);
                    }
                }
            }
            if let Some(p) = t.pieces.get(k) {
                out.push_str(&src[p.clone()]);
            }
        }
    }
}

/// document.xml for the current model.
pub fn document_xml(doc: &Doc) -> String {
    let src = &doc.src;
    let mut out = String::with_capacity(src.len() + 4096);
    out.push_str(&src[doc.prefix.clone()]);
    for e in &doc.body {
        out.push_str(&src[e.gap.clone()]);
        block_xml(doc, &e.block, &mut out);
    }
    out.push_str(&src[doc.suffix.clone()]);
    out
}

/// A new .docx: `orig` with `main` replaced by `doc_xml` and `parts`
/// replaced/added; everything else copied raw.
pub fn package(
    orig: &[u8],
    main: &str,
    doc_xml: &str,
    parts: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let err = |e: zip::result::ZipError| e.to_string();
    let mut zin = zip::ZipArchive::new(Cursor::new(orig)).map_err(err)?;
    let mut zout = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut done = std::collections::HashSet::new();
    for i in 0..zin.len() {
        let name = zin.by_index_raw(i).map_err(err)?.name().to_string();
        if name == main {
            zout.start_file(name.as_str(), opts).map_err(err)?;
            zout.write_all(doc_xml.as_bytes())
                .map_err(|e| e.to_string())?;
        } else if let Some(b) = parts.get(&name) {
            zout.start_file(name.as_str(), opts).map_err(err)?;
            zout.write_all(b).map_err(|e| e.to_string())?;
        } else {
            let f = zin.by_index_raw(i).map_err(err)?;
            zout.raw_copy_file(f).map_err(err)?;
        }
        done.insert(name);
    }
    for (name, b) in parts {
        if !done.contains(name) {
            zout.start_file(name.as_str(), opts).map_err(err)?;
            zout.write_all(b).map_err(|e| e.to_string())?;
        }
    }
    Ok(zout.finish().map_err(err)?.into_inner())
}

// ------------------------------------------------------- list definitions

const WML: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const NUMBERING_REL: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const NUMBERING_CT: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";

fn abstract_num(w: &str, id: u32, bullet: bool) -> String {
    let mut s = format!(
        r#"<{w}:abstractNum {w}:abstractNumId="{id}"><{w}:multiLevelType {w}:val="hybridMultilevel"/>"#
    );
    for l in 0..9u32 {
        let (fmt, text) = if bullet {
            ("bullet", ["•", "◦", "▪"][l as usize % 3].to_string())
        } else {
            (
                ["decimal", "lowerLetter", "lowerRoman"][l as usize % 3],
                format!("%{}.", l + 1),
            )
        };
        let _ = write!(
            s,
            r#"<{w}:lvl {w}:ilvl="{l}"><{w}:start {w}:val="1"/><{w}:numFmt {w}:val="{fmt}"/><{w}:lvlText {w}:val="{text}"/><{w}:lvlJc {w}:val="left"/><{w}:pPr><{w}:ind {w}:left="{}" {w}:hanging="360"/></{w}:pPr></{w}:lvl>"#,
            720 * (l + 1)
        );
    }
    let _ = write!(s, "</{w}:abstractNum>");
    s
}

/// Package edits that add a list definition.
pub struct ListAdded {
    pub num_id: String,
    pub numbering_xml: String,
    /// part name → new bytes ([Content_Types].xml / rels when the part is new)
    pub parts: BTreeMap<String, Vec<u8>>,
}

fn max_attr(xml: &str, elem: &str, attr: &str) -> u32 {
    let Ok(t) = tree(xml) else { return 0 };
    t.first()
        .map(|root| {
            root.kids
                .iter()
                .filter(|k| &*k.name == elem)
                .filter_map(|k| k.attr(xml, attr)?.parse::<u32>().ok())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Add a bullet/number list definition. `existing` = current numbering
/// part text if the document has one; otherwise a new part is created and
/// registered in the document rels and [Content_Types].xml (`rels_xml`,
/// `types_xml` = their current text).
pub fn add_list(
    bullet: bool,
    numbering_part: &str,
    existing: Option<&str>,
    rels_part: (&str, Option<&str>),
    types_xml: Option<&str>,
) -> Option<ListAdded> {
    let mut parts = BTreeMap::new();
    let xml = match existing {
        Some(x) => {
            let root = tree(x).ok()?.into_iter().next()?;
            let tag = &x[root.start + 1..root.tag_end];
            let w = tag
                .split(':')
                .next()
                .filter(|p| !p.contains(' '))
                .unwrap_or("w");
            let a = max_attr(x, "abstractNum", "abstractNumId") + 1;
            let n = max_attr(x, "num", "numId") + 1;
            let abs = abstract_num(w, a, bullet);
            let num =
                format!(r#"<{w}:num {w}:numId="{n}"><{w}:abstractNumId {w}:val="{a}"/></{w}:num>"#);
            // abstractNums must precede nums
            let first_num = root
                .kids
                .iter()
                .find(|k| matches!(&*k.name, "num" | "numIdMacAtCleanup"))
                .map(|k| k.start)
                .unwrap_or(root.inner.end);
            if root.self_closing() {
                return None;
            }
            let mut s = String::with_capacity(x.len() + abs.len() + num.len());
            s.push_str(&x[..first_num]);
            s.push_str(&abs);
            let last_num = root
                .kids
                .iter()
                .filter(|k| &*k.name == "num")
                .map(|k| k.end)
                .max()
                .unwrap_or(first_num);
            s.push_str(&x[first_num..last_num]);
            s.push_str(&num);
            s.push_str(&x[last_num..]);
            return Some(ListAdded {
                num_id: n.to_string(),
                numbering_xml: s,
                parts,
            });
        }
        None => format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{WML}">{}<w:num w:numId="1"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#,
            abstract_num("w", 1, bullet)
        ),
    };
    // a brand-new part: register it
    let (rels_name, rels) = rels_part;
    let rels = rels.unwrap_or(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#,
    );
    let ids: Vec<String> = super::parse::rels(rels).into_iter().map(|r| r.0).collect();
    let mut k = 1;
    while ids.contains(&format!("rIdOg{k}")) {
        k += 1;
    }
    let target = numbering_part.rsplit('/').next().unwrap_or(numbering_part);
    let rel = format!(r#"<Relationship Id="rIdOg{k}" Type="{NUMBERING_REL}" Target="{target}"/>"#);
    let at = rels.rfind("</Relationships>")?;
    parts.insert(
        rels_name.to_string(),
        format!("{}{rel}{}", &rels[..at], &rels[at..]).into_bytes(),
    );
    let types = types_xml?;
    let ov = format!(r#"<Override PartName="/{numbering_part}" ContentType="{NUMBERING_CT}"/>"#);
    let at = types.rfind("</Types>")?;
    parts.insert(
        "[Content_Types].xml".into(),
        format!("{}{ov}{}", &types[..at], &types[at..]).into_bytes(),
    );
    Some(ListAdded {
        num_id: "1".into(),
        numbering_xml: xml,
        parts,
    })
}
