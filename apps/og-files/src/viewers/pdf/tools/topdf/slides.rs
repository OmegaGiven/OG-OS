//! Presentations (.pptx, .odp): one slide per page at the slide's size.
//! Text boxes, pictures and tables are placed at their positions (EMU /
//! ODF lengths → points) with basic run formatting (bold, italic,
//! underline, strike, size, RGB colour), paragraph alignment, bullets and
//! solid fills/backgrounds. Placeholders without their own position take
//! it from the slide layout / master.
//!
//! Not rendered: theme colours and fonts, gradients, shapes' outlines and
//! geometry (boxes are rectangles), charts, SmartArt, animations, notes.

use super::ir::{Align, Block, Cell, Col, Doc, Files, Inline, Page, Para, Rgb, Style, Table};
use crate::viewers::docx::parse::{self, Node};
use std::collections::HashMap;
use std::io::{Cursor, Read};

const EMU_PER_PT: f32 = 12_700.0;
const PART_LIMIT: u64 = 64 * 1024 * 1024;
const MAX_SLIDES: usize = 2000;

type Zip<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

fn part_bytes(z: &mut Zip<'_>, name: &str) -> Option<Vec<u8>> {
    let f = z.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.take(PART_LIMIT).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn part_text(z: &mut Zip<'_>, name: &str) -> Option<String> {
    part_bytes(z, name).and_then(|b| String::from_utf8(b).ok())
}

fn num(n: &Node, src: &str, a: &str) -> Option<f32> {
    n.attr(src, a)
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite())
}

