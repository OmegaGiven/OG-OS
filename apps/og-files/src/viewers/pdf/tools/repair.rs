//! Native repair of damaged PDFs (replaces Stirling's Ghostscript/qpdf path).
//!
//! Steps:
//!   1. **Reconstruct** the file from its bytes, ignoring the xref: every
//!      `n g obj … endobj` is scanned in file order (later copies win, as in
//!      incremental updates), stream `/Length`s are re-measured against
//!      `endstream`, objects cut off by a truncated tail are dropped, cross
//!      reference streams are discarded and the trailer (`/Root`, `/Info`,
//!      `/ID`, `/Encrypt`) is taken from the last trailer or xref stream that
//!      names a root, else from the last `/Type /Catalog` object.
//!   2. lopdf parses that synthetic, well-formed file (object streams are
//!      expanded, unparsable objects are skipped) — unless the original
//!      parses with lopdf into at least as many pages, which then wins.
//!   3. [`structure::repair`] nulls dangling references, rebuilds a flat
//!      page tree (recovering loose `/Page` objects), prunes and renumbers.
//!   4. Every page of the result is rendered with hayro at low resolution;
//!      pages that still fail are listed in the report.
//!
//! Limits: content inside a damaged stream is kept as found (a truncated
//! Flate stream renders as far as it decodes); objects whose syntax lopdf
//! can't read are dropped; encrypted files keep their encryption and still
//! need their password to be opened for editing.

use super::structure;
use lopdf::Document;
use memchr::memmem;

/// Repair result: the rewritten file plus a short report.
pub struct Repaired {
    pub pdf: Vec<u8>,
    pub report: String,
}

pub fn repair(pdf: &[u8]) -> Result<Repaired, String> {
    let scan = reconstruct(pdf);
    let direct_pages = page_count_lopdf(pdf);
    let (source, how): (Vec<u8>, &str) = match &scan {
        Some(s) if s.objects > 0 && page_count_lopdf(&s.bytes) >= direct_pages => {
            (s.bytes.clone(), "rebuilt from a scan of the file's objects")
        }
        _ if direct_pages > 0 => (pdf.to_vec(), "re-read through the cross-reference table"),
        Some(s) if s.objects > 0 => (s.bytes.clone(), "rebuilt from a scan of the file's objects"),
        _ => return Err("no PDF objects could be recovered from this file".into()),
    };
    let out = structure::repair(&source)?;
    let pages = page_count_lopdf(&out);
    if pages == 0 {
        return Err("no pages could be recovered".into());
    }
    let bad = unrenderable_pages(&out)?;

    let mut report = format!(
        "Repair: {how}.\n{pages} page(s) recovered; {} → {}.",
        human(pdf.len()),
        human(out.len())
    );
    if let Some(s) = &scan {
        report.push_str(&format!("\nObjects found by scanning: {}", s.objects));
        if s.fixed_lengths > 0 {
            report.push_str(&format!("\nStream lengths corrected: {}", s.fixed_lengths));
        }
        if s.truncated > 0 {
            report.push_str(&format!(
                "\nObjects cut off by a truncated end of file (dropped): {}",
                s.truncated
            ));
        }
        if s.root_from_catalog_scan {
            report.push_str("\nThe trailer was missing; the catalog was located by scanning.");
        }
    }
    if bad.is_empty() {
        report.push_str("\nAll pages render.");
    } else {
        let list: Vec<String> = bad.iter().map(|p| (p + 1).to_string()).collect();
        report.push_str(&format!(
            "\nPages that still fail to render: {}",
            list.join(", ")
        ));
    }
    Ok(Repaired { pdf: out, report })
}

fn page_count_lopdf(pdf: &[u8]) -> usize {
    std::panic::catch_unwind(|| {
        Document::load_mem(pdf)
            .map(|d| d.get_pages().len())
            .unwrap_or(0)
    })
    .unwrap_or(0)
}

