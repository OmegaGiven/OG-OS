//! Native compression / optimisation (replaces Stirling's Ghostscript/qpdf
//! "compress-pdf").
//!
//! Levels follow Stirling's 1–9 scale:
//!
//! | level | what happens |
//! |---|---|
//! | 1 | lossless: merge duplicate objects, Flate-compress uncompressed streams, drop unused objects, object + xref streams |
//! | 2 | + recompress Flate/LZW/ASCII streams at maximum level, drop page thumbnails and private application data (`/PieceInfo`) |
//! | 3–9 | + images: downsample to a target resolution and re-encode (JPEG quality falls with the level) |
//!
//! Image targets (dpi / JPEG quality): 3 → 300/90, 4 → 220/85, 5 → 150/80,
//! 6 → 150/70, 7 → 120/60, 8 → 96/50, 9 → 72/40. The effective resolution
//! of every image is computed from the CTM at each `Do` on every page
//! (recursing into Form XObjects); the smallest placement wins. Images also
//! drawn from annotation appearances or patterns, images with colour-key
//! masks, decode arrays, CMYK/indexed/16-bit/1-bit data or unsupported
//! filters are never resampled. Photographic images become JPEG; line art,
//! few-colour images and images with a soft mask stay lossless (Flate); a
//! soft mask is resampled together with its image. A re-encoded image is
//! only kept when it is clearly smaller.
//!
//! `grayscale` converts image samples and simple `rg/RG/k/K` colour
//! operators in page and form content to grey.
//!
//! Safety net: the result must parse, have the same page count and render
//! at least the pages the original rendered; and it must be smaller — else
//! the original bytes are returned and the report says so.

use super::repair::unrenderable_pages;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::{HashMap, HashSet};
use std::io::Write;

const MAX_STREAM: usize = 256 << 20;
const MAX_PIXELS: u64 = 120_000_000;

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 1 (lossless) … 9 (strongest)
    pub level: u8,
    pub grayscale: bool,
}

pub struct Compressed {
    pub pdf: Vec<u8>,
    pub report: String,
}

#[derive(Default)]
struct Stats {
    images_reencoded: usize,
    images_downsampled: usize,
    images_kept: usize,
    merged: usize,
    flated: usize,
    stripped: usize,
    gray_ops: usize,
}

/// (target dpi, JPEG quality) for the image pass; None = no image pass.
fn image_params(level: u8) -> Option<(f32, u8)> {
    match level {
        0..=2 => None,
        3 => Some((300.0, 90)),
        4 => Some((220.0, 85)),
        5 => Some((150.0, 80)),
        6 => Some((150.0, 70)),
        7 => Some((120.0, 60)),
        8 => Some((96.0, 50)),
        _ => Some((72.0, 40)),
    }
}

pub fn compress(pdf: &[u8], opts: Options) -> Result<Compressed, String> {
    let level = opts.level.clamp(1, 9);
    let mut doc = Document::load_mem(pdf).map_err(|e| format!("not a readable PDF ({e})"))?;
    if doc.is_encrypted() {
        return Err("the document is password-protected; remove the password first".into());
    }
    let pages_before = doc.get_pages().len();
    let mut st = Stats::default();

    if level >= 2 {
        strip_bloat(&mut doc, &mut st);
    }
    if opts.grayscale {
        gray_content(&mut doc, &mut st);
    }
    let params = image_params(level);
    // merge duplicates first so a shared image is measured at all its
    // placements, then again for anything that became identical
    for pass in 0..6 {
        if pass == 1 && (params.is_some() || opts.grayscale) {
            images(&mut doc, params, opts.grayscale, &mut st);
        }
        let n = dedupe(&mut doc);
        st.merged += n;
        if n == 0 && pass >= 1 {
            break;
        }
    }
    flate_streams(&mut doc, level >= 2, &mut st);
    doc.prune_objects();
    doc.renumber_objects();
    let mut t = Dictionary::new();
    for key in [&b"Root"[..], b"Info", b"ID"] {
        if let Ok(v) = doc.trailer.get(key) {
            t.set(key.to_vec(), v.clone());
        }
    }
    doc.trailer = t;
    let mut buf = Vec::new();
    let so = lopdf::SaveOptions::builder()
        .use_object_streams(true)
        .use_xref_streams(true)
        .compression_level(9)
        .build();
    doc.save_with_options(&mut buf, so)
        .map_err(|e| format!("could not write PDF: {e}"))?;

    let keep = |why: String| Compressed {
        pdf: pdf.to_vec(),
        report: why,
    };
    // safety net
    let pages_after = Document::load_mem(&buf)
        .map(|d| d.get_pages().len())
        .unwrap_or(0);
    if pages_after != pages_before {
        return Ok(keep(format!(
            "Compression produced a file with {pages_after} instead of {pages_before} pages; \
             the original was kept unchanged ({}).",
            human(pdf.len())
        )));
    }
    let bad_after = match unrenderable_pages(&buf) {
        Ok(b) => b,
        Err(_) => {
            return Ok(keep(format!(
                "The compressed file didn't verify; the original was kept unchanged ({}).",
                human(pdf.len())
            )))
        }
    };
    if !bad_after.is_empty() {
        let bad_before: HashSet<usize> = unrenderable_pages(pdf)
            .unwrap_or_default()
            .into_iter()
            .collect();
        if bad_after.iter().any(|p| !bad_before.contains(p)) {
            return Ok(keep(format!(
                "Some pages failed to render after compression; the original was kept \
                 unchanged ({}).",
                human(pdf.len())
            )));
        }
    }
    if buf.len() >= pdf.len() {
        return Ok(keep(format!(
            "Already compact: the optimised file wasn't smaller ({} → {}), so the original \
             was kept unchanged.",
            human(pdf.len()),
            human(buf.len())
        )));
    }

    let saved = 100.0 * (1.0 - buf.len() as f64 / pdf.len().max(1) as f64);
    let mut report = format!(
        "Compressed (level {level}): {} → {} (−{saved:.1}%).",
        human(pdf.len()),
        human(buf.len())
    );
    if params.is_some() || opts.grayscale {
        report.push_str(&format!(
            "\nImages re-encoded: {} ({} downsampled); left as they were: {}.",
            st.images_reencoded, st.images_downsampled, st.images_kept
        ));
    }
    if st.merged > 0 {
        report.push_str(&format!("\nDuplicate objects merged: {}.", st.merged));
    }
    if st.flated > 0 {
        report.push_str(&format!("\nStreams (re)compressed: {}.", st.flated));
    }
    if st.stripped > 0 {
        report.push_str(&format!(
            "\nThumbnails / private application data removed: {}.",
            st.stripped
        ));
    }
    if opts.grayscale {
        report.push_str(&format!("\nColour operators turned grey: {}.", st.gray_ops));
    }
    Ok(Compressed { pdf: buf, report })
}

