//! Split several photos scanned together on one sheet into separate,
//! straightened images (Stirling-PDF's "extract-image-scans"), natively
//! with `imageproc`:
//!
//!  1. work on a copy downscaled to ≤ 1200 px (speed, and it smooths noise)
//!  2. background colour = median of the border pixels; a pixel is
//!     foreground when any channel differs from it by more than
//!     `tolerance`
//!  3. morphological close (bridges light areas inside photos), connected
//!     components; blobs smaller than `min_contour_area` are noise
//!  4. convex hull → minimum-area rectangle per blob (rotating calipers);
//!     rectangles smaller than `min_area` are dropped
//!  5. each photo is resampled (bilinear) from the full-resolution image in
//!     its own rotated frame — crop and deskew in one step — and trimmed by
//!     `border` pixels so no background sliver remains. Tilts below
//!     `angle_threshold` degrees are left as they are (axis-aligned crop).
//!
//! Works on image files and on rendered PDF pages. Limits: photos must be
//! separated by background (≳ 2 mm at 300 dpi) and differ from it; a photo
//! with a large paper-coloured edge region can be cropped short; tilt is
//! corrected up to ±45°.

use image::{DynamicImage, GrayImage, Luma, Rgb, RgbImage};
use imageproc::distance_transform::Norm;
use imageproc::point::Point;
use imageproc::region_labelling::{connected_components, Connectivity};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// tilts smaller than this (degrees) aren't straightened
    pub angle_threshold: f32,
    /// colour distance (0–255) still counted as background
    pub tolerance: u8,
    /// smallest photo kept, in pixels² of the input
    pub min_area: u32,
    /// smallest blob considered at all (noise filter), pixels²
    pub min_contour_area: u32,
    /// pixels trimmed from every edge of each photo
    pub border: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            angle_threshold: 1.0,
            tolerance: 30,
            min_area: 8000,
            min_contour_area: 500,
            border: 1,
        }
    }
}

/// Analysis resolution (long side).
const WORK_SIDE: f32 = 1200.0;
/// Render resolution for PDF pages.
const PAGE_DPI: f32 = 300.0;
const JPEG_QUALITY: u8 = 92;
/// Largest input accepted (pixels), like the renderer's cap.
const MAX_PIXELS: u64 = 120_000_000;

/// Detected photo: centre, size and tilt (radians, (-π/4, π/4]) in input
/// pixel coordinates.
#[derive(Debug, Clone, Copy)]
struct Found {
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
    angle: f32,
}

/// The separate photos in `img`, top-to-bottom then left-to-right.
pub fn split_image(img: &DynamicImage, opts: Options) -> Result<Vec<RgbImage>, String> {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    if w < 8 || h < 8 {
        return Err("the image is too small".into());
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err("the image is too large (over 120 megapixels)".into());
    }
    let scale = (WORK_SIDE / w.max(h) as f32).min(1.0);
    let small = if scale < 1.0 {
        image::imageops::resize(
            &rgb,
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        rgb.clone()
    };
    let bg = background(&small);
    let mask = foreground(&small, bg, opts.tolerance);
    let radius = ((small.width().max(small.height()) as f32 / 150.0).round() as u8).max(1);
    let mask = imageproc::morphology::close(&mask, Norm::LInf, radius);
    let found = find_rects(&mask, scale, opts);
    let trim = opts.border as f32 + 0.5 / scale;
    Ok(found
        .into_iter()
        .filter_map(|f| extract(&rgb, f, trim, bg))
        .collect())
}

/// Photos from an image file, encoded like the input (JPEG stays JPEG,
/// everything else becomes PNG), named `photo-01.png` ….
pub fn split_image_file(bytes: &[u8], opts: Options) -> Result<Vec<(String, Vec<u8>)>, String> {
    let fmt = image::guess_format(bytes).ok();
    let img = image::load_from_memory(bytes).map_err(|e| format!("can't read the image ({e})"))?;
    let photos = split_image(&img, opts)?;
    if photos.is_empty() {
        return Err("no photos found — try a lower tolerance or minimum area".into());
    }
    let jpeg = fmt == Some(image::ImageFormat::Jpeg);
    photos
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if jpeg {
                super::raster::encode_jpeg_rgb(p, JPEG_QUALITY)
                    .map(|b| (format!("photo-{:02}.jpg", i + 1), b))
            } else {
                encode_png(p).map(|b| (format!("photo-{:02}.png", i + 1), b))
            }
        })
        .collect()
}

