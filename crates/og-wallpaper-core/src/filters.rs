//! Turns an arbitrary photo/artwork into either a line-art or a 2-color
//! posterized image, in the caller's chosen colors — this is what lets a
//! theme "match the system" even though the source photo obviously
//! doesn't: the filter throws away the original palette entirely and
//! re-draws in whatever colors it's given.

use image::{DynamicImage, Rgba, RgbaImage};

/// Sobel edge detection, thresholded to a hard line/no-line decision —
/// not an anti-aliased "artsy" edge image, a clean line-art silhouette
/// closer to what og-wallpaper's original hand-drawn Tokyo scene looked
/// like, since that's the look being matched.
pub fn line_art(src: &DynamicImage, line: Rgba<u8>, background: Rgba<u8>, threshold: u32) -> RgbaImage {
    let gray = src.to_luma8();
    let (w, h) = gray.dimensions();
    let mut out = RgbaImage::from_pixel(w, h, background);

    if w < 3 || h < 3 {
        return out;
    }

    // clamp so gray.get_pixel never runs off the edge for x,y in 1..w-1 — no
    // separate wraparound/border handling needed for a 3x3 kernel.
    let px = |x: i64, y: i64| -> i32 { gray.get_pixel(x as u32, y as u32).0[0] as i32 };

    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let (xi, yi) = (x as i64, y as i64);
            let gx = -px(xi - 1, yi - 1) - 2 * px(xi - 1, yi) - px(xi - 1, yi + 1)
                + px(xi + 1, yi - 1) + 2 * px(xi + 1, yi) + px(xi + 1, yi + 1);
            let gy = -px(xi - 1, yi - 1) - 2 * px(xi, yi - 1) - px(xi + 1, yi - 1)
                + px(xi - 1, yi + 1) + 2 * px(xi, yi + 1) + px(xi + 1, yi + 1);
            let mag = ((gx * gx + gy * gy) as f64).sqrt() as u32;
            if mag > threshold {
                out.put_pixel(x, y, line);
            }
        }
    }
    out
}

/// Luminance-threshold posterize down to exactly two colors — silhouette
/// style, not a smooth gradient/dither.
pub fn two_color(src: &DynamicImage, dark: Rgba<u8>, light: Rgba<u8>, threshold: u8) -> RgbaImage {
    let gray = src.to_luma8();
    let (w, h) = gray.dimensions();
    let mut out = RgbaImage::new(w, h);
    for (x, y, p) in gray.enumerate_pixels() {
        out.put_pixel(x, y, if p.0[0] < threshold { dark } else { light });
    }
    out
}