pub(super) fn human(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.2} MB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{:.1} KB", n as f64 / 1024.0)
    }
}

// ------------------------------------------------------------ bloat / flate

fn strip_bloat(doc: &mut Document, st: &mut Stats) {
    let mut n = 0;
    for (_, o) in doc.objects.iter_mut() {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        if d.remove(b"PieceInfo").is_some() {
            n += 1;
        }
        if d.get(b"Type").and_then(Object::as_name).ok() == Some(b"Page")
            && d.remove(b"Thumb").is_some()
        {
            n += 1;
        }
    }
    st.stripped += n;
}

fn flate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

/// Flate-compress uncompressed streams; with `recompress`, also re-encode
/// Flate/LZW/ASCII/RunLength streams (no DecodeParms) at maximum level.
fn flate_streams(doc: &mut Document, recompress: bool, st: &mut Stats) {
    for (_, o) in doc.objects.iter_mut() {
        let Object::Stream(s) = o else { continue };
        let ty = s.dict.get(b"Type").and_then(Object::as_name).ok();
        if matches!(ty, Some(b"XRef") | Some(b"ObjStm") | Some(b"Metadata")) {
            continue;
        }
        let filters: Vec<Vec<u8>> = s
            .filters()
            .map(|f| f.into_iter().map(<[u8]>::to_vec).collect())
            .unwrap_or_default();
        if filters.is_empty() {
            if s.content.len() < 64 {
                continue;
            }
            let c = flate(&s.content);
            if c.len() + 20 < s.content.len() {
                s.dict.set("Filter", "FlateDecode");
                s.dict.remove(b"DecodeParms");
                s.set_content(c);
                st.flated += 1;
            }
            continue;
        }
        if !recompress || s.dict.has(b"DecodeParms") {
            continue;
        }
        let generic = filters.iter().all(|f| {
            matches!(
                f.as_slice(),
                b"FlateDecode"
                    | b"Fl"
                    | b"LZWDecode"
                    | b"LZW"
                    | b"ASCII85Decode"
                    | b"A85"
                    | b"ASCIIHexDecode"
                    | b"AHx"
                    | b"RunLengthDecode"
                    | b"RL"
            )
        });
        if !generic {
            continue;
        }
        let Ok(raw) = s.decompressed_content_with_limit(MAX_STREAM) else {
            continue;
        };
        let c = flate(&raw);
        if c.len() + 8 < s.content.len() {
            s.dict.set("Filter", "FlateDecode");
            s.set_content(c);
            st.flated += 1;
        }
    }
}

// ---------------------------------------------------------------- dedupe

fn dedupe(doc: &mut Document) -> usize {
    use std::hash::{Hash, Hasher};
    let root = doc.trailer.get(b"Root").and_then(Object::as_reference).ok();
    let info = doc.trailer.get(b"Info").and_then(Object::as_reference).ok();
    let mut seen: HashMap<(u64, String), ObjectId> = HashMap::new();
    let mut remap: HashMap<ObjectId, ObjectId> = HashMap::new();
    for (&id, o) in &doc.objects {
        if Some(id) == root || Some(id) == info {
            continue;
        }
        let key = match o {
            Object::Stream(s) => {
                let ty = s.dict.get(b"Type").and_then(Object::as_name).ok();
                if matches!(ty, Some(b"XRef") | Some(b"ObjStm")) {
                    continue;
                }
                let mut h = std::collections::hash_map::DefaultHasher::new();
                s.content.hash(&mut h);
                (h.finish(), format!("S{:?}", s.dict))
            }
            Object::Dictionary(d) => {
                let ty = d.get(b"Type").and_then(Object::as_name).ok();
                if !matches!(
                    ty,
                    Some(b"Font")
                        | Some(b"FontDescriptor")
                        | Some(b"ExtGState")
                        | Some(b"Encoding")
                ) {
                    continue;
                }
                (0, format!("D{d:?}"))
            }
            Object::Array(_) => (0, format!("A{o:?}")),
            _ => continue,
        };
        match seen.get(&key) {
            Some(&first) => {
                let same = match (doc.objects.get(&first), o) {
                    (Some(Object::Stream(a)), Object::Stream(b)) => a.content == b.content,
                    _ => true,
                };
                if same {
                    remap.insert(id, first);
                }
            }
            None => {
                seen.insert(key, id);
            }
        }
    }
    if remap.is_empty() {
        return 0;
    }
    doc.traverse_objects(|o| {
        if let Object::Reference(r) = o {
            if let Some(to) = remap.get(r) {
                *r = *to;
            }
        }
    });
    for id in remap.keys() {
        doc.objects.remove(id);
    }
    remap.len()
}

