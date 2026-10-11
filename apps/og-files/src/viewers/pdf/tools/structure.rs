//! lopdf-only document surgery: metadata, security, sanitize, attachments,
//! bookmarks, forms, text compare/redact, repair and lossless compression.
//!
//! Every function takes the current document bytes and either returns new
//! bytes, extracted files or a text report. Malformed input yields
//! `Err(String)`; object graphs are walked with visited sets / depth limits.

// The viewer UI that calls these isn't wired up yet; drop this once it is.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Document Info dictionary fields. Empty string = absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
    pub creator: String,
    pub producer: String,
    /// raw PDF date strings ("D:20261011120000Z"); shown/edited as text
    pub creation_date: String,
    pub mod_date: String,
}

/// What a password-protected output allows (owner password bypasses all).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Permissions {
    pub print: bool,
    pub print_high_quality: bool,
    pub modify: bool,
    pub copy: bool,
    pub annotate: bool,
    pub fill_forms: bool,
    pub accessibility: bool,
    pub assemble: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            print: true,
            print_high_quality: true,
            modify: true,
            copy: true,
            annotate: true,
            fill_forms: true,
            accessibility: true,
            assemble: true,
        }
    }
}

/// Things `sanitize` can strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sanitize {
    pub javascript: bool,
    pub embedded_files: bool,
    pub xmp_metadata: bool,
    pub document_info: bool,
    pub links: bool,
    pub fonts: bool,
}

impl Default for Sanitize {
    fn default() -> Self {
        Self {
            javascript: true,
            embedded_files: true,
            xmp_metadata: false,
            document_info: false,
            links: false,
            fonts: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    pub title: String,
    /// 0-based page index
    pub page: usize,
    /// 0 = top level
    pub level: usize,
}

/// Top-level bookmarks as (title, pages from that bookmark up to the next),
/// for "split by bookmarks". Bookmarks are ordered by page; pages before
/// the first bookmark form a leading "(before first bookmark)" group, and
/// bookmarks that would get no pages (same page as the next) are dropped.
pub fn bookmark_groups(pdf: &[u8]) -> Result<Vec<(String, Vec<usize>)>, String> {
    let doc = load(pdf)?;
    let n = doc.get_pages().len();
    let mut tops: Vec<(String, usize)> = read_outline(&doc)
        .into_iter()
        .filter(|b| b.level == 0 && b.page < n)
        .map(|b| (b.title, b.page))
        .collect();
    if tops.is_empty() {
        return Err("the document has no bookmarks".into());
    }
    tops.sort_by_key(|t| t.1);
    let mut groups = Vec::new();
    if tops[0].1 > 0 {
        groups.push((
            "(before first bookmark)".to_string(),
            (0..tops[0].1).collect(),
        ));
    }
    for (i, (title, start)) in tops.iter().enumerate() {
        let end = tops.get(i + 1).map(|t| t.1).unwrap_or(n);
        if end > *start {
            groups.push((title.clone(), (*start..end).collect()));
        }
    }
    Ok(groups)
}

/// Greedy page groups whose assembled size stays under `max_bytes` each
/// (a single page larger than the limit gets its own group). Sizes are
/// measured by really assembling candidate groups with `compose::build`
/// (galloping + binary search, so O(log n) builds per group); assumes the
/// size grows with the number of pages.
pub fn size_groups(pdf: &[u8], max_bytes: u64) -> Result<Vec<Vec<usize>>, String> {
    let n = load(pdf)?.get_pages().len();
    if n == 0 {
        return Err("the document has no pages".into());
    }
    let src = vec![Arc::new(pdf.to_vec())];
    let fits = |a: usize, b: usize| -> Result<bool, String> {
        let plan: Vec<_> = (a..b).map(|p| (0usize, p, 0u16)).collect();
        Ok(crate::viewers::pdf::compose::build(&src, &plan)?.len() as u64 <= max_bytes)
    };
    let mut groups = Vec::new();
    let mut start = 0;
    while start < n {
        let mut lo = start + 1; // a lone page is always accepted
        if fits(start, lo)? {
            let mut hi = None;
            let mut step = 1;
            while lo < n {
                let probe = (lo + step).min(n);
                if fits(start, probe)? {
                    lo = probe;
                    step *= 2;
                } else {
                    hi = Some(probe);
                    break;
                }
            }
            if let Some(mut hi) = hi {
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    if fits(start, mid)? {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
            }
        }
        groups.push((start..lo).collect());
        start = lo;
    }
    Ok(groups)
}

/// Set the CropBox of `pages`, trimming margins in points (left, bottom,
/// right, top) from the current visible box. Margins are in unrotated page
/// space (a /Rotate'd page is trimmed in its own coordinate system).
pub fn crop(pdf: &[u8], pages: &[usize], margins: [f32; 4]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let ids = page_ids(&doc);
    if margins.iter().any(|m| !m.is_finite()) {
        return Err("invalid margins".into());
    }
    for &p in pages {
        let id = *ids
            .get(p)
            .ok_or_else(|| format!("page {} doesn't exist", p + 1))?;
        let b = visible_box(&doc, id);
        let nb = [
            b[0] + margins[0],
            b[1] + margins[1],
            b[2] - margins[2],
            b[3] - margins[3],
        ];
        if nb[2] - nb[0] < 1.0 || nb[3] - nb[1] < 1.0 {
            return Err(format!("margins are larger than page {}", p + 1));
        }
        let dict = doc
            .get_dictionary_mut(id)
            .map_err(|_| "broken page object")?;
        dict.set(
            "CropBox",
            nb.iter().map(|&v| Object::Real(v)).collect::<Vec<_>>(),
        );
    }
    save(&mut doc)
}

pub fn metadata_get(pdf: &[u8]) -> Result<Metadata, String> {
    let doc = load(pdf)?;
    Ok(read_metadata(&doc))
}

/// Write `meta` into the Info dictionary (empty fields removed). With
/// `remove_all`, delete Info and the XMP metadata stream entirely instead.
/// Limitation: an existing XMP stream is left as is when fields are set, so
/// readers that prefer XMP may still show the old values.
pub fn metadata_set(pdf: &[u8], meta: &Metadata, remove_all: bool) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    if remove_all {
        doc.trailer.remove(b"Info");
        if let Some(cat) = catalog_mut(&mut doc) {
            cat.remove(b"Metadata");
        }
        doc.prune_objects();
        return save(&mut doc);
    }
    let fields: [(&str, &str); 8] = [
        ("Title", &meta.title),
        ("Author", &meta.author),
        ("Subject", &meta.subject),
        ("Keywords", &meta.keywords),
        ("Creator", &meta.creator),
        ("Producer", &meta.producer),
        ("CreationDate", &meta.creation_date),
        ("ModDate", &meta.mod_date),
    ];
    // keep custom keys of an existing Info dictionary
    let mut info = match doc.trailer.get(b"Info") {
        Ok(o) => dict_of(&doc, o).cloned().unwrap_or_default(),
        Err(_) => Dictionary::new(),
    };
    for (key, value) in fields {
        let value = value.trim();
        if value.is_empty() {
            info.remove(key.as_bytes());
        } else if key.ends_with("Date") {
            info.set(key, Object::string_literal(value));
        } else {
            info.set(key, lopdf::text_string(value));
        }
    }
    match doc.trailer.get(b"Info").and_then(Object::as_reference) {
        Ok(id) if doc.objects.contains_key(&id) => {
            doc.objects.insert(id, Object::Dictionary(info));
        }
        _ => {
            let id = doc.add_object(info);
            doc.trailer.set("Info", id);
        }
    }
    save(&mut doc)
}

/// Human-readable summary: version, page count + sizes, encryption,
/// metadata, fonts (embedded or not), image count, attachments, form
/// fields, bookmarks, JavaScript presence, signatures. A document that
/// needs a password only reports version and encryption.
pub fn info_report(pdf: &[u8]) -> Result<String, String> {
    let doc = load_any(pdf)?;
    let mut r = String::new();
    let line = |r: &mut String, s: String| {
        r.push_str(&s);
        r.push('\n');
    };
    line(&mut r, format!("PDF version: {}", doc.version));
    line(
        &mut r,
        format!("File size: {}", human_size(pdf.len() as u64)),
    );
    if doc.is_encrypted() {
        line(
            &mut r,
            "Encryption: yes, a password is required to open it".into(),
        );
        line(
            &mut r,
            "(other details are unavailable until the password is removed)".into(),
        );
        return Ok(r);
    }
    match &doc.encryption_state {
        Some(st) => {
            let p = st.permissions();
            let mut allowed = Vec::new();
            for (flag, name) in [
                (lopdf::Permissions::PRINTABLE, "print"),
                (lopdf::Permissions::MODIFIABLE, "modify"),
                (lopdf::Permissions::COPYABLE, "copy"),
                (lopdf::Permissions::ANNOTABLE, "annotate"),
                (lopdf::Permissions::FILLABLE, "fill forms"),
                (lopdf::Permissions::ASSEMBLABLE, "assemble"),
            ] {
                if p.contains(flag) {
                    allowed.push(name);
                }
            }
            line(
                &mut r,
                format!(
                    "Encryption: yes (V{} R{}), opens without a password; allowed: {}",
                    st.version(),
                    st.revision(),
                    if allowed.is_empty() {
                        "nothing".to_string()
                    } else {
                        allowed.join(", ")
                    }
                ),
            );
        }
        None => line(&mut r, "Encryption: none".into()),
    }

    let ids = page_ids(&doc);
    line(&mut r, format!("Pages: {}", ids.len()));
    let mut sizes: Vec<(String, usize)> = Vec::new();
    for &id in &ids {
        let b = visible_box(&doc, id);
        let (mut w, mut h) = (b[2] - b[0], b[3] - b[1]);
        let rot = inherited(&doc, id, b"Rotate")
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(0);
        if rot.rem_euclid(180) == 90 {
            std::mem::swap(&mut w, &mut h);
        }
        let label = format!("{:.0} x {:.0} pt{}", w, h, paper_name(w, h));
        match sizes.iter_mut().find(|s| s.0 == label) {
            Some(s) => s.1 += 1,
            None => sizes.push((label, 1)),
        }
    }
    for (label, count) in &sizes {
        line(&mut r, format!("  {label}: {count} page(s)"));
    }

    let m = read_metadata(&doc);
    line(&mut r, "Metadata:".into());
    let mut any = false;
    for (k, v) in [
        ("Title", &m.title),
        ("Author", &m.author),
        ("Subject", &m.subject),
        ("Keywords", &m.keywords),
        ("Creator", &m.creator),
        ("Producer", &m.producer),
        ("Created", &m.creation_date),
        ("Modified", &m.mod_date),
    ] {
        if !v.is_empty() {
            any = true;
            line(&mut r, format!("  {k}: {v}"));
        }
    }
    if !any {
        line(&mut r, "  (none)".into());
    }
    let xmp = catalog(&doc)
        .and_then(|c| c.get(b"Metadata").ok())
        .is_some();
    line(
        &mut r,
        format!("XMP metadata: {}", if xmp { "yes" } else { "no" }),
    );

    let fonts = font_list(&doc);
    line(&mut r, format!("Fonts: {}", fonts.len()));
    for (name, (sub, embedded)) in &fonts {
        line(
            &mut r,
            format!(
                "  {name} ({sub}){}",
                if *embedded {
                    ", embedded"
                } else {
                    ", not embedded"
                }
            ),
        );
    }

    let images = doc
        .objects
        .values()
        .filter(
            |o| matches!(o, Object::Stream(s) if name_of(&s.dict, b"Subtype") == Some(b"Image")),
        )
        .count();
    line(&mut r, format!("Images: {images}"));

    let att = attachment_specs(&doc);
    line(&mut r, format!("Attachments: {}", att.len()));
    for (name, _) in &att {
        line(&mut r, format!("  {name}"));
    }

    let fields = all_fields(&doc);
    let terminal: Vec<_> = fields.iter().filter(|f| f.terminal).collect();
    line(&mut r, format!("Form fields: {}", terminal.len()));
    for f in terminal.iter().take(50) {
        line(&mut r, format!("  {} ({})", f.name, f.kind));
    }
    if terminal.len() > 50 {
        line(&mut r, format!("  … {} more", terminal.len() - 50));
    }

    line(&mut r, format!("Bookmarks: {}", read_outline(&doc).len()));
    let js = collect_js(&doc);
    line(
        &mut r,
        format!(
            "JavaScript: {}",
            if js.is_empty() {
                "none".into()
            } else {
                format!("{} block(s)", js.len())
            }
        ),
    );

    let sigs: Vec<_> = fields.iter().filter(|f| f.kind == "signature").collect();
    if sigs.is_empty() {
        line(&mut r, "Signatures: none".into());
    } else {
        line(&mut r, format!("Signatures: {}", sigs.len()));
        for s in sigs {
            line(
                &mut r,
                format!(
                    "  {} ({})",
                    s.name,
                    if s.signed { "signed" } else { "empty" }
                ),
            );
        }
    }
    Ok(r)
}

/// AES-256 (PDF 2.0, V5/R6) encryption. Empty user password = opens
/// without a password but permissions still apply. An empty owner password
/// is replaced by a random one (so permissions can't be bypassed with "").
pub fn encrypt(
    pdf: &[u8],
    user_password: &str,
    owner_password: &str,
    perms: Permissions,
) -> Result<Vec<u8>, String> {
    use lopdf::encryption::crypt_filters::{Aes256CryptFilter, CryptFilter};
    use lopdf::Permissions as P;
    let mut doc = load(pdf)?;
    let mut p = P::empty();
    for (on, flag) in [
        (perms.print, P::PRINTABLE),
        (perms.print_high_quality, P::PRINTABLE_IN_HIGH_QUALITY),
        (perms.modify, P::MODIFIABLE),
        (perms.copy, P::COPYABLE),
        (perms.annotate, P::ANNOTABLE),
        (perms.fill_forms, P::FILLABLE),
        (perms.accessibility, P::COPYABLE_FOR_ACCESSIBILITY),
        (perms.assemble, P::ASSEMBLABLE),
    ] {
        if on {
            p |= flag;
        }
    }
    let owner = if owner_password.is_empty() {
        random_bytes(16)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    } else {
        owner_password.to_string()
    };
    if !doc.trailer.has(b"ID") {
        let a = Object::String(random_bytes(16), StringFormat::Hexadecimal);
        doc.trailer.set("ID", vec![a.clone(), a]);
    }
    if doc.version.as_str() < "1.7" {
        doc.version = "1.7".into();
    }
    if let Some(cat) = catalog_mut(&mut doc) {
        cat.set(
            "Extensions",
            dictionary! { "ADBE" => dictionary! { "BaseVersion" => Object::Name(b"1.7".to_vec()), "ExtensionLevel" => 8 } },
        );
    }
    let key = random_bytes(32);
    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
    let version = lopdf::EncryptionVersion::V5 {
        encrypt_metadata: true,
        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
        file_encryption_key: &key,
        stream_filter: b"StdCF".to_vec(),
        string_filter: b"StdCF".to_vec(),
        owner_password: &owner,
        user_password,
        permissions: p,
    };
    let state =
        lopdf::EncryptionState::try_from(version).map_err(|e| format!("encryption failed: {e}"))?;
    clean_trailer(&mut doc);
    doc.encrypt(&state)
        .map_err(|e| format!("encryption failed: {e}"))?;
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("could not write PDF: {e}"))?;
    Ok(buf)
}

/// Remove encryption, given the user or owner password. A file that opens
/// without a password (permission-only protection) is unlocked whatever
/// password is given, as most tools do.
pub fn decrypt(pdf: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let doc = load_any(pdf)?;
    let mut doc = if doc.is_encrypted() {
        let d = Document::load_mem_with_options(pdf, lopdf::LoadOptions::with_password(password))
            .map_err(|_| "wrong password".to_string())?;
        if d.is_encrypted() {
            return Err("wrong password".into());
        }
        d
    } else if doc.encryption_state.is_some() {
        doc
    } else {
        return Err("the document is not encrypted".into());
    };
    doc.trailer.remove(b"Encrypt");
    save(&mut doc)
}

/// Strip the selected kinds of content. JavaScript: document-level names,
/// all /AA additional actions, JavaScript /A, /OpenAction and /Next actions
/// and XFA forms. Links: Link annotations only. Fonts: embedded font
/// programs are dropped (viewers substitute fonts, so text may look off).
pub fn sanitize(pdf: &[u8], what: Sanitize) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let cat_id = catalog_id(&doc).ok_or("the document has no catalog")?;
    if what.javascript {
        let js_ids: HashSet<ObjectId> = doc
            .objects
            .iter()
            .filter(|(_, o)| o.as_dict().map(is_js_action).unwrap_or(false))
            .map(|(id, _)| *id)
            .collect();
        let is_js = |o: &Object| match o {
            Object::Reference(id) => js_ids.contains(id),
            Object::Dictionary(d) => is_js_action(d),
            _ => false,
        };
        for_each_dict_mut(&mut doc, &mut |d: &mut Dictionary| {
            d.remove(b"AA");
            for key in [&b"A"[..], b"OpenAction", b"Next"] {
                match d.get_mut(key) {
                    Ok(Object::Array(a)) => a.retain(|o| !is_js(o)),
                    Ok(o) if is_js(o) => {
                        d.remove(key);
                    }
                    _ => {}
                }
            }
            if d.has(b"Fields") {
                d.remove(b"XFA");
            }
        });
        if let Some(names) = sub_dict_mut(&mut doc, cat_id, b"Names") {
            names.remove(b"JavaScript");
        }
    }
    if what.embedded_files {
        if let Some(names) = sub_dict_mut(&mut doc, cat_id, b"Names") {
            names.remove(b"EmbeddedFiles");
        }
        if let Some(cat) = catalog_mut(&mut doc) {
            cat.remove(b"AF");
        }
        filter_annots(&mut doc, |d| {
            name_of(d, b"Subtype") != Some(b"FileAttachment")
        });
    }
    if what.links {
        filter_annots(&mut doc, |d| name_of(d, b"Subtype") != Some(b"Link"));
    }
    if what.xmp_metadata {
        let xmp: HashSet<ObjectId> = doc
            .objects
            .iter()
            .filter(|(_, o)| {
                matches!(o, Object::Stream(s) if name_of(&s.dict, b"Type") == Some(b"Metadata")
                    || name_of(&s.dict, b"Subtype") == Some(b"XML"))
            })
            .map(|(id, _)| *id)
            .collect();
        for_each_dict_mut(&mut doc, &mut |d: &mut Dictionary| {
            if matches!(d.get(b"Metadata"), Ok(Object::Reference(id)) if xmp.contains(id)) {
                d.remove(b"Metadata");
            }
        });
        if let Some(cat) = catalog_mut(&mut doc) {
            cat.remove(b"Metadata");
        }
    }
    if what.document_info {
        doc.trailer.remove(b"Info");
    }
    if what.fonts {
        for_each_dict_mut(&mut doc, &mut |d: &mut Dictionary| {
            if name_of(d, b"Type") == Some(b"FontDescriptor") {
                d.remove(b"FontFile");
                d.remove(b"FontFile2");
                d.remove(b"FontFile3");
            }
        });
    }
    doc.prune_objects();
    save(&mut doc)
}

