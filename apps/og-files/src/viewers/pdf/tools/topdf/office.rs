//! ODT / RTF (through the document viewer's block model), plain text, and
//! spreadsheets (through the sheet viewer's loaders).

use super::ir::{Align, Block, Cell, Col, Doc, Face, Inline, Page, Para, Style, Table};
use crate::viewers::{doc, sheet};
use std::path::Path;

fn runs(rs: &[doc::Run]) -> Vec<Inline> {
    rs.iter()
        .filter(|r| !r.text.is_empty())
        .map(|r| {
            Inline::Text(
                r.text.clone(),
                Style {
                    bold: r.bold,
                    italic: r.italic,
                    ..Default::default()
                },
            )
        })
        .collect()
}

pub fn doc_blocks(blocks: &[doc::Block]) -> Vec<Block> {
    blocks
        .iter()
        .map(|b| match b {
            doc::Block::Heading(l, rs) => Block::Heading(*l, runs(rs)),
            doc::Block::Para { runs: rs, indent } => Block::Para(Para {
                inl: runs(rs),
                indent: f32::from(*indent) * 18.0,
                ..Default::default()
            }),
            doc::Block::Table(rows) => Block::Table(Table {
                rows: rows
                    .iter()
                    .map(|r| {
                        r.iter()
                            .map(|c| Cell::text(c.clone(), Style::default()))
                            .collect()
                    })
                    .collect(),
                borders: true,
                ..Default::default()
            }),
            doc::Block::Note(s) => Block::Note(s.clone()),
        })
        .collect()
}

/// .odt / .rtf (and .docx files the editor model can't take).
pub fn rich(path: &Path) -> Result<Doc, String> {
    let blocks = match doc::load(path)? {
        doc::Doc::Rich(b) => doc_blocks(&b),
        doc::Doc::Book(chapters) => {
            let mut out = Vec::new();
            for (k, c) in chapters.iter().enumerate() {
                if k > 0 {
                    out.push(Block::PageBreak);
                }
                if !c.title.is_empty() && !matches!(c.blocks.first(), Some(doc::Block::Heading(..)))
                {
                    out.push(Block::Heading(
                        1,
                        vec![Inline::Text(c.title.clone(), Style::default())],
                    ));
                }
                out.extend(doc_blocks(&c.blocks));
            }
            out
        }
        doc::Doc::Markdown(_) => return Err("unexpected Markdown".into()),
    };
    let mut d = Doc::new(Page::A4);
    d.face = Face::Serif;
    d.blocks = blocks;
    Ok(d)
}