/// 0-based pages hayro can't render (open failure is an error).
pub(super) fn unrenderable_pages(pdf: &[u8]) -> Result<Vec<usize>, String> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::{render, RenderCache, RenderSettings};
    use std::sync::Arc;
    let data = Arc::new(pdf.to_vec());
    let doc = match std::panic::catch_unwind(|| Pdf::new(data)) {
        Ok(Ok(p)) => p,
        _ => return Err("the repaired file still can't be read".into()),
    };
    let settings = InterpreterSettings::default();
    let cache = RenderCache::new();
    let mut bad = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let (w, h) = page.render_dimensions();
        let s = (12.0 / 72.0f32).min(400.0 / w.max(h).max(1.0));
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let pm = render(
                page,
                &cache,
                &settings,
                &RenderSettings {
                    x_scale: s,
                    y_scale: s,
                    ..Default::default()
                },
            );
            pm.width() > 0
        }))
        .unwrap_or(false);
        if !ok {
            bad.push(i);
        }
    }
    Ok(bad)
}

fn human(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{:.0} KB", (n as f64 / 1024.0).max(1.0))
    }
}

// ---------------------------------------------------------------- scanner

pub(super) struct Scan {
    pub bytes: Vec<u8>,
    pub objects: usize,
    pub fixed_lengths: usize,
    pub truncated: usize,
    pub root_from_catalog_scan: bool,
}

struct RawObj {
    num: u32,
    gen: u16,
    /// normalized `dict stream…endstream` or plain body (between obj/endobj)
    body: Vec<u8>,
    is_xref: bool,
    is_catalog: bool,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\0')
}

fn is_regular(b: u8) -> bool {
    !is_ws(b) && !b"()<>[]{}/%".contains(&b)
}

