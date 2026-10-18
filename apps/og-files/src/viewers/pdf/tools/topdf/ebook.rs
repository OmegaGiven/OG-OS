//! E-books and comics.
//!
//! * EPUB: spine order, each XHTML chapter through [`html`] (images from the
//!   package), a page break between chapters, table-of-contents labels as
//!   chapter headings where a chapter has none (so the PDF outline follows
//!   the book's TOC).
//! * MOBI / AZW3: the `mobi` crate parses the headers; text records are
//!   decoded here (PalmDOC, trailing entries; HUFF/CDIC via the crate);
//!   the HTML-ish markup goes through [`html`], `recindex` / `kindle:embed`
//!   images are taken from the image records. DRM-protected books fail.
//! * FB2: the XML is mapped to HTML (sections, titles, emphasis, poems,
//!   epigraphs, embedded base64 images).
//! * CBZ / CBR / CB7 / CBT: every image in the archive, natural-sorted by
//!   path, one page each (existing images → PDF tool). RAR is read by the
//!   pure-Rust `rars` crate.

use super::html;
use super::ir::{Block, Doc, Face, Inline, Page, Style};
use crate::viewers::docx::parse::{self, Node};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::rc::Rc;

const ENTRY_LIMIT: u64 = 64 * 1024 * 1024;

type Zip<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

fn zip_bytes(z: &mut Zip<'_>, name: &str) -> Option<Vec<u8>> {
    let f = z.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.take(ENTRY_LIMIT).read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn zip_text(z: &mut Zip<'_>, name: &str) -> Option<String> {
    zip_bytes(z, name).map(|b| super::office::decode_text(&b))
}

fn inner_text(src: &str, n: &Node) -> String {
    let raw = &src[n.inner.clone()];
    // strip nested tags (rare in titles/labels)
    let mut s = String::new();
    let mut tag = false;
    for c in raw.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => s.push(c),
            _ => {}
        }
    }
    parse::unescape(&s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn walk_all<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for n in nodes {
        out.push(n);
        walk_all(&n.kids, out);
    }
}

/// href with fragment and percent-encoding removed, resolved against a dir.
fn resolve(dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    parse::resolve(dir, &html::percent_decode(href))
}

