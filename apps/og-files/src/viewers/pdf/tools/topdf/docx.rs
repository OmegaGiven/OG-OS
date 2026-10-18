//! Word .docx → document, through the app's own DOCX model (the one the
//! editor uses): paragraph/character styles resolved, headings, run
//! formatting (bold/italic/underline/strike/size/colour), alignment,
//! spacing and indents, numbered and bulleted lists with Word's own
//! numbering, tables (grid widths, column spans, nested tables), inline
//! images, hyperlinks, page breaks, and the page size and margins of the
//! final section.
//!
//! Not rendered: headers/footers, footnote text, floating-object
//! positioning, text boxes, vertical cell merges, charts/SmartArt.

use super::ir::{self, Block, Cell, Col, Doc, Files, Inline, Page, Para, Style, Table};
use crate::viewers::docx::model::{self, Editor, ObjKind};
use crate::viewers::docx::parse::{self, Node};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::Arc;

struct Cx<'a> {
    ed: &'a Editor,
    labels: HashMap<u64, String>,
    images: &'a HashMap<String, Arc<Vec<u8>>>,
    /// relationship id → hyperlink target
    links: HashMap<String, String>,
    files: Files,
    loaded: HashMap<String, Option<ir::Img>>,
    body_w: f32,
}

fn twips(v: Option<f32>) -> Option<f32> {
    v.filter(|v| v.is_finite()).map(|v| v / 20.0)
}

fn attr_in(xml: &str, local: &str) -> Option<String> {
    // r:id="…" / r:embed="…" on the first element of a raw XML snippet
    let needle = format!(":{local}=\"");
    let i = xml.find(&needle)? + needle.len();
    let j = xml[i..].find('"')?;
    Some(xml[i..i + j].to_string())
}

