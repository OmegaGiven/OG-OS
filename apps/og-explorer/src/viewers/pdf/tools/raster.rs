//! Operations that render pages with hayro and, where a new PDF is produced,
//! splice image-only pages back into the original document with lopdf.
//!
//! A rasterized page keeps its page object (so links/outline entries on
//! *other* pages that point at it still resolve) but gets new /Contents
//! (`q W 0 0 H 0 0 cm /Im0 Do Q`), /Resources (just the JPEG XObject) and a
//! MediaBox equal to the rendered visible area; /Rotate, the other page boxes
//! and /Annots are dropped. Pages that aren't rasterized keep their objects
//! untouched.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, RenderCache, RenderSettings};
use image::{GrayImage, ImageBuffer, Luma, Rgb, RgbImage, RgbaImage};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageFormat {
    Png,
    /// quality 1–100
    Jpeg(u8),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adjust {
    /// -1.0 … 1.0, 0 = unchanged
    pub brightness: f32,
    /// -1.0 … 1.0, 0 = unchanged
    pub contrast: f32,
    /// -1.0 (grey) … 1.0, 0 = unchanged
    pub saturation: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FakeScan {
    /// max random tilt per page, degrees
    pub tilt: f32,
    /// 0.0 … 1.0
    pub noise: f32,
    /// 0.0 … 1.0
    pub blur: f32,
    /// paper yellowing, 0.0 … 1.0
    pub yellow: f32,
    pub grayscale: bool,
}

impl Default for FakeScan {
    fn default() -> Self {
        Self { tilt: 0.8, noise: 0.25, blur: 0.2, yellow: 0.15, grayscale: true }
    }
}

/// Largest bitmap rendered for one page; higher DPI requests are lowered.
const MAX_PIXELS: f32 = 40_000_000.0;
/// hayro sizes pixmaps with u16; stay well inside that.
const MAX_SIDE: f32 = 30_000.0;
/// JPEG quality used by the whole-document colour filters.
const FILTER_QUALITY: u8 = 85;
const BLANK_DPI: f32 = 20.0;

/// Render `pages` to image files named `page-01.png` … (zero-padded).
pub fn render_pages(pdf: &[u8], pages: &[usize], dpi: f32, format: ImageFormat) -> Result<Vec<(String, Vec<u8>)>, String> {
    let doc = open_hayro(pdf)?;
    let count = doc.pages().len();
    let wanted = select(pages, count)?;
    let width = count.to_string().len().max(2);
    let mut out = Vec::with_capacity(wanted.len());
    let cache = RenderCache::new();
    for &i in &wanted {
        let (img, _) = render_page(&doc, &cache, i, dpi)?;
        let (bytes, ext) = match format {
            ImageFormat::Png => (encode_png(&img)?, "png"),
            ImageFormat::Jpeg(q) => (encode_jpeg_rgb(&rgba_to_rgb(&img), q)?, "jpg"),
        };
        out.push((format!("page-{:0width$}.{ext}", i + 1), bytes));
    }
    Ok(out)
}

/// Replace `pages` with image-only pages (JPEG at `quality`) — "flatten
/// completely" / "convert to image PDF". Other pages untouched.
pub fn rasterize(pdf: &[u8], pages: &[usize], dpi: f32, quality: u8) -> Result<Vec<u8>, String> {
    let doc = open_hayro(pdf)?;
    let wanted = select(pages, doc.pages().len())?;
    process(pdf, &doc, &wanted, dpi, quality, |_, img| Ok(PageImage::Rgb(rgba_to_rgb(&img))))
}

/// Whole document to greyscale (rasterized at `dpi`).
pub fn grayscale(pdf: &[u8], dpi: f32) -> Result<Vec<u8>, String> {
    let doc = open_hayro(pdf)?;
    let all: Vec<usize> = (0..doc.pages().len()).collect();
    process(pdf, &doc, &all, dpi, FILTER_QUALITY, |_, img| Ok(PageImage::Gray(to_gray(&rgba_to_rgb(&img)))))
}

/// Invert colours (dark mode for printing / reading), rasterized.
pub fn invert(pdf: &[u8], dpi: f32) -> Result<Vec<u8>, String> {
    let doc = open_hayro(pdf)?;
    let all: Vec<usize> = (0..doc.pages().len()).collect();
    process(pdf, &doc, &all, dpi, FILTER_QUALITY, |_, img| {
        let mut rgb = rgba_to_rgb(&img);
        for p in rgb.pixels_mut() {
            p.0 = [255 - p.0[0], 255 - p.0[1], 255 - p.0[2]];
        }
        Ok(PageImage::Rgb(rgb))
    })
}

pub fn adjust_colors(pdf: &[u8], adjust: Adjust, dpi: f32) -> Result<Vec<u8>, String> {
    let fin = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
    let (b, c, s) = (fin(adjust.brightness), fin(adjust.contrast), fin(adjust.saturation));
    let offset = b * 255.0;
    // contrast > 0 steepens towards a hard threshold, < 0 flattens to grey
    let factor = if c >= 0.0 { 1.0 / (1.0 - c * 0.99) } else { 1.0 + c };
    let sat = 1.0 + s;
    let doc = open_hayro(pdf)?;
    let all: Vec<usize> = (0..doc.pages().len()).collect();
    process(pdf, &doc, &all, dpi, FILTER_QUALITY, |_, img| {
        let mut rgb = rgba_to_rgb(&img);
        for p in rgb.pixels_mut() {
            let [r, g, bl] = p.0.map(|v| v as f32);
            let grey = 0.299 * r + 0.587 * g + 0.114 * bl;
            p.0 = [r, g, bl].map(|v| {
                let v = grey + (v - grey) * sat;
                let v = (v - 128.0) * factor + 128.0 + offset;
                v.round().clamp(0.0, 255.0) as u8
            });
        }
        Ok(PageImage::Rgb(rgb))
    })
}

/// Make the document look scanned.
pub fn fake_scan(pdf: &[u8], opts: FakeScan, dpi: f32) -> Result<Vec<u8>, String> {
    let unit = |v: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let tilt = if opts.tilt.is_finite() { opts.tilt.abs().min(45.0) } else { 0.0 };
    let (noise, blur, yellow) = (unit(opts.noise), unit(opts.blur), unit(opts.yellow));
    let doc = open_hayro(pdf)?;
    let all: Vec<usize> = (0..doc.pages().len()).collect();
    process(pdf, &doc, &all, dpi, 75, |page, img| {
        let mut rng = XorShift::new(page as u64);
        let mut rgb = rgba_to_rgb(&img);
        // effect sizes scale with the effective resolution (dpi may be capped)
        let px_per_pt = rgb.width() as f32 / img_points(&img, dpi).0.max(1.0);

        if tilt > 0.0 {
            let angle = rng.signed() * tilt;
            rgb = rotate_white(&rgb, angle.to_radians());
        }
        if yellow > 0.0 {
            let (gm, bm) = (1.0 - 0.05 * yellow, 1.0 - 0.22 * yellow);
            for p in rgb.pixels_mut() {
                p.0[1] = (p.0[1] as f32 * gm) as u8;
                p.0[2] = (p.0[2] as f32 * bm) as u8;
            }
        }
        if blur > 0.0 {
            let sigma = (blur * 1.2 * px_per_pt).max(0.3);
            rgb = image::imageops::blur(&rgb, sigma);
        }
        if noise > 0.0 {
            let amp = noise * 60.0;
            for p in rgb.pixels_mut() {
                // sum of uniforms ≈ gaussian grain, same offset on all channels
                let n = (rng.unit() + rng.unit() + rng.unit() - 1.5) * amp;
                p.0 = p.0.map(|v| (v as f32 + n).round().clamp(0.0, 255.0) as u8);
            }
        }
        Ok(if opts.grayscale { PageImage::Gray(to_gray(&rgb)) } else { PageImage::Rgb(rgb) })
    })
}

/// True redaction of rectangles: affected pages are rasterized at `dpi`
/// with the regions painted solid black, so the covered content is gone.
/// Regions are (page, x0, y0, x1, y1) in 0..1 of the page's visible area,
/// origin top-left.
pub fn redact_regions(pdf: &[u8], regions: &[(usize, f32, f32, f32, f32)], dpi: f32) -> Result<Vec<u8>, String> {
    let doc = open_hayro(pdf)?;
    let count = doc.pages().len();
    let mut by_page: BTreeMap<usize, Vec<[f32; 4]>> = BTreeMap::new();
    for &(page, x0, y0, x1, y1) in regions {
        if page >= count {
            return Err(format!("page {} doesn't exist (document has {count})", page + 1));
        }
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return Err("invalid redaction rectangle".into());
        }
        by_page.entry(page).or_default().push([x0, y0, x1, y1]);
    }
    if by_page.is_empty() {
        return Err("no regions to redact".into());
    }
    let pages: Vec<usize> = by_page.keys().copied().collect();
    process(pdf, &doc, &pages, dpi, 90, |page, img| {
        let mut rgb = rgba_to_rgb(&img);
        let (w, h) = (rgb.width() as f32, rgb.height() as f32);
        for r in by_page.get(&page).into_iter().flatten() {
            let (xa, xb) = (r[0].min(r[2]).clamp(0.0, 1.0), r[0].max(r[2]).clamp(0.0, 1.0));
            let (ya, yb) = (r[1].min(r[3]).clamp(0.0, 1.0), r[1].max(r[3]).clamp(0.0, 1.0));
            // round outwards: never leave a sliver of the covered content
            let (px0, px1) = ((xa * w).floor() as u32, ((xb * w).ceil() as u32).min(rgb.width()));
            let (py0, py1) = ((ya * h).floor() as u32, ((yb * h).ceil() as u32).min(rgb.height()));
            for y in py0..py1 {
                for x in px0..px1 {
                    rgb.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        }
        Ok(PageImage::Rgb(rgb))
    })
}

/// Indices of pages that are (nearly) blank: fewer than `max_ink` (0..1) of
/// pixels darker than near-white at a low-resolution render.
pub fn blank_pages(pdf: &[u8], max_ink: f32) -> Result<Vec<usize>, String> {
    let max_ink = if max_ink.is_finite() { max_ink.clamp(0.0, 1.0) } else { 0.0 };
    let doc = open_hayro(pdf)?;
    let cache = RenderCache::new();
    let mut blank = Vec::new();
    for i in 0..doc.pages().len() {
        let (img, _) = render_page(&doc, &cache, i, BLANK_DPI)?;
        let total = (img.width() as u64 * img.height() as u64).max(1);
        let ink = img.pixels().filter(|p| p.0[..3].iter().any(|&c| c < 230)).count() as u64;
        if ink as f64 / total as f64 <= max_ink as f64 {
            blank.push(i);
        }
    }
    Ok(blank)
}

// ---------------------------------------------------------------- helpers

enum PageImage {
    Rgb(RgbImage),
    Gray(GrayImage),
}

fn open_hayro(pdf: &[u8]) -> Result<Pdf, String> {
    let data = Arc::new(pdf.to_vec());
    match std::panic::catch_unwind(|| Pdf::new(data)) {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(format!("can't read PDF ({e:?})")),
        Err(_) => Err("can't read PDF".into()),
    }
}

/// Sorted, de-duplicated, bounds-checked page list.
fn select(pages: &[usize], count: usize) -> Result<Vec<usize>, String> {
    if pages.is_empty() {
        return Err("no pages selected".into());
    }
    let set: BTreeSet<usize> = pages.iter().copied().collect();
    if let Some(&bad) = set.iter().find(|&&p| p >= count) {
        return Err(format!("page {} doesn't exist (document has {count})", bad + 1));
    }
    Ok(set.into_iter().collect())
}

fn effective_scale(dpi: f32, w: f32, h: f32) -> Result<f32, String> {
    if !dpi.is_finite() || dpi <= 0.0 {
        return Err("invalid DPI".into());
    }
    let max_area = (MAX_PIXELS / (w * h).max(1.0)).sqrt();
    let max_side = MAX_SIDE / w.max(h).max(1.0);
    Ok((dpi / 72.0).min(max_area).min(max_side))
}

/// Render page `i` on white; returns the RGBA bitmap and the page's visible
/// size in points (rotation applied).
fn render_page<'a>(doc: &'a Pdf, cache: &RenderCache<'a>, i: usize, dpi: f32) -> Result<(RgbaImage, (f32, f32)), String> {
    let page = doc.pages().get(i).ok_or_else(|| format!("no page {}", i + 1))?;
    let (w, h) = page.render_dimensions();
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err(format!("page {} has no size", i + 1));
    }
    let scale = effective_scale(dpi, w, h)?;
    let settings = InterpreterSettings::default();
    let pm = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render(
            page,
            cache,
            &settings,
            &RenderSettings { x_scale: scale, y_scale: scale, bg_color: WHITE, ..Default::default() },
        )
    }))
    .map_err(|_| format!("page {} failed to render", i + 1))?;
    let (pw, ph) = (pm.width() as u32, pm.height() as u32);
    if pw == 0 || ph == 0 {
        return Err(format!("page {} is too small to render", i + 1));
    }
    // opaque white background ⇒ premultiplied == straight RGBA
    let img = RgbaImage::from_raw(pw, ph, pm.data_as_u8_slice().to_vec())
        .ok_or_else(|| format!("page {} rendered an invalid bitmap", i + 1))?;
    Ok((img, (w, h)))
}