// ---------------------------------------------------------------- content

type Matrix = [f32; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

fn nums(ops: &[Object]) -> Vec<f32> {
    ops.iter()
        .filter_map(|o| match o {
            Object::Integer(i) => Some(*i as f32),
            Object::Real(r) => Some(*r),
            _ => None,
        })
        .collect()
}

fn deref<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    let mut o = o;
    for _ in 0..16 {
        match o {
            Object::Reference(id) => match doc.objects.get(id) {
                Some(t) => o = t,
                None => return &Object::Null,
            },
            _ => return o,
        }
    }
    o
}

fn page_resources(doc: &Document, page: ObjectId) -> Option<Dictionary> {
    let mut id = page;
    for _ in 0..32 {
        let d = doc.get_dictionary(id).ok()?;
        if let Ok(r) = d.get(b"Resources") {
            if let Object::Dictionary(r) = deref(doc, r) {
                return Some(r.clone());
            }
        }
        id = d.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

/// Where images are placed: smallest effective pixels-per-inch per image,
/// plus images whose placement couldn't be measured.
#[derive(Default)]
struct Placements {
    ppi: HashMap<ObjectId, f32>,
    unknown: HashSet<ObjectId>,
}

fn xobject_ref(doc: &Document, res: &Dictionary, name: &[u8]) -> Option<ObjectId> {
    let xo = res.get(b"XObject").ok()?;
    let Object::Dictionary(xo) = deref(doc, xo) else {
        return None;
    };
    xo.get(name).and_then(Object::as_reference).ok()
}

/// Mark every image reachable from `res` (recursively through forms and
/// patterns) as unknown.
fn mark_unknown(doc: &Document, res: &Dictionary, pl: &mut Placements, depth: usize) {
    if depth > 12 {
        return;
    }
    for key in [&b"XObject"[..], b"Pattern"] {
        let Ok(d) = res.get(key) else { continue };
        let Object::Dictionary(d) = deref(doc, d) else {
            continue;
        };
        for (_, v) in d.iter() {
            let Ok(id) = v.as_reference() else { continue };
            let Ok(s) = doc.get_object(id).and_then(Object::as_stream) else {
                continue;
            };
            match s.dict.get(b"Subtype").and_then(Object::as_name).ok() {
                Some(b"Image") => {
                    pl.unknown.insert(id);
                }
                _ => {
                    if let Ok(Object::Dictionary(r)) =
                        s.dict.get(b"Resources").map(|r| deref(doc, r))
                    {
                        mark_unknown(doc, r, pl, depth + 1);
                    }
                }
            }
        }
    }
}

fn walk(
    doc: &Document,
    content: &[u8],
    res: &Dictionary,
    ctm0: Matrix,
    pl: &mut Placements,
    stack: &mut Vec<ObjectId>,
) {
    let Ok(ops) = Content::decode(content) else {
        mark_unknown(doc, res, pl, 0);
        return;
    };
    // tiling patterns: placement unknown
    if let Ok(Object::Dictionary(p)) = res.get(b"Pattern").map(|p| deref(doc, p)) {
        let mut sub = Dictionary::new();
        sub.set("XObject", Object::Dictionary(p.clone()));
        mark_unknown(doc, &sub, pl, 0);
    }
    let mut ctm = ctm0;
    let mut saved: Vec<Matrix> = Vec::new();
    for op in &ops.operations {
        match op.operator.as_str() {
            "q" => saved.push(ctm),
            "Q" => ctm = saved.pop().unwrap_or(ctm0),
            "cm" => {
                let n = nums(&op.operands);
                if n.len() == 6 {
                    ctm = mul(&[n[0], n[1], n[2], n[3], n[4], n[5]], &ctm);
                }
            }
            "Do" => {
                let Some(name) = op.operands.first().and_then(|o| o.as_name().ok()) else {
                    continue;
                };
                let Some(id) = xobject_ref(doc, res, name) else {
                    continue;
                };
                let Ok(s) = doc.get_object(id).and_then(Object::as_stream) else {
                    continue;
                };
                match s.dict.get(b"Subtype").and_then(Object::as_name).ok() {
                    Some(b"Image") => {
                        let w = s.dict.get(b"Width").and_then(Object::as_i64).unwrap_or(0) as f32;
                        let h = s.dict.get(b"Height").and_then(Object::as_i64).unwrap_or(0) as f32;
                        let pw = (ctm[0] * ctm[0] + ctm[1] * ctm[1]).sqrt() / 72.0;
                        let ph = (ctm[2] * ctm[2] + ctm[3] * ctm[3]).sqrt() / 72.0;
                        if pw <= 1e-4 || ph <= 1e-4 || w <= 0.0 || h <= 0.0 {
                            pl.unknown.insert(id);
                            continue;
                        }
                        let ppi = (w / pw).min(h / ph);
                        let e = pl.ppi.entry(id).or_insert(f32::INFINITY);
                        *e = e.min(ppi);
                    }
                    Some(b"Form") => {
                        if stack.contains(&id) || stack.len() > 12 {
                            continue;
                        }
                        let m = s
                            .dict
                            .get(b"Matrix")
                            .ok()
                            .and_then(|m| m.as_array().ok())
                            .map(|a| nums(a))
                            .filter(|n| n.len() == 6)
                            .map(|n| [n[0], n[1], n[2], n[3], n[4], n[5]])
                            .unwrap_or(IDENTITY);
                        let sub_res = match s.dict.get(b"Resources").map(|r| deref(doc, r)) {
                            Ok(Object::Dictionary(r)) => r.clone(),
                            _ => res.clone(),
                        };
                        let Ok(data) = s.decompressed_content_with_limit(MAX_STREAM) else {
                            mark_unknown(doc, &sub_res, pl, 0);
                            continue;
                        };
                        stack.push(id);
                        walk(doc, &data, &sub_res, mul(&m, &ctm), pl, stack);
                        stack.pop();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn placements(doc: &Document) -> Placements {
    let mut pl = Placements::default();
    for (_, page) in doc.get_pages() {
        let res = page_resources(doc, page).unwrap_or_default();
        match doc.get_page_content_with_limit(page, MAX_STREAM) {
            Ok(c) => walk(doc, &c, &res, IDENTITY, &mut pl, &mut Vec::new()),
            Err(_) => mark_unknown(doc, &res, &mut pl, 0),
        }
        // annotation appearances: placement not measured
        let annots = doc
            .get_dictionary(page)
            .ok()
            .and_then(|d| d.get(b"Annots").ok())
            .map(|a| deref(doc, a))
            .and_then(|a| a.as_array().ok())
            .cloned()
            .unwrap_or_default();
        for a in annots {
            let Object::Dictionary(a) = deref(doc, &a) else {
                continue;
            };
            let Ok(Object::Dictionary(ap)) = a.get(b"AP").map(|x| deref(doc, x)) else {
                continue;
            };
            let mut xo = Dictionary::new();
            for (k, v) in ap.iter() {
                match deref(doc, v) {
                    Object::Dictionary(states) => {
                        for (k2, v2) in states.iter() {
                            let mut name = k.clone();
                            name.extend_from_slice(k2);
                            xo.set(name, v2.clone());
                        }
                    }
                    _ => xo.set(k.clone(), v.clone()),
                }
            }
            let mut r = Dictionary::new();
            r.set("XObject", Object::Dictionary(xo));
            mark_unknown(doc, &r, &mut pl, 0);
        }
    }
    pl
}

// ---------------------------------------------------------------- images

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Jpeg,
    Lossless,
}

struct ImageJob {
    id: ObjectId,
    w: u32,
    h: u32,
    comps: usize,
    kind: Kind,
    data: Vec<u8>,
    smask: Option<(ObjectId, Vec<u8>)>,
    old_len: usize,
    /// downsample factor (≤ 1)
    scale: f32,
    /// has a mask: never JPEG unless it already was
    keep_lossless: bool,
}

struct ImageResult {
    id: ObjectId,
    w: u32,
    h: u32,
    gray: bool,
    jpeg: bool,
    data: Vec<u8>,
    smask: Option<(ObjectId, Vec<u8>)>,
    downsampled: bool,
}

/// Number of colour components for colour spaces we can re-encode as-is.
fn comps(doc: &Document, cs: &Object) -> Option<usize> {
    match deref(doc, cs) {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Some(1),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(3),
            _ => None,
        },
        Object::Array(a) => match a.first().and_then(|o| o.as_name().ok())? {
            b"ICCBased" => {
                let s = deref(doc, a.get(1)?).as_stream().ok()?;
                match s.dict.get(b"N").and_then(Object::as_i64).ok()? {
                    1 => Some(1),
                    3 => Some(3),
                    _ => None,
                }
            }
            b"CalGray" => Some(1),
            b"CalRGB" => Some(3),
            _ => None,
        },
        _ => None,
    }
}

/// Decoded 8-bit samples of an image we know how to handle.
fn decode_image(doc: &Document, s: &Stream) -> Option<(u32, u32, usize, Kind, Vec<u8>)> {
    let d = &s.dict;
    if d.get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false)
        || d.has(b"Decode")
        || d.has(b"Alternates")
        || d.has(b"OPI")
        || matches!(d.get(b"Mask"), Ok(Object::Array(_)))
        || d.get(b"BitsPerComponent").and_then(Object::as_i64).ok() != Some(8)
    {
        return None;
    }
    let w = d.get(b"Width").and_then(Object::as_i64).ok()?;
    let h = d.get(b"Height").and_then(Object::as_i64).ok()?;
    if w <= 0 || h <= 0 || (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    let n = comps(doc, d.get(b"ColorSpace").ok()?)?;
    let filters: Vec<Vec<u8>> = s
        .filters()
        .map(|f| f.into_iter().map(<[u8]>::to_vec).collect())
        .unwrap_or_default();
    let (w, h) = (w as u32, h as u32);
    let need = w as usize * h as usize * n;
    if filters.len() == 1 && matches!(filters[0].as_slice(), b"DCTDecode" | b"DCT") {
        let img = image::load_from_memory_with_format(&s.content, image::ImageFormat::Jpeg).ok()?;
        if img.width() != w || img.height() != h {
            return None;
        }
        let px = if n == 1 {
            img.to_luma8().into_raw()
        } else {
            img.to_rgb8().into_raw()
        };
        return Some((w, h, n, Kind::Jpeg, px));
    }
    let lossless = filters
        .iter()
        .all(|f| matches!(f.as_slice(), b"FlateDecode" | b"Fl" | b"LZWDecode" | b"LZW"));
    if !lossless {
        return None;
    }
    let raw = s.decompressed_content_with_limit(MAX_STREAM).ok()?;
    if raw.len() < need {
        return None;
    }
    Some((w, h, n, Kind::Lossless, raw[..need].to_vec()))
}

/// Soft mask samples (8-bit gray, Flate or none) and its size.
fn decode_smask(s: &Stream) -> Option<(u32, u32, Vec<u8>)> {
    let d = &s.dict;
    if d.has(b"Matte") || d.has(b"Decode") {
        return None;
    }
    if d.get(b"BitsPerComponent").and_then(Object::as_i64).ok() != Some(8) {
        return None;
    }
    let w = d.get(b"Width").and_then(Object::as_i64).ok()? as u32;
    let h = d.get(b"Height").and_then(Object::as_i64).ok()? as u32;
    let filters = s.filters().unwrap_or_default();
    if !filters.iter().all(|f| *f == b"FlateDecode" || *f == b"Fl") {
        return None;
    }
    let raw = s.decompressed_content_with_limit(MAX_STREAM).ok()?;
    let need = w as usize * h as usize;
    (raw.len() >= need && w > 0 && h > 0).then(|| (w, h, raw[..need].to_vec()))
}

/// Many distinct colours → photographic (JPEG-friendly).
fn is_photo(data: &[u8], comps: usize) -> bool {
    let px = data.len() / comps;
    if px < 64 * 64 {
        return false;
    }
    let step = (px / 20_000).max(1);
    let mut set = HashSet::new();
    for i in (0..px).step_by(step) {
        let c = &data[i * comps..i * comps + comps];
        let key = c.iter().fold(0u32, |a, &b| (a << 8) | (b as u32 >> 2));
        set.insert(key);
        if set.len() > 1024 {
            return true;
        }
    }
    false
}

fn process(job: ImageJob, quality: u8, gray: bool) -> Option<ImageResult> {
    use image::{imageops::FilterType, DynamicImage, GrayImage, RgbImage};
    let to_gray = gray && job.comps == 3;
    let mut img = if job.comps == 1 {
        DynamicImage::ImageLuma8(GrayImage::from_raw(job.w, job.h, job.data)?)
    } else {
        DynamicImage::ImageRgb8(RgbImage::from_raw(job.w, job.h, job.data)?)
    };
    if to_gray {
        img = DynamicImage::ImageLuma8(img.to_luma8());
    }
    let mut downsampled = false;
    let (mut w, mut h) = (job.w, job.h);
    if job.scale < 1.0 {
        w = ((job.w as f32 * job.scale).round() as u32).max(1);
        h = ((job.h as f32 * job.scale).round() as u32).max(1);
        img = img.resize_exact(w, h, FilterType::CatmullRom);
        downsampled = true;
    }
    let is_gray = job.comps == 1 || to_gray;
    let samples = if is_gray {
        img.to_luma8().into_raw()
    } else {
        img.to_rgb8().into_raw()
    };
    let smask = match job.smask {
        Some((id, raw)) if downsampled => {
            let m = GrayImage::from_raw(job.w, job.h, raw)?;
            let m = image::imageops::resize(&m, w, h, FilterType::CatmullRom);
            Some((id, flate(m.as_raw())))
        }
        _ => None,
    };
    let jpeg = job.kind == Kind::Jpeg
        || (!job.keep_lossless && is_photo(&samples, if is_gray { 1 } else { 3 }));
    let data = if jpeg {
        let mut buf = Vec::new();
        let ty = if is_gray {
            image::ExtendedColorType::L8
        } else {
            image::ExtendedColorType::Rgb8
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
            .encode(&samples, w, h, ty)
            .ok()?;
        buf
    } else {
        flate(&samples)
    };
    let new_len = data.len() + smask.as_ref().map(|s| s.1.len()).unwrap_or(0);
    // must be clearly smaller (unless the colours changed on request)
    let limit = if to_gray {
        job.old_len
    } else {
        job.old_len * 9 / 10
    };
    if new_len >= limit {
        return None;
    }
    Some(ImageResult {
        id: job.id,
        w,
        h,
        gray: to_gray,
        jpeg,
        data,
        smask,
        downsampled,
    })
}

fn images(doc: &mut Document, params: Option<(f32, u8)>, gray: bool, st: &mut Stats) {
    let pl = placements(doc);
    let (target, quality) = params.unwrap_or((f32::INFINITY, 92));
    // every image XObject reached by a Do (measured or not)
    let mut ids: Vec<ObjectId> = pl.ppi.keys().chain(pl.unknown.iter()).copied().collect();
    ids.sort();
    ids.dedup();
    let mut jobs = Vec::new();
    for id in ids {
        let Ok(s) = doc.get_object(id).and_then(Object::as_stream) else {
            continue;
        };
        let Some((w, h, comps, kind, data)) = decode_image(doc, s) else {
            st.images_kept += 1;
            continue;
        };
        let smask_id = s.dict.get(b"SMask").and_then(Object::as_reference).ok();
        let mut old_len = s.content.len();
        let smask = match smask_id {
            Some(mid) => {
                let Some(m) = doc.get_object(mid).ok().and_then(|o| o.as_stream().ok()) else {
                    st.images_kept += 1;
                    continue;
                };
                old_len += m.content.len();
                match decode_smask(m) {
                    Some((mw, mh, raw)) if mw == w && mh == h => Some((mid, raw)),
                    _ => None,
                }
            }
            None => None,
        };
        let has_mask = smask_id.is_some() || s.dict.has(b"Mask");
        let scale = match (pl.unknown.contains(&id), pl.ppi.get(&id)) {
            (false, Some(&ppi)) if ppi > target * 1.2 => target / ppi,
            _ => 1.0,
        };
        // a soft mask we can't resample in step blocks resampling
        let scale = if smask_id.is_some() && smask.is_none() {
            1.0
        } else {
            scale
        };
        if scale >= 1.0
            && !gray
            && kind == Kind::Lossless
            && params.map(|p| p.1 > 85).unwrap_or(true)
        {
            // nothing lossy to gain at this level
            st.images_kept += 1;
            continue;
        }
        if scale >= 1.0 && !gray && kind == Kind::Jpeg && quality >= 85 {
            st.images_kept += 1;
            continue;
        }
        jobs.push(ImageJob {
            id,
            w,
            h,
            comps,
            kind,
            data,
            smask,
            old_len,
            scale,
            keep_lossless: has_mask,
        });
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8);
    let results: Vec<Option<ImageResult>> = {
        let queue = std::sync::Mutex::new(jobs.into_iter().enumerate().collect::<Vec<_>>());
        let out = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| loop {
                    let next = queue.lock().ok().and_then(|mut q| q.pop());
                    let Some((i, job)) = next else { break };
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        process(job, quality, gray)
                    }))
                    .ok()
                    .flatten();
                    if let Ok(mut o) = out.lock() {
                        o.push((i, r));
                    }
                });
            }
        });
        let mut v = out.into_inner().unwrap_or_default();
        v.sort_by_key(|(i, _)| *i);
        v.into_iter().map(|(_, r)| r).collect()
    };
    for r in results {
        let Some(r) = r else {
            st.images_kept += 1;
            continue;
        };
        st.images_reencoded += 1;
        if r.downsampled {
            st.images_downsampled += 1;
        }
        if let Ok(Object::Stream(s)) = doc.get_object_mut(r.id) {
            s.dict.set("Width", r.w as i64);
            s.dict.set("Height", r.h as i64);
            s.dict.set("BitsPerComponent", 8);
            if r.gray {
                s.dict.set("ColorSpace", "DeviceGray");
            }
            s.dict
                .set("Filter", if r.jpeg { "DCTDecode" } else { "FlateDecode" });
            s.dict.remove(b"DecodeParms");
            s.set_content(r.data);
        }
        if let Some((mid, data)) = r.smask {
            if let Ok(Object::Stream(m)) = doc.get_object_mut(mid) {
                m.dict.set("Width", r.w as i64);
                m.dict.set("Height", r.h as i64);
                m.dict.set("Filter", "FlateDecode");
                m.dict.remove(b"DecodeParms");
                m.set_content(data);
            }
        }
    }
}