/// Photos on the given PDF pages (rendered at 300 dpi), as JPEGs named
/// `page-03-photo-01.jpg` ….
pub fn split_pdf(
    pdf: &[u8],
    pages: &[usize],
    opts: Options,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let doc = super::raster::open_hayro(pdf)?;
    let count = doc.pages().len();
    let wanted = super::raster::select(pages, count)?;
    let width = count.to_string().len().max(2);
    let cache = hayro::RenderCache::new();
    let mut out = Vec::new();
    for &i in &wanted {
        let (img, _) = super::raster::render_page(&doc, &cache, i, PAGE_DPI)?;
        let photos = split_image(&DynamicImage::ImageRgba8(img), opts)?;
        for (k, p) in photos.iter().enumerate() {
            let bytes = super::raster::encode_jpeg_rgb(p, JPEG_QUALITY)?;
            out.push((
                format!("page-{:0width$}-photo-{:02}.jpg", i + 1, k + 1),
                bytes,
            ));
        }
    }
    if out.is_empty() {
        return Err("no photos found — try a lower tolerance or minimum area".into());
    }
    Ok(out)
}

fn encode_png(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok(buf.into_inner())
}

/// Median colour of the outermost 2-pixel ring.
fn background(img: &RgbImage) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let mut ch: [Vec<u8>; 3] = Default::default();
    for y in 0..h {
        for x in 0..w {
            if x < 2 || y < 2 || x + 2 >= w || y + 2 >= h {
                let p = img.get_pixel(x, y).0;
                for c in 0..3 {
                    ch[c].push(p[c]);
                }
            }
        }
    }
    let mut out = [255u8; 3];
    for c in 0..3 {
        if !ch[c].is_empty() {
            ch[c].sort_unstable();
            out[c] = ch[c][ch[c].len() / 2];
        }
    }
    out
}

fn foreground(img: &RgbImage, bg: [u8; 3], tolerance: u8) -> GrayImage {
    let mut mask = GrayImage::new(img.width(), img.height());
    for (m, p) in mask.pixels_mut().zip(img.pixels()) {
        let d = (0..3)
            .map(|c| (p.0[c] as i16 - bg[c] as i16).unsigned_abs())
            .max()
            .unwrap_or(0);
        *m = Luma([if d > tolerance as u16 { 255 } else { 0 }]);
    }
    mask
}

/// Minimum-area rectangles of the mask's blobs, in input pixels.
fn find_rects(mask: &GrayImage, scale: f32, opts: Options) -> Vec<Found> {
    let labels = connected_components(mask, Connectivity::Eight, Luma([0u8]));
    let n = labels.pixels().map(|p| p.0[0]).max().unwrap_or(0) as usize;
    if n == 0 {
        return vec![];
    }
    // per label: pixel count and the outer corners of each row's extent
    let mut count = vec![0u64; n + 1];
    let mut rows: Vec<Vec<(u32, u32, u32)>> = vec![Vec::new(); n + 1];
    for y in 0..labels.height() {
        let mut x = 0;
        while x < labels.width() {
            let l = labels.get_pixel(x, y).0[0] as usize;
            if l == 0 {
                x += 1;
                continue;
            }
            let start = x;
            while x < labels.width() && labels.get_pixel(x, y).0[0] as usize == l {
                x += 1;
            }
            count[l] += (x - start) as u64;
            match rows[l].last_mut() {
                Some(r) if r.0 == y => {
                    r.1 = r.1.min(start);
                    r.2 = r.2.max(x);
                }
                _ => rows[l].push((y, start, x)),
            }
        }
    }
    let area_scale = 1.0 / (scale as f64 * scale as f64);
    let mut out = Vec::new();
    for l in 1..=n {
        if (count[l] as f64 * area_scale) < opts.min_contour_area as f64 {
            continue;
        }
        let mut pts = Vec::with_capacity(rows[l].len() * 4);
        for &(y, x0, x1) in &rows[l] {
            let (y, x0, x1) = (y as i32, x0 as i32, x1 as i32);
            pts.extend([
                Point::new(x0, y),
                Point::new(x0, y + 1),
                Point::new(x1, y),
                Point::new(x1, y + 1),
            ]);
        }
        let hull = imageproc::geometry::convex_hull(pts);
        let Some(mut f) = min_rect(&hull) else {
            continue;
        };
        if f.angle.to_degrees().abs() < opts.angle_threshold.max(0.0) {
            // axis-aligned bounding box instead
            let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for p in &hull {
                x0 = x0.min(p.x);
                y0 = y0.min(p.y);
                x1 = x1.max(p.x);
                y1 = y1.max(p.y);
            }
            f = Found {
                cx: (x0 + x1) as f32 / 2.0,
                cy: (y0 + y1) as f32 / 2.0,
                w: (x1 - x0) as f32,
                h: (y1 - y0) as f32,
                angle: 0.0,
            };
        }
        let f = Found {
            cx: f.cx / scale,
            cy: f.cy / scale,
            w: f.w / scale,
            h: f.h / scale,
            angle: f.angle,
        };
        if (f.w as f64 * f.h as f64) < opts.min_area as f64 {
            continue;
        }
        out.push(f);
    }
    // reading order: rows (centres within half a photo height), then x
    out.sort_by(|a, b| a.cy.total_cmp(&b.cy));
    let mut sorted: Vec<Found> = Vec::with_capacity(out.len());
    let mut row: Vec<Found> = Vec::new();
    for f in out {
        if let Some(first) = row.first() {
            if (f.cy - first.cy).abs() > first.h.min(f.h) / 2.0 {
                row.sort_by(|a, b| a.cx.total_cmp(&b.cx));
                sorted.append(&mut row);
            }
        }
        row.push(f);
    }
    row.sort_by(|a, b| a.cx.total_cmp(&b.cx));
    sorted.append(&mut row);
    sorted
}

