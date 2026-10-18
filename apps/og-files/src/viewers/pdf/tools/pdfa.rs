//! PDF/A-2b conversion (best effort) plus a built-in verifier.
//! Replaces Stirling's Ghostscript-based pdf/pdfa endpoint.
//!
//! [`convert`] does, on the lopdf object graph:
//!   * header `%PDF-1.7` with a binary comment, trailer `/ID`, no
//!     encryption (password-protected input is refused)
//!   * an sRGB OutputIntent (`GTS_PDFA1`, ICC v2 profile generated in code)
//!     unless a PDF/A output intent exists; DeviceCMYK content then gets a
//!     `/DefaultCMYK` ICC colour space (bundled CC0 CGATS profile) in every
//!     resource dictionary
//!   * XMP metadata (`pdfaid:part 2`, `conformance B`, Dublin Core title /
//!     creator / description, keywords, producer, creator tool, dates)
//!     written uncompressed and kept in sync with the Info dictionary
//!   * removes JavaScript, Launch/Sound/Movie/ResetForm/ImportData/Hide/
//!     SetOCGState/Rendition/Trans/GoTo3DView actions and every `/AA`,
//!     embedded files (`/EmbeddedFiles`, file-attachment annotations,
//!     `/AF`), XFA, `NeedAppearances`, 3D/Sound/Movie/Screen/RichMedia
//!     annotations, image `/Alternates` / `/OPI` / `Interpolate true`,
//!     PostScript and reference XObjects, `TR`/`TR2` transfer functions,
//!     external stream data (`/F`), and LZW (re-encoded as Flate)
//!   * annotations get the Print flag (and lose Hidden/Invisible/NoView)
//!     and keep only the normal appearance; optional-content configs get a
//!     `/Name`
//!   * fonts: non-embedded standard-14 fonts — and TrueType Arial / Times
//!     New Roman / Courier New lookalikes — are embedded as Type 1 from the
//!     metric-compatible Foxit set hayro already compiles in, with
//!     `/Widths` taken from the embedded program, a full FontDescriptor and
//!     a ToUnicode CMap. Other non-embedded fonts can't be fixed and are
//!     reported.
//!
//! [`verify`] is an independent rule check of the result (and works on any
//! file). It covers the rules listed in [`COVERAGE`]; it is not a full
//! veraPDF profile — content-stream operator validity, glyph presence for
//! used codes, CIDSet/CharSet, TrueType cmap rules and XMP extension
//! schemas are not checked.

use lopdf::content::Content;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;

const MAX_STREAM: usize = 256 << 20;
const CMYK_ICC: &[u8] = include_bytes!("../../../../assets/icc/CGATS001Compat-v2-micro.icc");

pub struct Converted {
    pub pdf: Vec<u8>,
    pub report: String,
}

/// Rules [`verify`] checks (shown in the report).
pub const COVERAGE: &str = "header & version, trailer ID, encryption, XMP (pdfaid, \
    unfiltered, Info consistency), output intent & ICC header, device colours vs. output \
    intent, font embedding (simple + CID, CIDToGIDMap), Type 1 width consistency, \
    forbidden actions & additional actions, annotation types / flags / appearances, \
    NeedAppearances & XFA, embedded files, LZW & external streams, image Interpolate / \
    Alternates / OPI, PostScript & reference XObjects, transfer functions & rendering \
    intents, optional content configs, name/string length limits";

// ------------------------------------------------------------------ helpers

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

fn name<'a>(d: &'a Dictionary, k: &[u8]) -> Option<&'a [u8]> {
    d.get(k).and_then(Object::as_name).ok()
}

fn catalog_id(doc: &Document) -> Option<ObjectId> {
    doc.trailer.get(b"Root").and_then(Object::as_reference).ok()
}

fn flate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = e.write_all(data);
    e.finish().unwrap_or_default()
}

fn text_of(o: &Object) -> Option<String> {
    let b = o.as_str().ok()?;
    Some(if b.starts_with(&[0xfe, 0xff]) {
        let u: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u)
    } else {
        b.iter().map(|&c| c as char).collect()
    })
}

fn text_obj(s: &str) -> Object {
    if s.chars().all(|c| (c as u32) < 128) {
        Object::string_literal(s)
    } else {
        let mut b = vec![0xfe, 0xff];
        for u in s.encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        Object::String(b, StringFormat::Hexadecimal)
    }
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\t' || c == '\n' || c == '\r' || c >= ' ')
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

// ------------------------------------------------------------------ dates

/// "D:YYYYMMDDHHmmSSOHH'mm'" → ("YYYY-MM-DDTHH:mm:SS+HH:mm", normalized PDF date).
fn pdf_date_to_xmp(s: &str) -> Option<(String, String)> {
    let d = s.trim().trim_start_matches("D:");
    let digits: String = d.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return None;
    }
    let part = |a: usize, b: usize, def: &str| -> String {
        digits
            .get(a..b)
            .map(str::to_string)
            .unwrap_or_else(|| def.to_string())
    };
    let (y, mo, da, h, mi, se) = (
        part(0, 4, "0000"),
        part(4, 6, "01"),
        part(6, 8, "01"),
        part(8, 10, "00"),
        part(10, 12, "00"),
        part(12, 14, "00"),
    );
    let rest = &d[digits.len()..];
    let (tz_x, tz_p) = match rest.chars().next() {
        Some(c @ ('+' | '-')) => {
            let t: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
            let hh = t.get(0..2).unwrap_or("00").to_string();
            let mm = t.get(2..4).unwrap_or("00").to_string();
            (format!("{c}{hh}:{mm}"), format!("{c}{hh}'{mm}'"))
        }
        _ => ("Z".to_string(), "Z".to_string()),
    };
    Some((
        format!("{y}-{mo}-{da}T{h}:{mi}:{se}{tz_x}"),
        format!("D:{y}{mo}{da}{h}{mi}{se}{tz_p}"),
    ))
}

fn now_pdf_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match der::DateTime::from_unix_duration(std::time::Duration::from_secs(secs)) {
        Ok(d) => format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}+00'00'",
            d.year(),
            d.month(),
            d.day(),
            d.hour(),
            d.minutes(),
            d.seconds()
        ),
        Err(_) => "D:19700101000000+00'00'".into(),
    }
}

// ------------------------------------------------------------------ ICC

/// Compact ICC v2 sRGB display profile (D50 PCS).
pub(super) fn srgb_icc() -> Vec<u8> {
    fn s15(v: f64) -> [u8; 4] {
        ((v * 65536.0).round() as i32).to_be_bytes()
    }
    fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
        let mut v = b"XYZ \0\0\0\0".to_vec();
        v.extend(s15(x));
        v.extend(s15(y));
        v.extend(s15(z));
        v
    }
    fn desc(s: &str) -> Vec<u8> {
        let mut v = b"desc\0\0\0\0".to_vec();
        v.extend(((s.len() + 1) as u32).to_be_bytes());
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        v.extend([0u8; 4 + 4]); // unicode language + count
        v.extend([0u8; 2 + 1 + 67]); // scriptcode code, count, data
        v
    }
    fn text(s: &str) -> Vec<u8> {
        let mut v = b"text\0\0\0\0".to_vec();
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        v
    }
    let mut curve = b"curv\0\0\0\0".to_vec();
    let n = 1024u32;
    curve.extend(n.to_be_bytes());
    for i in 0..n {
        let x = i as f64 / (n - 1) as f64;
        let y = if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        };
        curve.extend(((y * 65535.0).round() as u16).to_be_bytes());
    }
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"desc", desc("sRGB IEC61966-2.1 (og-files)")),
        (b"cprt", text("No copyright, use freely")),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rXYZ", xyz(0.436_074_7, 0.222_504_5, 0.013_932_2)),
        (b"gXYZ", xyz(0.385_064_9, 0.716_878_6, 0.097_104_5)),
        (b"bXYZ", xyz(0.143_080_4, 0.060_616_9, 0.714_173_3)),
        (b"rTRC", curve.clone()),
        (b"gTRC", curve.clone()),
        (b"bTRC", curve),
    ];
    let table_len = 4 + 12 * tags.len();
    let mut data = Vec::new();
    let mut entries = Vec::new();
    let mut shared: Option<(u32, u32)> = None;
    for (sig, body) in &tags {
        let off = (128 + table_len + data.len()) as u32;
        let (off, len) = if sig.ends_with(b"TRC") {
            if let Some(s) = shared {
                s
            } else {
                shared = Some((off, body.len() as u32));
                data.extend_from_slice(body);
                (off, body.len() as u32)
            }
        } else {
            data.extend_from_slice(body);
            (off, body.len() as u32)
        };
        while data.len() % 4 != 0 {
            data.push(0);
        }
        entries.push((*sig, off, len));
    }
    let total = 128 + table_len + data.len();
    let mut h = Vec::with_capacity(total);
    h.extend((total as u32).to_be_bytes());
    h.extend_from_slice(b"none"); // preferred CMM: none
    h.extend([0x02, 0x10, 0, 0]); // version 2.1
    h.extend_from_slice(b"mntr");
    h.extend_from_slice(b"RGB ");
    h.extend_from_slice(b"XYZ ");
    h.extend([0u8; 12]); // date
    h.extend_from_slice(b"acsp");
    h.extend([0u8; 4]); // platform
    h.extend([0u8; 4]); // flags
    h.extend([0u8; 4]); // manufacturer
    h.extend([0u8; 4]); // model
    h.extend([0u8; 8]); // attributes
    h.extend([0u8; 4]); // intent: perceptual
    h.extend(s15(0.9642));
    h.extend(s15(1.0));
    h.extend(s15(0.8249));
    h.extend([0u8; 4]); // creator
    h.resize(128, 0);
    h.extend((tags.len() as u32).to_be_bytes());
    for (sig, off, len) in entries {
        h.extend_from_slice(sig);
        h.extend(off.to_be_bytes());
        h.extend(len.to_be_bytes());
    }
    h.extend(data);
    h
}

/// (colour space signature, device class, major version) from an ICC header.
fn icc_header(p: &[u8]) -> Option<([u8; 4], [u8; 4], u8)> {
    if p.len() < 128 || &p[36..40] != b"acsp" {
        return None;
    }
    Some((p[16..20].try_into().ok()?, p[12..16].try_into().ok()?, p[8]))
}

// ------------------------------------------------------------------ Type 1

/// Metrics and encoding of an embedded font program (Type 1 or CFF).
pub(super) struct FontProgram {
    pub widths: HashMap<String, f32>,
    /// built-in encoding (None = StandardEncoding)
    pub encoding: Option<Vec<Option<String>>>,
    pub bbox: [f32; 4],
    pub italic_angle: f32,
}

fn decrypt(data: &[u8], mut r: u16, skip: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = (c as u16)
            .wrapping_add(r)
            .wrapping_mul(52845)
            .wrapping_add(22719);
    }
    out.into_iter().skip(skip).collect()
}

fn ws(b: u8) -> bool {
    matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | 0)
}