// ---------------------------------------------------------------- grey ops

fn gray_value(op: &str, n: &[f32]) -> Option<(&'static str, f32)> {
    match (op, n.len()) {
        ("rg", 3) => Some(("g", 0.299 * n[0] + 0.587 * n[1] + 0.114 * n[2])),
        ("RG", 3) => Some(("G", 0.299 * n[0] + 0.587 * n[1] + 0.114 * n[2])),
        ("k", 4) => Some(("g", gray_cmyk(n))),
        ("K", 4) => Some(("G", gray_cmyk(n))),
        _ => None,
    }
}

fn gray_cmyk(n: &[f32]) -> f32 {
    let r = (1.0 - n[0]) * (1.0 - n[3]);
    let g = (1.0 - n[1]) * (1.0 - n[3]);
    let b = (1.0 - n[2]) * (1.0 - n[3]);
    (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 1.0)
}

fn gray_ops(content: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut c = Content::decode(content).ok()?;
    let mut n = 0;
    for op in c.operations.iter_mut() {
        let v = nums(&op.operands);
        if v.len() != op.operands.len() {
            continue;
        }
        if let Some((new, g)) = gray_value(op.operator.as_str(), &v) {
            *op = Operation::new(new, vec![Object::Real((g * 1000.0).round() / 1000.0)]);
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    Some((c.encode().ok()?, n))
}

fn gray_content(doc: &mut Document, st: &mut Stats) {
    let mut targets: Vec<ObjectId> = Vec::new();
    for (_, page) in doc.get_pages() {
        targets.extend(doc.get_page_contents(page));
    }
    for (&id, o) in &doc.objects {
        if let Object::Stream(s) = o {
            if s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Form") {
                targets.push(id);
            }
        }
    }
    targets.sort();
    targets.dedup();
    for id in targets {
        let Ok(Object::Stream(s)) = doc.get_object_mut(id) else {
            continue;
        };
        let Ok(raw) = s.decompressed_content_with_limit(MAX_STREAM) else {
            continue;
        };
        if let Some((data, n)) = gray_ops(&raw) {
            s.dict.remove(b"DecodeParms");
            s.dict.remove(b"Filter");
            s.set_content(data);
            st.gray_ops += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn photo(w: u32, h: u32) -> Vec<u8> {
        // smooth gradients + deterministic noise: many colours
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        let mut seed = 0x1234_5678u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = (seed >> 28) as i32 - 8;
                let r = (x * 255 / w) as i32 + n;
                let g = (y * 255 / h) as i32 - n;
                let b = ((x + y) * 255 / (w + h)) as i32;
                v.extend([
                    r.clamp(0, 255) as u8,
                    g.clamp(0, 255) as u8,
                    b.clamp(0, 255) as u8,
                ]);
            }
        }
        v
    }

    /// One page: a 1200² Flate photo drawn 2" wide (600 ppi), a JPEG drawn
    /// 1" wide, a two-colour logo with a soft mask, the same photo bytes
    /// again as a separate object, and an uncompressed content stream.
    fn doc_with_images() -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages = doc.new_object_id();
        let px = photo(1200, 1200);
        let big = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1200, "Height" => 1200,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "FlateDecode" },
            flate(&px),
        ));
        let dup = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1200, "Height" => 1200,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "FlateDecode" },
            flate(&px),
        ));
        let mut jpg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 95)
            .encode(&photo(800, 600), 800, 600, image::ExtendedColorType::Rgb8)
            .unwrap();
        let jpeg = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 800, "Height" => 600,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode" },
            jpg,
        ));
        let (lw, lh) = (900u32, 900u32);
        let logo: Vec<u8> = (0..lw * lh)
            .flat_map(|i| {
                if (i % lw) / 90 % 2 == 0 {
                    [200u8, 30, 30]
                } else {
                    [255, 255, 255]
                }
            })
            .collect();
        let alpha: Vec<u8> = (0..lw * lh)
            .map(|i| if (i / lw) < lh / 2 { 255 } else { 128 })
            .collect();
        let smask = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => lw as i64, "Height" => lh as i64,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "FlateDecode" },
            flate(&alpha),
        ));
        let logo = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => lw as i64, "Height" => lh as i64,
                "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "FlateDecode", "SMask" => smask },
            flate(&logo),
        ));
        let content = b"q 0.9 0.2 0.2 rg 20 20 100 50 re f Q \
            q 144 0 0 144 36 600 cm /Im1 Do Q \
            q 72 0 0 54 300 600 cm /Im2 Do Q \
            q 100 0 0 100 300 400 cm /Im3 Do Q \
            q 72 0 0 72 36 300 cm /Im4 Do Q"
            .to_vec();
        let c = doc.add_object(Stream::new(dictionary! {}, content));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "Contents" => c,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! { "XObject" => dictionary! {
                "Im1" => big, "Im2" => jpeg, "Im3" => logo, "Im4" => dup } },
        });
        doc.objects.insert(
            pages,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
            ),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        doc.trailer.set("Root", cat);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn render(pdf: &[u8], dpi: f32) -> Vec<u8> {
        use hayro::hayro_interpret::InterpreterSettings;
        use hayro::{render, RenderCache, RenderSettings};
        let doc = hayro::hayro_syntax::Pdf::new(std::sync::Arc::new(pdf.to_vec())).unwrap();
        let cache = RenderCache::new();
        let pm = render(
            &doc.pages()[0],
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings {
                x_scale: dpi / 72.0,
                y_scale: dpi / 72.0,
                bg_color: hayro::vello_cpu::color::palette::css::WHITE,
                ..Default::default()
            },
        );
        pm.data_as_u8_slice().to_vec()
    }

    fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .map(|(x, y)| (*x as f64 - *y as f64).abs())
            .sum::<f64>()
            / a.len() as f64
    }

    fn image_dims(pdf: &[u8]) -> Vec<(i64, i64, String)> {
        let d = Document::load_mem(pdf).unwrap();
        let mut v: Vec<_> = d
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .filter(|s| s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image"))
            .map(|s| {
                (
                    s.dict.get(b"Width").unwrap().as_i64().unwrap(),
                    s.dict.get(b"Height").unwrap().as_i64().unwrap(),
                    s.filters()
                        .map(|f| {
                            f.iter()
                                .map(|x| String::from_utf8_lossy(x).into_owned())
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn lossless_level_merges_duplicates_only() {
        let src = doc_with_images();
        let out = compress(
            &src,
            Options {
                level: 1,
                grayscale: false,
            },
        )
        .unwrap();
        assert!(out.pdf.len() < src.len(), "{}", out.report);
        assert!(
            out.report.contains("Duplicate objects merged: 1"),
            "{}",
            out.report
        );
        // the four distinct images keep their pixels
        let dims = image_dims(&out.pdf);
        assert_eq!(dims.len(), 4, "{dims:?}");
        assert!(dims.iter().any(|d| d.0 == 1200));
        assert!(mean_diff(&render(&src, 36.0), &render(&out.pdf, 36.0)) < 0.5);
    }

    #[test]
    fn strong_level_downsamples_by_placement() {
        let src = doc_with_images();
        let out = compress(
            &src,
            Options {
                level: 9,
                grayscale: false,
            },
        )
        .unwrap();
        println!("{}", out.report);
        let ratio = out.pdf.len() as f64 / src.len() as f64;
        assert!(ratio < 0.2, "ratio {ratio}: {}", out.report);
        let dims = image_dims(&out.pdf);
        // photo drawn 2" wide at 72 dpi target → 144 px, JPEG
        assert!(
            dims.iter()
                .any(|d| d.0 == 144 && d.1 == 144 && d.2 == "DCTDecode"),
            "{dims:?}"
        );
        // logo (soft mask) stays lossless and its mask follows: 100/72" → 100 px
        let logo: Vec<_> = dims.iter().filter(|d| d.0 == 100).collect();
        assert_eq!(logo.len(), 2, "{dims:?}");
        assert!(logo.iter().all(|d| d.2 == "FlateDecode"), "{dims:?}");
        // looks the same at screen resolution
        let diff = mean_diff(&render(&src, 36.0), &render(&out.pdf, 36.0));
        assert!(diff < 6.0, "mean difference {diff}");
    }

    #[test]
    fn grayscale_images_and_colours() {
        let src = doc_with_images();
        let out = compress(
            &src,
            Options {
                level: 5,
                grayscale: true,
            },
        )
        .unwrap();
        let d = Document::load_mem(&out.pdf).unwrap();
        for s in d.objects.values().filter_map(|o| o.as_stream().ok()) {
            if s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image") {
                assert_eq!(
                    s.dict.get(b"ColorSpace").unwrap().as_name().unwrap(),
                    b"DeviceGray"
                );
            }
        }
        let page = *d.get_pages().values().next().unwrap();
        let c = String::from_utf8_lossy(&d.get_page_content(page)).into_owned();
        assert!(!c.contains(" rg") && c.contains(" g"), "{c}");
        let img = render(&out.pdf, 24.0);
        assert!(img
            .chunks(4)
            .all(|p| p[0].abs_diff(p[1]) <= 2 && p[1].abs_diff(p[2]) <= 2));
    }

    #[test]
    fn keeps_original_when_not_smaller() {
        let src = compress(
            &doc_with_images(),
            Options {
                level: 9,
                grayscale: false,
            },
        )
        .unwrap()
        .pdf;
        let again = compress(
            &src,
            Options {
                level: 1,
                grayscale: false,
            },
        )
        .unwrap();
        assert_eq!(again.pdf, src);
        assert!(again.report.contains("original"), "{}", again.report);
    }

    /// Manual survey over real files: `OG_PDF_SAMPLES=a.pdf:b.pdf
    /// OG_PDF_OUT=/tmp/dir cargo test -- --ignored sample_survey --nocapture`
    #[test]
    #[ignore]
    fn sample_survey() {
        let files = std::env::var("OG_PDF_SAMPLES").unwrap_or_default();
        let out_dir = std::env::var("OG_PDF_OUT").ok();
        for f in files.split(':').filter(|f| !f.is_empty()) {
            let src = std::fs::read(f).unwrap();
            let name = std::path::Path::new(f)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            println!("== {f} ({} bytes)", src.len());
            for level in [1u8, 2, 5, 9] {
                let t = std::time::Instant::now();
                match compress(
                    &src,
                    Options {
                        level,
                        grayscale: false,
                    },
                ) {
                    Ok(c) => {
                        let diff = if c.pdf != src {
                            mean_diff(&render(&src, 30.0), &render(&c.pdf, 30.0))
                        } else {
                            0.0
                        };
                        println!(
                            "  level {level}: {} → {} ({:.1}%) page-1 diff {diff:.2} in {:?}",
                            src.len(),
                            c.pdf.len(),
                            100.0 * c.pdf.len() as f64 / src.len() as f64,
                            t.elapsed()
                        );
                        if let Some(d) = &out_dir {
                            std::fs::write(format!("{d}/{name}-c{level}.pdf"), &c.pdf).unwrap();
                        }
                    }
                    Err(e) => println!("  level {level}: error {e}"),
                }
            }
            match super::super::pdfa::convert(&src) {
                Ok(c) => {
                    let v = super::super::pdfa::verify(&c.pdf).unwrap();
                    println!("  pdf/a: {} violations {v:?}", v.len());
                    if let Some(d) = &out_dir {
                        std::fs::write(format!("{d}/{name}-pdfa.pdf"), &c.pdf).unwrap();
                    }
                }
                Err(e) => println!("  pdf/a: error {e}"),
            }
            match super::super::repair::repair(&src) {
                Ok(r) => println!("  repair: {}", r.report.replace('\n', " | ")),
                Err(e) => println!("  repair: error {e}"),
            }
        }
    }

    #[test]
    fn unknown_placement_is_not_resampled() {
        // the photo is only used inside a tiling pattern: never downsampled
        let src = doc_with_images();
        let mut d = Document::load_mem(&src).unwrap();
        let page = *d.get_pages().values().next().unwrap();
        let res = d
            .get_dictionary(page)
            .unwrap()
            .get(b"Resources")
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        let im1 = res
            .get(b"XObject")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Im1")
            .unwrap()
            .clone();
        let pat = d.add_object(Stream::new(
            dictionary! { "PatternType" => 1, "PaintType" => 1, "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()], "XStep" => 10, "YStep" => 10,
            "Resources" => dictionary! { "XObject" => dictionary! { "P" => im1 } } },
            b"q 10 0 0 10 0 0 cm /P Do Q".to_vec(),
        ));
        let mut res = res;
        res.set("Pattern", dictionary! { "T" => pat });
        d.get_dictionary_mut(page).unwrap().set("Resources", res);
        let mut buf = Vec::new();
        d.save_to(&mut buf).unwrap();
        let out = compress(
            &buf,
            Options {
                level: 9,
                grayscale: false,
            },
        )
        .unwrap();
        assert!(
            image_dims(&out.pdf).iter().any(|x| x.0 == 1200),
            "{}",
            out.report
        );
    }
}
