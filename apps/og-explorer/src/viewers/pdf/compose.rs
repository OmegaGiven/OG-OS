//! Builds a new PDF from a page plan: any pages, from any number of source
//! PDFs, in any order, with extra rotation. That one primitive covers
//! reorder, delete, rotate, merge/insert, extract and split.
//!
//! lopdf (pure Rust, MIT). Known limits, see SPEC.md §5a: bookmarks
//! (outlines), form fields (AcroForm) and named destinations of the sources
//! are not carried over yet; page content, annotations, links within a page,
//! fonts and images are.

use lopdf::{dictionary, Document, Object, ObjectId};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

/// (source index, 0-based page in that source, extra clockwise rotation)
pub type PlanPage = (usize, usize, u16);

const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

pub fn build(sources: &[Arc<Vec<u8>>], plan: &[PlanPage]) -> Result<Vec<u8>, String> {
    if plan.is_empty() {
        return Err("no pages left".into());
    }
    let mut out = Document::with_version("1.7");
    let mut objects = BTreeMap::new();
    let mut page_ids: Vec<Vec<ObjectId>> = Vec::new();
    let mut next_id = 1;

    for (i, bytes) in sources.iter().enumerate() {
        let mut doc = Document::load_mem(bytes).map_err(|e| format!("source {}: {e}", i + 1))?;
        if doc.is_encrypted() {
            doc.decrypt("")
                .map_err(|_| format!("source {} is password-protected", i + 1))?;
        }
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;
        let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
        // pages are about to get a new parent, so copy down anything they
        // were inheriting from the old page tree
        for &pid in &pages {
            push_down_inherited(&mut doc, pid);
        }
        page_ids.push(pages);
        for (id, obj) in doc.objects {
            let ty = obj
                .as_dict()
                .ok()
                .and_then(|d| d.get_type().ok())
                .map(|t| t.to_vec());
            match ty.as_deref() {
                Some(b"Catalog" | b"Pages" | b"ObjStm" | b"XRef") => {}
                _ => {
                    objects.insert(id, obj);
                }
            }
        }
    }
    out.objects = objects;
    out.max_id = next_id;

    let pages_id = out.new_object_id();
    let mut kids = Vec::with_capacity(plan.len());
    let mut used = HashSet::new();
    // untouched copies of every page, so a duplicate starts from the
    // original and not from the first copy's edited rotation
    let pristine: BTreeMap<ObjectId, Object> = page_ids
        .iter()
        .flatten()
        .filter_map(|id| out.objects.get(id).map(|o| (*id, o.clone())))
        .collect();
    for &(src, page, rot) in plan {
        let mut id = *page_ids
            .get(src)
            .and_then(|p| p.get(page))
            .ok_or_else(|| format!("page {} of source {} doesn't exist", page + 1, src + 1))?;
        if !used.insert(id) {
            // same page twice (duplicate): needs its own page object
            let copy = pristine.get(&id).cloned().ok_or("missing page object")?;
            id = out.add_object(copy);
        }
        let dict = out.get_dictionary_mut(id).map_err(|e| e.to_string())?;
        dict.set("Parent", pages_id);
        let cur = dict
            .get(b"Rotate")
            .ok()
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(0);
        let total = (cur + rot as i64).rem_euclid(360);
        if total == 0 {
            dict.remove(b"Rotate");
        } else {
            dict.set("Rotate", total);
        }
        kids.push(Object::Reference(id));
    }
    let count = kids.len() as i64;
    out.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    let catalog = out.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    out.trailer.set("Root", catalog);
    out.prune_objects(); // drops deleted pages and anything only they used
    out.compress();
    let mut buf = Vec::new();
    out.save_to(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn push_down_inherited(doc: &mut Document, page: ObjectId) {
    let mut found: Vec<(&[u8], Object)> = Vec::new();
    let Ok(dict) = doc.get_dictionary(page) else {
        return;
    };
    let mut missing: Vec<&[u8]> = INHERITABLE
        .iter()
        .copied()
        .filter(|k| !dict.has(k))
        .collect();
    let mut parent = dict.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
    let mut guard = 0;
    while let (Some(pid), false) = (parent, missing.is_empty()) {
        guard += 1;
        if guard > 64 {
            break; // cyclic page tree
        }
        let Ok(pd) = doc.get_dictionary(pid) else {
            break;
        };
        missing.retain(|k| match pd.get(k) {
            Ok(v) => {
                found.push((k, v.clone()));
                false
            }
            Err(_) => true,
        });
        parent = pd.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
    }
    if let Ok(dict) = doc.get_dictionary_mut(page) {
        for (k, v) in found {
            dict.set(k.to_vec(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::Stream;

    /// n-page PDF whose pages share an inherited MediaBox/Resources on the
    /// Pages node (the case push_down_inherited exists for).
    fn sample(n: usize, tag: &str) -> Arc<Vec<u8>> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        for i in 0..n {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new(
                        "Tj",
                        vec![Object::string_literal(format!("{tag} page {}", i + 1))],
                    ),
                    Operation::new("ET", vec![]),
                ],
            };
            let c = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            kids.push(
                doc.add_object(
                    dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c },
                )
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
        Arc::new(buf)
    }

    fn page_texts(bytes: &[u8]) -> Vec<(String, i64, bool)> {
        let doc = Document::load_mem(bytes).unwrap();
        doc.get_pages()
            .into_iter()
            .map(|(n, id)| {
                let d = doc.get_dictionary(id).unwrap();
                let rot = d
                    .get(b"Rotate")
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0);
                let has_box = d.has(b"MediaBox") && d.has(b"Resources");
                (
                    doc.extract_text(&[n]).unwrap().trim().to_string(),
                    rot,
                    has_box,
                )
            })
            .collect()
    }

    #[test]
    fn reorder_rotate_delete_merge_duplicate() {
        let a = sample(3, "A");
        let b = sample(2, "B");
        let plan = [(0, 2, 90), (1, 0, 0), (0, 0, 270), (0, 0, 0)];
        let out = build(&[a, b], &plan).unwrap();
        let pages = page_texts(&out);
        assert_eq!(pages.len(), 4);
        assert_eq!(pages[0], ("A page 3".into(), 90, true));
        assert_eq!(pages[1], ("B page 1".into(), 0, true));
        assert_eq!(pages[2], ("A page 1".into(), 270, true));
        assert_eq!(pages[3], ("A page 1".into(), 0, true)); // duplicate got its own object
                                                            // and hayro can render the result
        let pdf = hayro::hayro_syntax::Pdf::new(out).unwrap();
        assert_eq!(pdf.pages().len(), 4);
    }

    #[test]
    fn empty_plan_is_an_error() {
        assert!(build(&[sample(1, "A")], &[]).is_err());
    }

    #[test]
    fn deleted_pages_are_pruned() {
        let a = sample(5, "A");
        let full = build(
            &[a.clone()],
            &[(0, 0, 0), (0, 1, 0), (0, 2, 0), (0, 3, 0), (0, 4, 0)],
        )
        .unwrap();
        let one = build(&[a], &[(0, 4, 0)]).unwrap();
        assert!(one.len() < full.len(), "{} !< {}", one.len(), full.len());
        assert_eq!(page_texts(&one)[0].0, "A page 5");
    }
}

/// Real-file check: `OGX_PDF=/path/to/file.pdf cargo test -p og-explorer real_file`
/// reverses and rotates every page, then makes sure hayro still renders all
/// of them. Skipped when OGX_PDF isn't set.
#[cfg(test)]
#[test]
fn real_file_roundtrip() {
    let Some(path) = std::env::var_os("OGX_PDF") else {
        return;
    };
    let bytes = Arc::new(std::fs::read(&path).unwrap());
    let n = Document::load_mem(&bytes).unwrap().get_pages().len();
    let plan: Vec<PlanPage> = (0..n).rev().map(|p| (0, p, 90)).collect();
    let out = build(&[bytes.clone(), bytes], &plan).unwrap();
    let pdf = hayro::hayro_syntax::Pdf::new(out.clone()).unwrap();
    assert_eq!(pdf.pages().len(), n);
    let cache = hayro::RenderCache::new();
    let settings = hayro::hayro_interpret::InterpreterSettings::default();
    for page in pdf.pages().iter() {
        let pm = hayro::render(page, &cache, &settings, &hayro::RenderSettings::default());
        assert!(pm.width() > 0 && pm.height() > 0);
    }
    std::fs::write(std::env::temp_dir().join("ogx-roundtrip.pdf"), out).unwrap();
}