/// Advance widths per glyph name from the eexec-encrypted part.
fn charstring_widths(binary: &[u8]) -> HashMap<String, f32> {
    let p = decrypt(binary, 55665, 4);
    let mut out = HashMap::new();
    let len_iv = memchr::memmem::find(&p, b"/lenIV")
        .and_then(|i| {
            let s: String = p[i + 6..]
                .iter()
                .skip_while(|&&b| ws(b))
                .take_while(|b| b.is_ascii_digit() || **b == b'-')
                .map(|&b| b as char)
                .collect();
            s.parse::<i64>().ok()
        })
        .unwrap_or(4);
    let Some(cs) = memchr::memmem::find(&p, b"/CharStrings") else {
        return out;
    };
    let Some(b) = memchr::memmem::find(&p[cs..], b"begin") else {
        return out;
    };
    let mut i = cs + b + 5;
    let tok = |i: &mut usize| -> Vec<u8> {
        while *i < p.len() && ws(p[*i]) {
            *i += 1;
        }
        let s = *i;
        while *i < p.len() && !ws(p[*i]) && (*i == s || p[*i] != b'/') {
            *i += 1;
        }
        p[s..*i].to_vec()
    };
    for _ in 0..20000 {
        let t = tok(&mut i);
        if t.is_empty() || t == b"end" {
            break;
        }
        if t[0] != b'/' {
            continue; // ND, |-, noaccess …
        }
        let gname = String::from_utf8_lossy(&t[1..]).into_owned();
        let n: usize = match String::from_utf8_lossy(&tok(&mut i)).parse() {
            Ok(n) => n,
            Err(_) => break,
        };
        let _rd = tok(&mut i);
        i += 1; // single space after RD
        let Some(enc) = p.get(i..i + n) else { break };
        i += n;
        let cs = decrypt(enc, 4330, len_iv.max(0) as usize);
        let mut st: Vec<f32> = Vec::new();
        let mut j = 0;
        while j < cs.len() {
            let v = cs[j];
            if v >= 32 {
                let (num, step) = match v {
                    32..=246 => (v as i32 - 139, 1),
                    247..=250 => (
                        ((v as i32 - 247) * 256) + *cs.get(j + 1).unwrap_or(&0) as i32 + 108,
                        2,
                    ),
                    251..=254 => (
                        -((v as i32 - 251) * 256) - *cs.get(j + 1).unwrap_or(&0) as i32 - 108,
                        2,
                    ),
                    _ => (
                        cs.get(j + 1..j + 5)
                            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                            .unwrap_or(0),
                        5,
                    ),
                };
                st.push(num as f32);
                j += step;
                continue;
            }
            if v == 13 && st.len() >= 2 {
                out.insert(gname.clone(), st[1]);
            } else if v == 12 && cs.get(j + 1) == Some(&7) && st.len() >= 3 {
                out.insert(gname.clone(), st[2]);
            }
            break;
        }
    }
    out
}