/// Every piece of JavaScript in the document with where it lives
/// (document-level names, OpenAction, page/annotation/field actions).
pub fn javascript_report(pdf: &[u8]) -> Result<String, String> {
    let doc = load(pdf)?;
    let hits = collect_js(&doc);
    if hits.is_empty() {
        return Ok("No JavaScript found.".into());
    }
    let mut r = format!("{} JavaScript block(s) found.\n", hits.len());
    for (place, code) in hits {
        r.push_str(&format!("\n== {place} ==\n{}\n", code.trim_end()));
    }
    Ok(r)
}

/// Remove every annotation (links, comments, widgets…) from all pages. The
/// AcroForm goes too, since its widgets are gone.
pub fn remove_annotations(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    for id in page_ids(&doc) {
        if let Ok(d) = doc.get_dictionary_mut(id) {
            d.remove(b"Annots");
        }
    }
    if let Some(cat) = catalog_mut(&mut doc) {
        cat.remove(b"AcroForm");
    }
    doc.prune_objects();
    save(&mut doc)
}

/// Remove image XObjects and their `Do` operators from page content.
/// Inline images (BI…EI) are dropped too; Form XObjects are processed
/// recursively. Every image XObject in the file is deleted (also ones used
/// only by annotation appearances).
pub fn remove_images(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let images: HashSet<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, o)| matches!(o, Object::Stream(s) if name_of(&s.dict, b"Subtype") == Some(b"Image")))
        .map(|(id, _)| *id)
        .collect();
    let mut forms_done = HashSet::new();
    for pid in page_ids(&doc) {
        let res = inherited(&doc, pid, b"Resources")
            .and_then(|o| dict_of(&doc, o))
            .cloned()
            .unwrap_or_default();
        let names = image_names(&doc, &res, &images);
        let forms = form_xobjects(&doc, &res);
        if let Ok(bytes) = doc.get_page_content_with_limit(pid, MAX_STREAM) {
            if let Ok(content) = Content::decode(&bytes) {
                let ops = strip_image_ops(content.operations, &names);
                if let Some(enc) = encode_ops(&ops) {
                    set_page_content(&mut doc, pid, enc);
                }
            }
        }
        strip_forms(&mut doc, forms, &res, &images, &mut forms_done, 0);
    }
    for id in &images {
        doc.objects.remove(id);
    }
    drop_refs_to(&mut doc, &images);
    doc.prune_objects();
    save(&mut doc)
}

/// Embed files (name, bytes) as document attachments. The EmbeddedFiles
/// name tree is rebuilt flat (existing entries kept); duplicate names get a
/// " (2)" style suffix.
pub fn attachments_add(pdf: &[u8], files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let cat_id = catalog_id(&doc).ok_or("the document has no catalog")?;
    let mut entries: Vec<(Vec<u8>, Object)> = match catalog(&doc)
        .and_then(|c| get_dict(&doc, c, b"Names"))
        .and_then(|n| get_dict(&doc, n, b"EmbeddedFiles"))
    {
        Some(tree) => {
            let mut out = Vec::new();
            name_tree(&doc, tree, &mut out, &mut HashSet::new(), 0);
            out.into_iter().map(|(k, v)| (k, v.clone())).collect()
        }
        None => Vec::new(),
    };
    for (name, bytes) in files {
        let base = file_name_only(name);
        let base = if base.is_empty() {
            "attachment".to_string()
        } else {
            base
        };
        let mut unique = base.clone();
        let mut k = 2;
        while entries
            .iter()
            .any(|(n, _)| text_of(&Object::String(n.clone(), StringFormat::Literal)) == unique)
        {
            unique = match base.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({k}).{ext}"),
                _ => format!("{base} ({k})"),
            };
            k += 1;
        }
        let mut stream = Stream::new(
            dictionary! { "Type" => "EmbeddedFile", "Params" => dictionary! { "Size" => bytes.len() as i64 } },
            bytes.clone(),
        );
        let _ = stream.compress();
        let sid = doc.add_object(stream);
        let spec = doc.add_object(dictionary! {
            "Type" => "Filespec",
            "F" => Object::string_literal(ascii_fallback(&unique)),
            "UF" => lopdf::text_string(&unique),
            "EF" => dictionary! { "F" => sid, "UF" => sid },
        });
        let key = match lopdf::text_string(&unique) {
            Object::String(b, _) => b,
            _ => unique.clone().into_bytes(),
        };
        entries.push((key, Object::Reference(spec)));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<Object> = entries
        .into_iter()
        .flat_map(|(k, v)| [Object::String(k, StringFormat::Literal), v])
        .collect();
    let tree = doc.add_object(dictionary! { "Names" => names });
    match sub_dict_mut(&mut doc, cat_id, b"Names") {
        Some(n) => n.set("EmbeddedFiles", tree),
        None => {
            let cat = catalog_mut(&mut doc).ok_or("the document has no catalog")?;
            cat.set("Names", dictionary! { "EmbeddedFiles" => tree });
        }
    }
    doc.prune_objects();
    save(&mut doc)
}

/// All embedded attachments as (file name, bytes): the EmbeddedFiles name
/// tree plus FileAttachment annotations. Names are reduced to their last
/// path component.
pub fn attachments_extract(pdf: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let doc = load(pdf)?;
    let mut out = Vec::new();
    for (name, sid) in attachment_specs(&doc) {
        let Some(Object::Stream(s)) = sid.and_then(|id| doc.objects.get(&id)) else {
            continue;
        };
        let bytes = if s.dict.has(b"Filter") {
            match s.decompressed_content_with_limit(MAX_STREAM) {
                Ok(b) => b,
                Err(_) => continue,
            }
        } else {
            s.content.clone()
        };
        out.push((name, bytes));
    }
    Ok(out)
}

/// Images on `pages` as files: JPEG streams passed through as `.jpg`,
/// others decoded to `.png` where the colour space allows. Names like
/// `p03-img1.png`. PNG covers 8-bit DeviceGray/DeviceRGB (and 1/3-component
/// ICCBased/Cal*) images, raw or Flate, with an 8-bit /SMask as alpha;
/// everything else (CMYK, Indexed, JPX, CCITT, JBIG2, 1-bit masks, inline
/// images) is skipped silently. Each image object is emitted once per page.
pub fn extract_images(pdf: &[u8], pages: &[usize]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let doc = load(pdf)?;
    let ids = page_ids(&doc);
    let mut out = Vec::new();
    for &p in pages {
        let pid = *ids
            .get(p)
            .ok_or_else(|| format!("page {} doesn't exist", p + 1))?;
        let res = inherited(&doc, pid, b"Resources")
            .and_then(|o| dict_of(&doc, o))
            .cloned()
            .unwrap_or_default();
        let mut found = Vec::new();
        collect_images(&doc, &res, &mut found, &mut HashSet::new(), 0);
        let mut k = 0;
        for id in found {
            if let Some((ext, bytes)) = convert_image(&doc, id) {
                k += 1;
                out.push((format!("p{:02}-img{k}.{ext}", p + 1), bytes));
            }
        }
    }
    Ok(out)
}

/// Outline entries in order. Entries without a resolvable destination take
/// the page of their first child (or the previous entry / page 0).
pub fn bookmarks_get(pdf: &[u8]) -> Result<Vec<Bookmark>, String> {
    let doc = load(pdf)?;
    Ok(read_outline(&doc))
}

