//! Spreadsheet targets: Excel (one sheet per detected table, or per page
//! with one row per text line when there are no tables) and CSV (one file
//! per table, or all lines).

use super::super::extract::{Item, Layout, Table};
use super::{slots, Slot};
use rust_xlsxwriter::{Format, Workbook};

/// "1,234.50" / "-12" / "3.5" → number; anything else stays text.
pub(super) fn as_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t.len() > 24 {
        return None;
    }
    let neg = t.starts_with('(') && t.ends_with(')');
    let t = if neg { &t[1..t.len() - 1] } else { t };
    let ok = t.chars().enumerate().all(|(i, c)| {
        c.is_ascii_digit() || c == '.' || c == ',' || (i == 0 && (c == '-' || c == '+'))
    });
    if !ok || !t.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    // thousands separators only in groups of three before the decimal point
    let int_part = t.split('.').next().unwrap_or("");
    if int_part.contains(',') {
        let groups: Vec<&str> = int_part.trim_start_matches(['-', '+']).split(',').collect();
        if groups[0].is_empty() || groups[0].len() > 3 || groups[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
    }
    // leading zeros ("007", ZIP codes) are identifiers, not numbers
    let digits = int_part.trim_start_matches(['-', '+']);
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    let v: f64 = t.replace(',', "").parse().ok()?;
    Some(if neg { -v } else { v })
}

fn sheet_name(base: &str, used: &mut Vec<String>) -> String {
    let clean: String = base
        .chars()
        .map(|c| if "[]:*?/\\".contains(c) { ' ' } else { c })
        .take(28)
        .collect();
    let mut name = clean.clone();
    let mut k = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
        name = format!("{} {k}", clean.chars().take(25).collect::<String>());
        k += 1;
    }
    used.push(name.clone());
    name
}

fn xerr(e: rust_xlsxwriter::XlsxError) -> String {
    format!("can't write the spreadsheet: {e}")
}

pub fn xlsx(layout: &Layout) -> Result<Vec<u8>, String> {
    let mut wb = Workbook::new();
    let bold = Format::new().set_bold();
    let wrap = Format::new()
        .set_text_wrap()
        .set_align(rust_xlsxwriter::FormatAlign::Top);
    let mut used = Vec::new();
    let tables: Vec<(usize, &Table)> = layout.tables().collect();
    if !tables.is_empty() {
        let mut per_page: std::collections::HashMap<usize, usize> = Default::default();
        for (page, t) in tables {
            let k = per_page.entry(page).or_default();
            *k += 1;
            let ws = wb.add_worksheet();
            ws.set_name(sheet_name(&format!("Page {page} Table {k}"), &mut used))
                .map_err(xerr)?;
            let sl = slots(t);
            let header = t.rows.len() > 1 && t.rows[0].iter().any(|c| c.bold());
            let mut widths = vec![8usize; t.cols()];
            for (r, row) in t.rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    if sl[r][c] != Slot::Origin {
                        continue;
                    }
                    let text = cell.text();
                    let (r32, c16) = (r as u32, c as u16);
                    let fmt = if header && r == 0 { &bold } else { &wrap };
                    if cell.colspan > 1 || cell.rowspan > 1 {
                        ws.merge_range(
                            r32,
                            c16,
                            r32 + cell.rowspan as u32 - 1,
                            c16 + cell.colspan - 1,
                            &text,
                            fmt,
                        )
                        .map_err(xerr)?;
                    } else if let Some(v) = as_number(&text).filter(|_| !(header && r == 0)) {
                        ws.write_number(r32, c16, v).map_err(xerr)?;
                    } else if !text.is_empty() {
                        ws.write_string_with_format(r32, c16, &text, fmt)
                            .map_err(xerr)?;
                    }
                    if cell.colspan <= 1 {
                        let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0);
                        widths[c] = widths[c].max(longest.min(60));
                    }
                }
            }
            for (c, w) in widths.iter().enumerate() {
                ws.set_column_width(c as u16, *w as f64 + 2.0)
                    .map_err(xerr)?;
            }
            if header {
                ws.set_freeze_panes(1, 0).map_err(xerr)?;
            }
        }
    } else {
        for p in &layout.pages {
            let ws = wb.add_worksheet();
            ws.set_name(sheet_name(&format!("Page {}", p.number), &mut used))
                .map_err(xerr)?;
            let mut r = 0u32;
            let mut width = 10usize;
            for it in &p.items {
                if let Item::Para(pa) = it {
                    let is_head = matches!(pa.kind, super::super::extract::Kind::Heading(_));
                    for l in &pa.lines {
                        let t = l.text().trim().to_string();
                        if t.is_empty() {
                            continue;
                        }
                        width = width.max(t.chars().count().min(100));
                        if is_head {
                            ws.write_string_with_format(r, 0, &t, &bold).map_err(xerr)?;
                        } else {
                            ws.write_string(r, 0, &t).map_err(xerr)?;
                        }
                        r += 1;
                        if r >= 1_048_000 {
                            break;
                        }
                    }
                }
            }
            ws.set_column_width(0, width as f64).map_err(xerr)?;
        }
    }
    wb.save_to_buffer().map_err(xerr)
}

fn csv_rows(t: &Table) -> Vec<Vec<String>> {
    t.rows
        .iter()
        .map(|row| row.iter().map(|c| c.text()).collect())
        .collect()
}

fn write_csv(rows: &[Vec<String>]) -> Result<Vec<u8>, String> {
    let mut w = csv::WriterBuilder::new()
        .flexible(true)
        .from_writer(Vec::new());
    for r in rows {
        w.write_record(r).map_err(|e| e.to_string())?;
    }
    w.into_inner().map_err(|e| e.to_string())
}

pub fn csv(layout: &Layout) -> Result<Vec<(String, Vec<u8>)>, String> {
    let tables: Vec<(usize, &Table)> = layout.tables().collect();
    match tables.len() {
        0 => {
            // no tables: one row per line, page number first
            let mut rows = vec![vec!["page".to_string(), "text".to_string()]];
            for p in &layout.pages {
                for it in &p.items {
                    if let Item::Para(pa) = it {
                        for l in &pa.lines {
                            let t = l.text().trim().to_string();
                            if !t.is_empty() {
                                rows.push(vec![p.number.to_string(), t]);
                            }
                        }
                    }
                }
            }
            Ok(vec![("converted.csv".into(), write_csv(&rows)?)])
        }
        1 => Ok(vec![(
            "converted.csv".into(),
            write_csv(&csv_rows(tables[0].1))?,
        )]),
        _ => {
            let mut per_page: std::collections::HashMap<usize, usize> = Default::default();
            let mut out = Vec::new();
            for (page, t) in tables {
                let k = per_page.entry(page).or_default();
                *k += 1;
                out.push((
                    format!("page{page:02}-table{k}.csv"),
                    write_csv(&csv_rows(t))?,
                ));
            }
            Ok(out)
        }
    }
}
