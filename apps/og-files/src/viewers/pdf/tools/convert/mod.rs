//! PDF → other formats, natively (Stirling's "convert from PDF" without the
//! server). Every target is built from the layout model of
//! [`super::extract`]: paragraphs with runs, headings, lists, tables and
//! images in reading order.
//!
//! | target | what comes out |
//! |---|---|
//! | Word / ODT / RTF | headings (styles Heading 1–3), paragraphs with bold/italic/size/colour/font runs, alignment, native lists, tables (ruled ones with borders, merged cells), images, a page break per PDF page, the PDF's page size |
//! | PowerPoint | one slide per page: the page rendered *without its text* as the slide picture, the text on top as editable text boxes in place (OCR text stays invisible) |
//! | HTML / EPUB | semantic XHTML (h1–h3, p, ul/ol, table, img); HTML is one self-contained file (images inline), EPUB 3 (+ NCX) with chapters at level-1 headings or per page |
//! | Markdown | CommonMark + GFM tables; images written as separate files next to it |
//! | Text | reading order, paragraphs separated by blank lines, tables tab-separated, a form feed between pages |
//! | Excel / CSV | detected tables (one sheet / one file each); without tables, every line of every page |
//! | XML | structured dump: page → heading/paragraph/list-item/table/image → line → span with positions, fonts and colours |

mod epub;
mod office;
mod sheet;
mod text;

#[cfg(test)]
mod tests;

use super::extract::{self, Item, Layout, Picture};
use std::io::{Cursor, Write};

/// Targets for PDF → other formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Docx,
    Odt,
    Xlsx,
    Pptx,
    Html,
    Markdown,
    Text,
    Rtf,
    Xml,
    Csv,
    Epub,
}

impl Target {
    pub const ALL: [Target; 11] = [
        Target::Docx,
        Target::Odt,
        Target::Xlsx,
        Target::Pptx,
        Target::Html,
        Target::Markdown,
        Target::Text,
        Target::Rtf,
        Target::Xml,
        Target::Csv,
        Target::Epub,
    ];

    pub fn ext(self) -> &'static str {
        match self {
            Target::Docx => "docx",
            Target::Odt => "odt",
            Target::Xlsx => "xlsx",
            Target::Pptx => "pptx",
            Target::Html => "html",
            Target::Markdown => "md",
            Target::Text => "txt",
            Target::Rtf => "rtf",
            Target::Xml => "xml",
            Target::Csv => "csv",
            Target::Epub => "epub",
        }
    }

    fn wants_images(self) -> bool {
        matches!(
            self,
            Target::Docx
                | Target::Odt
                | Target::Html
                | Target::Markdown
                | Target::Rtf
                | Target::Epub
                | Target::Xml
        )
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Target::Docx => "Word (.docx)",
            Target::Odt => "OpenDocument (.odt)",
            Target::Xlsx => "Excel (.xlsx)",
            Target::Pptx => "PowerPoint (.pptx)",
            Target::Html => "HTML",
            Target::Markdown => "Markdown",
            Target::Text => "Plain text",
            Target::Rtf => "RTF",
            Target::Xml => "XML",
            Target::Csv => "CSV (tables)",
            Target::Epub => "EPUB",
        })
    }
}

/// Convert `pdf` to `target`. Returns files named by suffix (the caller
/// writes them as `<stem>-<suffix>` next to the PDF): `converted.<ext>`,
/// plus images for Markdown and one CSV per table.
/// `stem` is the PDF's file name without extension — used for titles and
/// for links between the written files.
pub fn convert(pdf: &[u8], target: Target, stem: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut layout = extract::extract(
        pdf,
        extract::Options {
            images: target.wants_images(),
        },
    )?;
    if !matches!(target, Target::Pptx | Target::Xml) {
        unhide_ocr(&mut layout);
    }
    if layout.pages.is_empty() {
        return Err("this PDF has no pages".into());
    }
    let has_pics = layout
        .pages
        .iter()
        .any(|p| p.items.iter().any(|i| matches!(i, Item::Picture(_))));
    if !layout.has_text() && !(has_pics || target == Target::Pptx) {
        return Err("no text found in this PDF — if it is a scan, run OCR first".into());
    }
    let title = doc_title(&layout, stem);
    let one = |bytes: Vec<u8>| Ok(vec![(format!("converted.{}", target.ext()), bytes)]);
    match target {
        Target::Docx => one(office::docx(&layout)?),
        Target::Odt => one(office::odt(&layout)?),
        Target::Pptx => one(office::pptx(pdf, &layout)?),
        Target::Text => one(text::txt(&layout).into_bytes()),
        Target::Rtf => one(text::rtf(&layout).into_bytes()),
        Target::Html => one(text::html(&layout, &title).into_bytes()),
        Target::Xml => one(text::xml(&layout, stem).into_bytes()),
        Target::Markdown => {
            let (md, images) = text::markdown(&layout, &format!("{stem}-converted-img-"));
            let mut out = vec![("converted.md".to_string(), md.into_bytes())];
            out.extend(
                images
                    .into_iter()
                    .map(|(n, b)| (format!("converted-img-{n}"), b)),
            );
            Ok(out)
        }
        Target::Xlsx => one(sheet::xlsx(&layout)?),
        Target::Csv => sheet::csv(&layout),
        Target::Epub => one(epub::epub(&layout, &title, pdf)?),
    }
}