/// Replace the outline with `marks` (levels nest under the previous
/// shallower entry). Each entry jumps to the top of its page; top-level
/// entries are open, deeper ones collapsed.
pub fn bookmarks_set(pdf: &[u8], marks: &[Bookmark]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let ids = page_ids(&doc);
    if let Some(cat) = catalog_mut(&mut doc) {
        cat.remove(b"Outlines");
    }
    if marks.is_empty() {
        doc.prune_objects();
        return save(&mut doc);
    }
    // normalise levels and find parents
    let mut parent: Vec<Option<usize>> = Vec::with_capacity(marks.len());
    let mut levels: Vec<usize> = Vec::with_capacity(marks.len());
    let mut stack: Vec<usize> = Vec::new();
    for (i, m) in marks.iter().enumerate() {
        if m.page >= ids.len() {
            return Err(format!(
                "bookmark \"{}\" points to missing page {}",
                m.title,
                m.page + 1
            ));
        }
        let level = m.level.min(stack.len());
        stack.truncate(level);
        parent.push(stack.last().copied());
        levels.push(level);
        stack.push(i);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); marks.len()];
    let mut top = Vec::new();
    for (i, p) in parent.iter().enumerate() {
        match p {
            Some(p) => children[*p].push(i),
            None => top.push(i),
        }
    }
    // visible descendants if the node were open; level 0 is open
    let mut visible = vec![0i64; marks.len()];
    for i in (0..marks.len()).rev() {
        visible[i] = children[i]
            .iter()
            .map(|&c| 1 + if levels[c] == 0 { visible[c] } else { 0 })
            .sum();
    }
    let root = doc.new_object_id();
    let obj_ids: Vec<ObjectId> = (0..marks.len()).map(|_| doc.new_object_id()).collect();
    let link = |list: &[usize], i: usize| -> (Option<ObjectId>, Option<ObjectId>) {
        let pos = list.iter().position(|&x| x == i).unwrap_or(0);
        let prev = if pos > 0 {
            list.get(pos - 1).map(|&x| obj_ids[x])
        } else {
            None
        };
        (prev, list.get(pos + 1).map(|&x| obj_ids[x]))
    };
    for (i, m) in marks.iter().enumerate() {
        let page = ids[m.page];
        let top_y = visible_box(&doc, page)[3];
        let mut d = dictionary! {
            "Title" => lopdf::text_string(&m.title),
            "Parent" => parent[i].map(|p| obj_ids[p]).unwrap_or(root),
            "Dest" => vec![page.into(), "XYZ".into(), Object::Null, Object::Real(top_y), Object::Null],
        };
        let siblings = match parent[i] {
            Some(p) => &children[p],
            None => &top,
        };
        let (prev, next) = link(siblings, i);
        if let Some(p) = prev {
            d.set("Prev", p);
        }
        if let Some(n) = next {
            d.set("Next", n);
        }
        if let (Some(&f), Some(&l)) = (children[i].first(), children[i].last()) {
            d.set("First", obj_ids[f]);
            d.set("Last", obj_ids[l]);
            let open = levels[i] == 0;
            d.set(
                "Count",
                if open {
                    visible[i]
                } else {
                    -(children[i].len() as i64)
                },
            );
        }
        doc.objects.insert(obj_ids[i], Object::Dictionary(d));
    }
    let total: i64 = top.iter().map(|&t| 1 + visible[t]).sum();
    doc.objects.insert(
        root,
        Object::Dictionary(dictionary! {
            "Type" => "Outlines",
            "First" => obj_ids[top[0]],
            "Last" => obj_ids[*top.last().unwrap_or(&top[0])],
            "Count" => total,
        }),
    );
    let cat = catalog_mut(&mut doc).ok_or("the document has no catalog")?;
    cat.set("Outlines", root);
    doc.prune_objects();
    save(&mut doc)
}

/// Bake form-field appearances into page content and drop the AcroForm,
/// so fields can no longer be edited. Each visible widget's /AP /N stream
/// (the /AS state for checkboxes) is drawn at its /Rect via
/// `q cm /FxN Do Q` and added to the page's XObject resources. Widgets
/// without an appearance stream (NeedAppearances-only forms) vanish.
pub fn flatten_forms(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    for pid in page_ids(&doc) {
        let annots = page_annots(&doc, pid);
        if annots.is_empty() {
            continue;
        }
        let mut keep = Vec::new();
        let mut draws: Vec<(ObjectId, [f32; 6])> = Vec::new();
        for a in annots {
            let Some(d) = dict_of(&doc, &a) else { continue };
            if name_of(d, b"Subtype") != Some(b"Widget") {
                keep.push(a);
                continue;
            }
            let flags = d.get(b"F").and_then(Object::as_i64).unwrap_or(0);
            if flags & 2 != 0 {
                continue; // hidden
            }
            if let Some(draw) = widget_appearance(&doc, d) {
                draws.push(draw);
            }
        }
        set_page_annots(&mut doc, pid, keep);
        if draws.is_empty() {
            continue;
        }
        let mut res = inherited(&doc, pid, b"Resources")
            .and_then(|o| dict_of(&doc, o))
            .cloned()
            .unwrap_or_default();
        let mut xobjs = get_dict(&doc, &res, b"XObject")
            .cloned()
            .unwrap_or_default();
        let mut ops = String::from("Q\n");
        let mut n = 0;
        for (xid, m) in draws {
            let name = loop {
                n += 1;
                let candidate = format!("Fx{n}");
                if !xobjs.has(candidate.as_bytes()) {
                    break candidate;
                }
            };
            xobjs.set(name.clone(), xid);
            if let Ok(Object::Stream(s)) = doc.get_object_mut(xid) {
                s.dict.set("Type", "XObject");
                s.dict.set("Subtype", "Form");
            }
            ops.push_str(&format!(
                "q {} {} {} {} {} {} cm /{name} Do Q\n",
                fmt_num(m[0]),
                fmt_num(m[1]),
                fmt_num(m[2]),
                fmt_num(m[3]),
                fmt_num(m[4]),
                fmt_num(m[5])
            ));
        }
        res.set("XObject", xobjs);
        let mut contents = content_refs(&doc, pid);
        let open = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
        let mut close = Stream::new(Dictionary::new(), ops.into_bytes());
        let _ = close.compress();
        let close = doc.add_object(close);
        contents.insert(0, Object::Reference(open));
        contents.push(Object::Reference(close));
        if let Ok(d) = doc.get_dictionary_mut(pid) {
            d.set("Resources", res);
            d.set("Contents", contents);
        }
    }
    if let Some(cat) = catalog_mut(&mut doc) {
        cat.remove(b"AcroForm");
    }
    doc.prune_objects();
    save(&mut doc)
}

/// Clear ReadOnly flags on all form fields (field /Ff bit 1, and the
/// widget annotation ReadOnly/Locked flags).
pub fn unlock_forms(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let fields = all_fields(&doc);
    if fields.is_empty() {
        return Err("the document has no form fields".into());
    }
    for f in fields {
        if let Ok(d) = doc.get_dictionary_mut(f.id) {
            if let Ok(ff) = d.get(b"Ff").and_then(Object::as_i64) {
                d.set("Ff", ff & !1);
            }
            if let Ok(fl) = d.get(b"F").and_then(Object::as_i64) {
                d.set("F", fl & !(64 | 128));
            }
            d.remove(b"Lock");
        }
    }
    save(&mut doc)
}

/// Remove digital-signature fields/values (the document becomes unsigned).
/// Signature fields are deleted entirely (with their widgets), along with
/// /SigFlags and the catalog /Perms (DocMDP) entry.
pub fn remove_signatures(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let fields = all_fields(&doc);
    let mut drop: HashSet<ObjectId> = HashSet::new();
    for f in fields.iter().filter(|f| f.kind == "signature") {
        drop.insert(f.id);
    }
    // widgets/kids below a signature field
    loop {
        let before = drop.len();
        for f in &fields {
            if f.parent.map(|p| drop.contains(&p)).unwrap_or(false) {
                drop.insert(f.id);
            }
        }
        if drop.len() == before {
            break;
        }
    }
    if drop.is_empty() {
        return Err("the document has no signature fields".into());
    }
    for pid in page_ids(&doc) {
        let annots = page_annots(&doc, pid);
        let n = annots.len();
        let kept: Vec<Object> = annots
            .into_iter()
            .filter(|a| !matches!(a, Object::Reference(id) if drop.contains(id)))
            .collect();
        if kept.len() != n {
            set_page_annots(&mut doc, pid, kept);
        }
    }
    for obj in doc.objects.values_mut() {
        if let Object::Dictionary(d) = obj {
            for key in [&b"Kids"[..], b"Fields"] {
                if let Ok(Object::Array(a)) = d.get_mut(key) {
                    a.retain(|o| !matches!(o, Object::Reference(id) if drop.contains(id)));
                }
            }
        }
    }
    if let Some(cat_id) = catalog_id(&doc) {
        if let Some(form) = sub_dict_mut(&mut doc, cat_id, b"AcroForm") {
            if let Ok(Object::Array(a)) = form.get_mut(b"Fields") {
                a.retain(|o| !matches!(o, Object::Reference(id) if drop.contains(id)));
            }
            form.remove(b"SigFlags");
        }
    }
    for id in &drop {
        doc.objects.remove(id);
    }
    if let Some(cat) = catalog_mut(&mut doc) {
        cat.remove(b"Perms");
    }
    doc.prune_objects();
    save(&mut doc)
}

/// Extracted text per page (best effort; empty string for image-only pages).
pub fn page_texts(pdf: &[u8]) -> Result<Vec<String>, String> {
    let doc = load(pdf)?;
    Ok(texts_of(&doc))
}

/// Line-level text diff of two documents, page by page, as a readable
/// report ("- removed" / "+ added" lines with page headers). Lines are
/// trimmed, blank lines ignored; pages with very many lines (over 3000)
/// are compared as a whole block.
pub fn compare_text(a: &[u8], b: &[u8]) -> Result<String, String> {
    let ta = page_texts(a).map_err(|e| format!("first document: {e}"))?;
    let tb = page_texts(b).map_err(|e| format!("second document: {e}"))?;
    let mut r = format!(
        "First: {} page(s), second: {} page(s)\n",
        ta.len(),
        tb.len()
    );
    let mut changed = 0;
    for i in 0..ta.len().max(tb.len()) {
        let la: Vec<&str> = ta.get(i).map(|t| lines_of(t)).unwrap_or_default();
        let lb: Vec<&str> = tb.get(i).map(|t| lines_of(t)).unwrap_or_default();
        let diff = diff_lines(&la, &lb);
        if diff.iter().all(|d| d.0 == ' ') {
            continue;
        }
        changed += 1;
        let note = match (ta.get(i).is_some(), tb.get(i).is_some()) {
            (true, false) => " (only in first)",
            (false, true) => " (only in second)",
            _ => "",
        };
        r.push_str(&format!("\n=== Page {}{note} ===\n", i + 1));
        for (tag, line) in diff {
            if tag != ' ' {
                r.push_str(&format!("{tag} {line}\n"));
            }
        }
    }
    if changed == 0 {
        r.push_str("\nNo text differences.\n");
    } else {
        r.insert_str(0, &format!("{changed} page(s) differ\n"));
    }
    Ok(r)
}

/// A file-name-safe title for auto-rename: Info Title if meaningful, else
/// the first substantial line of text (first three pages). None if nothing
/// usable.
pub fn suggest_title(pdf: &[u8]) -> Result<Option<String>, String> {
    let doc = load(pdf)?;
    let meta = read_metadata(&doc);
    let mut t = meta.title.trim().to_string();
    for prefix in [
        "Microsoft Word - ",
        "Microsoft PowerPoint - ",
        "Microsoft Excel - ",
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.to_string();
        }
    }
    for ext in [
        ".docx", ".doc", ".pptx", ".ppt", ".xlsx", ".xls", ".pdf", ".odt", ".rtf", ".txt", ".tex",
    ] {
        if t.to_lowercase().ends_with(ext) {
            t.truncate(t.len() - ext.len());
        }
    }
    let low = t.trim().to_lowercase();
    let junk = [
        "", "untitled", "document", "title", "unknown", "none", "null", "slide 1", "page 1",
    ];
    if !junk.contains(&low.as_str()) && t.chars().filter(|c| c.is_alphanumeric()).count() >= 3 {
        let s = safe_file_name(&t);
        if !s.is_empty() {
            return Ok(Some(s));
        }
    }
    let ids = doc.get_pages();
    for (&n, _) in ids.iter().take(3) {
        let text = doc.extract_text(&[n]).unwrap_or_default();
        for line in text.lines() {
            let line = line.trim();
            let letters = line.chars().filter(|c| c.is_alphabetic()).count();
            if letters >= 5 && (line.split_whitespace().count() >= 2 || letters >= 8) {
                let s = safe_file_name(line);
                if !s.is_empty() {
                    return Ok(Some(s));
                }
            }
        }
    }
    Ok(None)
}

/// Remove occurrences of `terms` from text-showing operators (Tj/TJ/'/")
/// in page content. Returns (new pdf, occurrences removed). Works for
/// simple single-byte fonts; text in CID fonts is reported as not found.
/// A term must lie within one text operator (a TJ array counts as one);
/// the removed glyphs are replaced by an equal TJ gap (from /Widths, else
/// 500/1000 em), so following text keeps its position. Text in Form
/// XObjects, annotations or pages with undecodable inline images is not
/// touched.
pub fn redact_text(
    pdf: &[u8],
    terms: &[String],
    case_sensitive: bool,
) -> Result<(Vec<u8>, usize), String> {
    let terms: Vec<Vec<char>> = terms
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| {
            if case_sensitive {
                t.chars().collect()
            } else {
                t.chars().flat_map(char::to_lowercase).collect()
            }
        })
        .collect();
    if terms.is_empty() {
        return Err("no search terms given".into());
    }
    let mut doc = load(pdf)?;
    let mut total = 0;
    for pid in page_ids(&doc) {
        let res = inherited(&doc, pid, b"Resources")
            .and_then(|o| dict_of(&doc, o))
            .cloned()
            .unwrap_or_default();
        let mut fonts: HashMap<Vec<u8>, Option<FontInfo>> = HashMap::new();
        if let Some(fd) = get_dict(&doc, &res, b"Font") {
            for (name, f) in fd.iter() {
                fonts.insert(
                    name.clone(),
                    dict_of(&doc, f).and_then(|d| font_info(&doc, d)),
                );
            }
        }
        let Ok(bytes) = doc.get_page_content_with_limit(pid, MAX_STREAM) else {
            continue;
        };
        let Ok(content) = Content::decode(&bytes) else {
            continue;
        };
        let (ops, count) = redact_ops(content.operations, &fonts, &terms, case_sensitive);
        if count == 0 {
            continue;
        }
        if let Some(enc) = encode_ops(&ops) {
            set_page_content(&mut doc, pid, enc);
            total += count;
        }
    }
    doc.prune_objects();
    Ok((save(&mut doc)?, total))
}