pub fn epub(bytes: &[u8]) -> Result<Doc, String> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("not a valid EPUB (zip): {e}"))?;
    let container = zip_text(&mut z, "META-INF/container.xml")
        .ok_or("not an EPUB: no META-INF/container.xml")?;
    let opf_path = parse::tree(&container)
        .ok()
        .and_then(|nodes| {
            let mut all = Vec::new();
            walk_all(&nodes, &mut all);
            all.iter()
                .find(|n| &*n.name == "rootfile")
                .and_then(|n| n.attr(&container, "full-path"))
        })
        .ok_or("EPUB container names no package file")?;
    let opf = zip_text(&mut z, &opf_path).ok_or("EPUB package file missing")?;
    let opf_dir = parse::dir_of(&opf_path).to_string();
    let nodes = parse::tree(&opf).map_err(|e| format!("EPUB package file: {e}"))?;
    let mut all = Vec::new();
    walk_all(&nodes, &mut all);

    let title = all
        .iter()
        .find(|n| &*n.name == "title")
        .map(|n| inner_text(&opf, n))
        .filter(|t| !t.is_empty());
    // manifest id → (path, media type, properties)
    let mut manifest: HashMap<String, (String, String, String)> = HashMap::new();
    for n in all.iter().filter(|n| &*n.name == "item") {
        if let (Some(id), Some(href)) = (n.attr(&opf, "id"), n.attr(&opf, "href")) {
            manifest.insert(
                id,
                (
                    resolve(&opf_dir, &href),
                    n.attr(&opf, "media-type").unwrap_or_default(),
                    n.attr(&opf, "properties").unwrap_or_default(),
                ),
            );
        }
    }
    let spine_node = all.iter().find(|n| &*n.name == "spine");
    let spine: Vec<String> = spine_node
        .map(|s| {
            s.kids
                .iter()
                .filter(|k| &*k.name == "itemref")
                .filter(|k| k.attr(&opf, "linear").as_deref() != Some("no"))
                .filter_map(|k| k.attr(&opf, "idref"))
                .filter_map(|id| manifest.get(&id).map(|m| m.0.clone()))
                .collect()
        })
        .unwrap_or_default();
    if spine.is_empty() {
        return Err("EPUB has no chapters".into());
    }

    // TOC labels by chapter path (EPUB 3 nav, else EPUB 2 NCX)
    let mut toc: HashMap<String, String> = HashMap::new();
    let nav = manifest
        .values()
        .find(|m| m.2.split_whitespace().any(|p| p == "nav"))
        .map(|m| m.0.clone());
    let ncx = spine_node
        .and_then(|s| s.attr(&opf, "toc"))
        .and_then(|id| manifest.get(&id).map(|m| m.0.clone()))
        .or_else(|| {
            manifest
                .values()
                .find(|m| m.1 == "application/x-dtbncx+xml")
                .map(|m| m.0.clone())
        });
    if let Some(nav) = nav
        .as_ref()
        .and_then(|p| zip_text(&mut z, p).map(|t| (p.clone(), t)))
    {
        let dir = parse::dir_of(&nav.0).to_string();
        let doc = scraper::Html::parse_document(&nav.1);
        for a in doc.root_element().descendent_elements() {
            if a.value().name() == "a" {
                if let Some(h) = a.attr("href") {
                    let label = a
                        .text()
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    toc.entry(resolve(&dir, h)).or_insert(label);
                }
            }
        }
    } else if let Some((path, src)) = ncx
        .as_ref()
        .and_then(|p| zip_text(&mut z, p).map(|t| (p.clone(), t)))
    {
        let dir = parse::dir_of(&path).to_string();
        if let Ok(nodes) = parse::tree(&src) {
            let mut all = Vec::new();
            walk_all(&nodes, &mut all);
            for np in all.iter().filter(|n| &*n.name == "navPoint") {
                let label = np
                    .find("text")
                    .map(|t| inner_text(&src, t))
                    .unwrap_or_default();
                if let Some(h) = np.child("content").and_then(|c| c.attr(&src, "src")) {
                    toc.entry(resolve(&dir, &h)).or_insert(label);
                }
            }
        }
    }

    let mut d = Doc::new(Page::A4);
    d.face = Face::Serif;
    d.justify = true;
    d.title = title;
    for (k, path) in spine.iter().enumerate() {
        let Some(src) = zip_text(&mut z, path) else {
            d.blocks
                .push(Block::Note(format!("(missing chapter {path})")));
            continue;
        };
        let dir = parse::dir_of(path).to_string();
        let mut fetch = |s: &str| -> Option<Vec<u8>> { zip_bytes(&mut z, &resolve(&dir, s)) };
        let out = html::convert(&src, &mut d.files, &mut fetch, None);
        if k > 0 && !d.blocks.is_empty() {
            d.blocks.push(Block::PageBreak);
        }
        let label = toc.get(path).filter(|l| !l.is_empty());
        if let Some(l) = label {
            if !out
                .blocks
                .iter()
                .take(3)
                .any(|b| matches!(b, Block::Heading(..)))
            {
                d.blocks.push(Block::Heading(
                    1,
                    vec![Inline::Text(l.clone(), Style::default())],
                ));
            }
        }
        d.blocks.extend(out.blocks);
    }
    Ok(d)
}

// ------------------------------------------------------------------ MOBI

/// base-32 (0-9, A-V) record number of a `kindle:embed:XXXX` reference.
fn base32(s: &str) -> Option<usize> {
    let mut v = 0usize;
    for c in s.chars().take(8) {
        v = v * 32 + c.to_digit(32)? as usize;
    }
    Some(v)
}