/// Nominal size in points of a bitmap rendered at `dpi` (only used for
/// relative effect sizes).
fn img_points(img: &RgbaImage, dpi: f32) -> (f32, f32) {
    let s = (dpi / 72.0).max(0.01);
    (img.width() as f32 / s, img.height() as f32 / s)
}

fn rgba_to_rgb(img: &RgbaImage) -> RgbImage {
    let mut out = RgbImage::new(img.width(), img.height());
    for (d, s) in out.pixels_mut().zip(img.pixels()) {
        d.0 = [s.0[0], s.0[1], s.0[2]];
    }
    out
}

fn to_gray(img: &RgbImage) -> GrayImage {
    let mut out: GrayImage = ImageBuffer::new(img.width(), img.height());
    for (d, s) in out.pixels_mut().zip(img.pixels()) {
        let [r, g, b] = s.0.map(|v| v as f32);
        *d = Luma([(0.299 * r + 0.587 * g + 0.114 * b).round().clamp(0.0, 255.0) as u8]);
    }
    out
}

/// Rotate around the centre onto a same-size white canvas (bilinear).
fn rotate_white(img: &RgbImage, rad: f32) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (sin, cos) = rad.sin_cos();
    let mut out = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
    let sample = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            [255.0; 3]
        } else {
            img.get_pixel(x as u32, y as u32).0.map(|v| v as f32)
        }
    };
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            // inverse mapping: destination → source
            let sx = cos * dx + sin * dy + cx - 0.5;
            let sy = -sin * dx + cos * dy + cy - 0.5;
            let (x0, y0) = (sx.floor(), sy.floor());
            let (fx, fy) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let (a, b, c, d) = (sample(x0, y0), sample(x0 + 1, y0), sample(x0, y0 + 1), sample(x0 + 1, y0 + 1));
            let mut px = [0u8; 3];
            for k in 0..3 {
                let top = a[k] + (b[k] - a[k]) * fx;
                let bot = c[k] + (d[k] - c[k]) * fx;
                px[k] = (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(x, y, Rgb(px));
        }
    }
    out
}

struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        // splitmix the seed so neighbouring pages diverge; never zero
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        XorShift((z ^ (z >> 31)) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// 0.0 … 1.0
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// -1.0 … 1.0
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok(buf.into_inner())
}

fn encode_jpeg_rgb(img: &RgbImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100))
        .encode_image(img)
        .map_err(|e| format!("JPEG encoding failed: {e}"))?;
    Ok(buf)
}

fn encode_jpeg_gray(img: &GrayImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100))
        .encode_image(img)
        .map_err(|e| format!("JPEG encoding failed: {e}"))?;
    Ok(buf)
}

/// Render each page in `pages`, transform it with `f`, and splice the
/// results into the document as image-only pages.
fn process<F>(pdf: &[u8], doc: &Pdf, pages: &[usize], dpi: f32, quality: u8, mut f: F) -> Result<Vec<u8>, String>
where
    F: FnMut(usize, RgbaImage) -> Result<PageImage, String>,
{
    if pages.is_empty() {
        return Err("document has no pages".into());
    }
    let mut ldoc = Document::load_mem(pdf).map_err(|e| format!("can't read PDF ({e})"))?;
    if ldoc.is_encrypted() {
        ldoc.decrypt("").map_err(|_| "PDF is password-protected".to_string())?;
    }
    let page_ids: Vec<ObjectId> = ldoc.get_pages().into_values().collect();
    if page_ids.len() != doc.pages().len() {
        return Err("PDF page tree is inconsistent; try repairing it first".into());
    }
    let cache = RenderCache::new();
    for &i in pages {
        let (img, (w, h)) = render_page(doc, &cache, i, dpi)?;
        let (jpeg, cs, iw, ih) = match f(i, img)? {
            PageImage::Rgb(rgb) => (encode_jpeg_rgb(&rgb, quality)?, "DeviceRGB", rgb.width(), rgb.height()),
            PageImage::Gray(g) => (encode_jpeg_gray(&g, quality)?, "DeviceGray", g.width(), g.height()),
        };
        replace_page(&mut ldoc, page_ids[i], jpeg, cs, (iw, ih), (w, h))?;
    }
    ldoc.prune_objects();
    let mut buf = Vec::new();
    ldoc.save_to(&mut buf).map_err(|e| format!("can't write PDF ({e})"))?;
    Ok(buf)
}