/// Lenient re-read + clean rewrite (rebuilds xref, drops broken objects).
/// Also nulls references to missing objects, flattens the page tree
/// (recovering loose /Page objects when the tree is unusable) and prunes
/// unreachable objects. Can't recover a file lopdf can't parse at all.
pub fn repair(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    let existing: HashSet<ObjectId> = doc.objects.keys().copied().collect();
    doc.traverse_objects(|o| {
        if let Object::Reference(id) = o {
            if !existing.contains(id) {
                *o = Object::Null;
            }
        }
    });
    let mut pages = page_ids(&doc);
    if pages.is_empty() {
        pages = doc
            .objects
            .iter()
            .filter(|(_, o)| {
                o.as_dict()
                    .map(|d| name_of(d, b"Type") == Some(b"Page"))
                    .unwrap_or(false)
            })
            .map(|(id, _)| *id)
            .collect();
    }
    if pages.is_empty() {
        return Err("no pages could be recovered".into());
    }
    for &p in &pages {
        push_down_inherited(&mut doc, p);
    }
    let root = doc.new_object_id();
    for &p in &pages {
        if let Ok(d) = doc.get_dictionary_mut(p) {
            d.set("Parent", root);
            if !d.has(b"MediaBox") {
                d.set("MediaBox", vec![0.into(), 0.into(), 612.into(), 792.into()]);
            }
        }
    }
    let kids: Vec<Object> = pages.iter().map(|&p| Object::Reference(p)).collect();
    let count = kids.len() as i64;
    doc.objects.insert(
        root,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    match catalog_mut(&mut doc) {
        Some(cat) => cat.set("Pages", root),
        None => {
            let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
            doc.trailer.set("Root", cat);
        }
    }
    if !matches!(
        doc.trailer.get(b"Info"),
        Ok(Object::Reference(_)) | Ok(Object::Dictionary(_))
    ) {
        doc.trailer.remove(b"Info");
    }
    doc.prune_objects();
    doc.renumber_objects();
    doc.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    save(&mut doc)
}

/// Lossless size reduction: compress streams, object streams, prune unused
/// objects, de-duplicate identical streams. Already-compressed streams are
/// not recompressed; if the result isn't smaller the input is returned.
pub fn compress_lossless(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let mut doc = load(pdf)?;
    // de-duplicate byte-identical streams
    let mut by_hash: HashMap<u64, Vec<ObjectId>> = HashMap::new();
    for (id, o) in &doc.objects {
        if let Object::Stream(s) = o {
            if name_of(&s.dict, b"Type") == Some(b"XRef")
                || name_of(&s.dict, b"Type") == Some(b"ObjStm")
            {
                continue;
            }
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.content.hash(&mut h);
            s.dict.len().hash(&mut h);
            by_hash.entry(h.finish()).or_default().push(*id);
        }
    }
    let mut remap: HashMap<ObjectId, ObjectId> = HashMap::new();
    for ids in by_hash.values().filter(|v| v.len() > 1) {
        for (i, &a) in ids.iter().enumerate() {
            if remap.contains_key(&a) {
                continue;
            }
            for &b in &ids[i + 1..] {
                if remap.contains_key(&b) {
                    continue;
                }
                if let (Some(Object::Stream(sa)), Some(Object::Stream(sb))) =
                    (doc.objects.get(&a), doc.objects.get(&b))
                {
                    if sa.content == sb.content && sa.dict == sb.dict {
                        remap.insert(b, a);
                    }
                }
            }
        }
    }
    if !remap.is_empty() {
        doc.traverse_objects(|o| {
            if let Object::Reference(id) = o {
                if let Some(to) = remap.get(id) {
                    *id = *to;
                }
            }
        });
        for id in remap.keys() {
            doc.objects.remove(id);
        }
    }
    doc.compress();
    doc.prune_objects();
    doc.renumber_objects();
    clean_trailer(&mut doc);
    let mut buf = Vec::new();
    let opts = lopdf::SaveOptions::builder()
        .use_object_streams(true)
        .use_xref_streams(true)
        .build();
    doc.save_with_options(&mut buf, opts)
        .map_err(|e| format!("could not write PDF: {e}"))?;
    if buf.len() >= pdf.len() {
        return Ok(pdf.to_vec());
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// shared helpers

const MAX_DEPTH: usize = 64;
const MAX_STREAM: usize = 256 << 20;
static NULL: Object = Object::Null;

/// Load, accepting documents that still need a password (`is_encrypted`).
fn load_any(pdf: &[u8]) -> Result<Document, String> {
    Document::load_mem(pdf).map_err(|e| format!("not a readable PDF ({e})"))
}

/// Load for editing; documents that need a password are refused.
fn load(pdf: &[u8]) -> Result<Document, String> {
    let doc = load_any(pdf)?;
    if doc.is_encrypted() {
        return Err("the document is password-protected; remove the password first".into());
    }
    Ok(doc)
}

/// Keep only the trailer keys a classic xref trailer needs.
fn clean_trailer(doc: &mut Document) {
    let mut t = Dictionary::new();
    for key in [&b"Root"[..], b"Info", b"ID", b"Encrypt"] {
        if let Ok(v) = doc.trailer.get(key) {
            t.set(key.to_vec(), v.clone());
        }
    }
    doc.trailer = t;
}

fn save(doc: &mut Document) -> Result<Vec<u8>, String> {
    clean_trailer(doc);
    let mut buf = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| format!("could not write PDF: {e}"))?;
    Ok(buf)
}

/// Follow references (bounded) to the actual object; missing → Null.
fn resolve<'a>(doc: &'a Document, mut obj: &'a Object) -> &'a Object {
    for _ in 0..MAX_DEPTH {
        match obj {
            Object::Reference(id) => match doc.objects.get(id) {
                Some(o) => obj = o,
                None => return &NULL,
            },
            _ => return obj,
        }
    }
    &NULL
}

fn dict_of<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match resolve(doc, obj) {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

fn get<'a>(doc: &'a Document, d: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    d.get(key)
        .ok()
        .map(|o| resolve(doc, o))
        .filter(|o| !o.is_null())
}

fn get_dict<'a>(doc: &'a Document, d: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
    d.get(key).ok().and_then(|o| dict_of(doc, o))
}

fn name_of<'a>(d: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
    d.get(key).ok().and_then(|o| o.as_name().ok())
}

fn num(o: &Object) -> Option<f32> {
    o.as_float().ok().filter(|v| v.is_finite())
}

fn text_of(obj: &Object) -> String {
    lopdf::decode_text_string(obj).unwrap_or_else(|_| {
        obj.as_str()
            .map(|b| b.iter().map(|&c| c as char).collect())
            .unwrap_or_default()
    })
}

fn catalog_id(doc: &Document) -> Option<ObjectId> {
    doc.trailer.get(b"Root").ok()?.as_reference().ok()
}

fn catalog(doc: &Document) -> Option<&Dictionary> {
    doc.get_dictionary(catalog_id(doc)?).ok()
}

fn catalog_mut(doc: &mut Document) -> Option<&mut Dictionary> {
    let id = catalog_id(doc)?;
    doc.get_dictionary_mut(id).ok()
}

/// Mutable access to `holder[key]`, whether it's indirect or inline.
fn sub_dict_mut<'a>(
    doc: &'a mut Document,
    holder: ObjectId,
    key: &[u8],
) -> Option<&'a mut Dictionary> {
    let r = doc
        .get_dictionary(holder)
        .ok()?
        .get(key)
        .ok()?
        .as_reference()
        .ok();
    match r {
        Some(id) => doc.get_dictionary_mut(id).ok(),
        None => doc
            .get_dictionary_mut(holder)
            .ok()?
            .get_mut(key)
            .ok()?
            .as_dict_mut()
            .ok(),
    }
}

fn page_ids(doc: &Document) -> Vec<ObjectId> {
    doc.get_pages().into_values().collect()
}

/// A page attribute, looked up the page tree (bounded).
fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut d = doc.get_dictionary(page).ok()?;
    for _ in 0..MAX_DEPTH {
        if let Some(v) = get(doc, d, key) {
            return Some(v);
        }
        d = get_dict(doc, d, b"Parent")?;
    }
    None
}

fn rect_of(o: &Object) -> Option<[f32; 4]> {
    let a = o.as_array().ok()?;
    if a.len() < 4 {
        return None;
    }
    let v: Vec<f32> = a.iter().take(4).filter_map(num).collect();
    if v.len() < 4 {
        return None;
    }
    Some([
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ])
}

/// CropBox (clipped to MediaBox) or MediaBox; Letter if neither is usable.
fn visible_box(doc: &Document, page: ObjectId) -> [f32; 4] {
    let media = inherited(doc, page, b"MediaBox")
        .and_then(rect_of)
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    match inherited(doc, page, b"CropBox").and_then(rect_of) {
        Some(c) => {
            let r = [
                c[0].max(media[0]),
                c[1].max(media[1]),
                c[2].min(media[2]),
                c[3].min(media[3]),
            ];
            if r[2] > r[0] && r[3] > r[1] {
                r
            } else {
                media
            }
        }
        None => media,
    }
}

fn push_down_inherited(doc: &mut Document, page: ObjectId) {
    let mut found = Vec::new();
    for key in [&b"Resources"[..], b"MediaBox", b"CropBox", b"Rotate"] {
        let own = doc.get_dictionary(page).map(|d| d.has(key)).unwrap_or(true);
        if !own {
            if let Some(v) = inherited(doc, page, key) {
                found.push((key, v.clone()));
            }
        }
    }
    if let Ok(d) = doc.get_dictionary_mut(page) {
        for (k, v) in found {
            d.set(k.to_vec(), v);
        }
    }
}

/// Apply `f` to every dictionary (incl. stream dicts and nested inline
/// dicts) of every object.
fn for_each_dict_mut(doc: &mut Document, f: &mut dyn FnMut(&mut Dictionary)) {
    fn walk(o: &mut Object, f: &mut dyn FnMut(&mut Dictionary), depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match o {
            Object::Dictionary(d) => {
                f(d);
                for (_, v) in d.iter_mut() {
                    walk(v, f, depth + 1);
                }
            }
            Object::Stream(s) => {
                f(&mut s.dict);
                for (_, v) in s.dict.iter_mut() {
                    walk(v, f, depth + 1);
                }
            }
            Object::Array(a) => {
                for v in a.iter_mut() {
                    walk(v, f, depth + 1);
                }
            }
            _ => {}
        }
    }
    for o in doc.objects.values_mut() {
        walk(o, f, 0);
    }
}

/// Remove dictionary entries / array items that reference `gone`.
fn drop_refs_to(doc: &mut Document, gone: &HashSet<ObjectId>) {
    if gone.is_empty() {
        return;
    }
    let is_gone = |o: &Object| matches!(o, Object::Reference(id) if gone.contains(id));
    let mut fix = |o: &mut Object| match o {
        Object::Dictionary(d) => {
            let keys: Vec<Vec<u8>> = d
                .iter()
                .filter(|(_, v)| is_gone(v))
                .map(|(k, _)| k.clone())
                .collect();
            for k in keys {
                d.remove(&k);
            }
        }
        Object::Array(a) => a.retain(|v| !is_gone(v)),
        _ => {}
    };
    fn walk(o: &mut Object, fix: &mut dyn FnMut(&mut Object), depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        fix(o);
        match o {
            Object::Dictionary(d) => d.iter_mut().for_each(|(_, v)| walk(v, fix, depth + 1)),
            Object::Stream(s) => s.dict.iter_mut().for_each(|(_, v)| walk(v, fix, depth + 1)),
            Object::Array(a) => a.iter_mut().for_each(|v| walk(v, fix, depth + 1)),
            _ => {}
        }
    }
    for o in doc.objects.values_mut() {
        walk(o, &mut fix, 0);
    }
}

fn page_annots(doc: &Document, page: ObjectId) -> Vec<Object> {
    doc.get_dictionary(page)
        .ok()
        .and_then(|d| get(doc, d, b"Annots"))
        .and_then(|a| a.as_array().ok())
        .cloned()
        .unwrap_or_default()
}

fn set_page_annots(doc: &mut Document, page: ObjectId, annots: Vec<Object>) {
    if let Ok(d) = doc.get_dictionary_mut(page) {
        if annots.is_empty() {
            d.remove(b"Annots");
        } else {
            d.set("Annots", annots);
        }
    }
}

/// Keep only annotations for which `keep` returns true, on every page.
fn filter_annots(doc: &mut Document, keep: impl Fn(&Dictionary) -> bool) {
    for pid in page_ids(doc) {
        let annots = page_annots(doc, pid);
        let n = annots.len();
        let kept: Vec<Object> = annots
            .into_iter()
            .filter(|a| dict_of(doc, a).map(&keep).unwrap_or(false))
            .collect();
        if kept.len() != n {
            set_page_annots(doc, pid, kept);
        }
    }
}