/// PalmDOC (LZ77-style) decompression.
fn palmdoc(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        i += 1;
        match c {
            1..=8 => {
                let end = (i + c as usize).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            0x80..=0xbf => {
                let Some(&next) = input.get(i) else { break };
                i += 1;
                let pair = (u16::from(c) << 8) | u16::from(next);
                let dist = usize::from((pair >> 3) & 0x7ff);
                let len = usize::from(pair & 7) + 3;
                if dist == 0 || dist > out.len() {
                    continue;
                }
                let start = out.len() - dist;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
            0xc0..=0xff => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Size of the trailing entries the MOBI header's extra-data flags say
/// follow each text record.
fn trailing(data: &[u8], flags: u32) -> usize {
    let mut num = 0usize;
    let mut f = flags >> 1;
    while f != 0 {
        if f & 1 != 0 {
            // backward-encoded variable-width integer (its own size included)
            let end = data.len().saturating_sub(num);
            let mut bytes = Vec::new();
            for &b in data[..end].iter().rev().take(4) {
                bytes.push(b);
                if b & 0x80 != 0 {
                    break;
                }
            }
            bytes.reverse();
            let v = bytes
                .iter()
                .fold(0usize, |v, b| (v << 7) | usize::from(b & 0x7f));
            num += v;
        }
        f >>= 1;
    }
    if flags & 1 != 0 {
        if let Some(&b) = data.len().checked_sub(num + 1).and_then(|i| data.get(i)) {
            num += usize::from(b & 3) + 1;
        }
    }
    num.min(data.len())
}

fn cp1252(b: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    b.iter()
        .map(|&c| match c {
            0x80..=0x9f => HIGH[usize::from(c - 0x80)],
            c => c as char,
        })
        .collect()
}

pub fn mobi(bytes: Vec<u8>) -> Result<Doc, String> {
    let m = mobi::Mobi::new(&bytes).map_err(|e| format!("not a readable MOBI/AZW3 book: {e}"))?;
    // records sliced from the file itself (the crate's copy of the content
    // zero-fills the header area, record 0 included)
    let offs: Vec<usize> = m
        .metadata
        .records
        .records
        .iter()
        .map(|r| (r.offset as usize).min(bytes.len()))
        .collect();
    let recs: Vec<&[u8]> = offs
        .iter()
        .enumerate()
        .map(|(i, &o)| {
            let end = offs.get(i + 1).copied().unwrap_or(bytes.len()).max(o);
            &bytes[o..end]
        })
        .collect();
    let rec0 = *recs.first().ok_or("no records")?;
    let be16 = |o: usize| rec0.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let be32 = |o: usize| {
        rec0.get(o..o + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    if be16(12).unwrap_or(0) != 0 {
        return Err("this book is DRM-protected".into());
    }
    let compression = be16(0).unwrap_or(1);
    let count = usize::from(be16(8).unwrap_or(0));
    let header_len = be32(20).unwrap_or(0);
    let utf8 = be32(28) == Some(65001);
    let flags = if header_len >= 228 {
        be32(16 + 224).unwrap_or(0)
    } else {
        0
    };
    let first = usize::from(m.metadata.mobi.first_content_record.max(1));
    // the mobi crate's own text accessors drop the last record (off-by-one
    // in its range helper), so the records are decoded here
    let text = if compression == 17480 {
        m.content_as_string_lossy()
    } else {
        let mut buf = Vec::new();
        for r in recs.iter().skip(first).take(count) {
            let data = &r[..r.len() - trailing(r, flags)];
            if compression == 2 {
                buf.extend(palmdoc(data));
            } else {
                buf.extend_from_slice(data);
            }
        }
        if utf8 {
            String::from_utf8_lossy(&buf).into_owned()
        } else {
            cp1252(&buf)
        }
    };
    if text.trim().is_empty() {
        return Err("no readable text in this book".into());
    }
    let first_image = m.metadata.mobi.first_image_index as usize;
    let mut fetch = |src: &str| -> Option<Vec<u8>> {
        let n = if let Some(rest) = src.strip_prefix("kindle:embed:") {
            base32(rest.split(['?', '#']).next()?)?
        } else {
            src.trim().parse::<usize>().ok()?
        };
        let idx = first_image.checked_add(n.checked_sub(1)?)?;
        recs.get(idx).map(|r| r.to_vec())
    };
    // MOBI 6 markup: <img recindex="00001">; make it a src for the walker
    let text = text
        .replace("recindex=\"", "src=\"")
        .replace("recindex='", "src='");
    let mut d = Doc::new(Page::A4);
    d.face = Face::Serif;
    d.justify = true;
    let out = html::convert(&text, &mut d.files, &mut fetch, None);
    d.blocks = out.blocks;
    let t = m.title();
    d.title = Some(t).filter(|t| !t.trim().is_empty()).or(out.title);
    Ok(d)
}

// ------------------------------------------------------------------ FB2

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// FictionBook 2 → HTML.
pub fn fb2_to_html(src: &str) -> (Option<String>, String) {
    use quick_xml::events::Event as X;
    let mut r = quick_xml::Reader::from_str(src);
    let mut out = String::from("<html><body>");
    let mut binaries: HashMap<String, (String, String)> = HashMap::new();
    let mut title = None;
    let mut stack: Vec<String> = Vec::new();
    let mut section_depth = 0usize;
    let mut in_body = false;
    let mut cur_binary: Option<(String, String, String)> = None;
    // images become fb2bin:<id> placeholders, inlined at the end
    loop {
        let ev = match r.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            X::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let attr = |k: &str| {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == k.as_bytes())
                        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                };
                match name.as_str() {
                    "body" => in_body = true,
                    "binary" => {
                        cur_binary = Some((
                            attr("id").unwrap_or_default(),
                            attr("content-type").unwrap_or_else(|| "image/jpeg".into()),
                            String::new(),
                        ));
                    }
                    _ if !in_body => {}
                    "section" => section_depth += 1,
                    "title" => {
                        let h = (section_depth + 1).clamp(1, 6);
                        out.push_str(&format!("<h{h}>"));
                    }
                    "subtitle" => out.push_str("<h6>"),
                    "p" | "v" | "text-author" => out.push_str("<p>"),
                    "emphasis" => out.push_str("<em>"),
                    "strong" => out.push_str("<strong>"),
                    "strikethrough" => out.push_str("<s>"),
                    "sub" | "sup" | "code" => out.push_str(&format!("<{name}>")),
                    "epigraph" | "cite" | "annotation" => out.push_str("<blockquote>"),
                    "poem" | "stanza" => out.push_str("<div>"),
                    "table" | "tr" | "td" | "th" => out.push_str(&format!("<{name}>")),
                    "a" => out.push_str(&format!(
                        "<a href=\"{}\">",
                        esc(&attr("href").unwrap_or_default())
                    )),
                    _ => {}
                }
                stack.push(name);
            }
            X::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if in_body {
                    match name.as_str() {
                        "image" => {
                            let href = e
                                .attributes()
                                .flatten()
                                .find(|a| a.key.local_name().as_ref() == b"href")
                                .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                                .unwrap_or_default();
                            out.push_str(&format!(
                                "<img src=\"fb2bin:{}\">",
                                esc(href.trim_start_matches('#'))
                            ));
                        }
                        "empty-line" => out.push_str("<p></p>"),
                        _ => {}
                    }
                }
            }
            X::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                stack.pop();
                match name.as_str() {
                    "body" => in_body = false,
                    "binary" => {
                        if let Some((id, ct, data)) = cur_binary.take() {
                            binaries.insert(id, (ct, data));
                        }
                    }
                    _ if !in_body => {}
                    "section" => section_depth = section_depth.saturating_sub(1),
                    "title" => {
                        let h = (section_depth + 1).clamp(1, 6);
                        out.push_str(&format!("</h{h}>"));
                    }
                    "subtitle" => out.push_str("</h6>"),
                    "p" | "v" | "text-author" => out.push_str("</p>"),
                    "emphasis" => out.push_str("</em>"),
                    "strong" => out.push_str("</strong>"),
                    "strikethrough" => out.push_str("</s>"),
                    "sub" | "sup" | "code" => out.push_str(&format!("</{name}>")),
                    "epigraph" | "cite" | "annotation" => out.push_str("</blockquote>"),
                    "poem" | "stanza" => out.push_str("</div>"),
                    "table" | "tr" | "td" | "th" => out.push_str(&format!("</{name}>")),
                    "a" => out.push_str("</a>"),
                    _ => {}
                }
            }
            X::Text(t) => {
                let s = t.decode().map(|c| c.into_owned()).unwrap_or_default();
                let s = parse::unescape(&s);
                if let Some(b) = cur_binary.as_mut() {
                    b.2.push_str(&s);
                } else if in_body {
                    out.push_str(&esc(&s));
                } else if stack.last().is_some_and(|n| n == "book-title") && title.is_none() {
                    title = Some(s.trim().to_string());
                }
            }
            X::GeneralRef(g) => {
                if in_body {
                    let name = String::from_utf8_lossy(&g).into_owned();
                    out.push_str(&format!("&{name};"));
                }
            }
            X::CData(t) => {
                if in_body {
                    out.push_str(&esc(&String::from_utf8_lossy(&t)));
                }
            }
            X::Eof => break,
            _ => {}
        }
    }
    out.push_str("</body></html>");
    // inline the binaries as data: URIs
    for (id, (ct, data)) in binaries {
        let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
        out = out.replace(
            &format!("fb2bin:{}\"", esc(&id)),
            &format!("data:{ct};base64,{clean}\""),
        );
    }
    (title.filter(|t| !t.is_empty()), out)
}

pub fn fb2(bytes: &[u8]) -> Result<Doc, String> {
    let src = super::office::decode_text(bytes);
    if !src.contains("FictionBook") {
        return Err("not a FictionBook (.fb2) file".into());
    }
    let (title, html_src) = fb2_to_html(&src);
    let mut d = Doc::new(Page::A4);
    d.face = Face::Serif;
    d.justify = true;
    let mut fetch = |_: &str| None;
    let out = html::convert(&html_src, &mut d.files, &mut fetch, None);
    d.blocks = out.blocks;
    d.title = title;
    Ok(d)
}

// ---------------------------------------------------------------- comics

const IMAGE_EXTS: [&str; 8] = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];
const COMIC_BUDGET: usize = 2 * 1024 * 1024 * 1024;

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    !base.starts_with('.')
        && !lower.contains("__macosx/")
        && lower
            .rsplit_once('.')
            .is_some_and(|(_, e)| IMAGE_EXTS.contains(&e))
}