/// Scanned pages with an OCR text layer: the invisible text *is* the
/// content for reflowed formats, so show it and drop the page-sized scan
/// image behind it (it would repeat everything).
fn unhide_ocr(layout: &mut Layout) {
    for p in &mut layout.pages {
        let ocr = p.items.iter().any(|i| match i {
            Item::Para(pa) => pa.lines.iter().any(|l| l.spans.iter().any(|s| s.invisible)),
            Item::Table(t) => t
                .rows
                .iter()
                .flatten()
                .any(|c| c.lines.iter().any(|l| l.spans.iter().any(|s| s.invisible))),
            Item::Picture(_) => false,
        });
        if !ocr {
            continue;
        }
        let area = p.width * p.height;
        p.items.retain(|i| match i {
            Item::Picture(pic) => pic.bbox.w() * pic.bbox.h() < 0.5 * area,
            _ => true,
        });
        let unhide = |lines: &mut Vec<extract::Line>| {
            for l in lines {
                for s in &mut l.spans {
                    s.invisible = false;
                }
            }
        };
        for i in &mut p.items {
            match i {
                Item::Para(pa) => unhide(&mut pa.lines),
                Item::Table(t) => {
                    for c in t.rows.iter_mut().flatten() {
                        unhide(&mut c.lines);
                    }
                }
                Item::Picture(_) => {}
            }
        }
    }
}

/// First level-1 heading of the first page, else the file name.
fn doc_title(layout: &Layout, stem: &str) -> String {
    layout
        .pages
        .first()
        .and_then(|p| {
            p.items.iter().find_map(|i| match i {
                Item::Para(p) if p.kind == extract::Kind::Heading(1) => {
                    Some(p.text().trim().to_string())
                }
                _ => None,
            })
        })
        .filter(|t| !t.is_empty() && t.chars().count() <= 120)
        .unwrap_or_else(|| stem.to_string())
}

// ---------------------------------------------------------------- helpers

/// XML text/attribute escape; drops characters XML 1.0 can't carry.
pub(super) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            '\t' | '\n' | '\r' => o.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => o.push(c),
        }
    }
    o
}

/// Distinct pictures of the document (by id), in first-use order.
pub(super) fn pictures(layout: &Layout) -> Vec<&Picture> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in &layout.pages {
        for i in &p.items {
            if let Item::Picture(pic) = i {
                if seen.insert(pic.id) {
                    out.push(pic);
                }
            }
        }
    }
    out
}

pub(super) fn hex(c: [u8; 3]) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// Colour worth writing (not black / near black).
pub(super) fn colored(c: [u8; 3]) -> bool {
    c.iter().any(|&v| v > 40)
}

/// Font sizes are kept to half points.
pub(super) fn half(size: f32) -> f32 {
    ((size * 2.0).round() / 2.0).clamp(1.0, 1638.0)
}

/// Zip with the given entries. `stored` entries are written uncompressed
/// (`mimetype` of ODF/EPUB must be first and stored).
pub(super) fn zip(entries: Vec<(String, Vec<u8>, bool)>) -> Result<Vec<u8>, String> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data, stored) in entries {
        let opts = zip::write::SimpleFileOptions::default().compression_method(if stored {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        });
        z.start_file(name.as_str(), opts)
            .map_err(|e| format!("can't write {name}: {e}"))?;
        z.write_all(&data)
            .map_err(|e| format!("can't write {name}: {e}"))?;
    }
    z.finish()
        .map(|c| c.into_inner())
        .map_err(|e| format!("can't finish the file: {e}"))
}

/// A stable pseudo-UUID from content (EPUB identifier).
pub(super) fn uuid_of(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut a = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut a);
    let x = a.finish();
    let mut b = std::collections::hash_map::DefaultHasher::new();
    (x, bytes.len()).hash(&mut b);
    let y = b.finish();
    format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        (x >> 32) as u32,
        (x >> 16) as u16,
        x as u16 & 0xfff,
        (y >> 48) as u16 & 0xfff,
        y & 0xffff_ffff_ffff
    )
}

/// How each grid position of a table is written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Slot {
    /// a real cell (possibly spanning)
    Origin,
    /// covered by a cell to the left (or above-left): not written
    Skip,
    /// continues a vertically merged cell; carries that cell's colspan
    VCont { colspan: u16 },
}

pub(super) fn slots(t: &extract::Table) -> Vec<Vec<Slot>> {
    let nr = t.rows.len();
    let nc = t.cols();
    let mut out = vec![vec![Slot::Origin; nc]; nr];
    for r in 0..nr {
        for c in 0..nc {
            let cell = &t.rows[r][c];
            if cell.covered {
                if out[r][c] == Slot::Origin {
                    out[r][c] = Slot::Skip;
                }
                continue;
            }
            let (rs, cs) = (cell.rowspan.max(1) as usize, cell.colspan.max(1) as usize);
            for (rr, row) in out.iter_mut().enumerate().skip(r).take(rs) {
                for (cc, slot) in row.iter_mut().enumerate().skip(c).take(cs) {
                    if (rr, cc) == (r, c) {
                        continue;
                    }
                    *slot = if cc == c {
                        Slot::VCont { colspan: cs as u16 }
                    } else {
                        Slot::Skip
                    };
                }
            }
        }
    }
    out
}

/// Current UTC time as `YYYY-MM-DDThh:mm:ssZ`.
pub(super) fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