fn replace_page(
    doc: &mut Document,
    page: ObjectId,
    jpeg: Vec<u8>,
    color_space: &str,
    (iw, ih): (u32, u32),
    (w, h): (f32, f32),
) -> Result<(), String> {
    let image = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => iw as i64,
            "Height" => ih as i64,
            "ColorSpace" => color_space,
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    )
    .with_compression(false);
    let image_id = doc.add_object(image);
    let ops = format!("q {} 0 0 {} 0 0 cm /Im0 Do Q", fmt_num(w), fmt_num(h));
    let mut content = Stream::new(dictionary! {}, ops.into_bytes());
    let _ = content.compress();
    let content_id = doc.add_object(content);

    let rotate_inherited = inherited(doc, page, b"Rotate");
    let crop_inherited = inherited(doc, page, b"CropBox");
    let media_box: Vec<Object> = vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)];
    let dict = doc.get_dictionary_mut(page).map_err(|e| e.to_string())?;
    dict.set("Contents", content_id);
    dict.set("Resources", dictionary! { "XObject" => dictionary! { "Im0" => image_id } });
    dict.set("MediaBox", media_box.clone());
    for k in [&b"CropBox"[..], b"BleedBox", b"TrimBox", b"ArtBox", b"Rotate", b"Annots", b"Thumb", b"Group"] {
        dict.remove(k);
    }
    // inheritable keys would otherwise come back from the page tree
    if rotate_inherited {
        dict.set("Rotate", 0);
    }
    if crop_inherited {
        dict.set("CropBox", media_box);
    }
    Ok(())
}