/// Standard ray-casting point-in-polygon test — shared by
/// og-wallpaper-studio (rasterizing a traced Polygon into the "claimed"
/// mask so a later magic-wand click won't cross back into it) and
/// og-wallpaper's renderer (Rain/Twinkle rejection-sampling within a
/// Polygon region).
pub fn point_in_polygon(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let n = points.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Flood-fill from `(start_x, start_y)` across pixels within `threshold`
/// color-distance of the clicked pixel — a magic-wand selection, for
/// marking something like "the starry sky" that's one visually-coherent
/// area but not remotely a box or a shape worth tracing by hand.
///
/// Runs against whatever image the caller passes — og-wallpaper-studio
/// hands it the *filtered* (displayed) image, not the original photo, so
/// a click lands on what the user actually sees on screen.
///
/// `exclude` (full-image-sized, same dimensions as `img`, nonzero =
/// excluded), if given, is treated as an extra boundary the fill won't
/// cross — pixels already claimed by an earlier region stay that
/// region's, instead of also getting swept into whatever's selected
/// next. Pass `None` for a plain unconstrained fill.
///
/// Returns the mask cropped to its own tight bounding box (not the full
/// image — a scattered selection over a small area of a 6000px photo
/// shouldn't cost a full-image-sized mask buffer) as (mask bytes,
/// mask width, mask height, bbox x, bbox y).
pub fn magic_wand(img: &RgbaImage, start_x: u32, start_y: u32, threshold: u8, exclude: Option<&[u8]>) -> (Vec<u8>, u32, u32, u32, u32) {
    let (w, h) = img.dimensions();
    if start_x >= w || start_y >= h {
        return (Vec::new(), 0, 0, 0, 0);
    }
    if exclude.is_some_and(|e| e[(start_y * w + start_x) as usize] != 0) {
        // Clicked directly on already-claimed ground — nothing to fill.
        return (Vec::new(), 0, 0, 0, 0);
    }
    let target = *img.get_pixel(start_x, start_y);
    let thresh2 = (threshold as i32) * (threshold as i32) * 3;
    let dist2 = |p: Rgba<u8>| -> i32 {
        let dr = p.0[0] as i32 - target.0[0] as i32;
        let dg = p.0[1] as i32 - target.0[1] as i32;
        let db = p.0[2] as i32 - target.0[2] as i32;
        dr * dr + dg * dg + db * db
    };

    let mut visited = vec![false; (w * h) as usize];
    let mut filled: Vec<(u32, u32)> = Vec::new();
    let mut stack = vec![(start_x, start_y)];
    let (mut minx, mut miny, mut maxx, mut maxy) = (start_x, start_y, start_x, start_y);

    while let Some((x, y)) = stack.pop() {
        let idx = (y * w + x) as usize;
        if visited[idx] {
            continue;
        }
        visited[idx] = true;
        if exclude.is_some_and(|e| e[idx] != 0) {
            continue;
        }
        if dist2(*img.get_pixel(x, y)) > thresh2 {
            continue;
        }
        filled.push((x, y));
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x);
        maxy = maxy.max(y);
        // Checked against `visited` before pushing, not just after
        // popping — otherwise the stack can balloon to several times the
        // image's pixel count before it starts draining, since every
        // pixel gets pushed once per unvisited neighbor.
        if x > 0 && !visited[idx - 1] {
            stack.push((x - 1, y));
        }
        if x + 1 < w && !visited[idx + 1] {
            stack.push((x + 1, y));
        }
        if y > 0 && !visited[idx - w as usize] {
            stack.push((x, y - 1));
        }
        if y + 1 < h && !visited[idx + w as usize] {
            stack.push((x, y + 1));
        }
    }

    let (mw, mh) = (maxx - minx + 1, maxy - miny + 1);
    let mut mask = vec![0u8; (mw * mh) as usize];
    for (x, y) in filled {
        mask[((y - miny) * mw + (x - minx)) as usize] = 255;
    }
    (mask, mw, mh, minx, miny)
}

/// Exact-match pixel remap from the two colors a `line_art`/`two_color`
/// image was baked in (`old_line`/`old_bg`) to whatever the *current*
/// theme's colors are — see `Manifest::base_colors`' doc comment for why
/// this exists. Only meaningful for those two filters' hard-edged,
/// exactly-two-color output; anything not matching either color (there
/// shouldn't be anything, for real line-art/two-color output) is left
/// alone rather than guessed at.
pub fn remap_colors(img: &mut RgbaImage, old_line: Rgba<u8>, old_bg: Rgba<u8>, new_line: Rgba<u8>, new_bg: Rgba<u8>) {
    for p in img.pixels_mut() {
        if p.0[0..3] == old_line.0[0..3] {
            *p = Rgba([new_line.0[0], new_line.0[1], new_line.0[2], p.0[3]]);
        } else if p.0[0..3] == old_bg.0[0..3] {
            *p = Rgba([new_bg.0[0], new_bg.0[1], new_bg.0[2], p.0[3]]);
        }
    }
}

pub fn hex_to_rgba(hex: &str, alpha: u8) -> Rgba<u8> {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(hex.get(0..2).unwrap_or("00"), 16).unwrap_or(0);
    let g = u8::from_str_radix(hex.get(2..4).unwrap_or("00"), 16).unwrap_or(0);
    let b = u8::from_str_radix(hex.get(4..6).unwrap_or("00"), 16).unwrap_or(0);
    Rgba([r, g, b, alpha])
}