/// Decode text: UTF-8 (BOM stripped), UTF-16 with BOM, else Latin-1-ish
/// lossy UTF-8.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(b) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(b).into_owned();
    }
    let utf16 = |b: &[u8], le: bool| {
        let units: Vec<u16> = b
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(b) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return utf16(b, true);
    }
    if let Some(b) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return utf16(b, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        // not UTF-8: treat as Windows-1252/Latin-1
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Plain text: monospace keeps the file's layout (columns, ASCII art);
/// proportional reflows paragraphs separated by blank lines.
pub fn text(bytes: &[u8], mono: bool) -> Doc {
    let s = decode_text(bytes).replace("\r\n", "\n").replace('\r', "\n");
    let mut d = Doc::new(Page::A4);
    if mono {
        d.face = Face::Mono;
        d.size = 9.5;
        d.page.margin = [42.0, 42.0, 42.0, 42.0];
        d.blocks.push(Block::Code {
            text: s.trim_end().to_string(),
            shaded: false,
        });
    } else {
        for para in s.split("\n\n") {
            let t = para.split_whitespace().collect::<Vec<_>>().join(" ");
            if !t.is_empty() {
                d.blocks.push(Block::plain(t));
            }
        }
    }
    d
}

const MAX_SHEET_ROWS: usize = 10_000;
const MAX_CELLS: usize = 400_000;
const CELL_CHARS: usize = 2000;

fn is_number(s: &str) -> bool {
    let t = s.trim().trim_end_matches('%');
    !t.is_empty() && t.parse::<f64>().is_ok()
}

pub fn sheets(path: &Path) -> Result<Doc, String> {
    let sheets = sheet::load_sheets(path)?;
    let size = 9.0;
    let char_w = size * 0.55;
    let mut d = Doc::new(Page::A4);
    d.page.margin = [36.0, 36.0, 36.0, 36.0];
    d.size = size;
    // the widest sheet decides the orientation
    let mut prepared = Vec::new();
    let mut widest = 0.0f32;
    let mut cells_left = MAX_CELLS;
    for s in sheets.iter() {
        let mut rows: Vec<Vec<String>> = s.rows.clone();
        // trim empty rows at the end and empty columns at the right
        while rows
            .last()
            .is_some_and(|r| r.iter().all(|c| c.trim().is_empty()))
        {
            rows.pop();
        }
        let ncols = rows
            .iter()
            .map(|r| {
                r.iter()
                    .rposition(|c| !c.trim().is_empty())
                    .map_or(0, |i| i + 1)
            })
            .max()
            .unwrap_or(0);
        let mut truncated = s.truncated;
        let max_rows = MAX_SHEET_ROWS.min(cells_left / ncols.max(1));
        if rows.len() > max_rows {
            rows.truncate(max_rows);
            truncated = true;
        }
        cells_left = cells_left.saturating_sub(rows.len() * ncols);
        let mut widths = vec![2usize; ncols];
        for r in rows.iter().take(1000) {
            for (c, v) in r.iter().take(ncols).enumerate() {
                let longest = v.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                widths[c] = widths[c].max(longest.min(40));
            }
        }
        let w: Vec<f32> = widths.iter().map(|&n| n as f32 * char_w + 9.0).collect();
        widest = widest.max(w.iter().sum());
        prepared.push((s.name.clone(), rows, ncols, w, truncated));
    }
    if widest > d.page.body_w() {
        d.page = d.page.landscape();
    }
    let avail = d.page.body_w();
    let many = prepared.len() > 1;
    for (k, (name, rows, ncols, w, truncated)) in prepared.into_iter().enumerate() {
        if k > 0 {
            d.blocks.push(Block::PageBreak);
        }
        if many || !name.is_empty() {
            d.blocks.push(Block::Heading(
                2,
                vec![Inline::Text(name, Style::default())],
            ));
        }
        if ncols == 0 {
            d.blocks.push(Block::Note("(empty sheet)".into()));
            continue;
        }
        let total: f32 = w.iter().sum();
        let fits = total <= avail;
        let cols = w
            .iter()
            .map(|&x| if fits { Col::Pt(x) } else { Col::Fr(x) })
            .collect();
        let header = usize::from(rows.len() > 1);
        let rows = rows
            .into_iter()
            .enumerate()
            .map(|(ri, r)| {
                (0..ncols)
                    .map(|c| {
                        let mut v = r.get(c).cloned().unwrap_or_default();
                        if v.chars().count() > CELL_CHARS {
                            v = v.chars().take(CELL_CHARS).collect::<String>() + "…";
                        }
                        let st = Style {
                            bold: ri < header,
                            ..Default::default()
                        };
                        let align = if ri >= header && is_number(&v) {
                            Align::Right
                        } else {
                            Align::Left
                        };
                        Cell {
                            blocks: if v.is_empty() {
                                Vec::new()
                            } else {
                                vec![Block::Para(Para {
                                    inl: vec![Inline::Text(v, st)],
                                    align,
                                    ..Default::default()
                                })]
                            },
                            colspan: 1,
                            rowspan: 1,
                            fill: (ri < header).then_some([236, 236, 236]),
                        }
                    })
                    .collect()
            })
            .collect();
        d.blocks.push(Block::Table(Table {
            cols,
            header,
            rows,
            size: Some(if fits { size } else { 7.5 }),
            borders: true,
        }));
        if truncated {
            d.blocks.push(Block::Note(
                "Only the first rows of this sheet are included.".into(),
            ));
        }
    }
    Ok(d)
}