impl Cx<'_> {
    fn image(&mut self, rid: &str) -> Option<ir::Img> {
        if let Some(i) = self.loaded.get(rid) {
            return i.clone();
        }
        let img = self
            .images
            .get(rid)
            .and_then(|b| self.files.add(b.as_ref().clone()));
        self.loaded.insert(rid.to_string(), img.clone());
        img
    }

    fn inlines(&mut self, p: &model::Para) -> Vec<Inline> {
        let doc = &self.ed.doc;
        let mut out = Vec::new();
        for i in &p.inl {
            match i {
                model::Inline::Text(t) => {
                    let look = doc.run_look(p, t);
                    let st = Style {
                        bold: look.b.unwrap_or(false),
                        italic: look.i.unwrap_or(false),
                        underline: look.u.unwrap_or(false),
                        strike: look.s.unwrap_or(false),
                        size: look.size,
                        color: look.color.filter(|c| *c != [0, 0, 0]),
                        ..Default::default()
                    };
                    let text = t.text.replace('\u{AD}', "");
                    if !text.is_empty() {
                        out.push(Inline::Text(text, st));
                    }
                }
                model::Inline::Obj(o) => match &o.kind {
                    ObjKind::Hidden => {}
                    ObjKind::PageBreak => out.push(Inline::Text("\u{c}".into(), Style::default())),
                    ObjKind::Image { rid, w, h } => {
                        if let Some(mut img) = rid.as_deref().and_then(|r| self.image(r)) {
                            if *w > 0.0 && *h > 0.0 {
                                img.w = w * 0.75;
                                img.h = h * 0.75;
                            }
                            out.push(Inline::Image(img));
                        }
                    }
                    ObjKind::Link => {
                        let st = Style {
                            underline: true,
                            ..Default::default()
                        };
                        let text = Inline::Text(o.label.to_string(), st);
                        match attr_in(&o.xml, "id").and_then(|id| self.links.get(&id)) {
                            Some(url) => out.push(Inline::Link(url.clone(), vec![text])),
                            None => out.push(text),
                        }
                    }
                    ObjKind::Text => out.push(Inline::Text(o.label.to_string(), Style::default())),
                },
            }
        }
        out
    }

    fn para(&mut self, p: &model::Para, out: &mut Vec<Block>) {
        let doc = &self.ed.doc;
        let res = doc.styles.resolve(p.props.style.as_deref());
        let inl = self.inlines(p);
        // page breaks (U+000C markers) split the paragraph
        let mut parts: Vec<Vec<Inline>> = vec![Vec::new()];
        for i in inl {
            if matches!(&i, Inline::Text(t, _) if t == "\u{c}") {
                parts.push(Vec::new());
            } else if let Some(last) = parts.last_mut() {
                last.push(i);
            }
        }
        let n = parts.len();
        for (k, inl) in parts.into_iter().enumerate() {
            if k > 0 {
                out.push(Block::PageBreak);
            }
            if inl.is_empty() && n > 1 {
                continue;
            }
            if let Some(level) = res.heading.or(res.title.then_some(1)) {
                if !inl.is_empty() {
                    out.push(Block::Heading(level, inl));
                    continue;
                }
            }
            if let Some(label) = self.labels.get(&p.id) {
                let lvl = self.ed.list_of(p).map(|(_, l, _)| l).unwrap_or(0);
                let ind = twips(p.props.ind_left.or(res.ind_left))
                    .or_else(|| {
                        self.ed
                            .list_of(p)
                            .and_then(|(id, l, _)| doc.numbering.level(&id, l))
                            .and_then(|l| twips(l.ind_left))
                    })
                    .unwrap_or(18.0 * (f32::from(lvl) + 1.0));
                out.push(Block::Labeled {
                    label: label.clone(),
                    indent: (ind - 18.0).max(0.0),
                    inl,
                });
                continue;
            }
            let align = match p.props.align.or(res.align).unwrap_or_default() {
                model::Align::Left => ir::Align::Left,
                model::Align::Center => ir::Align::Center,
                model::Align::Right => ir::Align::Right,
                model::Align::Justify => ir::Align::Justify,
            };
            out.push(Block::Para(Para {
                inl,
                align,
                indent: twips(p.props.ind_left.or(res.ind_left))
                    .unwrap_or(0.0)
                    .clamp(0.0, 300.0),
                above: twips(p.props.before.or(res.before)).map(|v| v.min(72.0)),
                below: twips(p.props.after.or(res.after)).map(|v| v.min(72.0)),
            }));
        }
    }

    fn table(&mut self, t: &model::Table, out: &mut Vec<Block>, depth: usize) {
        let total: f32 = t.grid.iter().sum::<f32>() / 20.0;
        let cols = if t.grid.is_empty() || total <= 0.0 {
            Vec::new()
        } else if total <= self.body_w {
            t.grid
                .iter()
                .map(|w| Col::Pt((w / 20.0).max(6.0)))
                .collect()
        } else {
            t.grid
                .iter()
                .map(|w| Col::Fr((w / 20.0).max(1.0)))
                .collect()
        };
        let rows = t
            .rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| {
                        let mut blocks = Vec::new();
                        for e in &c.entries {
                            self.block(&e.block, &mut blocks, depth + 1);
                        }
                        Cell {
                            blocks,
                            colspan: c.span.max(1),
                            rowspan: 1,
                            fill: None,
                        }
                    })
                    .collect()
            })
            .collect();
        out.push(Block::Table(Table {
            cols,
            header: 0,
            rows,
            size: None,
            borders: true,
        }));
    }

    fn block(&mut self, b: &model::Block, out: &mut Vec<Block>, depth: usize) {
        match b {
            model::Block::Para(p) => self.para(p, out),
            model::Block::Table(t) => self.table(t, out, depth),
            model::Block::Opaque(o) if o.visible => {
                // nested tables and content controls are kept raw by the
                // model; parse them here
                let src = self.ed.doc.src.clone();
                let Some(xml) = src.get(o.span.clone()) else {
                    return;
                };
                let Ok(nodes) = parse::tree(xml) else {
                    return;
                };
                for n in &nodes {
                    self.raw(xml, n, out, depth);
                }
            }
            model::Block::Opaque(_) => {}
        }
    }

    fn raw(&mut self, xml: &str, n: &Node, out: &mut Vec<Block>, depth: usize) {
        if depth > 12 {
            return;
        }
        match &*n.name {
            "tbl" => {
                let t = parse::table_from(xml, n);
                self.table(&t, out, depth);
            }
            "p" => {
                let b = parse::block_from(xml, n, false);
                self.block(&b, out, depth);
            }
            "sdt" | "sdtContent" | "customXml" | "smartTag" => {
                for k in &n.kids {
                    self.raw(xml, k, out, depth + 1);
                }
            }
            _ => {
                let t = n.text(xml);
                if !t.trim().is_empty() {
                    out.push(Block::plain(t.trim()));
                }
            }
        }
    }
}

