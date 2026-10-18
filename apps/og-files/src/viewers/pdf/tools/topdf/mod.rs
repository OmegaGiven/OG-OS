//! Anything → PDF, natively (SPEC.md §5a "Convert to PDF").
//!
//! Every input is turned into a small document model ([`ir`]) which is
//! emitted as Typst source and typeset in-process by the Typst compiler
//! ([`engine`]) with bundled fonts — no LibreOffice, browser or server.
//! Comic archives skip Typst: their pages go through the existing
//! images → PDF tool.
//!
//! | input                         | route                                   |
//! |-------------------------------|-----------------------------------------|
//! | .docx                         | app's DOCX model ([`docx`])             |
//! | .odt .rtf                     | document viewer's block model           |
//! | .txt .text .log               | monospace (or reflowed) text            |
//! | .md .markdown                 | pulldown-cmark → HTML → [`html`]        |
//! | .html .htm .xhtml, URLs       | [`html`]                                |
//! | .eml                          | mail-parser → headers + body ([`mail`]) |
//! | .xlsx .xlsm .xls .xlsb .ods .csv .tsv | sheet viewer's loaders → tables |
//! | .pptx .odp                    | slide-sized pages ([`slides`])          |
//! | .svg                          | page sized to the drawing (vector)      |
//! | .epub .mobi .azw3 .fb2        | [`ebook`]                               |
//! | .cbz .cbr .cb7 .cbt           | images → PDF ([`ebook::comic`])         |

mod docx;
mod ebook;
pub mod engine;
mod html;
pub mod ir;
mod mail;
mod office;
mod slides;
#[cfg(test)]
mod tests;

use ir::{Block, Doc, Inline, Page};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Extensions [`file_to_pdf`] handles (images are converted elsewhere).
pub const EXTS: [&str; 31] = [
    "docx", "odt", "rtf", "txt", "text", "log", "md", "markdown", "html", "htm", "xhtml", "eml",
    "xlsx", "xlsm", "xls", "xlsb", "ods", "csv", "tsv", "pptx", "odp", "svg", "epub", "mobi",
    "azw3", "azw", "fb2", "cbz", "cbr", "cb7", "cbt",
];

/// largest input read (comic archives get more room)
const FILE_LIMIT: u64 = 256 * 1024 * 1024;
const COMIC_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
/// largest local image an HTML/Markdown file may pull in
const LOCAL_IMAGE_LIMIT: u64 = 64 * 1024 * 1024;

pub fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn supported(path: &Path) -> bool {
    EXTS.contains(&ext_of(path).as_str())
}