/// Rotating calipers over the hull edges.
fn min_rect(hull: &[Point<i32>]) -> Option<Found> {
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f64, Found)> = None;
    for i in 0..hull.len() {
        let (p, q) = (hull[i], hull[(i + 1) % hull.len()]);
        let (dx, dy) = ((q.x - p.x) as f64, (q.y - p.y) as f64);
        if dx == 0.0 && dy == 0.0 {
            continue;
        }
        let a = dy.atan2(dx);
        let (c, s) = (a.cos(), a.sin());
        let (mut u0, mut u1, mut v0, mut v1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for h in hull {
            let (x, y) = (h.x as f64, h.y as f64);
            let u = x * c + y * s;
            let v = -x * s + y * c;
            u0 = u0.min(u);
            u1 = u1.max(u);
            v0 = v0.min(v);
            v1 = v1.max(v);
        }
        let area = (u1 - u0) * (v1 - v0);
        if best.as_ref().is_none_or(|(b, _)| area < *b) {
            let (mu, mv) = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
            best = Some((
                area,
                Found {
                    cx: (mu * c - mv * s) as f32,
                    cy: (mu * s + mv * c) as f32,
                    w: (u1 - u0) as f32,
                    h: (v1 - v0) as f32,
                    angle: a as f32,
                },
            ));
        }
    }
    let (_, mut f) = best?;
    // normalise the tilt into (-45°, 45°], swapping sides as the axis turns
    let quarter = std::f32::consts::FRAC_PI_2;
    while f.angle > quarter / 2.0 + 1e-6 {
        f.angle -= quarter;
        std::mem::swap(&mut f.w, &mut f.h);
    }
    while f.angle <= -quarter / 2.0 + 1e-6 {
        f.angle += quarter;
        std::mem::swap(&mut f.w, &mut f.h);
    }
    Some(f)
}

/// Resample one photo upright from the full-resolution image.
fn extract(src: &RgbImage, f: Found, trim: f32, bg: [u8; 3]) -> Option<RgbImage> {
    let ow = (f.w - 2.0 * trim).round();
    let oh = (f.h - 2.0 * trim).round();
    if !(ow >= 1.0 && oh >= 1.0) || ow * oh > MAX_PIXELS as f32 {
        return None;
    }
    let (ow, oh) = (ow as u32, oh as u32);
    let (c, s) = (f.angle.cos(), f.angle.sin());
    let mut out = RgbImage::new(ow, oh);
    for v in 0..oh {
        for u in 0..ow {
            let lx = u as f32 + 0.5 - ow as f32 / 2.0;
            let ly = v as f32 + 0.5 - oh as f32 / 2.0;
            let x = f.cx + lx * c - ly * s;
            let y = f.cy + lx * s + ly * c;
            out.put_pixel(u, v, bilinear(src, x - 0.5, y - 0.5, bg));
        }
    }
    Some(out)
}

