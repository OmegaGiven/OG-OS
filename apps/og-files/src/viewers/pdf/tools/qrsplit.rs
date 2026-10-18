//! Auto-split at separator pages carrying a QR code (Stirling-PDF's
//! "auto-split-pdf"), natively: every page is rendered with hayro and
//! scanned for QR codes with `rqrr`. Separator pages are dropped; the pages
//! between them become separate documents.
//!
//! Stirling's printable separator sheet encodes its project URL
//! (`https://github.com/Stirling-Tools/Stirling-PDF`, older sheets
//! `…/Frooodle/Stirling-PDF`); by default only that marker splits, so
//! documents that merely contain QR codes (tickets, invoices) stay whole.
//! `any_qr` splits at every page with a readable QR code.
//!
//! Duplex mode: a separator sheet scanned double-sided leaves its back as
//! the next page. That page is dropped too, but only when it is blank
//! (≤ 1 % ink), so no content can be lost.
//!
//! Limits: QR codes smaller than ~2 cm or badly damaged may be missed
//! (pages are scanned at 200 dpi).

use super::raster::{open_hayro, render_page};
use hayro::RenderCache;
use std::sync::Arc;

pub const STIRLING_MARKERS: [&str; 2] = [
    "https://github.com/Stirling-Tools/Stirling-PDF",
    "https://github.com/Frooodle/Stirling-PDF",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// split at any QR code, not only Stirling's separator sheet
    pub any_qr: bool,
    /// drop the (blank) back of each separator sheet
    pub duplex: bool,
}

const SCAN_DPI: f32 = 200.0;
const BLANK_DPI: f32 = 20.0;
const BLANK_INK: f64 = 0.01;

/// Page groups between separators (0-based page indices).
pub fn groups(pdf: &[u8], opts: Options) -> Result<Vec<Vec<usize>>, String> {
    let doc = open_hayro(pdf)?;
    let n = doc.pages().len();
    let cache = RenderCache::new();
    let is_sep = |i: usize| -> Result<bool, String> {
        let (img, _) = render_page(&doc, &cache, i, SCAN_DPI)?;
        let codes = qr_codes(&image::DynamicImage::ImageRgba8(img).to_luma8());
        Ok(codes.iter().any(|c| opts.any_qr || is_marker(c)))
    };
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut found = false;
    let mut i = 0;
    while i < n {
        if is_sep(i)? {
            found = true;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if opts.duplex && i + 1 < n && is_blank(&doc, &cache, i + 1)? {
                i += 1;
            }
        } else {
            cur.push(i);
        }
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if !found {
        return Err(if opts.any_qr {
            "no page with a QR code was found".into()
        } else {
            "no Stirling-PDF separator page found (tick “Split at any QR code” \
             for other codes)"
                .into()
        });
    }
    if out.is_empty() {
        return Err("the document has only separator pages".into());
    }
    Ok(out)
}

/// Split into `part-01.pdf`, `part-02.pdf`, … (separators removed).
pub fn auto_split(pdf: &[u8], opts: Options) -> Result<Vec<(String, Vec<u8>)>, String> {
    let groups = groups(pdf, opts)?;
    let src = [Arc::new(pdf.to_vec())];
    groups
        .iter()
        .enumerate()
        .map(|(k, g)| {
            let plan: Vec<_> = g.iter().map(|&p| (0, p, 0)).collect();
            super::super::compose::build(&src, &plan).map(|b| (format!("part-{:02}.pdf", k + 1), b))
        })
        .collect()
}

fn is_marker(code: &str) -> bool {
    let c = code.trim().trim_end_matches('/');
    STIRLING_MARKERS.iter().any(|m| c.eq_ignore_ascii_case(m))
}

/// Decoded contents of every readable QR code in `img`.
pub fn qr_codes(img: &image::GrayImage) -> Vec<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut prepared = rqrr::PreparedImage::prepare(img.clone());
        prepared
            .detect_grids()
            .into_iter()
            .filter_map(|g| g.decode().ok().map(|(_, s)| s))
            .collect()
    }))
    .unwrap_or_default()
}

fn is_blank<'a>(
    doc: &'a hayro::hayro_syntax::Pdf,
    cache: &RenderCache<'a>,
    i: usize,
) -> Result<bool, String> {
    let (img, _) = render_page(doc, cache, i, BLANK_DPI)?;
    let total = (img.width() as u64 * img.height() as u64).max(1);
    let ink = img
        .pixels()
        .filter(|p| p.0[..3].iter().any(|&c| c < 230))
        .count() as u64;
    Ok(ink as f64 / total as f64 <= BLANK_INK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qr_png(text: &str) -> Vec<u8> {
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .min_dimensions(300, 300)
            .build();
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn content_png(shade: u8) -> Vec<u8> {
        let mut img = image::GrayImage::from_pixel(400, 500, image::Luma([255]));
        for y in 50..450 {
            for x in 50..350 {
                img.put_pixel(x, y, image::Luma([shade]));
            }
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn blank_png() -> Vec<u8> {
        let img = image::GrayImage::from_pixel(400, 500, image::Luma([255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn pdf(pages: Vec<Vec<u8>>) -> Vec<u8> {
        super::super::layout::images_to_pdf(&pages, Some(super::super::Paper::A4), 36.0).unwrap()
    }

    #[test]
    fn splits_at_stirling_separators() {
        let sep = qr_png(STIRLING_MARKERS[0]);
        let other = qr_png("https://example.com/ticket/42");
        let doc = pdf(vec![
            content_png(10),
            content_png(40),
            sep.clone(),
            content_png(80),
            other.clone(),
            sep.clone(),
            content_png(120),
        ]);
        let g = groups(&doc, Options::default()).unwrap();
        assert_eq!(g, vec![vec![0, 1], vec![3, 4], vec![6]]);
        // any QR also splits at the ticket page
        let g = groups(
            &doc,
            Options {
                any_qr: true,
                duplex: false,
            },
        )
        .unwrap();
        assert_eq!(g, vec![vec![0, 1], vec![3], vec![6]]);
        let files = auto_split(&doc, Options::default()).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].0, "part-01.pdf");
        let n = hayro::hayro_syntax::Pdf::new(Arc::new(files[1].1.clone()))
            .unwrap()
            .pages()
            .len();
        assert_eq!(n, 2);
    }

    #[test]
    fn duplex_drops_blank_backs_only() {
        let sep = qr_png(STIRLING_MARKERS[0]);
        let doc = pdf(vec![
            content_png(10),
            sep.clone(),
            blank_png(),
            content_png(40),
            sep,
            content_png(60), // not blank: kept
        ]);
        let opts = Options {
            any_qr: false,
            duplex: true,
        };
        assert_eq!(groups(&doc, opts).unwrap(), vec![vec![0], vec![3], vec![5]]);
        let plain = groups(&doc, Options::default()).unwrap();
        assert_eq!(plain, vec![vec![0], vec![2, 3], vec![5]]);
    }

    #[test]
    fn no_separator_is_an_error() {
        let doc = pdf(vec![content_png(10), content_png(20)]);
        assert!(groups(&doc, Options::default())
            .unwrap_err()
            .contains("separator"));
    }
}