/// Page size and margins from the last `w:sectPr`.
fn page_of(src: &str) -> Page {
    let mut page = Page::A4;
    // last *opening* sectPr tag (`<w:sectPr`, not `</w:sectPr>`)
    let Some(open) = src
        .match_indices("sectPr")
        .filter_map(|(i, _)| {
            let lt = src[..i].rfind('<')?;
            let between = &src[lt + 1..i];
            (!between.starts_with('/') && !between.contains(['>', ' '])).then_some(lt)
        })
        .last()
    else {
        return page;
    };
    let rest = &src[open..];
    // through the closing tag (or just the tag when self-closing)
    let tag_end = rest.find('>').map_or(rest.len(), |i| i + 1);
    let end = if rest[..tag_end].ends_with("/>") {
        tag_end
    } else {
        rest[tag_end..]
            .find("sectPr>")
            .map(|i| tag_end + i + "sectPr>".len())
            .unwrap_or(rest.len())
    };
    let snippet = &rest[..end];
    let Ok(nodes) = parse::tree(snippet) else {
        return page;
    };
    let Some(sect) = nodes.first() else {
        return page;
    };
    let num = |n: &Node, a: &str| {
        n.attr(snippet, a)
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map(|v| v / 20.0)
    };
    if let Some(sz) = sect.child("pgSz") {
        if let (Some(w), Some(h)) = (num(sz, "w"), num(sz, "h")) {
            if (72.0..=4000.0).contains(&w) && (72.0..=4000.0).contains(&h) {
                page.w = w;
                page.h = h;
            }
        }
    }
    if let Some(m) = sect.child("pgMar") {
        let side = |a: &str, d: f32| {
            num(m, a)
                .map(|v| v.abs().min(page.w.min(page.h) / 3.0))
                .unwrap_or(d)
        };
        page.margin = [
            side("top", page.margin[0]),
            side("right", page.margin[1]),
            side("bottom", page.margin[2]),
            side("left", page.margin[3]),
        ];
    }
    page
}

fn zip_text(bytes: &[u8], name: &str) -> Option<String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let f = z.by_name(name).ok()?;
    let mut s = String::new();
    f.take(16 * 1024 * 1024).read_to_string(&mut s).ok()?;
    Some(s)
}

/// `dc:title` from docProps/core.xml.
fn core_title(bytes: &[u8]) -> Option<String> {
    let core = zip_text(bytes, "docProps/core.xml")?;
    let i = core.find(":title>")? + ":title>".len();
    let j = core[i..].find('<')?;
    let t = parse::unescape(core[i..i + j].trim());
    (!t.is_empty()).then_some(t)
}

pub fn convert(bytes: Vec<u8>) -> Result<Doc, String> {
    let pkg = parse::open_package(bytes)?;
    let links = zip_text(&pkg.bytes, &parse::rels_path(&pkg.main))
        .map(|r| {
            parse::rels(&r)
                .into_iter()
                .filter(|(_, t, _)| t.ends_with("/hyperlink"))
                .map(|(id, _, target)| (id, target))
                .collect()
        })
        .unwrap_or_default();
    let title = core_title(&pkg.bytes);
    let page = page_of(&pkg.doc.src);
    let ed = Editor::new(pkg.doc);
    let labels = crate::viewers::docx::list_labels(&ed);
    let mut cx = Cx {
        ed: &ed,
        labels,
        images: &pkg.images,
        links,
        files: Files::default(),
        loaded: HashMap::new(),
        body_w: page.body_w(),
    };
    let mut blocks = Vec::new();
    for e in &ed.doc.body {
        cx.block(&e.block, &mut blocks, 0);
    }
    // base size from the document defaults
    let size = ed
        .doc
        .styles
        .resolve(None)
        .look
        .size
        .filter(|s| (5.0..=30.0).contains(s))
        .unwrap_or(11.0);
    let mut doc = Doc::new(page);
    doc.title = title.or_else(|| {
        blocks.iter().find_map(|b| match b {
            Block::Heading(_, inl) => Some(super::plain_text(inl)),
            _ => None,
        })
    });
    doc.face = ir::Face::Sans;
    doc.size = size;
    doc.blocks = blocks;
    doc.files = cx.files;
    Ok(doc)
}