/// `num gen obj` ending right before `kw` (the index of "obj").
fn header_before(buf: &[u8], kw: usize) -> Option<(u32, u16, usize)> {
    let mut i = kw;
    // whitespace between gen and obj (may be none? spec requires ≥1; be lenient)
    while i > 0 && is_ws(buf[i - 1]) {
        i -= 1;
    }
    let gen_end = i;
    while i > 0 && buf[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == gen_end || gen_end - i > 5 {
        return None;
    }
    let gen: u16 = std::str::from_utf8(&buf[i..gen_end]).ok()?.parse().ok()?;
    let ws_end = i;
    while i > 0 && is_ws(buf[i - 1]) {
        i -= 1;
    }
    if i == ws_end {
        return None;
    }
    let num_end = i;
    while i > 0 && buf[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == num_end || num_end - i > 10 {
        return None;
    }
    if i > 0 && is_regular(buf[i - 1]) {
        return None;
    }
    let num: u32 = std::str::from_utf8(&buf[i..num_end]).ok()?.parse().ok()?;
    if num == 0 {
        return None;
    }
    Some((num, gen, i))
}

/// Find the next object header at or after `from`: (num, gen, header start,
/// body start).
fn next_header(buf: &[u8], mut from: usize) -> Option<(u32, u16, usize, usize)> {
    let finder = memmem::Finder::new(b"obj");
    while let Some(off) = finder.find(&buf[from..]) {
        let kw = from + off;
        from = kw + 3;
        let after_ok = buf.get(kw + 3).map(|&b| !is_regular(b)).unwrap_or(true);
        let endobj = kw >= 3 && &buf[kw - 3..kw] == b"end";
        if !after_ok || endobj {
            continue;
        }
        if let Some((n, g, start)) = header_before(buf, kw) {
            return Some((n, g, start, kw + 3));
        }
    }
    None
}

/// Position of the `stream` keyword that starts stream data (not part of
/// `endstream`), searched before `limit`.
fn stream_kw(buf: &[u8], from: usize, limit: usize) -> Option<usize> {
    let finder = memmem::Finder::new(b"stream");
    let mut at = from;
    while let Some(off) = finder.find(&buf[at..limit]) {
        let k = at + off;
        let preceded_by_end = k >= 3 && &buf[k - 3..k] == b"end";
        let after_ok = buf.get(k + 6).map(|&b| !is_regular(b)).unwrap_or(true);
        if !preceded_by_end && after_ok {
            return Some(k);
        }
        at = k + 6;
    }
    None
}

/// Value of a `/Key n` or `/Key n g R` entry in raw dictionary text:
/// (start of the entry, end of the entry, number, is_reference).
fn find_key(dict: &[u8], key: &[u8]) -> Option<(usize, usize, i64, bool)> {
    let mut pat = Vec::with_capacity(key.len() + 1);
    pat.push(b'/');
    pat.extend_from_slice(key);
    let finder = memmem::Finder::new(&pat);
    let mut at = 0;
    while let Some(off) = finder.find(&dict[at..]) {
        let s = at + off;
        let mut i = s + pat.len();
        at = i;
        if dict.get(i).map(|&b| is_regular(b)).unwrap_or(false) {
            continue; // /Length1 etc.
        }
        while i < dict.len() && is_ws(dict[i]) {
            i += 1;
        }
        let n0 = i;
        while i < dict.len() && dict[i].is_ascii_digit() {
            i += 1;
        }
        if i == n0 {
            continue;
        }
        let n: i64 = std::str::from_utf8(&dict[n0..i]).ok()?.parse().ok()?;
        // optional "g R"
        let mut j = i;
        while j < dict.len() && is_ws(dict[j]) {
            j += 1;
        }
        let g0 = j;
        while j < dict.len() && dict[j].is_ascii_digit() {
            j += 1;
        }
        if j > g0 {
            let mut k = j;
            while k < dict.len() && is_ws(dict[k]) {
                k += 1;
            }
            if dict.get(k) == Some(&b'R')
                && dict.get(k + 1).map(|&b| !is_regular(b)).unwrap_or(true)
            {
                return Some((s, k + 1, n, true));
            }
        }
        return Some((s, i, n, false));
    }
    None
}

/// Raw text of a `/Key …` value used in trailers: `n g R` refs or `[ … ]`.
fn raw_value(dict: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let mut pat = vec![b'/'];
    pat.extend_from_slice(key);
    let s = memmem::find(dict, &pat)?;
    let mut i = s + pat.len();
    if dict.get(i).map(|&b| is_regular(b)).unwrap_or(false) {
        return None;
    }
    while i < dict.len() && is_ws(dict[i]) {
        i += 1;
    }
    if dict.get(i) == Some(&b'[') {
        let e = i + memchr::memchr(b']', &dict[i..])?;
        return Some(dict[i..=e].to_vec());
    }
    let (_, e, _, is_ref) = find_key(dict, key)?;
    is_ref.then(|| dict[i..e].to_vec())
}

fn has_name(dict: &[u8], key: &[u8], value: &[u8]) -> bool {
    let mut pat = vec![b'/'];
    pat.extend_from_slice(key);
    let finder = memmem::Finder::new(&pat);
    let mut at = 0;
    while let Some(off) = finder.find(&dict[at..]) {
        let mut i = at + off + pat.len();
        at = i;
        if dict.get(i).map(|&b| is_regular(b)).unwrap_or(false) {
            continue;
        }
        while i < dict.len() && is_ws(dict[i]) {
            i += 1;
        }
        if dict.get(i) == Some(&b'/') {
            let v0 = i + 1;
            let mut v1 = v0;
            while v1 < dict.len() && is_regular(dict[v1]) {
                v1 += 1;
            }
            if &dict[v0..v1] == value {
                return true;
            }
        }
    }
    false
}

/// Members of an object stream (Flate or unfiltered): (number, body).
fn unpack_objstm(dict: &[u8], data: &[u8]) -> Option<Vec<(u32, Vec<u8>)>> {
    use std::io::Read;
    if memmem::find(dict, b"/DecodeParms").is_some() {
        return None;
    }
    let raw = if has_name(dict, b"Filter", b"FlateDecode") {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(data)
            .take(256 << 20)
            .read_to_end(&mut out)
            .ok()?;
        out
    } else if memmem::find(dict, b"/Filter").is_none() {
        data.to_vec()
    } else {
        return None;
    };
    let (_, _, n, _) = find_key(dict, b"N")?;
    let (_, _, first, _) = find_key(dict, b"First")?;
    let first = usize::try_from(first).ok()?;
    let head = std::str::from_utf8(raw.get(..first)?).ok()?;
    let nums: Vec<usize> = head
        .split_ascii_whitespace()
        .map(|t| t.parse().ok())
        .collect::<Option<_>>()?;
    if nums.len() < 2 * n as usize {
        return None;
    }
    let mut out = Vec::new();
    for i in 0..n as usize {
        let start = first + nums[2 * i + 1];
        let end = if i + 1 < n as usize {
            first + nums[2 * i + 3]
        } else {
            raw.len()
        };
        let body = raw.get(start..end.max(start))?.to_vec();
        out.push((u32::try_from(nums[2 * i]).ok()?, body));
    }
    Some(out)
}

/// Rebuild a PDF from the objects found in `buf`, ignoring its xref.
pub(super) fn reconstruct(buf: &[u8]) -> Option<Scan> {
    let start = memmem::find(buf, b"%PDF-").unwrap_or(0);
    let version = buf
        .get(start + 5..start + 8)
        .filter(|v| v[0].is_ascii_digit() && v[1] == b'.' && v[2].is_ascii_digit())
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_else(|| "1.7".into());

    let mut objs: Vec<RawObj> = Vec::new();
    let mut trailer_dicts: Vec<Vec<u8>> = Vec::new();
    let mut fixed_lengths = 0;
    let mut truncated = 0;
    let endobj_f = memmem::Finder::new(b"endobj");
    let endstream_f = memmem::Finder::new(b"endstream");

    let mut pos = start;
    while let Some((num, gen, _hstart, body0)) = next_header(buf, pos) {
        // the object can't extend past the next header (if the file lost
        // an endobj) — but a stream may legitimately contain "obj" bytes, so
        // the stream case is measured first.
        let next = next_header(buf, body0).map(|h| h.2).unwrap_or(buf.len());
        let end_obj = endobj_f.find(&buf[body0..]).map(|o| body0 + o);
        let limit = end_obj.map(|e| e.min(next)).unwrap_or(next);
        match stream_kw(buf, body0, limit.max(body0)) {
            Some(skw) => {
                let dict = &buf[body0..skw];
                let mut d0 = skw + 6;
                if buf.get(d0) == Some(&b'\r') {
                    d0 += 1;
                }
                if buf.get(d0) == Some(&b'\n') {
                    d0 += 1;
                }
                // declared length if it lands on endstream
                let declared = find_key(dict, b"Length").and_then(|(_, _, n, is_ref)| {
                    if is_ref || n < 0 {
                        return None;
                    }
                    let e = d0.checked_add(n as usize)?;
                    let mut k = e;
                    while k < buf.len() && is_ws(buf[k]) {
                        k += 1;
                    }
                    buf.get(k..k + 9).filter(|s| *s == b"endstream").map(|_| e)
                });
                let (data_end, es) = match declared {
                    Some(e) => {
                        let es = e + endstream_f.find(&buf[e..]).unwrap_or(0);
                        (e, es)
                    }
                    None => match endstream_f.find(&buf[d0..]) {
                        Some(o) => {
                            let es = d0 + o;
                            let mut e = es;
                            if e > d0 && buf[e - 1] == b'\n' {
                                e -= 1;
                            }
                            if e > d0 && buf[e - 1] == b'\r' {
                                e -= 1;
                            }
                            // a direct /Length that missed endstream was wrong
                            // (indirect lengths are simply measured)
                            if matches!(find_key(dict, b"Length"), Some((_, _, _, false))) {
                                fixed_lengths += 1;
                            }
                            (e, es)
                        }
                        None => {
                            // truncated in the middle of the stream
                            truncated += 1;
                            break;
                        }
                    },
                };
                let data = &buf[d0..data_end];
                let mut dict = dict.to_vec();
                while let Some((s, e, _, _)) = find_key(&dict, b"Length") {
                    dict.drain(s..e);
                }
                let Some(open) = memmem::find(&dict, b"<<") else {
                    pos = es + 9;
                    continue;
                };
                let ins = format!(" /Length {} ", data.len());
                dict.splice(open + 2..open + 2, ins.bytes());
                let is_xref = has_name(&dict, b"Type", b"XRef");
                if is_xref {
                    trailer_dicts.push(dict.clone());
                }
                // object streams are unpacked in place, so "later wins"
                // applies to their members too
                if has_name(&dict, b"Type", b"ObjStm") {
                    if let Some(members) = unpack_objstm(&dict, data) {
                        for (num, body) in members {
                            let is_catalog = has_name(&body, b"Type", b"Catalog");
                            objs.push(RawObj {
                                num,
                                gen: 0,
                                body,
                                is_xref: false,
                                is_catalog,
                            });
                        }
                        pos = es + 9;
                        continue;
                    }
                }
                let mut body = dict;
                while body.last().map(|&b| is_ws(b)).unwrap_or(false) {
                    body.pop();
                }
                body.extend_from_slice(b"\nstream\n");
                body.extend_from_slice(data);
                body.extend_from_slice(b"\nendstream");
                let is_catalog = has_name(&body[..body.len().min(4096)], b"Type", b"Catalog");
                objs.push(RawObj {
                    num,
                    gen,
                    body,
                    is_xref,
                    is_catalog,
                });
                pos = es + 9;
            }
            None => {
                let (body_end, resume) = match end_obj {
                    Some(e) if e <= next => (e, e + 6),
                    _ if next < buf.len() => (next, next),
                    _ => {
                        // no endobj and nothing after: truncated object
                        truncated += 1;
                        break;
                    }
                };
                let body = buf[body0..body_end].to_vec();
                // trailer dictionaries before this object's header are
                // collected separately below
                let is_catalog = has_name(&body, b"Type", b"Catalog");
                objs.push(RawObj {
                    num,
                    gen,
                    body,
                    is_xref: false,
                    is_catalog,
                });
                pos = resume;
            }
        }
    }

    // classic trailers
    let tf = memmem::Finder::new(b"trailer");
    let mut at = start;
    while let Some(off) = tf.find(&buf[at..]) {
        let s = at + off + 7;
        at = s;
        let e = memmem::find(&buf[s..], b"startxref")
            .map(|o| s + o)
            .unwrap_or(buf.len().min(s + 4096));
        trailer_dicts.push(buf[s..e].to_vec());
    }

    if objs.is_empty() {
        return None;
    }

    // last definition wins
    let mut by_num: std::collections::BTreeMap<u32, usize> = Default::default();
    for (i, o) in objs.iter().enumerate() {
        if !o.is_xref {
            by_num.insert(o.num, i);
        }
    }
    let exists = |r: &[u8]| -> bool {
        let n: Option<u32> = std::str::from_utf8(r)
            .ok()
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse().ok());
        n.map(|n| by_num.contains_key(&n)).unwrap_or(false)
    };

    let mut root = None;
    let mut info = None;
    let mut id = None;
    let mut encrypt = None;
    for d in trailer_dicts.iter().rev() {
        if root.is_none() {
            if let Some(r) = raw_value(d, b"Root").filter(|r| exists(r)) {
                root = Some(r);
                info = raw_value(d, b"Info").filter(|r| exists(r));
                id = raw_value(d, b"ID").filter(|v| v.starts_with(b"["));
                encrypt = raw_value(d, b"Encrypt").filter(|r| exists(r));
            }
        }
    }
    let mut root_from_catalog_scan = false;
    if root.is_none() {
        let cat = by_num
            .values()
            .rev()
            .map(|&i| &objs[i])
            .find(|o| o.is_catalog)?;
        root = Some(format!("{} {} R", cat.num, cat.gen).into_bytes());
        root_from_catalog_scan = true;
    }

    let mut out = Vec::with_capacity(buf.len() + 1024);
    out.extend_from_slice(format!("%PDF-{version}\n").as_bytes());
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");
    let mut offsets: Vec<(u32, u16, usize)> = Vec::new();
    for &i in by_num.values() {
        let o = &objs[i];
        offsets.push((o.num, o.gen, out.len()));
        out.extend_from_slice(format!("{} {} obj\n", o.num, o.gen).as_bytes());
        out.extend_from_slice(&o.body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = by_num.keys().next_back().copied().unwrap_or(0) + 1;
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    let mut it = offsets.iter().peekable();
    for n in 1..size {
        match it.peek() {
            Some(&&(num, gen, off)) if num == n => {
                out.extend_from_slice(format!("{off:010} {gen:05} n \n").as_bytes());
                it.next();
            }
            _ => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(b"trailer\n<< /Size ");
    out.extend_from_slice(size.to_string().as_bytes());
    for (k, v) in [
        (&b"Root"[..], &root),
        (b"Info", &info),
        (b"ID", &id),
        (b"Encrypt", &encrypt),
    ] {
        if let Some(v) = v {
            out.extend_from_slice(b" /");
            out.extend_from_slice(k);
            out.push(b' ');
            out.extend_from_slice(v);
        }
    }
    out.extend_from_slice(format!(" >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes());
    Some(Scan {
        bytes: out,
        objects: by_num.len(),
        fixed_lengths,
        truncated,
        root_from_catalog_scan,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Object, Stream};

    /// A small n-page document with a text line per page, saved with a
    /// classic xref table and uncompressed streams.
    pub fn sample(n: usize) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let mut kids = vec![];
        for i in 0..n {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new(
                        "Tj",
                        vec![Object::string_literal(format!(
                            "Page {} of the sample",
                            i + 1
                        ))],
                    ),
                    Operation::new("ET", vec![]),
                    Operation::new("re", vec![72.into(), 72.into(), 200.into(), 100.into()]),
                    Operation::new("f", vec![]),
                ],
            };
            let c = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let p = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => c,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
            });
            kids.push(p.into());
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => n as i64 },
            ),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn texts(pdf: &[u8]) -> Vec<String> {
        let d = Document::load_mem(pdf).unwrap();
        d.get_pages()
            .keys()
            .map(|&n| d.extract_text(&[n]).unwrap_or_default())
            .collect()
    }

    fn check(out: &Repaired, n: usize) {
        let t = texts(&out.pdf);
        assert_eq!(t.len(), n, "{}", out.report);
        for (i, s) in t.iter().enumerate() {
            assert!(s.contains(&format!("Page {}", i + 1)), "page {i}: {s:?}");
        }
        assert!(unrenderable_pages(&out.pdf).unwrap().is_empty());
    }

    #[test]
    fn repairs_bad_xref_offsets() {
        let mut pdf = sample(3);
        // shift every xref offset by 7 bytes
        let x = memmem::rfind(&pdf, b"xref\n").unwrap();
        let tail = String::from_utf8_lossy(&pdf[x..]).into_owned();
        let bad: String = tail
            .lines()
            .map(|l| {
                let p: Vec<&str> = l.split(' ').collect();
                if p.len() >= 3 && p[0].len() == 10 && p[2] == "n" {
                    let off: usize = p[0].parse().unwrap();
                    format!("{:010} {} n ", off + 7, p[1])
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        pdf.truncate(x);
        pdf.extend_from_slice(bad.as_bytes());
        let out = repair(&pdf).unwrap();
        check(&out, 3);
    }

    #[test]
    fn repairs_wrong_stream_length() {
        let pdf = sample(2);
        let s = String::from_utf8_lossy(&pdf).into_owned();
        // every content stream gets a Length that is too short
        let mut out = String::new();
        let mut rest = s.as_str();
        while let Some(i) = rest.find("/Length ") {
            out.push_str(&rest[..i]);
            let after = &rest[i + 8..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            out.push_str("/Length 5");
            rest = &after[digits.len()..];
        }
        out.push_str(rest);
        let broken = out.into_bytes();
        let r = repair(&broken).unwrap();
        assert!(
            r.report.contains("Stream lengths corrected"),
            "{}",
            r.report
        );
        check(&r, 2);
    }

    #[test]
    fn repairs_truncated_tail() {
        let pdf = sample(3);
        // cut away the xref, the trailer and a bit before it
        let x = memmem::find(&pdf, b"xref").unwrap();
        let cut = &pdf[..x];
        let r = repair(cut).unwrap();
        assert!(r.report.contains("catalog was located"), "{}", r.report);
        check(&r, 3);
        // and a stream object cut off in the middle
        let mut half = cut.to_vec();
        half.extend_from_slice(b"\n50 0 obj\n<< /Length 99 >>\nstream\nabc");
        let r = repair(&half).unwrap();
        assert!(
            r.report.contains("truncated end of file (dropped): 1"),
            "{}",
            r.report
        );
        check(&r, 3);
    }

    #[test]
    fn missing_endobj_and_garbage() {
        let pdf = sample(2);
        let s = String::from_utf8_lossy(&pdf).replacen("endobj", "", 1);
        let mut b = s.into_bytes();
        let at = memmem::find(&b, b"1 0 obj").unwrap();
        b.splice(at..at, b"99 0 obj garbage << /Foo\n".iter().copied());
        let r = repair(&b).unwrap();
        check(&r, 2);
    }

    #[test]
    fn rejects_non_pdf() {
        assert!(repair(b"hello world").is_err());
    }
}