fn bilinear(img: &RgbImage, x: f32, y: f32, bg: [u8; 3]) -> Rgb<u8> {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let px = |xx: i64, yy: i64| -> [f32; 3] {
        if xx < 0 || yy < 0 || xx >= w || yy >= h {
            bg.map(|v| v as f32)
        } else {
            img.get_pixel(xx as u32, yy as u32).0.map(|v| v as f32)
        }
    };
    let (a, b, c, d) = (
        px(x0, y0),
        px(x0 + 1, y0),
        px(x0, y0 + 1),
        px(x0 + 1, y0 + 1),
    );
    let mut out = [0u8; 3];
    for i in 0..3 {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bot = c[i] + (d[i] - c[i]) * fx;
        out[i] = (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// White "scanner bed" with noisy photos: (cx, cy, w, h, degrees).
    fn sheet(w: u32, h: u32, photos: &[(f32, f32, f32, f32, f32)]) -> RgbImage {
        let mut img = RgbImage::from_pixel(w, h, Rgb([250, 250, 248]));
        let mut seed = 0x2545F491u32;
        for &(cx, cy, pw, ph, deg) in photos {
            let (c, s) = (deg.to_radians().cos(), deg.to_radians().sin());
            for y in 0..h {
                for x in 0..w {
                    let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                    // into the photo's frame
                    let u = dx * c + dy * s;
                    let v = -dx * s + dy * c;
                    if u.abs() <= pw / 2.0 && v.abs() <= ph / 2.0 {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        let base = ((u + pw / 2.0) / pw * 120.0) as u32;
                        let n = seed % 60;
                        img.put_pixel(
                            x,
                            y,
                            Rgb([
                                (base + n) as u8,
                                (40 + n * 2) as u8,
                                (180 - base / 2 + n / 2) as u8,
                            ]),
                        );
                    }
                }
            }
        }
        // a speck of dust: must be ignored
        for y in 20..24 {
            for x in 20..24 {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        img
    }

    const PHOTOS: [(f32, f32, f32, f32, f32); 3] = [
        (330.0, 260.0, 400.0, 300.0, 0.0),
        (1050.0, 300.0, 360.0, 260.0, 8.0),
        (600.0, 850.0, 300.0, 380.0, -12.0),
    ];

    fn close(a: u32, b: f32) -> bool {
        (a as f32 - b).abs() <= b * 0.04 + 4.0
    }

    #[test]
    fn finds_and_straightens_photos() {
        let img = DynamicImage::ImageRgb8(sheet(1500, 1150, &PHOTOS));
        let photos = split_image(&img, Options::default()).unwrap();
        assert_eq!(
            photos.len(),
            3,
            "{:?}",
            photos.iter().map(|p| p.dimensions()).collect::<Vec<_>>()
        );
        // reading order: the two top photos left→right, then the bottom one
        for (p, &(_, _, w, h, _)) in photos.iter().zip(&PHOTOS) {
            let (pw, ph) = p.dimensions();
            assert!(close(pw, w) && close(ph, h), "got {pw}×{ph}, want {w}×{h}");
            // no paper left at the corners after deskewing
            for (x, y) in [(1, 1), (pw - 2, 1), (1, ph - 2), (pw - 2, ph - 2)] {
                let px = p.get_pixel(x, y).0;
                assert!(
                    px.iter().any(|&c| c < 225),
                    "background at corner {x},{y}: {px:?}"
                );
            }
        }
    }

    #[test]
    fn min_area_filters_and_files_encode() {
        let img = sheet(1500, 1150, &PHOTOS);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let files = split_image_file(buf.get_ref(), Options::default()).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].0, "photo-01.png");
        let opts = Options {
            min_area: 118_000,
            ..Options::default()
        };
        assert_eq!(
            split_image(&DynamicImage::ImageRgb8(img.clone()), opts)
                .unwrap()
                .len(),
            1
        );
        let blank = RgbImage::from_pixel(400, 300, Rgb([255, 255, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        blank.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        assert!(split_image_file(buf.get_ref(), Options::default()).is_err());
    }

    #[test]
    fn splits_photos_on_pdf_pages() {
        let img = sheet(1500, 1150, &PHOTOS);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let pdf = super::super::layout::images_to_pdf(&[buf.into_inner()], None, 0.0).unwrap();
        let files = split_pdf(&pdf, &[0], Options::default()).unwrap();
        assert_eq!(
            files.len(),
            3,
            "{:?}",
            files.iter().map(|f| &f.0).collect::<Vec<_>>()
        );
        assert_eq!(files[0].0, "page-01-photo-01.jpg");
    }
}