/// The page's content streams as a list of references.
fn content_refs(doc: &Document, page: ObjectId) -> Vec<Object> {
    let Ok(d) = doc.get_dictionary(page) else {
        return Vec::new();
    };
    match d.get(b"Contents") {
        Ok(Object::Reference(id)) => match doc.objects.get(id) {
            Some(Object::Array(a)) => a.clone(),
            Some(_) => vec![Object::Reference(*id)],
            None => Vec::new(),
        },
        Ok(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

fn set_page_content(doc: &mut Document, page: ObjectId, bytes: Vec<u8>) {
    let mut s = Stream::new(Dictionary::new(), bytes);
    let _ = s.compress();
    let id = doc.add_object(s);
    if let Ok(d) = doc.get_dictionary_mut(page) {
        d.set("Contents", id);
    }
}

/// Encode content operations, writing inline images back as BI/ID/EI.
/// None if an inline image couldn't be parsed (re-encoding would lose it).
fn encode_ops(ops: &[Operation]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for op in ops {
        if op.operator == "BI" {
            let Some(Object::Stream(s)) = op.operands.first() else {
                return None;
            };
            let mut operands = Vec::new();
            for (k, v) in s.dict.iter() {
                operands.push(Object::Name(k.clone()));
                operands.push(v.clone());
            }
            let head = Content {
                operations: vec![Operation::new("ID", operands)],
            }
            .encode()
            .ok()?;
            out.extend_from_slice(b"BI ");
            out.extend_from_slice(&head);
            out.push(b' ');
            out.extend_from_slice(&s.content);
            out.extend_from_slice(b"\nEI\n");
        } else {
            out.extend_from_slice(
                &Content {
                    operations: std::slice::from_ref(op),
                }
                .encode()
                .ok()?,
            );
            out.push(b'\n');
        }
    }
    Some(out)
}

fn fmt_num(v: f32) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Pseudo-random bytes from `RandomState` (per-process random keys) mixed
/// with the clock and a counter. Good enough for keys/IVs here; no `rand`.
fn random_bytes(n: usize) -> Vec<u8> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = Vec::with_capacity(n + 8);
    let mut i = 0u64;
    while out.len() < n {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(t);
        h.write_u64(i);
        h.write_usize(&out as *const _ as usize);
        out.extend_from_slice(&h.finish().to_le_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

fn human_size(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.1} KiB", n as f64 / 1024.0),
        _ => format!("{:.1} MiB", n as f64 / 1_048_576.0),
    }
}

fn paper_name(w: f32, h: f32) -> String {
    for p in super::Paper::ALL {
        let (pw, ph) = p.size();
        if (w - pw).abs() < 3.0 && (h - ph).abs() < 3.0 {
            return format!(" ({p} portrait)");
        }
        if (w - ph).abs() < 3.0 && (h - pw).abs() < 3.0 {
            return format!(" ({p} landscape)");
        }
    }
    String::new()
}

fn read_metadata(doc: &Document) -> Metadata {
    let Some(info) = doc.trailer.get(b"Info").ok().and_then(|o| dict_of(doc, o)) else {
        return Metadata::default();
    };
    let f = |k: &[u8]| {
        get(doc, info, k)
            .map(text_of)
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    Metadata {
        title: f(b"Title"),
        author: f(b"Author"),
        subject: f(b"Subject"),
        keywords: f(b"Keywords"),
        creator: f(b"Creator"),
        producer: f(b"Producer"),
        creation_date: f(b"CreationDate"),
        mod_date: f(b"ModDate"),
    }
}

/// BaseFont → (subtype, embedded).
fn font_list(doc: &Document) -> BTreeMap<String, (String, bool)> {
    let embedded = |fd: Option<&Dictionary>| {
        fd.map(|fd| fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3"))
            .unwrap_or(false)
    };
    let mut out = BTreeMap::new();
    for o in doc.objects.values() {
        let Ok(d) = o.as_dict() else { continue };
        if name_of(d, b"Type") != Some(b"Font") {
            continue;
        }
        let sub = name_of(d, b"Subtype").unwrap_or(b"?");
        if sub == b"CIDFontType0" || sub == b"CIDFontType2" {
            continue; // reported via their Type0 parent
        }
        let name = get(doc, d, b"BaseFont")
            .and_then(|n| n.as_name().ok())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_else(|| "(unnamed)".into());
        let emb = match sub {
            b"Type3" => true,
            b"Type0" => get(doc, d, b"DescendantFonts")
                .and_then(|a| a.as_array().ok())
                .and_then(|a| a.first())
                .and_then(|f| dict_of(doc, f))
                .map(|f| embedded(get_dict(doc, f, b"FontDescriptor")))
                .unwrap_or(false),
            _ => embedded(get_dict(doc, d, b"FontDescriptor")),
        };
        out.insert(name, (String::from_utf8_lossy(sub).into_owned(), emb));
    }
    out
}

/// Flatten a name tree into (key bytes, raw value).
fn name_tree<'a>(
    doc: &'a Document,
    node: &'a Dictionary,
    out: &mut Vec<(Vec<u8>, &'a Object)>,
    seen: &mut HashSet<ObjectId>,
    depth: usize,
) {
    if depth > MAX_DEPTH || out.len() > 100_000 {
        return;
    }
    if let Some(Object::Array(names)) = get(doc, node, b"Names") {
        for pair in names.chunks(2) {
            if let [k, v] = pair {
                if let Ok(k) = resolve(doc, k).as_str() {
                    out.push((k.to_vec(), v));
                }
            }
        }
    }
    if let Some(Object::Array(kids)) = get(doc, node, b"Kids") {
        for k in kids {
            if let Object::Reference(id) = k {
                if !seen.insert(*id) {
                    continue;
                }
            }
            if let Some(d) = dict_of(doc, k) {
                name_tree(doc, d, out, seen, depth + 1);
            }
        }
    }
}

/// Attachment (name, embedded-file stream id): EmbeddedFiles + FileAttachment annots.
fn attachment_specs(doc: &Document) -> Vec<(String, Option<ObjectId>)> {
    let mut specs: Vec<(Option<String>, &Object)> = Vec::new();
    if let Some(tree) = catalog(doc)
        .and_then(|c| get_dict(doc, c, b"Names"))
        .and_then(|n| get_dict(doc, n, b"EmbeddedFiles"))
    {
        let mut out = Vec::new();
        name_tree(doc, tree, &mut out, &mut HashSet::new(), 0);
        for (k, v) in out {
            specs.push((Some(text_of(&Object::String(k, StringFormat::Literal))), v));
        }
    }
    for pid in page_ids(doc) {
        let Some(annots) = doc
            .get_dictionary(pid)
            .ok()
            .and_then(|d| get(doc, d, b"Annots"))
            .and_then(|a| a.as_array().ok())
        else {
            continue;
        };
        for a in annots {
            if let Some(d) = dict_of(doc, a) {
                if name_of(d, b"Subtype") == Some(b"FileAttachment") {
                    if let Ok(fs) = d.get(b"FS") {
                        specs.push((None, fs));
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (i, (key, spec)) in specs.into_iter().enumerate() {
        let Some(fs) = dict_of(doc, spec) else {
            continue;
        };
        let name = get(doc, fs, b"UF")
            .or_else(|| get(doc, fs, b"F"))
            .map(text_of)
            .filter(|s| !s.trim().is_empty())
            .or(key)
            .unwrap_or_default();
        let name = file_name_only(&name);
        let name = if name.is_empty() {
            format!("attachment-{}", i + 1)
        } else {
            name
        };
        let sid = get_dict(doc, fs, b"EF").and_then(|ef| {
            ef.get(b"UF")
                .or_else(|_| ef.get(b"F"))
                .ok()
                .and_then(|o| o.as_reference().ok())
        });
        if let Some(id) = sid {
            if !seen.insert(id) {
                continue;
            }
        }
        out.push((name, sid));
    }
    out
}

fn file_name_only(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

fn ascii_fallback(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn safe_file_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut out = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > 80 {
        out = out.chars().take(80).collect();
        if let Some(i) = out.rfind(' ') {
            if i > 40 {
                out.truncate(i);
            }
        }
    }
    out.trim_matches(|c: char| c == '.' || c == ' ' || c == '-')
        .to_string()
}

// --- outline --------------------------------------------------------------

fn read_outline(doc: &Document) -> Vec<Bookmark> {
    let Some(cat) = catalog(doc) else {
        return Vec::new();
    };
    let Some(root) = get_dict(doc, cat, b"Outlines") else {
        return Vec::new();
    };
    let page_index: HashMap<ObjectId, usize> = page_ids(doc)
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    let mut named: HashMap<Vec<u8>, &Object> = HashMap::new();
    if let Some(d) = get_dict(doc, cat, b"Dests") {
        for (k, v) in d.iter() {
            named.insert(k.clone(), v);
        }
    }
    if let Some(tree) = get_dict(doc, cat, b"Names").and_then(|n| get_dict(doc, n, b"Dests")) {
        let mut out = Vec::new();
        name_tree(doc, tree, &mut out, &mut HashSet::new(), 0);
        for (k, v) in out {
            named.entry(k).or_insert(v);
        }
    }
    let mut raw: Vec<(String, Option<usize>, usize)> = Vec::new();
    let mut seen = HashSet::new();
    walk_outline(doc, root, 0, &page_index, &named, &mut raw, &mut seen);
    // entries without a destination: first deeper child's page, else previous
    let mut out = Vec::with_capacity(raw.len());
    for i in 0..raw.len() {
        let page = raw[i].1.or_else(|| {
            raw[i + 1..]
                .iter()
                .take_while(|r| r.2 > raw[i].2)
                .find_map(|r| r.1)
        });
        let page = page
            .or_else(|| out.last().map(|b: &Bookmark| b.page))
            .unwrap_or(0);
        out.push(Bookmark {
            title: raw[i].0.clone(),
            page,
            level: raw[i].2,
        });
    }
    out
}

fn walk_outline(
    doc: &Document,
    parent: &Dictionary,
    level: usize,
    pages: &HashMap<ObjectId, usize>,
    named: &HashMap<Vec<u8>, &Object>,
    out: &mut Vec<(String, Option<usize>, usize)>,
    seen: &mut HashSet<ObjectId>,
) {
    if level > MAX_DEPTH {
        return;
    }
    let mut cur = parent.get(b"First").ok().cloned();
    while let Some(obj) = cur.take() {
        if out.len() > 100_000 {
            return;
        }
        if let Object::Reference(id) = obj {
            if !seen.insert(id) {
                return; // cycle
            }
        }
        let Some(item) = dict_of(doc, &obj) else {
            return;
        };
        let title = get(doc, item, b"Title").map(text_of).unwrap_or_default();
        let page = outline_dest_page(doc, item, pages, named);
        out.push((title.trim().to_string(), page, level));
        if item.has(b"First") {
            walk_outline(doc, item, level + 1, pages, named, out, seen);
        }
        cur = item.get(b"Next").ok().cloned();
    }
}

fn outline_dest_page(
    doc: &Document,
    item: &Dictionary,
    pages: &HashMap<ObjectId, usize>,
    named: &HashMap<Vec<u8>, &Object>,
) -> Option<usize> {
    let mut dest = get(doc, item, b"Dest");
    if dest.is_none() {
        let a = get_dict(doc, item, b"A")?;
        if name_of(a, b"S") != Some(b"GoTo") {
            return None;
        }
        dest = get(doc, a, b"D");
    }
    let mut dest = dest?;
    for _ in 0..4 {
        match dest {
            Object::Name(n) | Object::String(n, _) => dest = resolve(doc, named.get(n)?),
            Object::Dictionary(d) => dest = get(doc, d, b"D")?,
            Object::Array(a) => {
                return match a.first()? {
                    Object::Reference(id) => pages.get(id).copied(),
                    Object::Integer(i) if *i >= 0 && (*i as usize) < pages.len() => {
                        Some(*i as usize)
                    }
                    _ => None,
                };
            }
            _ => return None,
        }
    }
    None
}

// --- forms ----------------------------------------------------------------

struct Field {
    id: ObjectId,
    parent: Option<ObjectId>,
    name: String,
    kind: &'static str,
    terminal: bool,
    signed: bool,
}

/// Every node of the AcroForm field tree (fields and widget kids).
fn all_fields(doc: &Document) -> Vec<Field> {
    let mut out = Vec::new();
    let Some(form) = catalog(doc).and_then(|c| get_dict(doc, c, b"AcroForm")) else {
        return out;
    };
    let Some(Object::Array(fields)) = get(doc, form, b"Fields") else {
        return out;
    };
    let mut seen = HashSet::new();
    for f in fields {
        walk_field(doc, f, None, "", None, &mut out, &mut seen, 0);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_field(
    doc: &Document,
    obj: &Object,
    parent: Option<ObjectId>,
    prefix: &str,
    inherited_ft: Option<&[u8]>,
    out: &mut Vec<Field>,
    seen: &mut HashSet<ObjectId>,
    depth: usize,
) {
    if depth > MAX_DEPTH || out.len() > 100_000 {
        return;
    }
    let Object::Reference(id) = obj else { return };
    if !seen.insert(*id) {
        return;
    }
    let Some(d) = dict_of(doc, obj) else { return };
    let part = get(doc, d, b"T").map(text_of).unwrap_or_default();
    let name = match (prefix.is_empty(), part.is_empty()) {
        (true, _) => part.clone(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}.{part}"),
    };
    let ft = name_of(d, b"FT").or(inherited_ft);
    let kids = match get(doc, d, b"Kids") {
        Some(Object::Array(k)) => k.as_slice(),
        _ => &[],
    };
    let kind = match ft {
        Some(b"Tx") => "text",
        Some(b"Btn") => "button",
        Some(b"Ch") => "choice",
        Some(b"Sig") => "signature",
        _ => "widget",
    };
    // a node is a terminal field if none of its kids carry a /T
    let terminal = !part.is_empty()
        && !kids
            .iter()
            .any(|k| dict_of(doc, k).map(|kd| kd.has(b"T")).unwrap_or(false));
    out.push(Field {
        id: *id,
        parent,
        name: if name.is_empty() {
            "(unnamed)".into()
        } else {
            name.clone()
        },
        kind: if part.is_empty() && parent.is_some() {
            "widget"
        } else {
            kind
        },
        terminal,
        signed: d.has(b"V"),
    });
    for k in kids {
        walk_field(doc, k, Some(*id), &name, ft, out, seen, depth + 1);
    }
}

/// (appearance stream id, cm matrix) mapping the /AP /N form onto /Rect.
fn widget_appearance(doc: &Document, w: &Dictionary) -> Option<(ObjectId, [f32; 6])> {
    let rect = get(doc, w, b"Rect").and_then(rect_of)?;
    let ap = get_dict(doc, w, b"AP")?;
    let n_raw = ap.get(b"N").ok()?;
    let id = match resolve(doc, n_raw) {
        Object::Stream(_) => n_raw.as_reference().ok()?,
        Object::Dictionary(states) => {
            let pick = match name_of(w, b"AS") {
                Some(s) => states.get(s).ok()?,
                None if states.len() == 1 => states.iter().next()?.1,
                None => return None,
            };
            let id = pick.as_reference().ok()?;
            doc.get_object(id).ok()?.as_stream().ok()?;
            id
        }
        _ => return None,
    };
    let s = doc.get_object(id).ok()?.as_stream().ok()?;
    let bbox = get(doc, &s.dict, b"BBox").and_then(rect_of)?;
    let m: [f32; 6] = get(doc, &s.dict, b"Matrix")
        .and_then(|o| o.as_array().ok())
        .map(|a| a.iter().filter_map(num).collect::<Vec<_>>())
        .filter(|v| v.len() == 6)
        .map(|v| [v[0], v[1], v[2], v[3], v[4], v[5]])
        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let pts = [
        (bbox[0], bbox[1]),
        (bbox[2], bbox[1]),
        (bbox[0], bbox[3]),
        (bbox[2], bbox[3]),
    ]
    .map(|(x, y)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]));
    let x0 = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    let x1 = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let y0 = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let y1 = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    let (bw, bh) = (x1 - x0, y1 - y0);
    if bw.abs() < 1e-3 || bh.abs() < 1e-3 {
        return None;
    }
    let sx = (rect[2] - rect[0]) / bw;
    let sy = (rect[3] - rect[1]) / bh;
    Some((id, [sx, 0.0, 0.0, sy, rect[0] - sx * x0, rect[1] - sy * y0]))
}

// --- JavaScript -----------------------------------------------------------

fn is_js_action(d: &Dictionary) -> bool {
    name_of(d, b"S") == Some(b"JavaScript") || d.has(b"JS")
}

fn collect_js(doc: &Document) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    if let Some(cat) = catalog(doc) {
        if let Some(tree) =
            get_dict(doc, cat, b"Names").and_then(|n| get_dict(doc, n, b"JavaScript"))
        {
            let mut pairs = Vec::new();
            name_tree(doc, tree, &mut pairs, &mut HashSet::new(), 0);
            for (k, v) in pairs {
                let name = text_of(&Object::String(k, StringFormat::Literal));
                action_js(
                    doc,
                    v,
                    &format!("Document-level script \"{name}\""),
                    &mut out,
                    &mut seen,
                    0,
                );
            }
        }
        if let Ok(oa) = cat.get(b"OpenAction") {
            action_js(doc, oa, "Document OpenAction", &mut out, &mut seen, 0);
        }
        if let Some(aa) = get_dict(doc, cat, b"AA") {
            aa_js(doc, aa, "Document action", &mut out, &mut seen);
        }
    }
    let mut done_containers: HashSet<ObjectId> = HashSet::new();
    for (i, pid) in page_ids(doc).into_iter().enumerate() {
        let Ok(pd) = doc.get_dictionary(pid) else {
            continue;
        };
        if let Some(aa) = get_dict(doc, pd, b"AA") {
            aa_js(
                doc,
                aa,
                &format!("Page {} action", i + 1),
                &mut out,
                &mut seen,
            );
        }
        for a in page_annots(doc, pid) {
            if let Object::Reference(id) = a {
                done_containers.insert(id);
            }
            let Some(d) = dict_of(doc, &a) else { continue };
            let sub = String::from_utf8_lossy(name_of(d, b"Subtype").unwrap_or(b"?")).into_owned();
            let t = get(doc, d, b"T")
                .map(text_of)
                .map(|t| format!(" \"{t}\""))
                .unwrap_or_default();
            let place = format!("Page {}, {sub} annotation{t}", i + 1);
            if let Ok(act) = d.get(b"A") {
                action_js(doc, act, &place, &mut out, &mut seen, 0);
            }
            if let Some(aa) = get_dict(doc, d, b"AA") {
                aa_js(doc, aa, &place, &mut out, &mut seen);
            }
        }
    }
    for f in all_fields(doc) {
        if done_containers.contains(&f.id) {
            continue;
        }
        let Ok(d) = doc.get_dictionary(f.id) else {
            continue;
        };
        let place = format!("Form field \"{}\"", f.name);
        if let Ok(act) = d.get(b"A") {
            action_js(doc, act, &place, &mut out, &mut seen, 0);
        }
        if let Some(aa) = get_dict(doc, d, b"AA") {
            aa_js(doc, aa, &place, &mut out, &mut seen);
        }
    }
    // anything else that is an indirect JavaScript action
    for (id, o) in &doc.objects {
        if seen.contains(id) {
            continue;
        }
        if o.as_dict().map(is_js_action).unwrap_or(false) {
            action_js(
                doc,
                &Object::Reference(*id),
                &format!("Object {} {}", id.0, id.1),
                &mut out,
                &mut seen,
                0,
            );
        }
    }
    out
}

fn aa_js(
    doc: &Document,
    aa: &Dictionary,
    place: &str,
    out: &mut Vec<(String, String)>,
    seen: &mut HashSet<ObjectId>,
) {
    for (k, v) in aa.iter() {
        let trigger = String::from_utf8_lossy(k);
        action_js(doc, v, &format!("{place} /{trigger}"), out, seen, 0);
    }
}

fn action_js(
    doc: &Document,
    obj: &Object,
    place: &str,
    out: &mut Vec<(String, String)>,
    seen: &mut HashSet<ObjectId>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    if let Object::Reference(id) = obj {
        if !seen.insert(*id) {
            return;
        }
    }
    match resolve(doc, obj) {
        Object::Array(a) => {
            for o in a {
                action_js(doc, o, place, out, seen, depth + 1);
            }
        }
        Object::Dictionary(d) => {
            if is_js_action(d) {
                let code = match d.get(b"JS").ok().map(|o| resolve(doc, o)) {
                    Some(Object::Stream(s)) => {
                        let bytes = s
                            .decompressed_content_with_limit(MAX_STREAM)
                            .unwrap_or_else(|_| s.content.clone());
                        text_of(&Object::String(bytes, StringFormat::Literal))
                    }
                    Some(o @ Object::String(..)) => text_of(o),
                    _ => String::new(),
                };
                out.push((place.to_string(), code));
            }
            if let Ok(next) = d.get(b"Next") {
                action_js(doc, next, place, out, seen, depth + 1);
            }
        }
        _ => {}
    }
}

// --- images ---------------------------------------------------------------

/// XObject resource names that refer to images.
fn image_names(doc: &Document, res: &Dictionary, images: &HashSet<ObjectId>) -> HashSet<Vec<u8>> {
    let mut out = HashSet::new();
    if let Some(x) = get_dict(doc, res, b"XObject") {
        for (k, v) in x.iter() {
            if matches!(v, Object::Reference(id) if images.contains(id)) {
                out.insert(k.clone());
            }
        }
    }
    out
}

fn form_xobjects(doc: &Document, res: &Dictionary) -> Vec<ObjectId> {
    let mut out = Vec::new();
    if let Some(x) = get_dict(doc, res, b"XObject") {
        for (_, v) in x.iter() {
            if let Object::Reference(id) = v {
                if let Ok(Object::Stream(s)) = doc.get_object(*id) {
                    if name_of(&s.dict, b"Subtype") == Some(b"Form") {
                        out.push(*id);
                    }
                }
            }
        }
    }
    out
}

fn strip_image_ops(ops: Vec<Operation>, names: &HashSet<Vec<u8>>) -> Vec<Operation> {
    ops.into_iter()
        .filter(|op| match op.operator.as_str() {
            "BI" => false,
            "Do" => !op
                .operands
                .first()
                .and_then(|o| o.as_name().ok())
                .map(|n| names.contains(n))
                .unwrap_or(false),
            _ => true,
        })
        .collect()
}

fn strip_forms(
    doc: &mut Document,
    forms: Vec<ObjectId>,
    parent_res: &Dictionary,
    images: &HashSet<ObjectId>,
    done: &mut HashSet<ObjectId>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    for fid in forms {
        if !done.insert(fid) {
            continue;
        }
        let Ok(Object::Stream(s)) = doc.get_object(fid) else {
            continue;
        };
        let res = get_dict(doc, &s.dict, b"Resources")
            .cloned()
            .unwrap_or_else(|| parent_res.clone());
        let names = image_names(doc, &res, images);
        let inner = form_xobjects(doc, &res);
        let data = s
            .decompressed_content_with_limit(MAX_STREAM)
            .ok()
            .or_else(|| {
                if s.dict.has(b"Filter") {
                    None
                } else {
                    Some(s.content.clone())
                }
            });
        if let Some(data) = data {
            if let Ok(content) = Content::decode(&data) {
                let ops = strip_image_ops(content.operations, &names);
                if let Some(enc) = encode_ops(&ops) {
                    if let Ok(Object::Stream(s)) = doc.get_object_mut(fid) {
                        s.dict.remove(b"DecodeParms");
                        s.set_plain_content(enc);
                        let _ = s.compress();
                    }
                }
            }
        }
        strip_forms(doc, inner, &res, images, done, depth + 1);
    }
}

fn collect_images(
    doc: &Document,
    res: &Dictionary,
    out: &mut Vec<ObjectId>,
    seen: &mut HashSet<ObjectId>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    let Some(x) = get_dict(doc, res, b"XObject") else {
        return;
    };
    for (_, v) in x.iter() {
        let Object::Reference(id) = v else { continue };
        if !seen.insert(*id) {
            continue;
        }
        let Ok(Object::Stream(s)) = doc.get_object(*id) else {
            continue;
        };
        match name_of(&s.dict, b"Subtype") {
            Some(b"Image") => out.push(*id),
            Some(b"Form") => {
                let inner = get_dict(doc, &s.dict, b"Resources").unwrap_or(res);
                collect_images(doc, inner, out, seen, depth + 1);
            }
            _ => {}
        }
    }
}

/// Components of a colour space we can write as PNG (1 or 3).
fn simple_components(doc: &Document, cs: &Object) -> Option<usize> {
    match resolve(doc, cs) {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"CalGray" | b"G" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"RGB" => Some(3),
            _ => None,
        },
        Object::Array(a) => match a.first().and_then(|o| o.as_name().ok())? {
            b"ICCBased" => {
                let s = dict_of(doc, a.get(1)?)?;
                match get(doc, s, b"N").and_then(|n| n.as_i64().ok()) {
                    Some(1) => Some(1),
                    Some(3) => Some(3),
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

/// 8-bit samples of an unfiltered or Flate image stream.
fn raw_samples(s: &Stream) -> Option<Vec<u8>> {
    let filters = s.filters().unwrap_or_default();
    if filters.is_empty() {
        return Some(s.content.clone());
    }
    if filters.iter().all(|f| *f == b"FlateDecode" || *f == b"Fl") {
        return s.decompressed_content_with_limit(MAX_STREAM).ok();
    }
    None
}

fn convert_image(doc: &Document, id: ObjectId) -> Option<(&'static str, Vec<u8>)> {
    use image::ImageEncoder;
    let s = doc.get_object(id).ok()?.as_stream().ok()?;
    let filters = s.filters().unwrap_or_default();
    if filters
        .last()
        .map(|f| *f == b"DCTDecode" || *f == b"DCT")
        .unwrap_or(false)
    {
        if filters.len() == 1 {
            return Some(("jpg", s.content.clone()));
        }
        return None;
    }
    let w = get(doc, &s.dict, b"Width")?.as_i64().ok()?;
    let h = get(doc, &s.dict, b"Height")?.as_i64().ok()?;
    if w <= 0 || h <= 0 || w * h > 100_000_000 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    if get(doc, &s.dict, b"BitsPerComponent")?.as_i64().ok()? != 8 {
        return None;
    }
    let n = simple_components(doc, get(doc, &s.dict, b"ColorSpace")?)?;
    let data = raw_samples(s)?;
    if data.len() < w * h * n {
        return None;
    }
    let data = &data[..w * h * n];
    // optional soft mask of the same size
    let alpha = get(doc, &s.dict, b"SMask")
        .and_then(|m| m.as_stream().ok())
        .and_then(|m| {
            let mw = get(doc, &m.dict, b"Width")?.as_i64().ok()? as usize;
            let mh = get(doc, &m.dict, b"Height")?.as_i64().ok()? as usize;
            let bpc = get(doc, &m.dict, b"BitsPerComponent")?.as_i64().ok()?;
            if mw != w || mh != h || bpc != 8 {
                return None;
            }
            let a = raw_samples(m)?;
            (a.len() >= w * h).then(|| a[..w * h].to_vec())
        });
    let (pixels, ty) = match (n, alpha) {
        (1, None) => (data.to_vec(), image::ExtendedColorType::L8),
        (3, None) => (data.to_vec(), image::ExtendedColorType::Rgb8),
        (_, Some(a)) => {
            let mut px = Vec::with_capacity(w * h * (n + 1));
            for (i, chunk) in data.chunks_exact(n).enumerate() {
                px.extend_from_slice(chunk);
                px.push(a[i]);
            }
            (
                px,
                if n == 1 {
                    image::ExtendedColorType::La8
                } else {
                    image::ExtendedColorType::Rgba8
                },
            )
        }
        _ => return None,
    };
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(&pixels, w as u32, h as u32, ty)
        .ok()?;
    Some(("png", buf))
}

// --- text -----------------------------------------------------------------

fn texts_of(doc: &Document) -> Vec<String> {
    doc.get_pages()
        .keys()
        .map(|&n| {
            doc.extract_text(&[n])
                .map(|t| t.trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

fn lines_of(t: &str) -> Vec<&str> {
    t.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
}

/// LCS diff: (' ' | '-' | '+', line).
fn diff_lines<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(char, &'a str)> {
    let (n, m) = (a.len(), b.len());
    if n * m > 9_000_000 {
        if a == b {
            return a.iter().map(|l| (' ', *l)).collect();
        }
        return a
            .iter()
            .map(|l| ('-', *l))
            .chain(b.iter().map(|l| ('+', *l)))
            .collect();
    }
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[idx(i, j)] = if a[i] == b[j] {
                t[idx(i + 1, j + 1)] + 1
            } else {
                t[idx(i + 1, j)].max(t[idx(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if t[idx(i + 1, j)] >= t[idx(i, j + 1)] {
            out.push(('-', a[i]));
            i += 1;
        } else {
            out.push(('+', b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| ('-', *l)));
    out.extend(b[j..].iter().map(|l| ('+', *l)));
    out
}

/// Per-byte text and advance width (1/1000 em) of a simple font.
struct FontInfo {
    chars: Vec<String>,
    widths: Vec<f32>,
}

fn font_info(doc: &Document, font: &Dictionary) -> Option<FontInfo> {
    if name_of(font, b"Subtype") == Some(b"Type0") {
        return None;
    }
    let enc = font.get_font_encoding(doc).ok()?;
    let chars = (0u8..=255)
        .map(|b| Document::decode_text(&enc, &[b]).unwrap_or_default())
        .collect();
    let missing = get_dict(doc, font, b"FontDescriptor")
        .and_then(|fd| get(doc, fd, b"MissingWidth"))
        .and_then(num)
        .filter(|w| *w > 0.0)
        .unwrap_or(500.0);
    let mut widths = vec![missing; 256];
    let first = get(doc, font, b"FirstChar")
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    if let Some(Object::Array(ws)) = get(doc, font, b"Widths") {
        for (i, w) in ws.iter().enumerate() {
            let code = first + i as i64;
            if (0..256).contains(&code) {
                if let Some(w) = num(resolve(doc, w)) {
                    widths[code as usize] = w;
                }
            }
        }
    }
    Some(FontInfo { chars, widths })
}

enum Piece {
    Num(f32),
    Byte(u8, bool),
}

/// Rewrite text operators, removing matched terms. Returns (ops, count).
fn redact_ops(
    ops: Vec<Operation>,
    fonts: &HashMap<Vec<u8>, Option<FontInfo>>,
    terms: &[Vec<char>],
    case_sensitive: bool,
) -> (Vec<Operation>, usize) {
    let mut out = Vec::with_capacity(ops.len());
    let mut font: Option<&FontInfo> = None;
    let mut count = 0;
    for op in ops {
        match op.operator.as_str() {
            "Tf" => {
                font = op
                    .operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|n| fonts.get(n))
                    .and_then(|f| f.as_ref());
                out.push(op);
            }
            "Tj" | "'" | "\"" | "TJ" => {
                let Some(f) = font else {
                    out.push(op);
                    continue;
                };
                let src = if op.operator == "TJ" {
                    op.operands.first()
                } else {
                    op.operands.last()
                };
                let mut pieces = Vec::new();
                match src {
                    Some(Object::String(s, _)) => {
                        pieces.extend(s.iter().map(|&b| Piece::Byte(b, false)))
                    }
                    Some(Object::Array(a)) => {
                        for o in a {
                            match o {
                                Object::String(s, _) => {
                                    pieces.extend(s.iter().map(|&b| Piece::Byte(b, false)))
                                }
                                o => {
                                    if let Some(v) = num(o) {
                                        pieces.push(Piece::Num(v))
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let n = mark_terms(&mut pieces, f, terms, case_sensitive);
                if n == 0 {
                    out.push(op);
                    continue;
                }
                count += n;
                let array = rebuild_tj(&pieces, f);
                match op.operator.as_str() {
                    "'" => out.push(Operation::new("T*", vec![])),
                    "\"" => {
                        if let [aw, ac, _] = op.operands.as_slice() {
                            out.push(Operation::new("Tw", vec![aw.clone()]));
                            out.push(Operation::new("Tc", vec![ac.clone()]));
                        }
                        out.push(Operation::new("T*", vec![]));
                    }
                    _ => {}
                }
                out.push(Operation::new("TJ", vec![Object::Array(array)]));
            }
            _ => out.push(op),
        }
    }
    (out, count)
}

/// Mark bytes belonging to term matches as removed; returns matches.
fn mark_terms(
    pieces: &mut [Piece],
    f: &FontInfo,
    terms: &[Vec<char>],
    case_sensitive: bool,
) -> usize {
    let mut hay: Vec<char> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        if let Piece::Byte(b, _) = p {
            for c in f.chars[*b as usize].chars() {
                if case_sensitive {
                    hay.push(c);
                    owner.push(i);
                } else {
                    for lc in c.to_lowercase() {
                        hay.push(lc);
                        owner.push(i);
                    }
                }
            }
        }
    }
    let mut count = 0;
    let mut removed = vec![false; hay.len()];
    for term in terms {
        let mut i = 0;
        while i + term.len() <= hay.len() {
            if !removed[i] && hay[i..i + term.len()] == term[..] {
                for r in &mut removed[i..i + term.len()] {
                    *r = true;
                }
                count += 1;
                i += term.len();
            } else {
                i += 1;
            }
        }
    }
    for (k, r) in removed.iter().enumerate() {
        if *r {
            if let Piece::Byte(_, gone) = &mut pieces[owner[k]] {
                *gone = true;
            }
        }
    }
    count
}

fn rebuild_tj(pieces: &[Piece], f: &FontInfo) -> Vec<Object> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut gap = 0.0f32;
    let flush_str = |out: &mut Vec<Object>, cur: &mut Vec<u8>| {
        if !cur.is_empty() {
            out.push(Object::String(std::mem::take(cur), StringFormat::Literal));
        }
    };
    for p in pieces {
        match p {
            Piece::Byte(b, false) => {
                if gap != 0.0 {
                    out.push(Object::Real(gap));
                    gap = 0.0;
                }
                cur.push(*b);
            }
            Piece::Byte(b, true) => {
                flush_str(&mut out, &mut cur);
                gap -= f.widths[*b as usize];
            }
            Piece::Num(v) => {
                flush_str(&mut out, &mut cur);
                gap += v;
            }
        }
    }
    flush_str(&mut out, &mut cur);
    if gap != 0.0 {
        out.push(Object::Real(gap));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_content(lines: &[&str]) -> Vec<u8> {
        let mut ops = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 18.into()]),
            Operation::new("TL", vec![24.into()]),
            Operation::new("Td", vec![72.into(), 700.into()]),
        ];
        for l in lines {
            ops.push(Operation::new("Tj", vec![Object::string_literal(*l)]));
            ops.push(Operation::new("T*", vec![]));
        }
        ops.push(Operation::new("ET", vec![]));
        Content { operations: ops }.encode().unwrap()
    }

    /// n-page document; page i shows `"{tag} page {i+1}"` plus `extra` lines.
    fn build(
        n: usize,
        tag: &str,
        extra: &[&str],
        edit: impl FnOnce(&mut Document, &[ObjectId]),
    ) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        let mut ids = Vec::new();
        for i in 0..n {
            let first = format!("{tag} page {}", i + 1);
            let mut lines = vec![first.as_str()];
            lines.extend_from_slice(extra);
            let c = doc.add_object(Stream::new(dictionary! {}, text_content(&lines)));
            let id = doc.add_object(
                dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c },
            );
            ids.push(id);
            kids.push(id.into());
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
        edit(&mut doc, &ids);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn sample(n: usize, tag: &str) -> Vec<u8> {
        build(n, tag, &[], |_, _| {})
    }

    /// lopdf reloads it and hayro parses it with the same page count.
    fn valid(bytes: &[u8], pages: usize) -> Document {
        let doc = Document::load_mem(bytes).expect("lopdf reload");
        assert_eq!(doc.get_pages().len(), pages);
        let pdf = hayro::hayro_syntax::Pdf::new(bytes.to_vec()).expect("hayro parse");
        assert_eq!(pdf.pages().len(), pages);
        doc
    }

    fn cat(doc: &Document) -> &Dictionary {
        catalog(doc).unwrap()
    }

    #[test]
    fn metadata_roundtrip_and_remove() {
        let pdf = sample(1, "A");
        let meta = Metadata {
            title: "Grüße report".into(),
            author: "Ada".into(),
            keywords: "x, y".into(),
            creation_date: "D:20261011120000Z".into(),
            ..Default::default()
        };
        let out = metadata_set(&pdf, &meta, false).unwrap();
        valid(&out, 1);
        assert_eq!(metadata_get(&out).unwrap(), meta);
        let cleared = metadata_set(&out, &Metadata::default(), true).unwrap();
        valid(&cleared, 1);
        assert_eq!(metadata_get(&cleared).unwrap(), Metadata::default());
    }

    #[test]
    fn encrypt_then_decrypt() {
        let pdf = sample(2, "S");
        let perms = Permissions {
            copy: false,
            ..Default::default()
        };
        let enc = encrypt(&pdf, "user", "owner", perms).unwrap();
        let locked = Document::load_mem(&enc).unwrap();
        assert!(locked.is_encrypted());
        assert!(page_texts(&enc).is_err());
        assert!(decrypt(&enc, "nope").is_err());
        for pw in ["user", "owner"] {
            let dec = decrypt(&enc, pw).unwrap();
            valid(&dec, 2);
            assert_eq!(page_texts(&dec).unwrap()[1], "S page 2");
        }
        // empty user password: opens, permissions recorded
        let open = encrypt(&pdf, "", "", perms).unwrap();
        let doc = Document::load_mem(&open).unwrap();
        assert!(!doc.is_encrypted());
        assert!(!doc
            .encryption_state
            .as_ref()
            .unwrap()
            .permissions()
            .contains(lopdf::Permissions::COPYABLE));
        assert!(info_report(&open)
            .unwrap()
            .contains("opens without a password"));
        valid(&decrypt(&open, "").unwrap(), 2);
        assert!(decrypt(&pdf, "x").is_err());
    }

    fn with_js() -> Vec<u8> {
        build(1, "J", &[], |doc, _| {
            let act = doc.add_object(dictionary! { "S" => "JavaScript", "JS" => Object::string_literal("app.alert('hi');") });
            let named = doc.add_object(dictionary! { "S" => "JavaScript", "JS" => Object::string_literal("var named = 1;") });
            let cat_id = catalog_id(doc).unwrap();
            let c = doc.get_dictionary_mut(cat_id).unwrap();
            c.set("OpenAction", act);
            c.set(
                "Names",
                dictionary! { "JavaScript" => dictionary! { "Names" => vec![Object::string_literal("init"), named.into()] } },
            );
        })
    }

    #[test]
    fn javascript_report_and_sanitize() {
        let pdf = with_js();
        let rep = javascript_report(&pdf).unwrap();
        assert!(rep.contains("2 JavaScript block(s)"), "{rep}");
        assert!(rep.contains("app.alert('hi');") && rep.contains("OpenAction"));
        assert!(rep.contains("\"init\"") && rep.contains("var named"));
        assert!(info_report(&pdf)
            .unwrap()
            .contains("JavaScript: 2 block(s)"));
        let clean = sanitize(
            &pdf,
            Sanitize {
                document_info: true,
                xmp_metadata: true,
                links: true,
                fonts: true,
                ..Default::default()
            },
        )
        .unwrap();
        let doc = valid(&clean, 1);
        assert!(!cat(&doc).has(b"OpenAction"));
        assert_eq!(javascript_report(&clean).unwrap(), "No JavaScript found.");
    }

    #[test]
    fn attachments_roundtrip() {
        let pdf = sample(1, "A");
        let files = vec![
            ("notes.txt".to_string(), b"hello world".to_vec()),
            ("dir/data.bin".to_string(), vec![0u8, 1, 2, 255]),
        ];
        let out = attachments_add(&pdf, &files).unwrap();
        valid(&out, 1);
        let mut got = attachments_extract(&out).unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("data.bin".to_string(), vec![0u8, 1, 2, 255]),
                ("notes.txt".to_string(), b"hello world".to_vec())
            ]
        );
        // adding again keeps old ones and renames the duplicate
        let out2 = attachments_add(&out, &[("notes.txt".to_string(), b"v2".to_vec())]).unwrap();
        let names: Vec<String> = attachments_extract(&out2)
            .unwrap()
            .into_iter()
            .map(|a| a.0)
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"notes (2).txt".to_string()), "{names:?}");
        assert!(info_report(&out2).unwrap().contains("Attachments: 3"));
        // sanitize default strips them
        let clean = sanitize(&out2, Sanitize::default()).unwrap();
        assert!(attachments_extract(&clean).unwrap().is_empty());
    }

    #[test]
    fn bookmarks_roundtrip_and_groups() {
        let pdf = sample(6, "B");
        let marks = vec![
            Bookmark {
                title: "Intro".into(),
                page: 0,
                level: 0,
            },
            Bookmark {
                title: "Détails".into(),
                page: 1,
                level: 1,
            },
            Bookmark {
                title: "Deep".into(),
                page: 2,
                level: 2,
            },
            Bookmark {
                title: "Part 2".into(),
                page: 3,
                level: 0,
            },
            Bookmark {
                title: "End".into(),
                page: 5,
                level: 0,
            },
        ];
        let out = bookmarks_set(&pdf, &marks).unwrap();
        valid(&out, 6);
        assert_eq!(bookmarks_get(&out).unwrap(), marks);
        let groups = bookmark_groups(&out).unwrap();
        assert_eq!(
            groups,
            vec![
                ("Intro".to_string(), vec![0, 1, 2]),
                ("Part 2".to_string(), vec![3, 4]),
                ("End".to_string(), vec![5])
            ]
        );
        assert!(bookmarks_set(
            &pdf,
            &[Bookmark {
                title: "x".into(),
                page: 9,
                level: 0
            }]
        )
        .is_err());
        let none = bookmarks_set(&out, &[]).unwrap();
        assert!(bookmarks_get(&none).unwrap().is_empty());
        assert!(bookmark_groups(&none).is_err());
    }

    #[test]
    fn size_groups_split() {
        let pdf = sample(6, "Z");
        assert_eq!(
            size_groups(&pdf, u64::MAX).unwrap(),
            vec![vec![0, 1, 2, 3, 4, 5]]
        );
        assert_eq!(
            size_groups(&pdf, 1).unwrap(),
            (0..6).map(|p| vec![p]).collect::<Vec<_>>()
        );
        let two =
            crate::viewers::pdf::compose::build(&[Arc::new(pdf.clone())], &[(0, 0, 0), (0, 1, 0)])
                .unwrap()
                .len();
        let groups = size_groups(&pdf, two as u64).unwrap();
        assert!(
            groups.len() >= 3 && groups.iter().all(|g| g.len() <= 2),
            "{groups:?}"
        );
        assert_eq!(groups.concat(), (0..6).collect::<Vec<_>>());
    }

    #[test]
    fn crop_sets_cropbox() {
        let pdf = sample(2, "C");
        let out = crop(&pdf, &[1], [10.0, 20.0, 30.0, 40.0]).unwrap();
        let doc = valid(&out, 2);
        let ids = page_ids(&doc);
        assert_eq!(visible_box(&doc, ids[1]), [10.0, 20.0, 582.0, 752.0]);
        assert_eq!(visible_box(&doc, ids[0]), [0.0, 0.0, 612.0, 792.0]);
        assert!(crop(&pdf, &[0], [400.0, 0.0, 400.0, 0.0]).is_err());
        assert!(crop(&pdf, &[5], [1.0; 4]).is_err());
    }

    #[test]
    fn remove_annotations_drops_links() {
        let pdf = build(1, "L", &[], |doc, ids| {
            let link = doc.add_object(dictionary! {
                "Type" => "Annot", "Subtype" => "Link",
                "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") },
            });
            doc.get_dictionary_mut(ids[0])
                .unwrap()
                .set("Annots", vec![link.into()]);
        });
        let doc = Document::load_mem(&pdf).unwrap();
        assert_eq!(page_annots(&doc, page_ids(&doc)[0]).len(), 1);
        let links_gone = sanitize(
            &pdf,
            Sanitize {
                links: true,
                ..Default::default()
            },
        )
        .unwrap();
        let d = valid(&links_gone, 1);
        assert!(page_annots(&d, page_ids(&d)[0]).is_empty());
        let out = remove_annotations(&pdf).unwrap();
        let doc = valid(&out, 1);
        assert!(page_annots(&doc, page_ids(&doc)[0]).is_empty());
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut buf = Vec::new();
        let px: Vec<u8> = (0..8 * 8).flat_map(|i| [i as u8 * 4, 100, 200]).collect();
        image::codecs::jpeg::JpegEncoder::new(&mut buf)
            .encode(&px, 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    fn with_images() -> Vec<u8> {
        let jpeg = jpeg_bytes();
        build(1, "I", &[], move |doc, ids| {
            let jpg = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 8,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
                },
                jpeg,
            ));
            let mut raw = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 2,
                    "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
                },
                vec![0, 80, 160, 255],
            );
            raw.compress().unwrap();
            let raw = doc.add_object(raw);
            // a form XObject that draws the gray image
            let form = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
                    "Resources" => dictionary! { "XObject" => dictionary! { "Im2" => raw } },
                },
                b"q 1 0 0 1 0 0 cm /Im2 Do Q".to_vec(),
            ));
            let content = b"q 100 0 0 100 50 50 cm /Im1 Do Q\nq 50 0 0 50 300 300 cm /Fm1 Do Q\nBT /F1 12 Tf 72 72 Td (keep me) Tj ET".to_vec();
            let c = doc.add_object(Stream::new(dictionary! {}, content));
            let font = doc.add_object(
                dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
            );
            let p = doc.get_dictionary_mut(ids[0]).unwrap();
            p.set("Contents", c);
            p.set(
                "Resources",
                dictionary! {
                    "Font" => dictionary! { "F1" => font },
                    "XObject" => dictionary! { "Im1" => jpg, "Fm1" => form },
                },
            );
        })
    }

    #[test]
    fn extract_and_remove_images() {
        let pdf = with_images();
        let jpeg = jpeg_bytes();
        let files = extract_images(&pdf, &[0]).unwrap();
        assert_eq!(
            files.len(),
            2,
            "{:?}",
            files.iter().map(|f| &f.0).collect::<Vec<_>>()
        );
        assert_eq!(files[0], ("p01-img1.jpg".to_string(), jpeg));
        assert_eq!(files[1].0, "p01-img2.png");
        let png = image::load_from_memory(&files[1].1).unwrap().to_luma8();
        assert_eq!(png.as_raw(), &vec![0, 80, 160, 255]);
        assert!(extract_images(&pdf, &[3]).is_err());
        assert!(info_report(&pdf).unwrap().contains("Images: 2"));

        let out = remove_images(&pdf).unwrap();
        let doc = valid(&out, 1);
        assert!(extract_images(&out, &[0]).unwrap().is_empty());
        let content =
            String::from_utf8_lossy(&doc.get_page_content(page_ids(&doc)[0])).into_owned();
        assert!(!content.contains("/Im1"), "{content}");
        assert!(content.contains("/Fm1 Do"));
        assert!(page_texts(&out).unwrap()[0].contains("keep me"));
    }

    #[test]
    fn compare_finds_differences() {
        let a = build(2, "T", &["same line", "old wording"], |_, _| {});
        let b = build(2, "T", &["same line", "new wording"], |_, _| {});
        let rep = compare_text(&a, &b).unwrap();
        assert!(
            rep.contains("=== Page 1 ===")
                && rep.contains("- old wording")
                && rep.contains("+ new wording"),
            "{rep}"
        );
        assert!(!rep.contains("same line"));
        assert!(compare_text(&a, &a)
            .unwrap()
            .contains("No text differences."));
        let c = sample(3, "T");
        assert!(compare_text(&a, &c)
            .unwrap()
            .contains("Page 3 (only in second)"));
    }

    #[test]
    fn suggest_title_from_info_or_text() {
        let pdf = build(1, "#", &["Quarterly Results: Q3/2026"], |_, _| {});
        // first line "# page 1" has too few letters → the next line wins
        assert_eq!(
            suggest_title(&pdf).unwrap().as_deref(),
            Some("Quarterly Results Q3 2026")
        );
        let meta = Metadata {
            title: "Microsoft Word - Budget Plan.docx".into(),
            ..Default::default()
        };
        let titled = metadata_set(&pdf, &meta, false).unwrap();
        assert_eq!(
            suggest_title(&titled).unwrap().as_deref(),
            Some("Budget Plan")
        );
        let blank = build(1, "", &[], |_, _| {});
        assert_eq!(suggest_title(&blank).unwrap(), None);
    }

    #[test]
    fn redact_removes_terms() {
        let pdf = build(2, "R", &["my Secret code", "secret again"], |_, _| {});
        let (out, n) = redact_text(&pdf, &["secret".to_string()], false).unwrap();
        assert_eq!(n, 4);
        valid(&out, 2);
        for t in page_texts(&out).unwrap() {
            assert!(!t.to_lowercase().contains("secret"), "{t}");
            assert!(t.contains("my") && t.contains("code"), "{t}");
        }
        let (_, n) = redact_text(&pdf, &["secret".to_string()], true).unwrap();
        assert_eq!(n, 2);
        assert!(redact_text(&pdf, &[], false).is_err());
    }

    #[test]
    fn repair_and_compress_reload() {
        let mut pdf = build(3, "P", &[], |doc, ids| {
            // dangling reference + duplicate streams
            doc.get_dictionary_mut(ids[0])
                .unwrap()
                .set("Thumb", (999, 0));
            let dup = doc
                .get_dictionary(ids[1])
                .unwrap()
                .get(b"Contents")
                .unwrap()
                .clone();
            let s = doc.get_object(dup.as_reference().unwrap()).unwrap().clone();
            let copy = doc.add_object(s);
            doc.get_dictionary_mut(ids[2])
                .unwrap()
                .set("Contents", copy);
            doc.get_dictionary_mut(ids[1]).unwrap().set("Contents", dup);
        });
        let fixed = repair(&pdf).unwrap();
        valid(&fixed, 3);
        assert_eq!(page_texts(&fixed).unwrap()[2], "P page 2");
        let small = compress_lossless(&fixed).unwrap();
        valid(&small, 3);
        assert!(small.len() <= fixed.len());
        assert_eq!(page_texts(&small).unwrap(), page_texts(&fixed).unwrap());
        // garbage in front still loads
        let mut junk = b"garbage\n".to_vec();
        junk.append(&mut pdf);
        valid(&repair(&junk).unwrap(), 3);
        assert!(repair(b"not a pdf").is_err());
    }

    #[test]
    fn info_report_mentions_basics() {
        let pdf = sample(3, "I");
        let r = info_report(&pdf).unwrap();
        assert!(r.contains("Pages: 3"), "{r}");
        assert!(
            r.contains("612 x 792 pt (Letter portrait): 3 page(s)"),
            "{r}"
        );
        assert!(r.contains("Helvetica (Type1), not embedded"), "{r}");
        assert!(r.contains("Encryption: none"));
        let enc = encrypt(&pdf, "pw", "", Permissions::default()).unwrap();
        assert!(info_report(&enc).unwrap().contains("password is required"));
        assert!(info_report(b"garbage").is_err());
    }

    fn with_form() -> Vec<u8> {
        build(1, "F", &[], |doc, ids| {
            let ap = doc.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 100.into(), 20.into()],
                    "Resources" => dictionary! {},
                },
                b"0 0 1 rg 0 0 100 20 re f".to_vec(),
            ));
            let field = doc.add_object(dictionary! {
                "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx", "T" => Object::string_literal("name"),
                "V" => Object::string_literal("Ada"), "Ff" => 1, "F" => 4 | 64,
                "Rect" => vec![100.into(), 500.into(), 300.into(), 540.into()],
                "AP" => dictionary! { "N" => ap }, "P" => ids[0],
                "AA" => dictionary! { "K" => dictionary! { "S" => "JavaScript", "JS" => Object::string_literal("AFNumber_Keystroke();") } },
            });
            let sig = doc.add_object(dictionary! {
                "Type" => "Annot", "Subtype" => "Widget", "FT" => "Sig", "T" => Object::string_literal("sig1"),
                "V" => dictionary! { "Type" => "Sig", "Filter" => "Adobe.PPKLite" },
                "Rect" => vec![0.into(), 0.into(), 0.into(), 0.into()], "P" => ids[0],
            });
            doc.get_dictionary_mut(ids[0])
                .unwrap()
                .set("Annots", vec![field.into(), sig.into()]);
            let cat_id = catalog_id(doc).unwrap();
            doc.get_dictionary_mut(cat_id).unwrap().set(
                "AcroForm",
                dictionary! { "Fields" => vec![field.into(), sig.into()], "SigFlags" => 3 },
            );
        })
    }

    #[test]
    fn forms_flatten_unlock_unsign() {
        let pdf = with_form();
        let r = info_report(&pdf).unwrap();
        assert!(
            r.contains("Form fields: 2") && r.contains("sig1 (signed)"),
            "{r}"
        );
        assert!(javascript_report(&pdf)
            .unwrap()
            .contains("AFNumber_Keystroke"));

        let unlocked = unlock_forms(&pdf).unwrap();
        let doc = valid(&unlocked, 1);
        let f = all_fields(&doc);
        let name = f.iter().find(|f| f.name == "name").unwrap();
        let d = doc.get_dictionary(name.id).unwrap();
        assert_eq!(d.get(b"Ff").unwrap().as_i64().unwrap(), 0);
        assert_eq!(d.get(b"F").unwrap().as_i64().unwrap(), 4);

        let unsigned = remove_signatures(&pdf).unwrap();
        let doc = valid(&unsigned, 1);
        let f = all_fields(&doc);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].name, "name");
        assert_eq!(page_annots(&doc, page_ids(&doc)[0]).len(), 1);
        assert!(remove_signatures(&unsigned).is_err());

        let flat = flatten_forms(&pdf).unwrap();
        let doc = valid(&flat, 1);
        assert!(!cat(&doc).has(b"AcroForm"));
        let pid = page_ids(&doc)[0];
        assert!(page_annots(&doc, pid).is_empty());
        let content = String::from_utf8_lossy(&doc.get_page_content(pid)).into_owned();
        assert!(
            content.contains("q 2 0 0 2 100 500 cm /Fx1 Do Q"),
            "{content}"
        );
        let res = inherited(&doc, pid, b"Resources")
            .and_then(|o| dict_of(&doc, o))
            .unwrap();
        assert!(get_dict(&doc, res, b"XObject").unwrap().has(b"Fx1"));
        assert!(page_texts(&flat).unwrap()[0].contains("F page 1"));
        // and it renders
        let pdf = hayro::hayro_syntax::Pdf::new(flat).unwrap();
        let cache = hayro::RenderCache::new();
        let settings = hayro::hayro_interpret::InterpreterSettings::default();
        let pm = hayro::render(
            &pdf.pages()[0],
            &cache,
            &settings,
            &hayro::RenderSettings::default(),
        );
        assert!(pm.width() > 0);
    }

    #[test]
    fn page_texts_per_page() {
        let pdf = sample(3, "Q");
        assert_eq!(
            page_texts(&pdf).unwrap(),
            vec!["Q page 1", "Q page 2", "Q page 3"]
        );
        assert!(page_texts(b"%PDF-1.4 broken").is_err());
    }

    #[test]
    fn cyclic_outline_does_not_hang() {
        let pdf = build(1, "C", &[], |doc, ids| {
            let root = doc.new_object_id();
            let a = doc.new_object_id();
            doc.objects.insert(
                a,
                Object::Dictionary(dictionary! { "Title" => Object::string_literal("loop"), "Parent" => root, "Next" => a, "First" => a, "Dest" => vec![ids[0].into(), "Fit".into()] }),
            );
            doc.objects.insert(
                root,
                Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => a, "Last" => a }),
            );
            let cat_id = catalog_id(doc).unwrap();
            doc.get_dictionary_mut(cat_id)
                .unwrap()
                .set("Outlines", root);
        });
        assert_eq!(bookmarks_get(&pdf).unwrap().len(), 1);
    }

    /// Real-file check: `OGX_PDF=/path/file.pdf cargo test real_file_structure`
    /// runs the non-interactive tools and checks every output still loads
    /// and parses with the same page count. Skipped when OGX_PDF isn't set.
    #[test]
    fn real_file_structure() {
        let Some(path) = std::env::var_os("OGX_PDF") else {
            return;
        };
        let pdf = std::fs::read(path).unwrap();
        let n = Document::load_mem(&pdf).unwrap().get_pages().len();
        let r = info_report(&pdf).unwrap();
        assert!(r.contains(&format!("Pages: {n}")));
        let outs = [
            ("repair", repair(&pdf).unwrap()),
            ("compress", compress_lossless(&pdf).unwrap()),
            ("sanitize", sanitize(&pdf, Sanitize::default()).unwrap()),
            ("flatten", flatten_forms(&pdf).unwrap()),
            ("annots", remove_annotations(&pdf).unwrap()),
            ("images", remove_images(&pdf).unwrap()),
            ("crop", crop(&pdf, &[0], [5.0; 4]).unwrap()),
            (
                "redact",
                redact_text(&pdf, &["the".into()], false).unwrap().0,
            ),
        ];
        for (name, out) in outs {
            eprintln!("{name}: {} -> {} bytes", pdf.len(), out.len());
            valid(&out, n);
        }
        let _ = extract_images(&pdf, &(0..n.min(5)).collect::<Vec<_>>()).unwrap();
        let _ = javascript_report(&pdf).unwrap();
        let _ = bookmarks_get(&pdf).unwrap();
        let _ = attachments_extract(&pdf).unwrap();
    }
}