fn hex(v: &str) -> Option<Rgb> {
    if v.len() != 6 {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&v[i..i + 2], 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

/// `<a:solidFill><a:srgbClr val="…"/></a:solidFill>` directly under `n`.
fn solid(n: &Node, src: &str) -> Option<Rgb> {
    let f = n.child("solidFill")?;
    let c = f.child("srgbClr")?;
    hex(&c.attr(src, "val")?)
}

// ------------------------------------------------------------------ PPTX

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Group transform: child coordinates → slide points.
#[derive(Clone, Copy)]
struct Xf {
    dx: f32,
    dy: f32,
    sx: f32,
    sy: f32,
}

impl Xf {
    const ID: Xf = Xf {
        dx: 0.0,
        dy: 0.0,
        sx: 1.0,
        sy: 1.0,
    };

    fn apply(&self, r: Rect) -> Rect {
        Rect {
            x: self.dx + r.x * self.sx,
            y: self.dy + r.y * self.sy,
            w: r.w * self.sx,
            h: r.h * self.sy,
        }
    }
}

fn xfrm_rect(x: &Node, src: &str) -> Option<Rect> {
    let off = x.child("off")?;
    let ext = x.child("ext")?;
    Some(Rect {
        x: num(off, src, "x")? / EMU_PER_PT,
        y: num(off, src, "y")? / EMU_PER_PT,
        w: num(ext, src, "cx")? / EMU_PER_PT,
        h: num(ext, src, "cy")? / EMU_PER_PT,
    })
}

/// Placeholder key: (type, idx).
fn ph_of(n: &Node, src: &str) -> Option<(String, Option<String>)> {
    let ph = n.find("nvPr")?.child("ph")?;
    Some((
        ph.attr(src, "type").unwrap_or_else(|| "body".into()),
        ph.attr(src, "idx"),
    ))
}

/// Placeholder positions of a layout or master part.
struct Phs {
    by_idx: HashMap<String, Rect>,
    by_type: HashMap<String, Rect>,
}

fn placeholders(src: &str) -> Phs {
    let mut p = Phs {
        by_idx: HashMap::new(),
        by_type: HashMap::new(),
    };
    let Ok(nodes) = parse::tree(src) else {
        return p;
    };
    let mut stack: Vec<&Node> = nodes.iter().collect();
    while let Some(n) = stack.pop() {
        if &*n.name == "sp" {
            if let (Some((ty, idx)), Some(r)) = (
                ph_of(n, src),
                n.child("spPr")
                    .and_then(|s| s.child("xfrm"))
                    .and_then(|x| xfrm_rect(x, src)),
            ) {
                if let Some(i) = idx {
                    p.by_idx.entry(i).or_insert(r);
                }
                p.by_type.entry(ty).or_insert(r);
            }
        }
        stack.extend(n.kids.iter());
    }
    p
}

fn is_title(ty: &str) -> bool {
    matches!(ty, "title" | "ctrTitle")
}

struct Slide<'a> {
    src: &'a str,
    rels: HashMap<String, String>,
    layouts: [&'a Phs; 2],
}

struct Deck<'z, 'b> {
    z: &'z mut Zip<'b>,
    files: Files,
}

fn runs_of(p: &Node, src: &str, default_size: f32) -> Vec<Inline> {
    let mut out = Vec::new();
    for k in &p.kids {
        match &*k.name {
            "r" | "fld" => {
                let text = k
                    .child("t")
                    .map(|t| parse::unescape(&src[t.inner.clone()]))
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let mut st = Style {
                    size: Some(default_size),
                    ..Default::default()
                };
                if let Some(rp) = k.child("rPr") {
                    let on = |a: &str| rp.attr(src, a).is_some_and(|v| v == "1" || v == "true");
                    st.bold = on("b");
                    st.italic = on("i");
                    st.underline = rp.attr(src, "u").is_some_and(|v| v != "none");
                    st.strike = rp.attr(src, "strike").is_some_and(|v| v != "noStrike");
                    if let Some(sz) = num(rp, src, "sz") {
                        st.size = Some((sz / 100.0).clamp(4.0, 200.0));
                    }
                    st.color = solid(rp, src);
                    if let Some(b) = num(rp, src, "baseline") {
                        st.sup = b > 0.0;
                        st.sub = b < 0.0;
                    }
                }
                out.push(Inline::Text(text, st));
            }
            "br" => out.push(Inline::Break),
            _ => {}
        }
    }
    out
}

fn text_body(tx: &Node, src: &str, ty: &str) -> Vec<Block> {
    let title = is_title(ty);
    let bulleted = matches!(ty, "body" | "obj");
    let default_size = if title {
        36.0
    } else if ty == "subTitle" {
        22.0
    } else {
        18.0
    };
    let mut out = Vec::new();
    let mut counters = [0u32; 9];
    for p in tx.kids.iter().filter(|k| &*k.name == "p") {
        let ppr = p.child("pPr");
        let lvl = ppr
            .and_then(|pp| num(pp, src, "lvl"))
            .unwrap_or(0.0)
            .clamp(0.0, 8.0) as usize;
        let mut inl = runs_of(p, src, default_size);
        if inl.is_empty() {
            out.push(Block::Para(Para::default()));
            continue;
        }
        let align = match ppr.and_then(|pp| pp.attr(src, "algn")).as_deref() {
            Some("ctr") => Align::Center,
            Some("r") => Align::Right,
            Some("just") | Some("dist") => Align::Justify,
            _ => Align::Left,
        };
        let no_bullet = ppr.is_some_and(|pp| pp.child("buNone").is_some());
        let bu_char = ppr
            .and_then(|pp| pp.child("buChar"))
            .and_then(|b| b.attr(src, "char"));
        let auto = ppr.and_then(|pp| pp.child("buAutoNum")).is_some();
        let marker = if no_bullet {
            None
        } else if auto {
            counters[lvl] += 1;
            for c in counters.iter_mut().skip(lvl + 1) {
                *c = 0;
            }
            Some(format!("{}.", counters[lvl]))
        } else if let Some(c) = bu_char {
            Some(c)
        } else if bulleted {
            Some(["•", "–", "▪"][lvl % 3].to_string())
        } else {
            None
        };
        if let Some(m) = marker {
            let st = match inl.first() {
                Some(Inline::Text(_, s)) => Style {
                    size: s.size,
                    color: s.color,
                    ..Default::default()
                },
                _ => Style::default(),
            };
            inl.insert(0, Inline::Text(format!("{m} "), st));
        }
        out.push(Block::Para(Para {
            inl,
            align,
            indent: lvl as f32 * 20.0,
            above: None,
            below: None,
        }));
    }
    out
}

impl Deck<'_, '_> {
    fn image(&mut self, sl: &Slide<'_>, rid: &str) -> Option<super::ir::Img> {
        let path = sl.rels.get(rid)?.clone();
        let b = part_bytes(self.z, &path)?;
        self.files.add(b)
    }

    fn shapes(&mut self, sl: &Slide<'_>, tree: &Node, xf: Xf, out: &mut Vec<Block>, depth: usize) {
        if depth > 16 {
            return;
        }
        let src = sl.src;
        for n in &tree.kids {
            match &*n.name {
                "sp" => {
                    let ph = ph_of(n, src);
                    let ty = ph.as_ref().map(|p| p.0.clone()).unwrap_or_default();
                    let sppr = n.child("spPr");
                    let rect = sppr
                        .and_then(|s| s.child("xfrm"))
                        .and_then(|x| xfrm_rect(x, src))
                        .map(|r| xf.apply(r))
                        .or_else(|| {
                            let (ty, idx) = ph.as_ref()?;
                            sl.layouts.iter().find_map(|l| {
                                idx.as_ref()
                                    .and_then(|i| l.by_idx.get(i))
                                    .or_else(|| l.by_type.get(ty))
                                    .copied()
                            })
                        });
                    let Some(r) = rect else { continue };
                    let fill = sppr.and_then(|s| solid(s, src));
                    let blocks = n
                        .child("txBody")
                        .map(|tx| text_body(tx, src, &ty))
                        .unwrap_or_default();
                    let has_text = blocks
                        .iter()
                        .any(|b| matches!(b, Block::Para(p) if !p.inl.is_empty()));
                    if !has_text && fill.is_none() {
                        continue;
                    }
                    let anchor = n
                        .child("txBody")
                        .and_then(|t| t.child("bodyPr"))
                        .and_then(|b| b.attr(src, "anchor"));
                    let middle =
                        anchor.as_deref() == Some("ctr") || (anchor.is_none() && is_title(&ty));
                    out.push(Block::Place {
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                        fill,
                        middle,
                        blocks: if has_text { blocks } else { Vec::new() },
                    });
                }
                "pic" => {
                    let rect = n
                        .child("spPr")
                        .and_then(|s| s.child("xfrm"))
                        .and_then(|x| xfrm_rect(x, src))
                        .map(|r| xf.apply(r));
                    let rid = n.find("blip").and_then(|b| b.attr(src, "embed"));
                    if let (Some(r), Some(rid)) = (rect, rid) {
                        if let Some(mut img) = self.image(sl, &rid) {
                            img.w = r.w.max(1.0);
                            img.h = r.h.max(1.0);
                            out.push(Block::Place {
                                x: r.x - 3.0,
                                y: r.y - 3.0,
                                w: r.w + 6.0,
                                h: r.h + 6.0,
                                fill: None,
                                middle: false,
                                blocks: vec![Block::Image(img, Align::Left)],
                            });
                        }
                    }
                }
                "grpSp" => {
                    let inner = n
                        .child("grpSpPr")
                        .and_then(|g| g.child("xfrm"))
                        .and_then(|x| {
                            let r = xfrm_rect(x, src)?;
                            let co = x.child("chOff")?;
                            let ce = x.child("chExt")?;
                            let (cx, cy) = (
                                num(co, src, "x")? / EMU_PER_PT,
                                num(co, src, "y")? / EMU_PER_PT,
                            );
                            let (cw, ch) = (
                                num(ce, src, "cx")? / EMU_PER_PT,
                                num(ce, src, "cy")? / EMU_PER_PT,
                            );
                            let sx = if cw > 0.0 { r.w / cw } else { 1.0 };
                            let sy = if ch > 0.0 { r.h / ch } else { 1.0 };
                            let g = Xf {
                                dx: r.x - cx * sx,
                                dy: r.y - cy * sy,
                                sx,
                                sy,
                            };
                            // compose with the outer transform
                            Some(Xf {
                                dx: xf.dx + g.dx * xf.sx,
                                dy: xf.dy + g.dy * xf.sy,
                                sx: g.sx * xf.sx,
                                sy: g.sy * xf.sy,
                            })
                        })
                        .unwrap_or(xf);
                    self.shapes(sl, n, inner, out, depth + 1);
                }
                "graphicFrame" => {
                    let rect = n
                        .child("xfrm")
                        .and_then(|x| xfrm_rect(x, src))
                        .map(|r| xf.apply(r));
                    let tbl = n.find("tbl");
                    if let (Some(r), Some(tbl)) = (rect, tbl) {
                        let cols: Vec<Col> = tbl
                            .child("tblGrid")
                            .map(|g| {
                                g.kids
                                    .iter()
                                    .filter_map(|c| num(c, src, "w"))
                                    .map(|w| Col::Fr((w / EMU_PER_PT).max(1.0)))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let rows = tbl
                            .kids
                            .iter()
                            .filter(|k| &*k.name == "tr")
                            .map(|tr| {
                                tr.kids
                                    .iter()
                                    .filter(|k| &*k.name == "tc")
                                    .filter(|tc| {
                                        tc.attr(src, "hMerge").is_none()
                                            && tc.attr(src, "vMerge").is_none()
                                    })
                                    .map(|tc| Cell {
                                        blocks: tc
                                            .child("txBody")
                                            .map(|t| text_body(t, src, "tbl"))
                                            .unwrap_or_default(),
                                        colspan: num(tc, src, "gridSpan")
                                            .unwrap_or(1.0)
                                            .clamp(1.0, 64.0)
                                            as u16,
                                        rowspan: num(tc, src, "rowSpan")
                                            .unwrap_or(1.0)
                                            .clamp(1.0, 64.0)
                                            as u16,
                                        fill: tc.child("tcPr").and_then(|p| solid(p, src)),
                                    })
                                    .collect()
                            })
                            .collect();
                        out.push(Block::Place {
                            x: r.x,
                            y: r.y,
                            w: r.w,
                            h: r.h.max(20.0) * 4.0,
                            fill: None,
                            middle: false,
                            blocks: vec![Block::Table(Table {
                                cols,
                                header: 0,
                                rows,
                                size: None,
                                borders: true,
                            })],
                        });
                    }
                }
                "AlternateContent" => {
                    // take the fallback rendition
                    if let Some(fb) = n.child("Fallback") {
                        self.shapes(sl, fb, xf, out, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }
}

fn rels_map(z: &mut Zip<'_>, part: &str) -> HashMap<String, (String, String)> {
    let dir = parse::dir_of(part).to_string();
    part_text(z, &parse::rels_path(part))
        .map(|r| {
            parse::rels(&r)
                .into_iter()
                .map(|(id, ty, target)| (id, (ty, parse::resolve(&dir, &target))))
                .collect()
        })
        .unwrap_or_default()
}

fn bg_of(src: &str) -> Option<Rgb> {
    let nodes = parse::tree(src).ok()?;
    let root = nodes.first()?;
    let bgpr = root.find("bg")?.find("bgPr")?;
    solid(bgpr, src)
}

pub fn pptx(bytes: &[u8]) -> Result<Doc, String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("not a valid .pptx (zip): {e}"))?;
    let pres_path = "ppt/presentation.xml";
    let pres = part_text(&mut z, pres_path)
        .ok_or("not a PowerPoint file: ppt/presentation.xml missing")?;
    let nodes = parse::tree(&pres).map_err(|e| format!("presentation.xml: {e}"))?;
    let root = nodes.first().ok_or("empty presentation.xml")?;
    let (w, h) = root
        .find("sldSz")
        .and_then(|s| {
            Some((
                num(s, &pres, "cx")? / EMU_PER_PT,
                num(s, &pres, "cy")? / EMU_PER_PT,
            ))
        })
        .filter(|(w, h)| *w >= 36.0 && *h >= 36.0 && *w <= 14_400.0 && *h <= 14_400.0)
        .unwrap_or((720.0, 405.0));
    let rels = rels_map(&mut z, pres_path);
    let slide_paths: Vec<String> = root
        .find("sldIdLst")
        .map(|l| {
            l.kids
                .iter()
                .filter_map(|s| {
                    // r:id (the bare local name "id" is the numeric slide id)
                    let tag = &pres[s.start..s.tag_end];
                    let i = tag.find(":id=\"")? + 5;
                    let j = tag[i..].find('"')?;
                    Some(tag[i..i + j].to_string())
                })
                .filter_map(|rid| rels.get(&rid).map(|r| r.1.clone()))
                .collect()
        })
        .unwrap_or_default();
    if slide_paths.is_empty() {
        return Err("this presentation has no slides".into());
    }

    let mut d = Doc::new(Page {
        w,
        h,
        margin: [0.0; 4],
    });
    let mut layout_cache: HashMap<String, (Phs, Option<Rgb>)> = HashMap::new();
    let empty = Phs {
        by_idx: HashMap::new(),
        by_type: HashMap::new(),
    };
    let mut deck = Deck {
        z: &mut z,
        files: Files::default(),
    };
    let mut title = None;
    for path in slide_paths.iter().take(MAX_SLIDES) {
        let Some(src) = part_text(deck.z, path) else {
            continue;
        };
        let srels = rels_map(deck.z, path);
        let layout_path = srels
            .values()
            .find(|(t, _)| t.ends_with("/slideLayout"))
            .map(|(_, p)| p.clone());
        let mut chain = Vec::new();
        if let Some(lp) = layout_path {
            if !layout_cache.contains_key(&lp) {
                let lsrc = part_text(deck.z, &lp).unwrap_or_default();
                let lbg = bg_of(&lsrc);
                layout_cache.insert(lp.clone(), (placeholders(&lsrc), lbg));
                let master = rels_map(deck.z, &lp)
                    .values()
                    .find(|(t, _)| t.ends_with("/slideMaster"))
                    .map(|(_, p)| p.clone());
                if let Some(mp) = master {
                    let key = format!("master:{lp}");
                    let msrc = part_text(deck.z, &mp).unwrap_or_default();
                    let mbg = bg_of(&msrc);
                    layout_cache.insert(key, (placeholders(&msrc), mbg));
                }
            }
            chain.push(lp.clone());
            chain.push(format!("master:{lp}"));
        }
        let get = |k: Option<&String>| k.and_then(|k| layout_cache.get(k));
        let l0 = get(chain.first());
        let l1 = get(chain.get(1));
        let sl = Slide {
            src: &src,
            rels: srels.into_iter().map(|(id, (_, p))| (id, p)).collect(),
            layouts: [
                l0.map(|l| &l.0).unwrap_or(&empty),
                l1.map(|l| &l.0).unwrap_or(&empty),
            ],
        };
        let fill = bg_of(&src)
            .or(l0.and_then(|l| l.1))
            .or(l1.and_then(|l| l.1));
        let mut blocks = Vec::new();
        if let Ok(nodes) = parse::tree(&src) {
            if let Some(tree) = nodes.first().and_then(|r| r.find("spTree")) {
                deck.shapes(&sl, tree, Xf::ID, &mut blocks, 0);
            }
            if title.is_none() {
                title = first_title(&nodes, &src);
            }
        }
        d.blocks.push(Block::Page { fill, blocks });
    }
    d.files = deck.files;
    d.title = title;
    Ok(d)
}

fn first_title(nodes: &[Node], src: &str) -> Option<String> {
    let mut stack: Vec<&Node> = nodes.iter().collect();
    while let Some(n) = stack.pop() {
        if &*n.name == "sp" && ph_of(n, src).is_some_and(|(t, _)| is_title(&t)) {
            let t = n.text(src).split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                return Some(t);
            }
        }
        stack.extend(n.kids.iter().rev());
    }
    None
}

// ------------------------------------------------------------------- ODP

/// ODF length ("2.5cm", "12pt", "1in") → points.
fn odf_len(v: &str) -> Option<f32> {
    let v = v.trim();
    let split = v.find(|c: char| c.is_ascii_alphabetic() || c == '%')?;
    let (n, unit) = v.split_at(split);
    let n: f32 = n.trim().parse().ok().filter(|x: &f32| x.is_finite())?;
    Some(match unit {
        "cm" => n * 72.0 / 2.54,
        "mm" => n * 72.0 / 25.4,
        "in" | "inch" => n * 72.0,
        "pt" => n,
        "pc" => n * 12.0,
        "px" => n * 0.75,
        _ => return None,
    })
}

#[derive(Default, Clone)]
struct OdfStyle {
    st: Style,
    align: Option<Align>,
    fill: Option<Rgb>,
}

fn odf_styles(src: &str, roots: &[Node], out: &mut HashMap<String, OdfStyle>) {
    let mut stack: Vec<&Node> = roots.iter().collect();
    while let Some(n) = stack.pop() {
        if &*n.name == "style" {
            if let Some(name) = n.attr(src, "name") {
                let mut s = OdfStyle::default();
                for k in &n.kids {
                    match &*k.name {
                        "text-properties" => {
                            s.st.bold = k.attr(src, "font-weight").is_some_and(|w| {
                                w == "bold" || w.parse::<u32>().is_ok_and(|v| v >= 600)
                            });
                            s.st.italic = k
                                .attr(src, "font-style")
                                .is_some_and(|v| v == "italic" || v == "oblique");
                            s.st.underline = k
                                .attr(src, "text-underline-style")
                                .is_some_and(|v| v != "none");
                            s.st.strike = k
                                .attr(src, "text-line-through-style")
                                .is_some_and(|v| v != "none");
                            s.st.size = k.attr(src, "font-size").and_then(|v| odf_len(&v));
                            s.st.color = k
                                .attr(src, "color")
                                .and_then(|c| hex(c.trim_start_matches('#')));
                        }
                        "paragraph-properties" => {
                            s.align = match k.attr(src, "text-align").as_deref() {
                                Some("center") => Some(Align::Center),
                                Some("end") | Some("right") => Some(Align::Right),
                                Some("justify") => Some(Align::Justify),
                                Some(_) => Some(Align::Left),
                                None => None,
                            };
                        }
                        "graphic-properties" | "drawing-page-properties" => {
                            if k.attr(src, "fill").as_deref() == Some("solid") {
                                s.fill = k
                                    .attr(src, "fill-color")
                                    .and_then(|c| hex(c.trim_start_matches('#')));
                            }
                        }
                        _ => {}
                    }
                }
                out.insert(name, s);
            }
        }
        stack.extend(n.kids.iter());
    }
}

struct Odp<'a> {
    src: &'a str,
    styles: &'a HashMap<String, OdfStyle>,
}

impl Odp<'_> {
    fn style(&self, n: &Node) -> OdfStyle {
        n.attr(self.src, "style-name")
            .and_then(|s| self.styles.get(&s).cloned())
            .unwrap_or_default()
    }

    fn spans(&self, n: &Node, st: &Style, out: &mut Vec<Inline>) {
        // walk mixed content in source order: text between child elements
        let src = self.src;
        let mut pos = n.inner.start;
        for k in &n.kids {
            if k.start > pos {
                let t = parse::unescape(&src[pos..k.start]);
                if !t.is_empty() {
                    out.push(Inline::Text(t, st.clone()));
                }
            }
            match &*k.name {
                "span" | "a" => {
                    let mut s2 = st.clone();
                    let ks = self.style(k).st;
                    s2.bold |= ks.bold;
                    s2.italic |= ks.italic;
                    s2.underline |= ks.underline;
                    s2.strike |= ks.strike;
                    s2.size = ks.size.or(s2.size);
                    s2.color = ks.color.or(s2.color);
                    self.spans(k, &s2, out);
                }
                "s" => out.push(Inline::Text(
                    " ".repeat(num(k, src, "c").unwrap_or(1.0).clamp(1.0, 50.0) as usize),
                    st.clone(),
                )),
                "tab" => out.push(Inline::Text("    ".into(), st.clone())),
                "line-break" => out.push(Inline::Break),
                _ => {}
            }
            pos = k.end;
        }
        if n.inner.end > pos {
            let t = parse::unescape(&src[pos..n.inner.end]);
            if !t.is_empty() {
                out.push(Inline::Text(t, st.clone()));
            }
        }
    }

    fn paras(&self, n: &Node, size: f32, depth: usize, bullet: bool, out: &mut Vec<Block>) {
        for k in &n.kids {
            match &*k.name {
                "p" | "h" => {
                    let ps = self.style(k);
                    let mut st = ps.st.clone();
                    st.size = st.size.or(Some(size));
                    let mut inl = Vec::new();
                    self.spans(k, &st, &mut inl);
                    if bullet && !inl.is_empty() {
                        inl.insert(
                            0,
                            Inline::Text(
                                format!("{} ", ["•", "–", "▪"][depth % 3]),
                                Style {
                                    size: st.size,
                                    ..Default::default()
                                },
                            ),
                        );
                    }
                    out.push(Block::Para(Para {
                        inl,
                        align: ps.align.unwrap_or_default(),
                        indent: depth.saturating_sub(1) as f32 * 20.0,
                        above: None,
                        below: None,
                    }));
                }
                "list" if depth < 9 => self.paras(k, size, depth + 1, true, out),
                "list-item" | "list-header" => self.paras(k, size, depth, bullet, out),
                _ => {}
            }
        }
    }

    fn frame(&self, n: &Node, z: &mut Zip<'_>, files: &mut Files, out: &mut Vec<Block>) {
        let src = self.src;
        let len = |a: &str| n.attr(src, a).and_then(|v| odf_len(&v));
        let (Some(x), Some(y), Some(w), Some(h)) =
            (len("x"), len("y"), len("width"), len("height"))
        else {
            return;
        };
        let class = n.attr(src, "class").unwrap_or_default();
        let size = match class.as_str() {
            "title" => 36.0,
            "subtitle" => 22.0,
            _ => 18.0,
        };
        let fill = self.style(n).fill;
        let mut blocks = Vec::new();
        for k in &n.kids {
            match &*k.name {
                "text-box" => self.paras(k, size, 0, class == "outline", &mut blocks),
                "image" => {
                    if let Some(href) = k.attr(src, "href") {
                        if let Some(mut img) =
                            part_bytes(z, href.trim_start_matches("./")).and_then(|b| files.add(b))
                        {
                            img.w = w;
                            img.h = h;
                            blocks.push(Block::Image(img, Align::Left));
                        }
                    }
                }
                _ => {}
            }
        }
        // shapes carry their text directly
        if matches!(&*n.name, "custom-shape" | "rect" | "ellipse") {
            self.paras(n, size, 0, false, &mut blocks);
        }
        if blocks.is_empty() && fill.is_none() {
            return;
        }
        let middle = class == "title" || matches!(&*n.name, "custom-shape" | "rect" | "ellipse");
        let pad = if blocks.iter().any(|b| matches!(b, Block::Image(..))) {
            3.0
        } else {
            0.0
        };
        out.push(Block::Place {
            x: x - pad,
            y: y - pad,
            w: w + 2.0 * pad,
            h: h + 2.0 * pad,
            fill,
            middle,
            blocks,
        });
    }
}

pub fn odp(bytes: &[u8]) -> Result<Doc, String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("not a valid .odp (zip): {e}"))?;
    let content = part_text(&mut z, "content.xml")
        .ok_or("not an OpenDocument presentation: content.xml missing")?;
    let styles_xml = part_text(&mut z, "styles.xml").unwrap_or_default();
    let cnodes = parse::tree(&content).map_err(|e| format!("content.xml: {e}"))?;
    let snodes = parse::tree(&styles_xml).unwrap_or_default();
    let mut styles = HashMap::new();
    odf_styles(&styles_xml, &snodes, &mut styles);
    // page size: first page layout of the styles part
    let (mut w, mut h) = (792.0, 612.0);
    let mut stack: Vec<&Node> = snodes.iter().collect();
    while let Some(n) = stack.pop() {
        if &*n.name == "page-layout-properties" {
            if let (Some(pw), Some(ph)) = (
                n.attr(&styles_xml, "page-width").and_then(|v| odf_len(&v)),
                n.attr(&styles_xml, "page-height").and_then(|v| odf_len(&v)),
            ) {
                if (36.0..=14_400.0).contains(&pw) && (36.0..=14_400.0).contains(&ph) {
                    (w, h) = (pw, ph);
                }
            }
            break;
        }
        stack.extend(n.kids.iter().rev());
    }
    let mut content_styles = HashMap::new();
    odf_styles(&content, &cnodes, &mut content_styles);
    let odp_styles: HashMap<String, OdfStyle> = styles.into_iter().chain(content_styles).collect();
    let odp = Odp {
        src: &content,
        styles: &odp_styles,
    };
    let root = cnodes.first().ok_or("empty content.xml")?;
    let pres = root
        .find("presentation")
        .ok_or("not a presentation document")?;
    let mut d = Doc::new(Page {
        w,
        h,
        margin: [0.0; 4],
    });
    let mut files = Files::default();
    let mut title = None;
    for page in pres
        .kids
        .iter()
        .filter(|k| &*k.name == "page")
        .take(MAX_SLIDES)
    {
        let fill = odp.style(page).fill;
        let mut blocks = Vec::new();
        for k in &page.kids {
            match &*k.name {
                "frame" | "custom-shape" | "rect" | "ellipse" => {
                    if title.is_none() && k.attr(&content, "class").as_deref() == Some("title") {
                        let mut inl = Vec::new();
                        if let Some(tb) = k.child("text-box") {
                            for p in &tb.kids {
                                odp.spans(p, &Style::default(), &mut inl);
                            }
                        }
                        let t = super::plain_text(&inl);
                        if !t.is_empty() {
                            title = Some(t);
                        }
                    }
                    odp.frame(k, &mut z, &mut files, &mut blocks);
                }
                "g" => {
                    for g in &k.kids {
                        odp.frame(g, &mut z, &mut files, &mut blocks);
                    }
                }
                _ => {}
            }
        }
        d.blocks.push(Block::Page { fill, blocks });
    }
    if d.blocks.is_empty() {
        return Err("this presentation has no slides".into());
    }
    d.files = files;
    d.title = title;
    Ok(d)
}