/// Page images of a comic archive, natural-sorted by path.
pub fn comic_pages(ext: &str, bytes: Vec<u8>) -> Result<Vec<Vec<u8>>, String> {
    let mut pages: Vec<(String, Vec<u8>)> = Vec::new();
    let mut used = 0usize;
    let mut take =
        |name: String, data: Vec<u8>, pages: &mut Vec<(String, Vec<u8>)>| -> Result<(), String> {
            used += data.len();
            if used > COMIC_BUDGET {
                return Err("comic too large to convert".into());
            }
            pages.push((name, data));
            Ok(())
        };
    match ext {
        "cbz" => {
            let mut z = zip::ZipArchive::new(Cursor::new(&bytes[..]))
                .map_err(|e| format!("not a valid CBZ (zip): {e}"))?;
            for i in 0..z.len() {
                let Ok(f) = z.by_index(i) else { continue };
                if f.is_dir() || !is_image(f.name()) {
                    continue;
                }
                let name = f.name().to_string();
                let mut buf = Vec::new();
                f.take(256 * 1024 * 1024)
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                take(name, buf, &mut pages)?;
            }
        }
        "cbt" => {
            let mut a = tar::Archive::new(Cursor::new(&bytes[..]));
            for entry in a
                .entries()
                .map_err(|e| format!("not a valid CBT (tar): {e}"))?
            {
                let mut e = entry.map_err(|e| e.to_string())?;
                let name = e
                    .path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !e.header().entry_type().is_file() || !is_image(&name) {
                    continue;
                }
                let mut buf = Vec::new();
                (&mut e)
                    .take(256 * 1024 * 1024)
                    .read_to_end(&mut buf)
                    .map_err(|e| e.to_string())?;
                take(name, buf, &mut pages)?;
            }
        }
        "cb7" => {
            let mut r = sevenz_rust2::ArchiveReader::new(
                Cursor::new(&bytes[..]),
                sevenz_rust2::Password::empty(),
            )
            .map_err(|e| format!("not a valid CB7 (7z): {e}"))?;
            let mut err = None;
            r.for_each_entries(|entry, rd| {
                if entry.is_directory() || !is_image(entry.name()) {
                    // must still drain solid streams
                    let _ = std::io::copy(rd, &mut std::io::sink());
                    return Ok(true);
                }
                let mut buf = Vec::new();
                rd.take(256 * 1024 * 1024).read_to_end(&mut buf)?;
                if let Err(e) = take(entry.name().to_string(), buf, &mut pages) {
                    err = Some(e);
                    return Ok(false);
                }
                Ok(true)
            })
            .map_err(|e| e.to_string())?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        "cbr" => {
            let a = rars::ArchiveReader::read(&bytes)
                .map_err(|e| format!("not a readable CBR (rar): {e}"))?;
            // one sequential pass (solid archives decode each member once)
            let sink: Rc<RefCell<Vec<(String, Vec<u8>)>>> = Rc::default();
            let s2 = sink.clone();
            a.extract_to(None, move |meta| {
                let name = meta.name_lossy();
                if meta.is_directory || !is_image(&name) {
                    return Ok(Box::new(std::io::sink()) as Box<dyn Write>);
                }
                s2.borrow_mut().push((name, Vec::new()));
                let idx = s2.borrow().len() - 1;
                Ok(Box::new(Entry(s2.clone(), idx)) as Box<dyn Write>)
            })
            .map_err(|e| format!("CBR: {e}"))?;
            let got = std::mem::take(&mut *sink.borrow_mut());
            for (name, data) in got {
                take(name, data, &mut pages)?;
            }
        }
        _ => return Err("not a comic archive".into()),
    }
    if pages.is_empty() {
        return Err("no pictures in this comic archive".into());
    }
    pages.sort_by(|a, b| crate::fsops::natural_cmp(&a.0, &b.0));
    Ok(pages.into_iter().map(|(_, b)| b).collect())
}

/// Writer appending to one extracted entry.
struct Entry(Rc<RefCell<Vec<(String, Vec<u8>)>>>, usize);

impl Write for Entry {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let mut v = self.0.borrow_mut();
        let total: usize = v.iter().map(|e| e.1.len()).sum();
        if total + b.len() > COMIC_BUDGET {
            return Err(std::io::Error::other("comic too large to convert"));
        }
        v[self.1].1.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn comic(ext: &str, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let pages = comic_pages(ext, bytes)?;
    super::super::layout::images_to_pdf(&pages, None, 0.0)
}