pub(crate) fn plain_text(inl: &[Inline]) -> String {
    let mut s = String::new();
    for i in inl {
        match i {
            Inline::Text(t, _) => s.push_str(t),
            Inline::Link(_, inner) => s.push_str(&plain_text(inner)),
            Inline::Break => s.push(' '),
            Inline::Image(_) => {}
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > limit {
        return Err(format!(
            "file too large to convert ({} MB, limit {} MB)",
            len / (1024 * 1024),
            limit / (1024 * 1024)
        ));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

/// Reads images referenced by a local HTML/Markdown file: relative to its
/// folder (or absolute paths, or file:// URLs). Never touches the network.
fn local_fetch(dir: &Path) -> impl FnMut(&str) -> Option<Vec<u8>> + '_ {
    move |src: &str| {
        let src = src.split(['?', '#']).next().unwrap_or(src);
        let lower = src.to_ascii_lowercase();
        if lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("//") {
            return None;
        }
        let rel = src.strip_prefix("file://").unwrap_or(src);
        let rel = html::percent_decode(rel);
        let p = Path::new(&rel);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        let meta = std::fs::metadata(&full).ok()?;
        if !meta.is_file() || meta.len() > LOCAL_IMAGE_LIMIT {
            return None;
        }
        std::fs::read(full).ok()
    }
}

fn stem(path: &Path) -> Option<String> {
    path.file_stem().map(|s| s.to_string_lossy().into_owned())
}

/// HTML text → document.
fn html_doc(src: &str, fetch: &mut html::Fetch<'_>, link_base: Option<url::Url>) -> Doc {
    let mut d = Doc::new(Page::A4);
    let out = html::convert(src, &mut d.files, fetch, link_base);
    d.title = out.title;
    d.blocks = out.blocks;
    d
}

pub fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::{html::push_html, Options, Parser};
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_SMART_PUNCTUATION;
    let mut out = String::with_capacity(md.len() * 3 / 2);
    push_html(&mut out, Parser::new_ext(md, opts));
    out
}

/// A drawing on a page of its own size (vector all the way: Typst embeds
/// the SVG through krilla-svg, the successor of svg2pdf).
fn svg_doc(bytes: Vec<u8>) -> Result<Doc, String> {
    let mut d = Doc::new(Page::A4);
    let img = d.files.add(bytes).ok_or("not a valid SVG drawing")?;
    let (w, h) = (img.w.clamp(1.0, 14_400.0), img.h.clamp(1.0, 14_400.0));
    d.page = Page {
        w,
        h,
        margin: [0.0; 4],
    };
    let img = ir::Img { w, h, ..img };
    d.blocks.push(Block::Image(img, ir::Align::Left));
    Ok(d)
}

/// Convert a file to PDF bytes (blocking; run off the UI thread).
pub fn file_to_pdf(path: &Path) -> Result<Vec<u8>, String> {
    let ext = ext_of(path);
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut doc = match ext.as_str() {
        "cbz" | "cbr" | "cb7" | "cbt" => {
            return ebook::comic(&ext, read_limited(path, COMIC_LIMIT)?);
        }
        "docx" => match docx::convert(read_limited(path, FILE_LIMIT)?) {
            Ok(d) => d,
            // documents the editor model refuses still read as text
            Err(_) => office::rich(path)?,
        },
        "odt" | "rtf" => office::rich(path)?,
        "txt" | "text" | "log" => office::text(&read_limited(path, FILE_LIMIT)?, true),
        "md" | "markdown" => {
            let src = office::decode_text(&read_limited(path, FILE_LIMIT)?);
            let mut fetch = local_fetch(dir);
            let mut d = html_doc(&markdown_to_html(&src), &mut fetch, None);
            d.face = ir::Face::Serif;
            d
        }
        "html" | "htm" | "xhtml" => {
            let src = office::decode_text(&read_limited(path, FILE_LIMIT)?);
            let mut fetch = local_fetch(dir);
            html_doc(&src, &mut fetch, None)
        }
        "eml" => mail::convert(&read_limited(path, FILE_LIMIT)?)?,
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" | "csv" | "tsv" => office::sheets(path)?,
        "pptx" => slides::pptx(&read_limited(path, FILE_LIMIT)?)?,
        "odp" => slides::odp(&read_limited(path, FILE_LIMIT)?)?,
        "svg" => svg_doc(read_limited(path, FILE_LIMIT)?)?,
        "epub" => ebook::epub(&read_limited(path, FILE_LIMIT)?)?,
        "mobi" | "azw3" | "azw" => ebook::mobi(read_limited(path, FILE_LIMIT)?)?,
        "fb2" => ebook::fb2(&read_limited(path, FILE_LIMIT)?)?,
        "doc" | "ppt" => {
            return Err(format!(
                "legacy binary .{ext} files can't be converted — save it as .{ext}x first"
            ))
        }
        other => return Err(format!("can't convert .{other} files to PDF")),
    };
    if doc.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        doc.title = stem(path);
    }
    doc.to_pdf()
}

// ------------------------------------------------------------------ web

const PAGE_LIMIT: u64 = 16 * 1024 * 1024;
const WEB_IMAGE_LIMIT: u64 = 16 * 1024 * 1024;
const WEB_IMAGES_TOTAL: u64 = 128 * 1024 * 1024;
const WEB_IMAGES_MAX: usize = 300;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirects(8)
        .user_agent(concat!(
            "og-files/",
            env!("CARGO_PKG_VERSION"),
            " (convert to PDF)"
        ))
        .build()
}

fn get(agent: &ureq::Agent, url: &str, limit: u64) -> Result<(Vec<u8>, String, String), String> {
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("the server answered {code}"),
        other => other.to_string(),
    })?;
    let final_url = resp.get_url().to_string();
    let ctype = resp.content_type().to_ascii_lowercase();
    let mut buf = Vec::new();
    resp.into_reader()
        .take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > limit {
        return Err(format!("larger than {} MB", limit / (1024 * 1024)));
    }
    Ok((buf, ctype, final_url))
}

/// Fetch a web page (and its images) and typeset it. Scripts don't run:
/// pages that build their content in JavaScript come out (nearly) empty.
pub fn url_to_pdf(url: &str) -> Result<Vec<u8>, String> {
    let url = url.trim();
    let parsed = url::Url::parse(url).map_err(|_| "enter a full http:// or https:// address")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("enter a full http:// or https:// address".into());
    }
    let agent = agent();
    let (bytes, ctype, final_url) = get(&agent, parsed.as_str(), PAGE_LIMIT)?;
    let base = url::Url::parse(&final_url).unwrap_or(parsed);
    let mut doc = if ctype.contains("svg") {
        svg_doc(bytes)?
    } else if ctype.starts_with("image/") {
        return super::layout::images_to_pdf(&[bytes], None, 0.0);
    } else if ctype == "application/pdf" {
        return Ok(bytes);
    } else if ctype.starts_with("text/plain") {
        office::text(&bytes, true)
    } else if ctype.contains("markdown") {
        let html = markdown_to_html(&office::decode_text(&bytes));
        web_html(&agent, &html, &base)
    } else {
        web_html(&agent, &office::decode_text(&bytes), &base)
    };
    if doc.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        doc.title = Some(base.to_string());
    }
    doc.to_pdf()
}

fn web_html(agent: &ureq::Agent, html_src: &str, base: &url::Url) -> Doc {
    let mut total = 0u64;
    let mut count = 0usize;
    let mut fetch = |src: &str| -> Option<Vec<u8>> {
        let u = base.join(src.trim()).ok()?;
        if !matches!(u.scheme(), "http" | "https")
            || count >= WEB_IMAGES_MAX
            || total >= WEB_IMAGES_TOTAL
        {
            return None;
        }
        count += 1;
        let (b, _, _) = get(agent, u.as_str(), WEB_IMAGE_LIMIT).ok()?;
        total += b.len() as u64;
        Some(b)
    };
    html_doc(html_src, &mut fetch, Some(base.clone()))
}