pub(super) fn parse_type1(clear: &[u8], binary: &[u8]) -> FontProgram {
    let text = String::from_utf8_lossy(clear).into_owned();
    let nums_after = |key: &str| -> Vec<f32> {
        text.find(key)
            .map(|i| {
                text[i + key.len()..]
                    .trim_start()
                    .trim_start_matches(['{', '['])
                    .split(|c: char| c == '}' || c == ']' || c == '\n')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let bb = nums_after("/FontBBox");
    let bbox = if bb.len() == 4 {
        [bb[0], bb[1], bb[2], bb[3]]
    } else {
        [0.0, -200.0, 1000.0, 900.0]
    };
    let italic_angle = nums_after("/ItalicAngle").first().copied().unwrap_or(0.0);
    let encoding = if text.contains("/Encoding StandardEncoding") {
        None
    } else {
        let mut enc: Vec<Option<String>> = vec![None; 256];
        let mut rest = text.as_str();
        while let Some(i) = rest.find("dup ") {
            rest = &rest[i + 4..];
            let mut it = rest.split_whitespace();
            if let (Some(code), Some(n), Some("put")) = (it.next(), it.next(), it.next()) {
                if let (Ok(c), Some(n)) = (code.parse::<usize>(), n.strip_prefix('/')) {
                    if c < 256 {
                        enc[c] = Some(n.to_string());
                    }
                }
            }
        }
        Some(enc)
    };
    let widths = charstring_widths(binary);
    FontProgram {
        widths,
        encoding,
        bbox,
        italic_angle,
    }
}

// ------------------------------------------------------------------ CFF

fn be(d: &[u8], at: usize, n: usize) -> Option<usize> {
    let b = d.get(at..at + n)?;
    Some(b.iter().fold(0usize, |a, &x| (a << 8) | x as usize))
}

/// CFF INDEX at `pos`: items and the position after it.
fn cff_index(d: &[u8], pos: usize) -> Option<(Vec<&[u8]>, usize)> {
    let count = be(d, pos, 2)?;
    if count == 0 {
        return Some((vec![], pos + 2));
    }
    let os = *d.get(pos + 2)? as usize;
    if !(1..=4).contains(&os) {
        return None;
    }
    let offs: Vec<usize> = (0..=count)
        .map(|i| be(d, pos + 3 + i * os, os))
        .collect::<Option<_>>()?;
    let base = pos + 3 + (count + 1) * os - 1;
    let items = offs
        .windows(2)
        .map(|w| d.get(base + w[0]..base + w[1]))
        .collect::<Option<Vec<_>>>()?;
    Some((items, base + offs[count]))
}

fn cff_real(d: &[u8], i: &mut usize) -> f64 {
    let mut s = String::new();
    *i += 1;
    while *i < d.len() {
        let byte = d[*i];
        *i += 1;
        for nib in [byte >> 4, byte & 15] {
            match nib {
                0..=9 => s.push((b'0' + nib) as char),
                10 => s.push('.'),
                11 => s.push('E'),
                12 => s.push_str("E-"),
                14 => s.push('-'),
                15 => return s.parse().unwrap_or(0.0),
                _ => {}
            }
        }
    }
    s.parse().unwrap_or(0.0)
}

/// CFF DICT: operator (12 x → 1200 + x) → operands.
fn cff_dict(d: &[u8]) -> HashMap<u16, Vec<f64>> {
    let mut out = HashMap::new();
    let mut st: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        match b {
            0..=21 => {
                let op = if b == 12 {
                    i += 1;
                    1200 + *d.get(i).unwrap_or(&0) as u16
                } else {
                    b as u16
                };
                out.insert(op, std::mem::take(&mut st));
                i += 1;
            }
            28 => {
                st.push(be(d, i + 1, 2).unwrap_or(0) as i16 as f64);
                i += 3;
            }
            29 => {
                st.push(be(d, i + 1, 4).unwrap_or(0) as i32 as f64);
                i += 5;
            }
            30 => st.push(cff_real(d, &mut i)),
            32..=246 => {
                st.push(b as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                st.push(((b as f64 - 247.0) * 256.0) + *d.get(i + 1).unwrap_or(&0) as f64 + 108.0);
                i += 2;
            }
            251..=254 => {
                st.push(-((b as f64 - 251.0) * 256.0) - *d.get(i + 1).unwrap_or(&0) as f64 - 108.0);
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

fn subr_bias(n: usize) -> i64 {
    if n < 1240 {
        107
    } else if n < 33900 {
        1131
    } else {
        32768
    }
}

/// Advance width encoded in a Type 2 charstring (None = the default
/// width). Follows subroutine calls up to the first stack-clearing
/// operator.
fn t2_width(cs: &[u8], local: &[&[u8]], global: &[&[u8]]) -> Option<f64> {
    fn run(
        cs: &[u8],
        st: &mut Vec<f64>,
        local: &[&[u8]],
        global: &[&[u8]],
        depth: usize,
    ) -> Result<Option<Option<f64>>, ()> {
        // Ok(None) = returned from a subroutine, keep going
        if depth > 10 {
            return Err(());
        }
        let mut i = 0;
        while i < cs.len() {
            let b = cs[i];
            let v = match b {
                28 => {
                    let v = be(cs, i + 1, 2).ok_or(())? as i16 as f64;
                    i += 3;
                    v
                }
                32..=246 => {
                    i += 1;
                    b as f64 - 139.0
                }
                247..=250 => {
                    let v = ((b as f64 - 247.0) * 256.0) + *cs.get(i + 1).ok_or(())? as f64 + 108.0;
                    i += 2;
                    v
                }
                251..=254 => {
                    let v =
                        -((b as f64 - 251.0) * 256.0) - *cs.get(i + 1).ok_or(())? as f64 - 108.0;
                    i += 2;
                    v
                }
                255 => {
                    let v = be(cs, i + 1, 4).ok_or(())? as i32 as f64 / 65536.0;
                    i += 5;
                    v
                }
                10 | 29 => {
                    let idx = st.pop().ok_or(())? as i64;
                    let subrs = if b == 10 { local } else { global };
                    let k = idx + subr_bias(subrs.len());
                    let sub = subrs.get(usize::try_from(k).map_err(|_| ())?).ok_or(())?;
                    if let Some(r) = run(sub, st, local, global, depth + 1)? {
                        return Ok(Some(r));
                    }
                    i += 1;
                    continue;
                }
                11 => return Ok(None), // return
                _ => {
                    let n = st.len();
                    let has = match b {
                        1 | 3 | 18 | 23 | 19 | 20 => n % 2 == 1,
                        21 => n > 2,
                        4 | 22 => n > 1,
                        14 => n == 1 || n == 5,
                        _ => return Err(()),
                    };
                    return Ok(Some(if has { st.first().copied() } else { None }));
                }
            };
            st.push(v);
        }
        Ok(None)
    }
    let mut st = Vec::new();
    match run(cs, &mut st, local, global, 0) {
        Ok(Some(w)) => w,
        _ => None,
    }
}

/// Metrics of a bare CFF (Type1C) font.
pub(super) fn parse_cff(d: &[u8]) -> Option<FontProgram> {
    use read_fonts::ps::string::Sid;
    let hdr = *d.get(2)? as usize;
    let (_names, p) = cff_index(d, hdr)?;
    let (tops, p) = cff_index(d, p)?;
    let (strings, p) = cff_index(d, p)?;
    let (global, _) = cff_index(d, p).unwrap_or_default();
    let top = cff_dict(tops.first()?);
    let name_of = |sid: usize| -> Option<String> {
        match Sid::new(sid as u16).resolve_standard() {
            Ok(s) => Some(String::from_utf8_lossy(s).into_owned()),
            Err(i) => strings
                .get(i)
                .map(|s| String::from_utf8_lossy(s).into_owned()),
        }
    };
    let cs_off = *top.get(&17)?.first()? as usize;
    let (charstrings, _) = cff_index(d, cs_off)?;
    let n = charstrings.len();
    let (mut dflt, mut nominal) = (0.0, 0.0);
    let mut local: Vec<&[u8]> = Vec::new();
    if let Some(pr) = top.get(&18).filter(|v| v.len() == 2) {
        let (size, off) = (pr[0] as usize, pr[1] as usize);
        if let Some(pd) = d.get(off..off + size) {
            let pd = cff_dict(pd);
            dflt = pd.get(&20).and_then(|v| v.first().copied()).unwrap_or(0.0);
            nominal = pd.get(&21).and_then(|v| v.first().copied()).unwrap_or(0.0);
            if let Some(lo) = pd.get(&19).and_then(|v| v.first()) {
                if let Some((l, _)) = cff_index(d, off + *lo as usize) {
                    local = l;
                }
            }
        }
    }
    // glyph id → SID
    let mut sids = vec![0usize; n];
    match top
        .get(&15)
        .and_then(|v| v.first())
        .map(|&x| x as usize)
        .unwrap_or(0)
    {
        0 => {
            for (g, s) in sids.iter_mut().enumerate() {
                *s = g;
            }
        }
        1 | 2 => return None, // expert charsets: not a text font
        off => {
            let fmt = *d.get(off)?;
            let mut g = 1;
            let mut at = off + 1;
            while g < n {
                match fmt {
                    0 => {
                        sids[g] = be(d, at, 2)?;
                        at += 2;
                        g += 1;
                    }
                    1 | 2 => {
                        let first = be(d, at, 2)?;
                        let left = if fmt == 1 {
                            be(d, at + 2, 1)?
                        } else {
                            be(d, at + 2, 2)?
                        };
                        at += if fmt == 1 { 3 } else { 4 };
                        for k in 0..=left {
                            if g + k < n {
                                sids[g + k] = first + k;
                            }
                        }
                        g += left + 1;
                    }
                    _ => return None,
                }
            }
        }
    }
    let names: Vec<Option<String>> = sids.iter().map(|&s| name_of(s)).collect();
    let mut widths = HashMap::new();
    for (g, cs) in charstrings.iter().enumerate() {
        let w = t2_width(cs, &local, &global)
            .map(|w| w + nominal)
            .unwrap_or(dflt);
        if let Some(nm) = &names[g] {
            widths.insert(nm.clone(), w as f32);
        }
    }
    // built-in encoding
    let encoding = match top
        .get(&16)
        .and_then(|v| v.first())
        .map(|&x| x as usize)
        .unwrap_or(0)
    {
        0 | 1 => None,
        off => {
            let mut enc: Vec<Option<String>> = vec![None; 256];
            let fmt = *d.get(off)?;
            let mut at = off + 1;
            match fmt & 0x7f {
                0 => {
                    let nc = *d.get(at)? as usize;
                    at += 1;
                    for k in 0..nc {
                        let code = *d.get(at + k)? as usize;
                        enc[code] = names.get(k + 1).cloned().flatten();
                    }
                    at += nc;
                }
                1 => {
                    let nr = *d.get(at)? as usize;
                    at += 1;
                    let mut g = 1;
                    for _ in 0..nr {
                        let first = *d.get(at)? as usize;
                        let left = *d.get(at + 1)? as usize;
                        at += 2;
                        for k in 0..=left {
                            if first + k < 256 {
                                enc[first + k] = names.get(g).cloned().flatten();
                            }
                            g += 1;
                        }
                    }
                }
                _ => return None,
            }
            if fmt & 0x80 != 0 {
                let ns = *d.get(at)? as usize;
                at += 1;
                for k in 0..ns {
                    let code = *d.get(at + k * 3)? as usize;
                    let sid = be(d, at + k * 3 + 1, 2)?;
                    enc[code] = name_of(sid);
                }
            }
            Some(enc)
        }
    };
    let bb = top.get(&5).cloned().unwrap_or_default();
    let bbox = if bb.len() == 4 {
        [bb[0] as f32, bb[1] as f32, bb[2] as f32, bb[3] as f32]
    } else {
        [0.0, -200.0, 1000.0, 900.0]
    };
    let italic_angle = top
        .get(&1202)
        .and_then(|v| v.first().copied())
        .unwrap_or(0.0) as f32;
    Some(FontProgram {
        widths,
        encoding,
        bbox,
        italic_angle,
    })
}

// ------------------------------------------------------------------ encodings

const ASCII_NAMES: [&str; 95] = [
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
];

const LATIN1_HIGH: [&str; 96] = [
    "space",
    "exclamdown",
    "cent",
    "sterling",
    "currency",
    "yen",
    "brokenbar",
    "section",
    "dieresis",
    "copyright",
    "ordfeminine",
    "guillemotleft",
    "logicalnot",
    "hyphen",
    "registered",
    "macron",
    "degree",
    "plusminus",
    "twosuperior",
    "threesuperior",
    "acute",
    "mu",
    "paragraph",
    "periodcentered",
    "cedilla",
    "onesuperior",
    "ordmasculine",
    "guillemotright",
    "onequarter",
    "onehalf",
    "threequarters",
    "questiondown",
    "Agrave",
    "Aacute",
    "Acircumflex",
    "Atilde",
    "Adieresis",
    "Aring",
    "AE",
    "Ccedilla",
    "Egrave",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Igrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Eth",
    "Ntilde",
    "Ograve",
    "Oacute",
    "Ocircumflex",
    "Otilde",
    "Odieresis",
    "multiply",
    "Oslash",
    "Ugrave",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Yacute",
    "Thorn",
    "germandbls",
    "agrave",
    "aacute",
    "acircumflex",
    "atilde",
    "adieresis",
    "aring",
    "ae",
    "ccedilla",
    "egrave",
    "eacute",
    "ecircumflex",
    "edieresis",
    "igrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "eth",
    "ntilde",
    "ograve",
    "oacute",
    "ocircumflex",
    "otilde",
    "odieresis",
    "divide",
    "oslash",
    "ugrave",
    "uacute",
    "ucircumflex",
    "udieresis",
    "yacute",
    "thorn",
    "ydieresis",
];

const WIN_EXTRA: [(u8, &str); 27] = [
    (128, "Euro"),
    (130, "quotesinglbase"),
    (131, "florin"),
    (132, "quotedblbase"),
    (133, "ellipsis"),
    (134, "dagger"),
    (135, "daggerdbl"),
    (136, "circumflex"),
    (137, "perthousand"),
    (138, "Scaron"),
    (139, "guilsinglleft"),
    (140, "OE"),
    (142, "Zcaron"),
    (145, "quoteleft"),
    (146, "quoteright"),
    (147, "quotedblleft"),
    (148, "quotedblright"),
    (149, "bullet"),
    (150, "endash"),
    (151, "emdash"),
    (152, "tilde"),
    (153, "trademark"),
    (154, "scaron"),
    (155, "guilsinglright"),
    (156, "oe"),
    (158, "zcaron"),
    (159, "Ydieresis"),
];

const STD_HIGH: [(u8, &str); 56] = [
    (161, "exclamdown"),
    (162, "cent"),
    (163, "sterling"),
    (164, "fraction"),
    (165, "yen"),
    (166, "florin"),
    (167, "section"),
    (168, "currency"),
    (169, "quotesingle"),
    (170, "quotedblleft"),
    (171, "guillemotleft"),
    (172, "guilsinglleft"),
    (173, "guilsinglright"),
    (174, "fi"),
    (175, "fl"),
    (177, "endash"),
    (178, "dagger"),
    (179, "daggerdbl"),
    (180, "periodcentered"),
    (182, "paragraph"),
    (183, "bullet"),
    (184, "quotesinglbase"),
    (185, "quotedblbase"),
    (186, "quotedblright"),
    (187, "guillemotright"),
    (188, "ellipsis"),
    (189, "perthousand"),
    (191, "questiondown"),
    (193, "grave"),
    (194, "acute"),
    (195, "circumflex"),
    (196, "tilde"),
    (197, "macron"),
    (198, "breve"),
    (199, "dotaccent"),
    (200, "dieresis"),
    (202, "ring"),
    (203, "cedilla"),
    (205, "hungarumlaut"),
    (206, "ogonek"),
    (207, "caron"),
    (208, "emdash"),
    (225, "AE"),
    (227, "ordfeminine"),
    (232, "Lslash"),
    (233, "Oslash"),
    (234, "OE"),
    (235, "ordmasculine"),
    (241, "ae"),
    (245, "dotlessi"),
    (248, "lslash"),
    (249, "oslash"),
    (250, "oe"),
    (251, "germandbls"),
    (39, "quoteright"),
    (96, "quoteleft"),
];

fn win_ansi() -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = vec![None; 256];
    for (i, n) in ASCII_NAMES.iter().enumerate() {
        v[32 + i] = Some(n.to_string());
    }
    for (i, n) in LATIN1_HIGH.iter().enumerate() {
        v[160 + i] = Some(n.to_string());
    }
    for (c, n) in WIN_EXTRA {
        v[c as usize] = Some(n.to_string());
    }
    v
}

fn standard() -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = vec![None; 256];
    for (i, n) in ASCII_NAMES.iter().enumerate() {
        v[32 + i] = Some(n.to_string());
    }
    for (c, n) in STD_HIGH {
        v[c as usize] = Some(n.to_string());
    }
    v
}

/// Glyph names per code for a simple font, given the program's built-in
/// encoding.
fn font_encoding(
    doc: &Document,
    font: &Dictionary,
    builtin: &Option<Vec<Option<String>>>,
) -> Vec<Option<String>> {
    let base_of = |n: &[u8]| -> Option<Vec<Option<String>>> {
        match n {
            b"WinAnsiEncoding" => Some(win_ansi()),
            b"StandardEncoding" => Some(standard()),
            // MacRoman: ASCII part is shared; high codes fall back to Standard names
            b"MacRomanEncoding" => Some(standard()),
            _ => None,
        }
    };
    let default = || builtin.clone().unwrap_or_else(standard);
    match font.get(b"Encoding").map(|e| deref(doc, e)) {
        Ok(Object::Name(n)) => base_of(n).unwrap_or_else(default),
        Ok(Object::Dictionary(e)) => {
            let mut v = name(e, b"BaseEncoding")
                .and_then(base_of)
                .unwrap_or_else(default);
            if let Ok(Object::Array(diffs)) = e.get(b"Differences").map(|d| deref(doc, d)) {
                let mut code = 0usize;
                for d in diffs {
                    match d {
                        Object::Integer(c) => code = (*c).clamp(0, 255) as usize,
                        Object::Name(n) => {
                            if code < 256 {
                                v[code] = Some(String::from_utf8_lossy(n).into_owned());
                            }
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
            v
        }
        _ => default(),
    }
}

// ------------------------------------------------------------------ fonts

use hayro::hayro_interpret::font::StandardFont;

/// Standard-14 replacement for a font name (subset prefix stripped).
fn standard_for(base: &str, flags: i64) -> Option<StandardFont> {
    let b = base
        .split_once('+')
        .map(|(p, r)| if p.len() == 6 { r } else { base })
        .unwrap_or(base);
    let l = b.to_ascii_lowercase().replace([' ', '_'], "");
    let bold = l.contains("bold") || l.contains("black") || l.contains("heavy");
    let italic = l.contains("italic") || l.contains("oblique") || flags & 64 != 0;
    use StandardFont::*;
    let fam = if l.starts_with("helvetica") || l.starts_with("arial") && !l.contains("unicode") {
        0
    } else if l.starts_with("times") {
        1
    } else if l.starts_with("courier") {
        2
    } else if l == "symbol" || l.starts_with("symbol,") || l == "symbolmt" {
        return Some(Symbol);
    } else if l.starts_with("zapfdingbats") || l == "dingbats" {
        return Some(ZapfDingBats);
    } else {
        return None;
    };
    Some(match (fam, bold, italic) {
        (0, false, false) => Helvetica,
        (0, true, false) => HelveticaBold,
        (0, false, true) => HelveticaOblique,
        (0, true, true) => HelveticaBoldOblique,
        (1, false, false) => TimesRoman,
        (1, true, false) => TimesBold,
        (1, false, true) => TimesItalic,
        (1, true, true) => TimesBoldItalic,
        (2, false, false) => Courier,
        (2, true, false) => CourierBold,
        (2, false, true) => CourierOblique,
        _ => CourierBoldOblique,
    })
}

/// (Flags, Ascent, Descent, CapHeight, XHeight, StemV) from the AFM data
/// of the standard fonts.
fn descriptor_metrics(f: StandardFont) -> (i64, i64, i64, i64, i64, i64) {
    use StandardFont::*;
    let italic = matches!(
        f,
        HelveticaOblique
            | HelveticaBoldOblique
            | TimesItalic
            | TimesBoldItalic
            | CourierOblique
            | CourierBoldOblique
    );
    let it = if italic { 64 } else { 0 };
    match f {
        Helvetica | HelveticaOblique => (32 + it, 718, -207, 718, 523, 88),
        HelveticaBold | HelveticaBoldOblique => (32 + it + 262144, 718, -207, 718, 532, 140),
        TimesRoman | TimesItalic => (34 + it, 683, -217, 662, 450, 84),
        TimesBold | TimesBoldItalic => (34 + it + 262144, 683, -217, 676, 461, 139),
        Courier | CourierOblique => (35 + it, 629, -157, 562, 426, 51),
        CourierBold | CourierBoldOblique => (35 + it + 262144, 629, -157, 562, 439, 106),
        Symbol => (4, 1010, -293, 1010, 0, 85),
        ZapfDingBats => (4, 820, -143, 820, 0, 90),
    }
}

fn std_key(f: StandardFont) -> &'static str {
    f.postscript_name()
}

struct Embedded {
    font_file: ObjectId,
    program: FontProgram,
}

/// Embed missing fonts; returns (embedded font names, unfixable names).
fn embed_fonts(doc: &mut Document) -> (Vec<String>, Vec<String>) {
    let mut cache: HashMap<&'static str, Embedded> = HashMap::new();
    let mut done = Vec::new();
    let mut failed = Vec::new();
    let ids: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, o)| {
            o.as_dict()
                .map(|d| {
                    name(d, b"Type") == Some(b"Font") || (d.has(b"BaseFont") && d.has(b"Subtype"))
                })
                .unwrap_or(false)
        })
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        let Ok(font) = doc.get_dictionary(id).cloned() else {
            continue;
        };
        let sub = name(&font, b"Subtype").unwrap_or(b"").to_vec();
        if !matches!(sub.as_slice(), b"Type1" | b"MMType1" | b"TrueType") {
            continue;
        }
        let desc = font
            .get(b"FontDescriptor")
            .ok()
            .map(|d| deref(doc, d).clone());
        let desc_d = desc.as_ref().and_then(|d| d.as_dict().ok());
        let embedded = desc_d
            .map(|d| d.has(b"FontFile") || d.has(b"FontFile2") || d.has(b"FontFile3"))
            .unwrap_or(false);
        if embedded {
            continue;
        }
        let base = name(&font, b"BaseFont")
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        let flags = desc_d
            .and_then(|d| d.get(b"Flags").and_then(Object::as_i64).ok())
            .unwrap_or(0);
        let Some(std) = standard_for(&base, flags) else {
            failed.push(base);
            continue;
        };
        let symbolic_std = matches!(std, StandardFont::Symbol | StandardFont::ZapfDingBats);
        // TrueType symbolic fonts (no encoding) can't be remapped to Type 1
        if sub == b"TrueType" && !symbolic_std && !font.has(b"Encoding") && flags & 4 != 0 {
            failed.push(base);
            continue;
        }
        let key = std_key(std);
        if !cache.contains_key(key) {
            let (data, _) = std.get_font_data();
            let bytes: &[u8] = (*data).as_ref();
            // hayro's Foxit standard fonts are bare CFF (Type1C) programs
            let Some(program) = parse_cff(bytes) else {
                failed.push(base);
                continue;
            };
            let mut s = Stream::new(
                dictionary! { "Subtype" => "Type1C", "Filter" => "FlateDecode" },
                flate(bytes),
            );
            s.allows_compression = false;
            let font_file = doc.add_object(s);
            cache.insert(key, Embedded { font_file, program });
        }
        let e = &cache[key];
        let enc = font_encoding(doc, &font, &e.program.encoding);
        let notdef = e.program.widths.get(".notdef").copied().unwrap_or(0.0);
        let first = font
            .get(b"FirstChar")
            .and_then(Object::as_i64)
            .unwrap_or(0)
            .clamp(0, 255);
        let last = font
            .get(b"LastChar")
            .and_then(Object::as_i64)
            .unwrap_or(255)
            .clamp(first, 255);
        let widths: Vec<Object> = (first..=last)
            .map(|c| {
                let w = enc[c as usize]
                    .as_ref()
                    .and_then(|n| e.program.widths.get(n))
                    .copied()
                    .unwrap_or(notdef);
                Object::Integer(w.round() as i64)
            })
            .collect();
        let (fl, asc, dsc, cap, xh, stem) = descriptor_metrics(std);
        let b = e.program.bbox;
        let fd = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => Object::Name(key.as_bytes().to_vec()),
            "Flags" => fl,
            "FontBBox" => vec![b[0].into(), b[1].into(), b[2].into(), b[3].into()],
            "ItalicAngle" => Object::Real(e.program.italic_angle),
            "Ascent" => asc,
            "Descent" => dsc,
            "CapHeight" => cap,
            "XHeight" => xh,
            "StemV" => stem,
            "FontFile3" => e.font_file,
        });
        let mut f = font.clone();
        f.set("Type", "Font");
        f.set("Subtype", "Type1");
        f.set("BaseFont", Object::Name(key.as_bytes().to_vec()));
        f.set("FirstChar", first);
        f.set("LastChar", last);
        f.set("Widths", widths);
        f.set("FontDescriptor", fd);
        if sub == b"TrueType" && symbolic_std {
            f.remove(b"Encoding");
        }
        // ToUnicode for text fonts
        if !symbolic_std && !f.has(b"ToUnicode") {
            let mut entries = Vec::new();
            for c in first..=last {
                if let Some(u) = enc[c as usize].as_deref().and_then(glyph_unicode) {
                    entries.push((c as u8, u));
                }
            }
            if !entries.is_empty() {
                let tu = doc.add_object(Stream::new(dictionary! {}, to_unicode_cmap(&entries)));
                f.set("ToUnicode", tu);
            }
        }
        doc.objects.insert(id, Object::Dictionary(f));
        done.push(if base.is_empty() {
            key.to_string()
        } else {
            base
        });
    }
    (done, failed)
}

/// Unicode for the glyph names of the tables above (+ uniXXXX).
fn glyph_unicode(n: &str) -> Option<char> {
    if let Some(h) = n.strip_prefix("uni").filter(|h| h.len() == 4) {
        return u32::from_str_radix(h, 16).ok().and_then(char::from_u32);
    }
    if let Some(i) = ASCII_NAMES.iter().position(|&a| a == n) {
        return char::from_u32(32 + i as u32);
    }
    if n == "quoteright" {
        return Some('\u{2019}');
    }
    if n == "quoteleft" {
        return Some('\u{2018}');
    }
    if let Some(i) = LATIN1_HIGH.iter().position(|&a| a == n) {
        return char::from_u32(160 + i as u32);
    }
    Some(match n {
        "Euro" => '€',
        "quotesinglbase" => '‚',
        "florin" => 'ƒ',
        "quotedblbase" => '„',
        "ellipsis" => '…',
        "dagger" => '†',
        "daggerdbl" => '‡',
        "circumflex" => 'ˆ',
        "perthousand" => '‰',
        "Scaron" => 'Š',
        "guilsinglleft" => '‹',
        "OE" => 'Œ',
        "Zcaron" => 'Ž',
        "quotedblleft" => '“',
        "quotedblright" => '”',
        "bullet" => '•',
        "endash" => '–',
        "emdash" => '—',
        "tilde" => '˜',
        "trademark" => '™',
        "scaron" => 'š',
        "guilsinglright" => '›',
        "oe" => 'œ',
        "zcaron" => 'ž',
        "Ydieresis" => 'Ÿ',
        "fraction" => '⁄',
        "fi" => '\u{FB01}',
        "fl" => '\u{FB02}',
        "breve" => '˘',
        "dotaccent" => '˙',
        "ring" => '˚',
        "hungarumlaut" => '˝',
        "ogonek" => '˛',
        "caron" => 'ˇ',
        "Lslash" => 'Ł',
        "lslash" => 'ł',
        "dotlessi" => 'ı',
        _ => return None,
    })
}

fn to_unicode_cmap(entries: &[(u8, char)]) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for chunk in entries.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (c, u) in chunk {
            let mut buf = [0u16; 2];
            let hex: String = u
                .encode_utf16(&mut buf)
                .iter()
                .map(|x| format!("{x:04X}"))
                .collect();
            s.push_str(&format!("<{c:02X}> <{hex}>\n"));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

// ------------------------------------------------------------------ actions

const FORBIDDEN_ACTIONS: [&[u8]; 11] = [
    b"Launch",
    b"Sound",
    b"Movie",
    b"ResetForm",
    b"ImportData",
    b"JavaScript",
    b"Hide",
    b"SetOCGState",
    b"Rendition",
    b"Trans",
    b"GoTo3DView",
];

fn action_forbidden(doc: &Document, a: &Object) -> bool {
    let Object::Dictionary(d) = deref(doc, a) else {
        return false;
    };
    let s = name(d, b"S").unwrap_or(b"");
    if FORBIDDEN_ACTIONS.contains(&s) {
        return true;
    }
    if s == b"Named" {
        let n = name(d, b"N").unwrap_or(b"");
        return !matches!(n, b"NextPage" | b"PrevPage" | b"FirstPage" | b"LastPage");
    }
    false
}

const FORBIDDEN_ANNOTS: [&[u8]; 6] = [
    b"3D",
    b"Sound",
    b"Movie",
    b"Screen",
    b"RichMedia",
    b"FileAttachment",
];

fn is_annot(d: &Dictionary) -> bool {
    name(d, b"Type") == Some(b"Annot") || (d.has(b"Rect") && d.has(b"Subtype") && !d.has(b"BBox"))
}

// ------------------------------------------------------------------ colour

#[derive(Default)]
struct ColourUse {
    rgb: bool,
    cmyk: bool,
}

fn note_cs(doc: &Document, cs: &Object, u: &mut ColourUse, depth: usize) {
    if depth > 6 {
        return;
    }
    match deref(doc, cs) {
        Object::Name(n) => match n.as_slice() {
            b"DeviceRGB" | b"RGB" => u.rgb = true,
            b"DeviceCMYK" | b"CMYK" => u.cmyk = true,
            _ => {}
        },
        Object::Array(a) => {
            let fam = a.first().and_then(|o| o.as_name().ok()).unwrap_or(b"");
            match fam {
                b"Indexed" | b"I" => {
                    if let Some(b) = a.get(1) {
                        note_cs(doc, b, u, depth + 1)
                    }
                }
                b"Separation" | b"DeviceN" => {
                    if let Some(b) = a.get(2) {
                        note_cs(doc, b, u, depth + 1)
                    }
                }
                b"Pattern" => {
                    if let Some(b) = a.get(1) {
                        note_cs(doc, b, u, depth + 1)
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn colour_use(doc: &Document) -> ColourUse {
    let mut u = ColourUse::default();
    for o in doc.objects.values() {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            _ => continue,
        };
        for key in [&b"ColorSpace"[..], b"CS"] {
            if let Ok(cs) = d.get(key) {
                match deref(doc, cs) {
                    // resource /ColorSpace dictionaries
                    Object::Dictionary(m) if key == b"ColorSpace" => {
                        for (k, v) in m.iter() {
                            if !k.starts_with(b"Default") {
                                note_cs(doc, v, &mut u, 0);
                            }
                        }
                    }
                    other => note_cs(doc, other, &mut u, 0),
                }
            }
        }
        if let Object::Stream(s) = o {
            let d = &s.dict;
            let is_content = name(d, b"Subtype") == Some(b"Form")
                || d.has(b"PatternType")
                || !(d.has(b"Subtype")
                    || d.has(b"Type")
                    || d.has(b"N")
                    || d.has(b"Length1")
                    || d.has(b"FunctionType")
                    || d.has(b"ShadingType"));
            if is_content {
                if let Ok(data) = s.decompressed_content_with_limit(MAX_STREAM) {
                    if let Ok(c) = Content::decode(&data) {
                        for op in &c.operations {
                            match op.operator.as_str() {
                                "rg" | "RG" => u.rgb = true,
                                "k" | "K" => u.cmyk = true,
                                "cs" | "CS" => {
                                    if let Some(n) = op.operands.first() {
                                        note_cs(doc, n, &mut u, 0);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
    u
}

/// Every resource dictionary in the file: (owner object, is the value a
/// reference to a separate object).
fn resource_dicts(doc: &Document) -> Vec<(ObjectId, Option<ObjectId>)> {
    let mut v = Vec::new();
    for (&id, o) in &doc.objects {
        let d = match o {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            _ => continue,
        };
        match d.get(b"Resources") {
            Ok(Object::Reference(r)) => v.push((id, Some(*r))),
            Ok(Object::Dictionary(_)) => v.push((id, None)),
            _ => {}
        }
    }
    v
}

fn add_default_cs(doc: &mut Document, key: &str, cs: ObjectId) -> usize {
    let mut n = 0;
    // pages without resources anywhere still need one
    for (_, p) in doc.get_pages() {
        let has = {
            let mut id = p;
            let mut found = false;
            for _ in 0..32 {
                let Ok(d) = doc.get_dictionary(id) else { break };
                if d.has(b"Resources") {
                    found = true;
                    break;
                }
                match d.get(b"Parent").and_then(Object::as_reference) {
                    Ok(pp) => id = pp,
                    Err(_) => break,
                }
            }
            found
        };
        if !has {
            if let Ok(d) = doc.get_dictionary_mut(p) {
                d.set("Resources", Dictionary::new());
            }
        }
    }
    let targets = resource_dicts(doc);
    let mut done: HashSet<ObjectId> = HashSet::new();
    for (owner, r) in targets {
        let res: Option<&mut Dictionary> = match r {
            Some(rid) => {
                if !done.insert(rid) {
                    continue;
                }
                doc.get_object_mut(rid)
                    .ok()
                    .and_then(|o| o.as_dict_mut().ok())
            }
            None => doc.get_object_mut(owner).ok().and_then(|o| match o {
                Object::Dictionary(d) => d
                    .get_mut(b"Resources")
                    .ok()
                    .and_then(|r| r.as_dict_mut().ok()),
                Object::Stream(s) => s
                    .dict
                    .get_mut(b"Resources")
                    .ok()
                    .and_then(|r| r.as_dict_mut().ok()),
                _ => None,
            }),
        };
        let Some(res) = res else { continue };
        // a referenced /ColorSpace dict is rare; inline it
        let mut csd = match res.get(b"ColorSpace") {
            Ok(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        if let Ok(Object::Reference(_)) = res.get(b"ColorSpace") {
            // leave referenced colour-space dicts alone; handled when visited
            continue;
        }
        if !csd.has(key.as_bytes()) {
            csd.set(key, cs);
            res.set("ColorSpace", csd);
            n += 1;
        }
    }
    n
}

// ------------------------------------------------------------------ convert

pub fn convert(pdf: &[u8]) -> Result<Converted, String> {
    let mut doc = Document::load_mem(pdf).map_err(|e| format!("not a readable PDF ({e})"))?;
    if doc.is_encrypted() || doc.trailer.has(b"Encrypt") {
        return Err("the document is password-protected; remove the password first".into());
    }
    let cat_id = catalog_id(&doc).ok_or("the PDF has no catalog")?;
    let mut notes: Vec<String> = Vec::new();
    let mut removed: BTreeMap<&'static str, usize> = BTreeMap::new();
    let bump = |k: &'static str, removed: &mut BTreeMap<&'static str, usize>| {
        *removed.entry(k).or_default() += 1;
    };

    doc.version = "1.7".into();
    doc.binary_mark = vec![0xE2, 0xE3, 0xCF, 0xD3];

    // --- catalog
    {
        let cat = doc.get_dictionary(cat_id).cloned().unwrap_or_default();
        let mut cat = cat;
        if cat.remove(b"AA").is_some() {
            bump("additional actions", &mut removed);
        }
        if let Ok(a) = cat.get(b"OpenAction") {
            if action_forbidden(&doc, a) {
                cat.remove(b"OpenAction");
                bump("forbidden actions", &mut removed);
            }
        }
        if let Ok(v) = cat.get(b"Version").and_then(Object::as_name) {
            if v > &b"1.7"[..] {
                cat.remove(b"Version");
            }
        }
        for k in [
            &b"Collection"[..],
            b"NeedsRendering",
            b"Requirements",
            b"AF",
        ] {
            cat.remove(k);
        }
        if let Ok(Object::Dictionary(p)) = cat.get(b"Perms").map(|p| deref(&doc, p).clone()) {
            let mut p = p;
            let keys: Vec<Vec<u8>> = p.iter().map(|(k, _)| k.clone()).collect();
            for k in keys {
                if k != b"UR3" && k != b"DocMDP" {
                    p.remove(&k);
                }
            }
            cat.set("Perms", p);
        }
        // names: JavaScript + EmbeddedFiles
        if let Ok(n) = cat.get(b"Names") {
            let nid = n.as_reference().ok();
            if let Object::Dictionary(names) = deref(&doc, n).clone() {
                let mut names = names;
                if names.remove(b"JavaScript").is_some() {
                    bump("document JavaScript", &mut removed);
                }
                if names.remove(b"EmbeddedFiles").is_some() {
                    bump("embedded files", &mut removed);
                }
                match nid {
                    Some(id) => {
                        doc.objects.insert(id, Object::Dictionary(names));
                    }
                    None => cat.set("Names", names),
                }
            }
        }
        // AcroForm
        if let Ok(f) = cat.get(b"AcroForm") {
            let fid = f.as_reference().ok();
            if let Object::Dictionary(form) = deref(&doc, f).clone() {
                let mut form = form;
                if form.remove(b"XFA").is_some() {
                    bump("XFA forms", &mut removed);
                }
                if form
                    .get(b"NeedAppearances")
                    .and_then(Object::as_bool)
                    .unwrap_or(false)
                {
                    notes.push(
                        "Form fields relied on NeedAppearances; viewers will now show their stored \
                         appearances only."
                            .into(),
                    );
                }
                form.remove(b"NeedAppearances");
                match fid {
                    Some(id) => {
                        doc.objects.insert(id, Object::Dictionary(form));
                    }
                    None => cat.set("AcroForm", form),
                }
            }
        }
        // optional content
        if let Ok(oc) = cat.get(b"OCProperties") {
            let oid = oc.as_reference().ok();
            if let Object::Dictionary(ocp) = deref(&doc, oc).clone() {
                let mut ocp = ocp;
                let fix = |c: &mut Dictionary, i: usize| {
                    if !c.has(b"Name") {
                        c.set("Name", Object::string_literal(format!("Config {i}")));
                    }
                    c.remove(b"AS");
                };
                if let Ok(Object::Dictionary(d)) = ocp.get_mut(b"D") {
                    fix(d, 0);
                }
                if let Ok(Object::Array(cfgs)) = ocp.get_mut(b"Configs") {
                    for (i, c) in cfgs.iter_mut().enumerate() {
                        if let Object::Dictionary(d) = c {
                            fix(d, i + 1);
                        }
                    }
                }
                match oid {
                    Some(id) => {
                        doc.objects.insert(id, Object::Dictionary(ocp));
                    }
                    None => cat.set("OCProperties", ocp),
                }
            }
        }
        doc.objects.insert(cat_id, Object::Dictionary(cat));
    }

    // --- every object
    let ids: Vec<ObjectId> = doc.objects.keys().copied().collect();
    let mut drop_annots: HashSet<ObjectId> = HashSet::new();
    for id in ids {
        let Some(mut o) = doc.objects.remove(&id) else {
            continue;
        };
        let snapshot_forbidden = |d: &Dictionary, k: &[u8], doc: &Document| -> bool {
            d.get(k).map(|a| action_forbidden(doc, a)).unwrap_or(false)
        };
        {
            let (d, is_stream) = match &mut o {
                Object::Dictionary(d) => (d, false),
                Object::Stream(s) => (&mut s.dict, true),
                _ => {
                    doc.objects.insert(id, o);
                    continue;
                }
            };
            if d.remove(b"AA").is_some() {
                bump("additional actions", &mut removed);
            }
            if snapshot_forbidden(d, b"A", &doc) {
                d.remove(b"A");
                bump("forbidden actions", &mut removed);
            }
            if snapshot_forbidden(d, b"Next", &doc) {
                d.remove(b"Next");
            }
            d.remove(b"AF");
            if is_stream {
                for k in [&b"F"[..], b"FFilter", b"FDecodeParms"] {
                    d.remove(k);
                }
            }
            if is_annot(d) {
                let st = name(d, b"Subtype").unwrap_or(b"").to_vec();
                if FORBIDDEN_ANNOTS.contains(&st.as_slice()) {
                    drop_annots.insert(id);
                }
                let f = d.get(b"F").and_then(Object::as_i64).unwrap_or(0);
                let nf = (f | 4) & !(1 | 2 | 32 | 256);
                if nf != f {
                    d.set("F", nf);
                }
                if let Ok(Object::Dictionary(ap)) = d.get_mut(b"AP") {
                    ap.remove(b"D");
                    ap.remove(b"R");
                }
            }
            match name(d, b"Subtype") {
                Some(b"Image") => {
                    if d.get(b"Interpolate")
                        .and_then(Object::as_bool)
                        .unwrap_or(false)
                    {
                        d.set("Interpolate", false);
                    }
                    d.remove(b"Alternates");
                    d.remove(b"OPI");
                }
                Some(b"Form") => {
                    d.remove(b"OPI");
                    d.remove(b"Ref");
                    d.remove(b"PS");
                    if name(d, b"Subtype2") == Some(b"PS") {
                        d.remove(b"Subtype2");
                    }
                }
                Some(b"PS") => {
                    notes.push("A PostScript XObject was emptied.".into());
                    d.set("Subtype", "Form");
                    d.set("BBox", vec![0.into(), 0.into(), 0.into(), 0.into()]);
                }
                _ => {}
            }
            if name(d, b"Type") == Some(b"ExtGState") {
                d.remove(b"TR");
                if d.get(b"TR2")
                    .map(|t| t.as_name().ok() != Some(b"Default"))
                    .unwrap_or(false)
                {
                    d.remove(b"TR2");
                }
                if let Some(ri) = name(d, b"RI") {
                    if !matches!(
                        ri,
                        b"RelativeColorimetric"
                            | b"AbsoluteColorimetric"
                            | b"Perceptual"
                            | b"Saturation"
                    ) {
                        d.set("RI", "RelativeColorimetric");
                    }
                }
            }
        }
        // PostScript XObject content
        if let Object::Stream(s) = &mut o {
            if s.dict
                .get(b"BBox")
                .ok()
                .and_then(|b| b.as_array().ok())
                .map(|a| {
                    a.iter()
                        .all(|x| x.as_float().map(|v| v == 0.0).unwrap_or(false))
                })
                .unwrap_or(false)
                && name(&s.dict, b"Subtype") == Some(b"Form")
                && s.content.starts_with(b"%!")
            {
                s.set_content(Vec::new());
                s.dict.remove(b"Filter");
            }
            // LZW → Flate
            let lzw = s
                .filters()
                .map(|f| f.iter().any(|x| *x == b"LZWDecode" || *x == b"LZW"))
                .unwrap_or(false);
            if lzw {
                if let Ok(raw) = s.decompressed_content_with_limit(MAX_STREAM) {
                    s.dict.remove(b"DecodeParms");
                    s.dict.set("Filter", "FlateDecode");
                    s.set_content(flate(&raw));
                    bump("LZW streams re-encoded", &mut removed);
                }
            }
        }
        doc.objects.insert(id, o);
    }
    if !drop_annots.is_empty() {
        removed.insert(
            "forbidden annotations (3D, media, file attachments)",
            drop_annots.len(),
        );
        for (_, page) in doc.get_pages() {
            let annots = doc
                .get_dictionary(page)
                .ok()
                .and_then(|d| d.get(b"Annots").ok().cloned());
            match annots {
                Some(Object::Array(a)) => {
                    let keep: Vec<Object> = a
                        .into_iter()
                        .filter(|x| {
                            x.as_reference()
                                .map(|r| !drop_annots.contains(&r))
                                .unwrap_or(true)
                        })
                        .collect();
                    if let Ok(d) = doc.get_dictionary_mut(page) {
                        d.set("Annots", keep);
                    }
                }
                Some(Object::Reference(r)) => {
                    if let Ok(Object::Array(a)) = doc.get_object_mut(r) {
                        a.retain(|x| {
                            x.as_reference()
                                .map(|r| !drop_annots.contains(&r))
                                .unwrap_or(true)
                        });
                    }
                }
                _ => {}
            }
        }
        for id in &drop_annots {
            doc.objects.remove(id);
        }
    }
    // annotations without an appearance can't be fixed generically
    let mut no_ap = 0;
    for o in doc.objects.values() {
        if let Object::Dictionary(d) = o {
            if is_annot(d) && !d.has(b"AP") {
                let st = name(d, b"Subtype").unwrap_or(b"");
                let zero = d
                    .get(b"Rect")
                    .and_then(Object::as_array)
                    .map(|r| {
                        let n: Vec<f32> = r.iter().filter_map(|x| x.as_float().ok()).collect();
                        n.len() == 4 && ((n[2] - n[0]).abs() < 0.01 || (n[3] - n[1]).abs() < 0.01)
                    })
                    .unwrap_or(false);
                if st != b"Popup" && st != b"Link" && !zero {
                    no_ap += 1;
                }
            }
        }
    }
    if no_ap > 0 {
        notes.push(format!(
            "{no_ap} annotation(s) have no appearance stream (not fixable automatically)."
        ));
    }

    // --- fonts
    let (embedded, unfixable) = embed_fonts(&mut doc);

    // --- output intent
    let cat = doc.get_dictionary(cat_id).cloned().unwrap_or_default();
    let mut intent_cs: Option<[u8; 4]> = None;
    if let Ok(Object::Array(ois)) = cat.get(b"OutputIntents").map(|o| deref(&doc, o)) {
        for oi in ois {
            if let Object::Dictionary(d) = deref(&doc, oi) {
                if name(d, b"S") == Some(b"GTS_PDFA1") {
                    if let Ok(Object::Stream(p)) =
                        d.get(b"DestOutputProfile").map(|p| deref(&doc, p))
                    {
                        let data = p
                            .decompressed_content_with_limit(MAX_STREAM)
                            .unwrap_or_default();
                        intent_cs = icc_header(&data).map(|h| h.0);
                    }
                }
            }
        }
    }
    if intent_cs.is_none() {
        let icc = srgb_icc();
        let mut s = Stream::new(
            dictionary! { "N" => 3, "Filter" => "FlateDecode" },
            flate(&icc),
        );
        s.allows_compression = false;
        let pid = doc.add_object(s);
        let oi = doc.add_object(dictionary! {
            "Type" => "OutputIntent",
            "S" => "GTS_PDFA1",
            "OutputConditionIdentifier" => Object::string_literal("sRGB IEC61966-2.1"),
            "RegistryName" => Object::string_literal("http://www.color.org"),
            "Info" => Object::string_literal("sRGB IEC61966-2.1"),
            "DestOutputProfile" => pid,
        });
        let mut list = match cat.get(b"OutputIntents").map(|o| deref(&doc, o)) {
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![],
        };
        list.insert(0, oi.into());
        if let Ok(c) = doc.get_dictionary_mut(cat_id) {
            c.set("OutputIntents", list);
        }
        intent_cs = Some(*b"RGB ");
    }
    let usage = colour_use(&doc);
    if usage.cmyk && intent_cs != Some(*b"CMYK") {
        let mut s = Stream::new(
            dictionary! { "N" => 4, "Filter" => "FlateDecode" },
            flate(CMYK_ICC),
        );
        s.allows_compression = false;
        let pid = doc.add_object(s);
        let cs = doc.add_object(Object::Array(vec!["ICCBased".into(), pid.into()]));
        let n = add_default_cs(&mut doc, "DefaultCMYK", cs);
        if n > 0 {
            notes.push(format!(
                "DeviceCMYK colours are now interpreted through a CMYK ICC profile \
                 (DefaultCMYK in {n} resource dictionaries)."
            ));
        }
    }
    if usage.rgb && intent_cs == Some(*b"CMYK") {
        let mut s = Stream::new(
            dictionary! { "N" => 3, "Filter" => "FlateDecode" },
            flate(&srgb_icc()),
        );
        s.allows_compression = false;
        let pid = doc.add_object(s);
        let cs = doc.add_object(Object::Array(vec!["ICCBased".into(), pid.into()]));
        add_default_cs(&mut doc, "DefaultRGB", cs);
    }

    // --- metadata (Info + XMP)
    let info_id = match doc.trailer.get(b"Info") {
        Ok(Object::Reference(r)) if doc.get_dictionary(*r).is_ok() => *r,
        _ => {
            let id = doc.add_object(Dictionary::new());
            doc.trailer.set("Info", id);
            id
        }
    };
    let mut info = doc.get_dictionary(info_id).cloned().unwrap_or_default();
    let get_text = |k: &[u8], info: &Dictionary, doc: &Document| -> Option<String> {
        info.get(k)
            .ok()
            .map(|o| deref(doc, o))
            .and_then(text_of)
            .filter(|s| !s.trim().is_empty())
    };
    let title = get_text(b"Title", &info, &doc);
    let author = get_text(b"Author", &info, &doc);
    let subject = get_text(b"Subject", &info, &doc);
    let keywords = get_text(b"Keywords", &info, &doc);
    let creator = get_text(b"Creator", &info, &doc);
    let producer = get_text(b"Producer", &info, &doc)
        .map(|p| {
            if p.contains("og-files") {
                p
            } else {
                format!("{p}; og-files PDF/A")
            }
        })
        .unwrap_or_else(|| "og-files PDF/A".to_string());
    let created = get_text(b"CreationDate", &info, &doc).and_then(|d| pdf_date_to_xmp(&d));
    let now = now_pdf_date();
    let modified = pdf_date_to_xmp(&now);
    // Info mirrors exactly what goes into XMP
    for (k, v) in [
        (&b"Title"[..], &title),
        (b"Author", &author),
        (b"Subject", &subject),
        (b"Keywords", &keywords),
        (b"Creator", &creator),
    ] {
        match v {
            Some(s) => info.set(k.to_vec(), text_obj(s)),
            None => {
                info.remove(k);
            }
        }
    }
    info.set("Producer", text_obj(&producer));
    match &created {
        Some((_, p)) => info.set("CreationDate", Object::string_literal(p.clone())),
        None => {
            info.remove(b"CreationDate");
        }
    }
    if let Some((_, p)) = &modified {
        info.set("ModDate", Object::string_literal(p.clone()));
    }
    info.remove(b"Trapped");
    doc.objects.insert(info_id, Object::Dictionary(info));

    let mut x = String::from(
        "<?xpacket begin=\"\u{FEFF}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         <rdf:Description rdf:about=\"\"\n\
         xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"\n\
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n\
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n\
         xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\n\
         <pdfaid:part>2</pdfaid:part>\n<pdfaid:conformance>B</pdfaid:conformance>\n\
         <dc:format>application/pdf</dc:format>\n",
    );
    if let Some(t) = &title {
        x.push_str(&format!(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n",
            xml_escape(t)
        ));
    }
    if let Some(a) = &author {
        x.push_str(&format!(
            "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n",
            xml_escape(a)
        ));
    }
    if let Some(s) = &subject {
        x.push_str(&format!(
            "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>\n",
            xml_escape(s)
        ));
    }
    if let Some(k) = &keywords {
        x.push_str(&format!("<pdf:Keywords>{}</pdf:Keywords>\n", xml_escape(k)));
    }
    x.push_str(&format!(
        "<pdf:Producer>{}</pdf:Producer>\n",
        xml_escape(&producer)
    ));
    if let Some(c) = &creator {
        x.push_str(&format!(
            "<xmp:CreatorTool>{}</xmp:CreatorTool>\n",
            xml_escape(c)
        ));
    }
    if let Some((c, _)) = &created {
        x.push_str(&format!("<xmp:CreateDate>{c}</xmp:CreateDate>\n"));
    }
    if let Some((m, _)) = &modified {
        x.push_str(&format!(
            "<xmp:ModifyDate>{m}</xmp:ModifyDate>\n<xmp:MetadataDate>{m}</xmp:MetadataDate>\n"
        ));
    }
    x.push_str("</rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n");
    x.push_str(&" ".repeat(1024));
    x.push_str("\n<?xpacket end=\"w\"?>");
    let mut meta = Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        x.into_bytes(),
    );
    meta.allows_compression = false;
    let mid = doc.add_object(meta);
    if let Ok(c) = doc.get_dictionary_mut(cat_id) {
        c.set("Metadata", mid);
    }

    // --- trailer ID
    if !matches!(doc.trailer.get(b"ID"), Ok(Object::Array(a)) if a.len() == 2) {
        let seed = format!("{}{}{:?}", pdf.len(), now, std::time::SystemTime::now());
        let h = sha2::Sha256::digest(seed.as_bytes());
        let id = Object::String(h[..16].to_vec(), StringFormat::Hexadecimal);
        doc.trailer.set("ID", vec![id.clone(), id]);
    }
    doc.prune_objects();
    let mut t = Dictionary::new();
    for key in [&b"Root"[..], b"Info", b"ID"] {
        if let Ok(v) = doc.trailer.get(key) {
            t.set(key.to_vec(), v.clone());
        }
    }
    doc.trailer = t;
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("could not write PDF: {e}"))?;

    // --- verify and report
    let violations = verify(&buf)?;
    let mut report = String::from("PDF/A-2b conversion (best effort).\n");
    if !embedded.is_empty() {
        report.push_str(&format!(
            "Fonts embedded (metric-compatible Type 1): {}\n",
            embedded.join(", ")
        ));
    }
    for (k, n) in &removed {
        report.push_str(&format!("Removed / fixed — {k}: {n}\n"));
    }
    for n in &notes {
        report.push_str(&format!("Note: {n}\n"));
    }
    if !unfixable.is_empty() {
        let mut u = unfixable.clone();
        u.sort();
        u.dedup();
        report.push_str(&format!(
            "Fonts that couldn't be embedded (no substitute bundled): {}\n",
            u.join(", ")
        ));
    }
    report.push('\n');
    if violations.is_empty() {
        report.push_str("Built-in verifier: no violations found.\n");
    } else {
        report.push_str(&format!(
            "Built-in verifier: {} remaining violation(s) — the file is NOT fully PDF/A-2b:\n",
            violations.len()
        ));
        for v in &violations {
            report.push_str(&format!("  • {v}\n"));
        }
    }
    report.push_str(&format!(
        "Checked: {COVERAGE}.\nNot checked: content-stream operators, glyphs used vs. present, \
         CIDSet/CharSet, TrueType cmap tables, XMP extension schemas. Use veraPDF for a formal \
         validation."
    ));
    Ok(Converted { pdf: buf, report })
}

use sha2::Digest as _;

// ------------------------------------------------------------------ verify

/// PDF/A-2b rule check. Returns human-readable violations (empty = none
/// found by the covered rules).
pub fn verify(pdf: &[u8]) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = Vec::new();
    // header
    let hdr_ok = pdf.starts_with(b"%PDF-1.")
        && pdf
            .get(7)
            .map(|c| (b'0'..=b'7').contains(c))
            .unwrap_or(false);
    if !hdr_ok {
        v.push("header must be %PDF-1.0 … %PDF-1.7".into());
    }
    let second = pdf
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .map(|i| {
            let mut j = i;
            while j < pdf.len() && (pdf[j] == b'\n' || pdf[j] == b'\r') {
                j += 1;
            }
            j
        })
        .unwrap_or(0);
    let bin_ok = pdf.get(second) == Some(&b'%')
        && pdf
            .get(second + 1..second + 5)
            .map(|b| b.iter().all(|&c| c > 127))
            .unwrap_or(false);
    if !bin_ok {
        v.push("the header must be followed by a comment with four bytes > 127".into());
    }
    let doc = Document::load_mem(pdf).map_err(|e| format!("not a readable PDF ({e})"))?;
    if doc.trailer.has(b"Encrypt") || doc.is_encrypted() {
        v.push("encryption is not allowed".into());
    }
    if !matches!(doc.trailer.get(b"ID"), Ok(Object::Array(a)) if a.len() == 2) {
        v.push("the trailer has no /ID".into());
    }
    let Some(cat_id) = catalog_id(&doc) else {
        v.push("no catalog".into());
        return Ok(v);
    };
    let cat = doc.get_dictionary(cat_id).cloned().unwrap_or_default();

    // XMP
    match cat.get(b"Metadata").map(|m| deref(&doc, m)) {
        Ok(Object::Stream(m)) => {
            if m.dict.has(b"Filter") {
                v.push("the XMP metadata stream must not be compressed".into());
            }
            let xmp = String::from_utf8_lossy(&m.content).into_owned();
            let part = xmp_value(&xmp, "pdfaid:part");
            let conf = xmp_value(&xmp, "pdfaid:conformance");
            if part.as_deref() != Some("2") {
                v.push(format!(
                    "XMP pdfaid:part is {:?}, expected 2",
                    part.unwrap_or_default()
                ));
            }
            if !matches!(conf.as_deref(), Some("B") | Some("U") | Some("A")) {
                v.push("XMP pdfaid:conformance missing".into());
            }
            // Info ↔ XMP
            if let Ok(Object::Dictionary(info)) = doc.trailer.get(b"Info").map(|i| deref(&doc, i)) {
                let pairs: [(&[u8], &str); 6] = [
                    (b"Title", "dc:title"),
                    (b"Author", "dc:creator"),
                    (b"Subject", "dc:description"),
                    (b"Keywords", "pdf:Keywords"),
                    (b"Creator", "xmp:CreatorTool"),
                    (b"Producer", "pdf:Producer"),
                ];
                for (k, x) in pairs {
                    let iv = info.get(k).ok().map(|o| deref(&doc, o)).and_then(text_of);
                    if let Some(iv) = iv.filter(|s| !s.is_empty()) {
                        let xv = xmp_value(&xmp, x).map(|s| xml_unescape(&s));
                        if xv.as_deref() != Some(iv.as_str()) {
                            v.push(format!(
                                "Info /{} doesn't match XMP {x}",
                                String::from_utf8_lossy(k)
                            ));
                        }
                    }
                }
                for (k, x) in [
                    (&b"CreationDate"[..], "xmp:CreateDate"),
                    (b"ModDate", "xmp:ModifyDate"),
                ] {
                    if let Some(iv) = info.get(k).ok().map(|o| deref(&doc, o)).and_then(text_of) {
                        let want = pdf_date_to_xmp(&iv).map(|p| p.0);
                        let have = xmp_value(&xmp, x);
                        if want.is_none() || !same_instant(want.as_deref(), have.as_deref()) {
                            v.push(format!(
                                "Info /{} doesn't match XMP {x}",
                                String::from_utf8_lossy(k)
                            ));
                        }
                    }
                }
            }
        }
        _ => v.push("no XMP metadata stream in the catalog".into()),
    }

    // output intents
    let mut intent: Option<[u8; 4]> = None;
    let mut profiles: HashSet<ObjectId> = HashSet::new();
    if let Ok(Object::Array(ois)) = cat.get(b"OutputIntents").map(|o| deref(&doc, o)) {
        for oi in ois {
            let Object::Dictionary(d) = deref(&doc, oi) else {
                continue;
            };
            if name(d, b"S") != Some(b"GTS_PDFA1") {
                continue;
            }
            match d.get(b"DestOutputProfile") {
                Ok(Object::Reference(r)) => {
                    profiles.insert(*r);
                    if let Ok(Object::Stream(p)) = doc.get_object(*r) {
                        let data = p
                            .decompressed_content_with_limit(MAX_STREAM)
                            .unwrap_or_default();
                        match icc_header(&data) {
                            Some((cs, class, ver)) => {
                                if ver > 4 {
                                    v.push("output intent ICC profile version > 4".into());
                                }
                                if &class != b"mntr" && &class != b"prtr" {
                                    v.push("output intent ICC profile must be a display or printer profile".into());
                                }
                                let n = p.dict.get(b"N").and_then(Object::as_i64).unwrap_or(0);
                                let want = match &cs {
                                    b"RGB " => 3,
                                    b"CMYK" => 4,
                                    b"GRAY" => 1,
                                    _ => 0,
                                };
                                if n != want {
                                    v.push("output intent ICC /N doesn't match the profile".into());
                                }
                                intent = Some(cs);
                            }
                            None => v.push("output intent ICC profile is invalid".into()),
                        }
                    }
                }
                _ => v.push("PDF/A output intent without DestOutputProfile".into()),
            }
        }
    }
    if profiles.len() > 1 {
        v.push("several PDF/A output intents with different profiles".into());
    }
    let usage = colour_use(&doc);
    let all_res_have = |key: &[u8]| -> bool {
        resource_dicts(&doc).into_iter().all(|(owner, r)| {
            let res = match r {
                Some(rid) => doc.get_dictionary(rid).ok(),
                None => doc.objects.get(&owner).and_then(|o| match o {
                    Object::Dictionary(d) => {
                        d.get(b"Resources").ok().and_then(|r| r.as_dict().ok())
                    }
                    Object::Stream(s) => {
                        s.dict.get(b"Resources").ok().and_then(|r| r.as_dict().ok())
                    }
                    _ => None,
                }),
            };
            res.and_then(|r| r.get(b"ColorSpace").ok())
                .map(|c| deref(&doc, c))
                .and_then(|c| c.as_dict().ok())
                .map(|c| c.has(key))
                .unwrap_or(false)
        })
    };
    if usage.rgb && intent != Some(*b"RGB ") && !all_res_have(b"DefaultRGB") {
        v.push("DeviceRGB is used without an RGB output intent or DefaultRGB".into());
    }
    if usage.cmyk && intent != Some(*b"CMYK") && !all_res_have(b"DefaultCMYK") {
        v.push("DeviceCMYK is used without a CMYK output intent or DefaultCMYK".into());
    }
    if intent.is_none() && (usage.rgb || usage.cmyk) {
        v.push("no PDF/A output intent".into());
    }

    // catalog-level
    if cat.has(b"AA") {
        v.push("the catalog has additional actions (/AA)".into());
    }
    if cat
        .get(b"OpenAction")
        .map(|a| action_forbidden(&doc, a))
        .unwrap_or(false)
    {
        v.push("forbidden OpenAction".into());
    }
    if let Ok(Object::Dictionary(n)) = cat.get(b"Names").map(|n| deref(&doc, n)) {
        if n.has(b"JavaScript") {
            v.push("document-level JavaScript".into());
        }
        if n.has(b"EmbeddedFiles") {
            v.push(
                "embedded files (only PDF/A files are allowed; this converter removes them)".into(),
            );
        }
    }
    if let Ok(Object::Dictionary(f)) = cat.get(b"AcroForm").map(|n| deref(&doc, n)) {
        if f.get(b"NeedAppearances")
            .and_then(Object::as_bool)
            .unwrap_or(false)
        {
            v.push("AcroForm NeedAppearances is true".into());
        }
        if f.has(b"XFA") {
            v.push("XFA form data".into());
        }
    }
    if let Ok(Object::Dictionary(oc)) = cat.get(b"OCProperties").map(|n| deref(&doc, n)) {
        let mut cfgs: Vec<&Dictionary> = vec![];
        if let Ok(Object::Dictionary(d)) = oc.get(b"D").map(|d| deref(&doc, d)) {
            cfgs.push(d);
        }
        if let Ok(Object::Array(a)) = oc.get(b"Configs").map(|d| deref(&doc, d)) {
            cfgs.extend(a.iter().filter_map(|c| deref(&doc, c).as_dict().ok()));
        }
        for c in cfgs {
            if !c.has(b"Name") {
                v.push("optional-content configuration without /Name".into());
            }
            if c.has(b"AS") {
                v.push("optional-content configuration with /AS".into());
            }
        }
    }

    // objects
    let used = used_codes(&doc);
    let mut font_issues: BTreeMap<String, String> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut count = |k: String| *counts.entry(k).or_default() += 1;
    for (&oid, o) in &doc.objects {
        let (d, stream) = match o {
            Object::Dictionary(d) => (d, None),
            Object::Stream(s) => (&s.dict, Some(s)),
            _ => continue,
        };
        if d.has(b"AA") {
            count("additional actions (/AA) on pages, annotations or fields".into());
        }
        if d.get(b"A")
            .map(|a| action_forbidden(&doc, a))
            .unwrap_or(false)
        {
            count("forbidden action (JavaScript, Launch, media…)".into());
        }
        if name(d, b"S")
            .map(|s| FORBIDDEN_ACTIONS.contains(&s))
            .unwrap_or(false)
        {
            count("forbidden action dictionary".into());
        }
        if let Some(s) = stream {
            if d.has(b"F") || d.has(b"FFilter") {
                count("stream with external data (/F)".into());
            }
            if s.filters()
                .map(|f| f.iter().any(|x| *x == b"LZWDecode" || *x == b"LZW"))
                .unwrap_or(false)
            {
                count("LZW-compressed stream".into());
            }
        }
        if is_annot(d) {
            let st = name(d, b"Subtype").unwrap_or(b"").to_vec();
            if FORBIDDEN_ANNOTS.contains(&st.as_slice()) {
                count(format!(
                    "forbidden annotation type /{}",
                    String::from_utf8_lossy(&st)
                ));
            }
            let f = d.get(b"F").and_then(Object::as_i64).unwrap_or(0);
            if st != b"Popup" && (f & 4 == 0 || f & (1 | 2 | 32 | 256) != 0) {
                count("annotation not printable or hidden (/F flags)".into());
            }
            if let Ok(Object::Dictionary(ap)) = d.get(b"AP").map(|a| deref(&doc, a)) {
                if ap.has(b"D") || ap.has(b"R") || !ap.has(b"N") {
                    count("annotation appearance dictionary must contain only /N".into());
                }
            } else if st != b"Popup" && st != b"Link" {
                let zero = d
                    .get(b"Rect")
                    .and_then(Object::as_array)
                    .map(|r| {
                        let n: Vec<f32> = r.iter().filter_map(|x| x.as_float().ok()).collect();
                        n.len() == 4 && ((n[2] - n[0]).abs() < 0.01 || (n[3] - n[1]).abs() < 0.01)
                    })
                    .unwrap_or(false);
                if !zero {
                    count("annotation without appearance stream".into());
                }
            }
        }
        match name(d, b"Subtype") {
            Some(b"Image") => {
                if d.get(b"Interpolate")
                    .and_then(Object::as_bool)
                    .unwrap_or(false)
                {
                    count("image with Interpolate true".into());
                }
                if d.has(b"Alternates") || d.has(b"OPI") {
                    count("image with Alternates/OPI".into());
                }
            }
            Some(b"Form") => {
                if d.has(b"Ref")
                    || d.has(b"OPI")
                    || d.has(b"PS")
                    || name(d, b"Subtype2") == Some(b"PS")
                {
                    count("reference / PostScript form XObject".into());
                }
            }
            Some(b"PS") => count("PostScript XObject".into()),
            _ => {}
        }
        if name(d, b"Type") == Some(b"ExtGState") {
            if d.has(b"TR")
                || d.get(b"TR2")
                    .map(|t| t.as_name().ok() != Some(b"Default"))
                    .unwrap_or(false)
            {
                count("transfer function in ExtGState".into());
            }
            if let Some(ri) = name(d, b"RI") {
                if !matches!(
                    ri,
                    b"RelativeColorimetric"
                        | b"AbsoluteColorimetric"
                        | b"Perceptual"
                        | b"Saturation"
                ) {
                    count("unknown rendering intent".into());
                }
            }
        }
        if name(d, b"Type") == Some(b"Font") || (d.has(b"BaseFont") && d.has(b"Subtype")) {
            check_font(&doc, d, used.get(&oid), &mut font_issues);
        }
        if name(d, b"Type") == Some(b"Filespec") && d.has(b"EF") {
            count("embedded file stream".into());
        }
        for (k, val) in d.iter() {
            if k.len() > 127 {
                count("name longer than 127 bytes".into());
            }
            if let Object::String(s, _) = val {
                if s.len() > 32767 {
                    count("string longer than 32767 bytes".into());
                }
            }
        }
    }
    for (k, n) in counts {
        v.push(if n > 1 { format!("{k} ({n}×)") } else { k });
    }
    for (f, why) in font_issues {
        v.push(format!("font {f}: {why}"));
    }
    Ok(v)
}

fn check_font(
    doc: &Document,
    d: &Dictionary,
    used: Option<&[bool; 256]>,
    issues: &mut BTreeMap<String, String>,
) {
    let sub = name(d, b"Subtype").unwrap_or(b"");
    let base = name(d, b"BaseFont")
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_else(|| "(unnamed)".into());
    match sub {
        b"Type3" => {}
        b"Type0" => {
            let desc = d
                .get(b"DescendantFonts")
                .map(|x| deref(doc, x))
                .ok()
                .and_then(|a| a.as_array().ok())
                .and_then(|a| a.first())
                .map(|x| deref(doc, x))
                .and_then(|x| x.as_dict().ok());
            match desc {
                Some(cid) => {
                    let fd = cid
                        .get(b"FontDescriptor")
                        .map(|x| deref(doc, x))
                        .ok()
                        .and_then(|x| x.as_dict().ok());
                    let emb = fd
                        .map(|f| f.has(b"FontFile2") || f.has(b"FontFile3") || f.has(b"FontFile"))
                        .unwrap_or(false);
                    if !emb {
                        issues.insert(base, "not embedded".into());
                    } else if name(cid, b"Subtype") == Some(b"CIDFontType2")
                        && !cid.has(b"CIDToGIDMap")
                    {
                        issues.insert(base, "CIDFontType2 without CIDToGIDMap".into());
                    }
                }
                None => {
                    issues.insert(base, "Type0 font without descendant".into());
                }
            }
        }
        b"Type1" | b"MMType1" | b"TrueType" => {
            let fd = d
                .get(b"FontDescriptor")
                .map(|x| deref(doc, x))
                .ok()
                .and_then(|x| x.as_dict().ok());
            let Some(fd) = fd else {
                issues.insert(base, "not embedded (no font descriptor)".into());
                return;
            };
            if !(fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3")) {
                issues.insert(base, "not embedded".into());
                return;
            }
            // width consistency for embedded Type 1 programs we can parse
            if let Ok(Object::Stream(ff)) = fd.get(b"FontFile").map(|x| deref(doc, x)) {
                let l1 = ff
                    .dict
                    .get(b"Length1")
                    .and_then(Object::as_i64)
                    .unwrap_or(0) as usize;
                let l2 = ff
                    .dict
                    .get(b"Length2")
                    .and_then(Object::as_i64)
                    .unwrap_or(0) as usize;
                if let Ok(data) = ff.decompressed_content_with_limit(MAX_STREAM) {
                    if l1 > 0 && l2 > 0 && l1 + l2 <= data.len() {
                        let bin = &data[l1..l1 + l2];
                        // binary eexec only (hex PFA variants are skipped)
                        if bin.iter().take(4).any(|b| !b.is_ascii_hexdigit()) {
                            let prog = parse_type1(&data[..l1], bin);
                            if !prog.widths.is_empty() {
                                let enc = font_encoding(doc, d, &prog.encoding);
                                let first = d
                                    .get(b"FirstChar")
                                    .and_then(Object::as_i64)
                                    .unwrap_or(0)
                                    .clamp(0, 255);
                                if let Ok(Object::Array(ws)) =
                                    d.get(b"Widths").map(|x| deref(doc, x))
                                {
                                    let bad = ws.iter().enumerate().find_map(|(i, w)| {
                                        let code = first as usize + i;
                                        // only glyphs the content actually shows
                                        if !used.map(|u| u.get(code) == Some(&true)).unwrap_or(true)
                                        {
                                            return None;
                                        }
                                        let n = enc.get(code)?.as_ref()?;
                                        let &pw = prog.widths.get(n)?;
                                        let w = deref(doc, w).as_float().unwrap_or(pw);
                                        ((w - pw).abs() > 1.0).then(|| format!("{n}: {w} vs {pw}"))
                                    });
                                    if let Some(b) = bad {
                                        issues.insert(
                                            base,
                                            format!("/Widths don't match the embedded font program ({b})"),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Character codes shown with each simple font (by font object), from page
/// content and the form XObjects it draws. Fonts not in the map weren't
/// seen in page content (annotations, patterns…).
fn used_codes(doc: &Document) -> HashMap<ObjectId, [bool; 256]> {
    fn walk(
        doc: &Document,
        content: &[u8],
        res: &Dictionary,
        out: &mut HashMap<ObjectId, [bool; 256]>,
        depth: usize,
    ) {
        let Ok(c) = Content::decode(content) else {
            return;
        };
        let fonts = res
            .get(b"Font")
            .map(|f| deref(doc, f))
            .ok()
            .and_then(|f| f.as_dict().ok());
        let xobjects = res
            .get(b"XObject")
            .map(|f| deref(doc, f))
            .ok()
            .and_then(|f| f.as_dict().ok());
        let mut cur: Option<ObjectId> = None;
        let mark = |cur: Option<ObjectId>, s: &[u8], out: &mut HashMap<ObjectId, [bool; 256]>| {
            if let Some(id) = cur {
                let e = out.entry(id).or_insert([false; 256]);
                for &b in s {
                    e[b as usize] = true;
                }
            }
        };
        for op in &c.operations {
            match op.operator.as_str() {
                "Tf" => {
                    cur = op
                        .operands
                        .first()
                        .and_then(|n| n.as_name().ok())
                        .and_then(|n| fonts?.get(n).ok())
                        .and_then(|r| r.as_reference().ok());
                }
                "Tj" | "'" | "\"" => {
                    if let Some(Object::String(s, _)) = op.operands.last() {
                        mark(cur, s, out);
                    }
                }
                "TJ" => {
                    if let Some(Object::Array(a)) = op.operands.first() {
                        for x in a {
                            if let Object::String(s, _) = x {
                                mark(cur, s, out);
                            }
                        }
                    }
                }
                "Do" if depth < 8 => {
                    let Some(id) = op
                        .operands
                        .first()
                        .and_then(|n| n.as_name().ok())
                        .and_then(|n| xobjects?.get(n).ok())
                        .and_then(|r| r.as_reference().ok())
                    else {
                        continue;
                    };
                    let Ok(Object::Stream(s)) = doc.get_object(id) else {
                        continue;
                    };
                    if name(&s.dict, b"Subtype") != Some(b"Form") {
                        continue;
                    }
                    let sub = match s.dict.get(b"Resources").map(|r| deref(doc, r)) {
                        Ok(Object::Dictionary(r)) => r.clone(),
                        _ => res.clone(),
                    };
                    if let Ok(data) = s.decompressed_content_with_limit(MAX_STREAM) {
                        walk(doc, &data, &sub, out, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = HashMap::new();
    for (_, page) in doc.get_pages() {
        let mut res = None;
        let mut id = page;
        for _ in 0..32 {
            let Ok(d) = doc.get_dictionary(id) else { break };
            if let Ok(Object::Dictionary(r)) = d.get(b"Resources").map(|r| deref(doc, r)) {
                res = Some(r.clone());
                break;
            }
            match d.get(b"Parent").and_then(Object::as_reference) {
                Ok(p) => id = p,
                Err(_) => break,
            }
        }
        if let (Some(res), Ok(c)) = (res, doc.get_page_content_with_limit(page, MAX_STREAM)) {
            walk(doc, &c, &res, &mut out, 0);
        }
    }
    out
}

fn xmp_value(xmp: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let i = xmp.find(&open)?;
    let rest = &xmp[i + open.len()..];
    // attribute form: pdfaid:part="2"
    if let Some(eq) = rest.strip_prefix('=') {
        let q = eq.chars().next()?;
        let v: String = eq[1..].chars().take_while(|&c| c != q).collect();
        return Some(v);
    }
    let gt = rest.find('>')?;
    let body = &rest[gt + 1..];
    let close = format!("</{tag}>");
    let end = body.find(&close)?;
    let inner = &body[..end];
    // rdf:Alt / rdf:Seq → first li
    if let Some(li) = inner.find("<rdf:li") {
        let after = &inner[li..];
        let s = after.find('>')? + 1;
        let e = after.find("</rdf:li>")?;
        return Some(after[s..e].to_string());
    }
    Some(inner.trim().to_string())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn same_instant(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let norm = |s: &str| s.replace("+00:00", "Z");
            norm(a) == norm(b)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewers::pdf::tools::repair::tests::sample;

    #[test]
    fn foxit_widths_match_afm() {
        for (f, glyph, w) in [
            (StandardFont::Helvetica, "a", 556.0),
            (StandardFont::Helvetica, "W", 944.0),
            (StandardFont::HelveticaBold, "a", 556.0),
            (StandardFont::TimesRoman, "a", 444.0),
            (StandardFont::TimesBold, "W", 1000.0),
            (StandardFont::Courier, "m", 600.0),
            (StandardFont::Symbol, "alpha", 631.0),
            (StandardFont::ZapfDingBats, "a1", 974.0),
        ] {
            let (data, _) = f.get_font_data();
            let p = parse_cff((*data).as_ref()).unwrap();
            assert!(p.widths.len() > 100, "{f:?}: {} glyphs", p.widths.len());
            assert_eq!(p.widths.get(glyph).copied(), Some(w), "{f:?} {glyph}");
        }
    }

    #[test]
    fn srgb_profile_is_well_formed() {
        let p = srgb_icc();
        assert_eq!(
            u32::from_be_bytes(p[0..4].try_into().unwrap()) as usize,
            p.len()
        );
        let (cs, class, ver) = icc_header(&p).unwrap();
        assert_eq!((&cs, &class, ver), (b"RGB ", b"mntr", 2));
        assert_eq!(p.len() % 4, 0);
    }

    #[test]
    fn converts_sample_and_verifies_clean() {
        let src = sample(2);
        // the sample uses non-embedded Helvetica and DeviceGray fills
        let before = verify(&src).unwrap();
        assert!(before.iter().any(|v| v.contains("XMP")), "{before:?}");
        assert!(before.iter().any(|v| v.contains("Helvetica")), "{before:?}");
        let out = convert(&src).unwrap();
        assert!(out.report.contains("no violations found"), "{}", out.report);
        assert!(verify(&out.pdf).unwrap().is_empty());
        // text survives and renders
        let d = Document::load_mem(&out.pdf).unwrap();
        let t = d.extract_text(&[1]).unwrap();
        assert!(t.contains("Page 1"), "{t}");
        assert!(
            crate::viewers::pdf::tools::repair::unrenderable_pages(&out.pdf)
                .unwrap()
                .is_empty()
        );
        assert!(out.pdf.starts_with(b"%PDF-1.7"));
    }

    #[test]
    fn removes_forbidden_things_and_handles_cmyk() {
        let src = sample(1);
        let mut doc = Document::load_mem(&src).unwrap();
        let cat = catalog_id(&doc).unwrap();
        let js = doc.add_object(
            dictionary! { "S" => "JavaScript", "JS" => Object::string_literal("app.alert(1)") },
        );
        let page = *doc.get_pages().values().next().unwrap();
        let annot = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![10.into(), 10.into(), 30.into(), 30.into()],
            "F" => 2, "AA" => dictionary! { "E" => js },
        });
        let movie = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Movie", "Rect" => vec![0.into(), 0.into(), 5.into(), 5.into()],
        });
        doc.get_dictionary_mut(page)
            .unwrap()
            .set("Annots", vec![annot.into(), movie.into()]);
        // CMYK content
        let c = doc.add_object(Stream::new(
            dictionary! {},
            b"0 1 1 0 k 0 0 10 10 re f".to_vec(),
        ));
        let mut contents = vec![doc
            .get_dictionary(page)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .clone()];
        contents.push(c.into());
        doc.get_dictionary_mut(page)
            .unwrap()
            .set("Contents", contents);
        let c = doc.get_dictionary_mut(cat).unwrap();
        c.set("OpenAction", js);
        c.set(
            "AcroForm",
            dictionary! { "Fields" => vec![], "NeedAppearances" => true },
        );
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let before = verify(&buf).unwrap();
        assert!(
            before.iter().any(|v| v.contains("OpenAction")),
            "{before:?}"
        );
        assert!(
            before.iter().any(|v| v.contains("NeedAppearances")),
            "{before:?}"
        );
        let out = convert(&buf).unwrap();
        let after = verify(&out.pdf).unwrap();
        // the text annotation has no appearance: honestly reported
        assert!(
            after.iter().all(|v| v.contains("without appearance")),
            "{after:?}\n{}",
            out.report
        );
        assert!(out.report.contains("DefaultCMYK"), "{}", out.report);
        assert!(!out.pdf.windows(10).any(|w| w == b"JavaScript"));
        assert!(!out.pdf.windows(6).any(|w| w == b"/Movie"));
    }

    #[test]
    fn info_xmp_dates_roundtrip() {
        let (x, p) = pdf_date_to_xmp("D:20240102030405+01'30'").unwrap();
        assert_eq!(x, "2024-01-02T03:04:05+01:30");
        assert_eq!(p, "D:20240102030405+01'30'");
        assert_eq!(pdf_date_to_xmp("D:2024").unwrap().0, "2024-01-01T00:00:00Z");
    }
}