/// Whether `key` is set on an ancestor of `page` in the page tree.
fn inherited(doc: &Document, page: ObjectId, key: &[u8]) -> bool {
    let mut parent = doc
        .get_dictionary(page)
        .ok()
        .and_then(|d| d.get(b"Parent").ok())
        .and_then(|p| p.as_reference().ok());
    let mut guard = 0;
    while let Some(pid) = parent {
        guard += 1;
        if guard > 64 {
            return false;
        }
        let Ok(d) = doc.get_dictionary(pid) else { return false };
        if d.has(key) {
            return true;
        }
        parent = d.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
    }
    false
}

fn fmt_num(v: f32) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};

    /// n-page PDF: each page has "<tag> page i" text and a filled black
    /// rectangle at (72,72)-(272,272); pages listed in `empty` have no
    /// content at all. MediaBox/Resources inherited from the Pages node.
    fn sample(n: usize, tag: &str, empty: &[usize]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        for i in 0..n {
            let operations = if empty.contains(&i) {
                vec![]
            } else {
                vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(format!("{tag} page {}", i + 1))]),
                    Operation::new("ET", vec![]),
                    Operation::new("rg", vec![0.into(), 0.into(), 0.into()]),
                    Operation::new("re", vec![72.into(), 72.into(), 200.into(), 200.into()]),
                    Operation::new("f", vec![]),
                ]
            };
            let content = Content { operations };
            let c = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            kids.push(
                doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c })
                    .into(),
            );
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => kids, "Count" => n as i64,
                "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn render(pdf: &[u8], page: usize, dpi: f32) -> RgbaImage {
        let doc = open_hayro(pdf).unwrap();
        let cache = RenderCache::new();
        render_page(&doc, &cache, page, dpi).unwrap().0
    }

    /// pixel at a page position given in points from the top-left
    fn px(img: &RgbaImage, x_pt: f32, y_pt: f32) -> [u8; 3] {
        let s = img.width() as f32 / 612.0;
        let p = img.get_pixel((x_pt * s) as u32, (y_pt * s) as u32).0;
        [p[0], p[1], p[2]]
    }

    // the sample's rectangle spans x 72..272, y (top-left) 520..720
    const RECT: (f32, f32) = (172.0, 620.0);
    const WHITE_SPOT: (f32, f32) = (450.0, 400.0);

    fn text_of(pdf: &[u8], page: u32) -> String {
        let doc = Document::load_mem(pdf).unwrap();
        doc.extract_text(&[page]).unwrap_or_default().trim().to_string()
    }

    fn page_count(pdf: &[u8]) -> usize {
        Document::load_mem(pdf).unwrap().get_pages().len()
    }

    #[test]
    fn render_pages_names_and_decodes() {
        let pdf = sample(3, "A", &[]);
        let files = render_pages(&pdf, &[2, 0], 36.0, ImageFormat::Png).unwrap();
        let names: Vec<_> = files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, ["page-01.png", "page-03.png"]);
        let img = image::load_from_memory(&files[0].1).unwrap();
        assert_eq!((img.width(), img.height()), (306, 396));
        let jpg = render_pages(&pdf, &[1], 36.0, ImageFormat::Jpeg(80)).unwrap();
        assert_eq!(jpg[0].0, "page-02.jpg");
        assert!(image::load_from_memory(&jpg[0].1).is_ok());
        // padding follows the page count
        let many = sample(120, "B", &[]);
        let f = render_pages(&many, &[4], 10.0, ImageFormat::Png).unwrap();
        assert_eq!(f[0].0, "page-005.png");
        assert!(render_pages(&pdf, &[3], 36.0, ImageFormat::Png).is_err());
        assert!(render_pages(&pdf, &[], 36.0, ImageFormat::Png).is_err());
        assert!(render_pages(b"not a pdf", &[0], 36.0, ImageFormat::Png).is_err());
    }

    #[test]
    fn rasterize_replaces_only_selected_pages() {
        let pdf = sample(3, "A", &[]);
        let out = rasterize(&pdf, &[1], 50.0, 80).unwrap();
        assert_eq!(page_count(&out), 3);
        assert!(text_of(&out, 1).contains("A page 1"));
        assert!(text_of(&out, 2).is_empty());
        assert!(text_of(&out, 3).contains("A page 3"));
        let doc = Document::load_mem(&out).unwrap();
        let pid = doc.get_pages()[&2];
        assert_eq!(doc.get_page_images(pid).unwrap().len(), 1);
        let d = doc.get_dictionary(pid).unwrap();
        assert!(!d.has(b"Rotate") && !d.has(b"Annots"));
        // the image page still looks like the original
        let r = render(&out, 1, 36.0);
        assert!(px(&r, RECT.0, RECT.1).iter().all(|&c| c < 60));
        assert!(px(&r, WHITE_SPOT.0, WHITE_SPOT.1).iter().all(|&c| c > 220));
        assert!(rasterize(&pdf, &[5], 50.0, 80).is_err());
    }

    #[test]
    fn rasterize_handles_rotated_pages() {
        let pdf = sample(1, "R", &[]);
        let mut doc = Document::load_mem(&pdf).unwrap();
        let pid = doc.get_pages()[&1];
        doc.get_dictionary_mut(pid).unwrap().set("Rotate", 90);
        let mut rotated = Vec::new();
        doc.save_to(&mut rotated).unwrap();
        let out = rasterize(&rotated, &[0], 30.0, 80).unwrap();
        let img = render(&out, 0, 30.0);
        assert!(img.width() > img.height(), "landscape after baking rotation");
    }

    #[test]
    fn grayscale_invert_adjust_change_pixels() {
        let pdf = sample(2, "C", &[]);

        let g = grayscale(&pdf, 40.0).unwrap();
        assert_eq!(page_count(&g), 2);
        assert!(text_of(&g, 1).is_empty());
        let r = render(&g, 0, 36.0);
        assert!(px(&r, RECT.0, RECT.1).iter().all(|&c| c < 60));

        let inv = invert(&pdf, 40.0).unwrap();
        let r = render(&inv, 0, 36.0);
        assert!(px(&r, WHITE_SPOT.0, WHITE_SPOT.1).iter().all(|&c| c < 40), "white became dark");
        assert!(px(&r, RECT.0, RECT.1).iter().all(|&c| c > 215), "black became light");

        let dark = adjust_colors(&pdf, Adjust { brightness: -0.5, contrast: 0.0, saturation: 0.0 }, 40.0).unwrap();
        let r = render(&dark, 1, 36.0);
        let w = px(&r, WHITE_SPOT.0, WHITE_SPOT.1);
        assert!(w.iter().all(|&c| (110..=150).contains(&c)), "{w:?}");

        let flat = adjust_colors(&pdf, Adjust { brightness: 0.0, contrast: -1.0, saturation: 0.0 }, 40.0).unwrap();
        let r = render(&flat, 0, 36.0);
        assert!(px(&r, RECT.0, RECT.1).iter().all(|&c| (110..=150).contains(&c)));
    }

    #[test]
    fn fake_scan_is_deterministic_and_changes_pixels() {
        let pdf = sample(2, "S", &[]);
        let opts = FakeScan { grayscale: false, yellow: 1.0, ..FakeScan::default() };
        let a = fake_scan(&pdf, opts, 40.0).unwrap();
        let b = fake_scan(&pdf, opts, 40.0).unwrap();
        assert_eq!(page_count(&a), 2);
        assert!(text_of(&a, 1).is_empty());
        let (ra, rb) = (render(&a, 0, 36.0), render(&b, 0, 36.0));
        assert_eq!(ra.as_raw(), rb.as_raw());
        // yellow paper: blue clearly below red away from the content
        let p = px(&ra, WHITE_SPOT.0, WHITE_SPOT.1);
        assert!(p[2] as i32 + 25 < p[0] as i32, "{p:?}");
        let grey = fake_scan(&pdf, FakeScan::default(), 40.0).unwrap();
        let r = render(&grey, 1, 36.0);
        let p = px(&r, WHITE_SPOT.0, WHITE_SPOT.1);
        assert!(p[0].abs_diff(p[2]) < 12, "{p:?}");
        assert!(px(&r, RECT.0, RECT.1).iter().all(|&c| c < 110));
    }

    #[test]
    fn redact_paints_black_and_removes_text() {
        let pdf = sample(2, "SECRET", &[]);
        // text baseline at y=700 → top-left y ≈ 92/792
        let out = redact_regions(&pdf, &[(0, 0.5, 0.4, 0.9, 0.6), (0, 0.1, 0.08, 0.6, 0.13)], 40.0).unwrap();
        assert_eq!(page_count(&out), 2);
        assert!(text_of(&out, 1).is_empty());
        assert!(text_of(&out, 2).contains("SECRET page 2"));
        let r = render(&out, 0, 36.0);
        assert!(px(&r, WHITE_SPOT.0, WHITE_SPOT.1).iter().all(|&c| c < 30), "region black");
        assert!(px(&r, 100.0, 300.0).iter().all(|&c| c > 220), "outside untouched");
        assert!(redact_regions(&pdf, &[(4, 0.0, 0.0, 1.0, 1.0)], 40.0).is_err());
        assert!(redact_regions(&pdf, &[], 40.0).is_err());
    }

    #[test]
    fn blank_pages_detects_empty_page() {
        let pdf = sample(3, "B", &[1]);
        assert_eq!(blank_pages(&pdf, 0.001).unwrap(), vec![1]);
        assert_eq!(blank_pages(&pdf, 1.0).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn untouched_pages_keep_their_objects() {
        let pdf = sample(2, "K", &[]);
        let before = Document::load_mem(&pdf).unwrap();
        let p1 = before.get_pages()[&1];
        let out = rasterize(&pdf, &[1], 30.0, 70).unwrap();
        let after = Document::load_mem(&out).unwrap();
        assert_eq!(after.get_pages()[&1], p1);
        assert_eq!(
            format!("{:?}", before.get_object(p1).unwrap()),
            format!("{:?}", after.get_object(p1).unwrap())
        );
    }

    #[test]
    fn huge_dpi_is_capped() {
        let s = effective_scale(100_000.0, 612.0, 792.0).unwrap();
        assert!(612.0 * s * 792.0 * s <= MAX_PIXELS * 1.01);
        assert!(effective_scale(f32::NAN, 1.0, 1.0).is_err());
        assert!(effective_scale(0.0, 1.0, 1.0).is_err());
    }
}
