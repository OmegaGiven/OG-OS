//! Round-trip and editing tests. Every .docx here is built in memory (or in
//! a temp dir); no user files are touched.

use super::model::{Align, Attr, Editor, Motion, Path, Pos};
use super::parse::{open_package, tree};
use super::styles::Kind;
use super::{write, Act, Msg, Opened, State};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml""#;

pub(super) const BODY: &str = r#"
    <w:p w14:paraId="1A2B3C4D" w:rsidR="00AB12CD"><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Doc Title</w:t></w:r></w:p>
    <w:p><w:pPr><w:pStyle w:val="berschrift1"/></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p>
    <w:p><w:r w:rsidR="00C0FFEE"><w:rPr><w:i/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">Hello brave </w:t></w:r><w:r><w:t>world</w:t></w:r><w:bookmarkStart w:id="0" w:name="bm"/><w:r><w:t>.</w:t></w:r><w:bookmarkEnd w:id="0"/></w:p>
    <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>first bullet</w:t></w:r></w:p>
    <w:p><w:r><w:t xml:space="preserve">Before </w:t></w:r><w:r><w:drawing><wp:inline><wp:extent cx="952500" cy="476250"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:t xml:space="preserve"> after</w:t></w:r></w:p>
    <w:p><w:hyperlink r:id="rId5"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>a link</w:t></w:r></w:hyperlink><w:r><w:t xml:space="preserve"> and &amp; text &lt;ok&gt;</w:t></w:r></w:p>
    <w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
      <w:tr><w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
      <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
    <w:p><w:r><w:br w:type="page"/></w:r></w:p>
    <w:p/>
    <w:p><w:r><w:t>Last line</w:t></w:r></w:p>
    <w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>
  "#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="berschrift1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/><w:color w:val="2F5496"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:rPr><w:sz w:val="56"/></w:rPr></w:style>
<w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>
</w:styles>"#;

const NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val=""/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([200, 30, 30]));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn doc_xml(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {NS}><w:body>{body}</w:body></w:document>"#
    )
}

/// A .docx in memory; `numbering` off leaves numbering.xml out entirely.
pub(super) fn build(body: &str, numbering: bool) -> Vec<u8> {
    let mut types = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#,
    );
    let mut rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/>"#,
    );
    if numbering {
        types.push_str(r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#);
        rels.push_str(r#"<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#);
    }
    types.push_str("</Types>");
    rels.push_str("</Relationships>");
    let mut files: Vec<(&str, Vec<u8>)> = vec![
        ("[Content_Types].xml", types.into_bytes()),
        (
            "_rels/.rels",
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#
                .to_vec(),
        ),
        ("word/document.xml", doc_xml(body).into_bytes()),
        ("word/styles.xml", STYLES.as_bytes().to_vec()),
        ("word/_rels/document.xml.rels", rels.into_bytes()),
        ("word/media/image1.png", png()),
    ];
    if numbering {
        files.push(("word/numbering.xml", NUMBERING.as_bytes().to_vec()));
    }
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        z.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        z.write_all(&data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().to_string(), b)
        })
        .collect()
}

fn entry(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
    entries(bytes)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|e| e.1)
}

fn editor(bytes: Vec<u8>) -> (Editor, super::parse::Package) {
    let pkg = open_package(bytes).unwrap();
    (Editor::new(pkg.doc.clone()), pkg)
}

fn save(ed: &Editor, pkg: &super::parse::Package) -> Vec<u8> {
    let xml = write::document_xml(&ed.doc);
    write::package(&pkg.bytes, &pkg.main, &xml, &BTreeMap::new()).unwrap()
}

fn top(b: usize) -> Path {
    Path { b, cell: None }
}

fn texts(ed: &Editor) -> Vec<String> {
    ed.flow
        .iter()
        .map(|&p| {
            let para = ed.doc.para(p).unwrap();
            para.plain(0..para.len())
        })
        .collect()
}

/// Span of the n-th top-level body element in the original source.
fn body_span(src: &str, n: usize) -> std::ops::Range<usize> {
    let t = tree(src).unwrap();
    let body = t[0].child("body").unwrap();
    let k = &body.kids[n];
    k.start..k.end
}

// ------------------------------------------------------------------ tests

#[test]
fn untouched_document_round_trips_byte_identical() {
    let bytes = build(BODY, true);
    let (ed, pkg) = editor(bytes.clone());
    let src = String::from_utf8(entry(&bytes, "word/document.xml").unwrap()).unwrap();
    assert_eq!(write::document_xml(&ed.doc), src);
    let out = save(&ed, &pkg);
    let (a, b) = (entries(&bytes), entries(&out));
    assert_eq!(a.len(), b.len());
    for ((na, da), (nb, db)) in a.iter().zip(&b) {
        assert_eq!(na, nb, "entry order");
        assert_eq!(da, db, "{na} changed");
    }
}

#[test]
fn model_reads_structure() {
    let (ed, _) = editor(build(BODY, true));
    let t = texts(&ed);
    assert_eq!(t[0], "Doc Title");
    assert_eq!(t[1], "Intro");
    assert_eq!(t[2], "Hello brave world.");
    assert_eq!(t[4], "Before  after", "image object has no text");
    assert_eq!(t[5], "a link and & text <ok>");
    assert!(t.contains(&"A1".to_string()) && t.contains(&"B2".to_string()));
    assert_eq!(t.last().unwrap(), "Last line");
    let st = &ed.doc.styles;
    assert_eq!(st.kind(Some("berschrift1")), Kind::Heading(1));
    assert_eq!(st.kind(Some("Title")), Kind::Title);
    assert_eq!(st.id_for(Kind::Heading(1)).as_deref(), Some("berschrift1"));
    assert_eq!(st.id_for(Kind::Heading(2)).as_deref(), Some("Heading2"));
    assert_eq!(
        ed.list_of(ed.doc.para(top(3)).unwrap()).map(|l| l.2),
        Some(true)
    );
}

#[test]
fn editing_one_paragraph_changes_only_that_paragraph() {
    let bytes = build(BODY, true);
    let (mut ed, pkg) = editor(bytes.clone());
    let src = String::from_utf8(entry(&bytes, "word/document.xml").unwrap()).unwrap();
    ed.set_sel(Pos::new(top(2), 6), Pos::new(top(2), 6));
    ed.insert("very ");
    let out = write::document_xml(&ed.doc);
    let span = body_span(&src, 2);
    assert_eq!(&out[..span.start], &src[..span.start]);
    let tail = &src[span.end..];
    assert!(out.ends_with(tail));
    let new_p = &out[span.start..out.len() - tail.len()];
    assert!(new_p.starts_with(r#"<w:p>"#), "{new_p}");
    // the edited run keeps its rPr, the bookmark survives in place
    assert!(new_p.contains(r#"<w:rPr><w:i/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">Hello very brave </w:t>"#), "{new_p}");
    assert!(new_p.contains(
        r#"<w:bookmarkStart w:id="0" w:name="bm"/><w:r><w:t>.</w:t></w:r><w:bookmarkEnd w:id="0"/>"#
    ));
    // still a valid package that reads back the same
    let saved = save(&ed, &pkg);
    let (ed2, _) = editor(saved);
    assert_eq!(texts(&ed2)[2], "Hello very brave world.");
}

#[test]
fn open_tag_with_attributes_is_kept() {
    let (mut ed, _) = editor(build(BODY, true));
    ed.set_sel(Pos::new(top(0), 3), Pos::new(top(0), 3));
    ed.insert("X");
    let out = write::document_xml(&ed.doc);
    assert!(out.contains(r#"<w:p w14:paraId="1A2B3C4D" w:rsidR="00AB12CD"><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>DocX Title</w:t></w:r></w:p>"#), "{out}");
}

#[test]
fn bold_toggle_merges_into_the_runs_rpr() {
    let (mut ed, pkg) = editor(build(BODY, true));
    // "brave" inside the italic run
    ed.set_sel(Pos::new(top(2), 6), Pos::new(top(2), 11));
    assert!(!ed.attr_on(Attr::Bold));
    assert!(ed.attr_on(Attr::Italic));
    ed.toggle(Attr::Bold);
    assert!(ed.attr_on(Attr::Bold));
    let out = write::document_xml(&ed.doc);
    assert!(
        out.contains(r#"<w:r><w:rPr><w:b/><w:i/><w:sz w:val="24"/></w:rPr><w:t>brave</w:t></w:r>"#),
        "{out}"
    );
    assert!(out.contains(r#"<w:r><w:rPr><w:i/><w:sz w:val="24"/></w:rPr><w:t xml:space="preserve">Hello </w:t></w:r>"#));
    // toggling again removes it (the original had no <w:b/>)
    ed.toggle(Attr::Bold);
    assert!(!ed.attr_on(Attr::Bold));
    let out = write::document_xml(&ed.doc);
    assert!(!out.contains("<w:b/>"), "{out}");
    // un-bolding a heading (bold by style) writes an explicit off
    ed.set_sel(Pos::new(top(1), 0), Pos::new(top(1), 5));
    assert!(ed.attr_on(Attr::Bold));
    ed.toggle(Attr::Bold);
    let out = write::document_xml(&ed.doc);
    assert!(
        out.contains(r#"<w:r><w:rPr><w:b w:val="0"/></w:rPr><w:t>Intro</w:t></w:r>"#),
        "{out}"
    );
    let (ed2, _) = editor(save(&ed, &pkg));
    assert_eq!(texts(&ed2)[1], "Intro");
}

#[test]
fn typing_style_applies_to_next_text() {
    let (mut ed, _) = editor(build(BODY, true));
    let end = ed.doc.para(top(9)).unwrap().len();
    ed.set_sel(Pos::new(top(9), end), Pos::new(top(9), end));
    ed.toggle(Attr::Underline);
    ed.insert("!");
    let out = write::document_xml(&ed.doc);
    assert!(out.contains(r#"<w:r><w:t>Last line</w:t></w:r><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>!</w:t></w:r>"#), "{out}");
}

#[test]
fn split_and_merge_paragraphs() {
    let bytes = build(BODY, true);
    let (mut ed, pkg) = editor(bytes.clone());
    let n = ed.flow.len();
    // Enter after "Hello "
    ed.set_sel(Pos::new(top(2), 6), Pos::new(top(2), 6));
    ed.enter();
    assert_eq!(ed.flow.len(), n + 1);
    assert_eq!(ed.head, Pos::new(top(3), 0));
    let t = texts(&ed);
    assert_eq!(t[2], "Hello ");
    assert_eq!(t[3], "brave world.");
    // new paragraph has no copied paraId/open tag, keeps formatting
    let out = write::document_xml(&ed.doc);
    assert!(
        out.contains(r#"<w:p><w:r><w:rPr><w:i/><w:sz w:val="24"/></w:rPr><w:t>brave </w:t></w:r>"#)
            || out.contains(r#"<w:t xml:space="preserve">brave </w:t>"#),
        "{out}"
    );
    let (ed2, _) = editor(save(&ed, &pkg));
    assert_eq!(texts(&ed2)[3], "brave world.");
    // Backspace at the start merges back
    ed.backspace(false);
    assert_eq!(ed.flow.len(), n);
    assert_eq!(texts(&ed)[2], "Hello brave world.");
    assert_eq!(ed.head, Pos::new(top(2), 6));
    // Delete at the end of a paragraph pulls the next one up
    let end = ed.doc.para(top(0)).unwrap().len();
    ed.set_sel(Pos::new(top(0), end), Pos::new(top(0), end));
    ed.delete(false);
    assert_eq!(texts(&ed)[0], "Doc TitleIntro");
    // Enter at the end of a heading starts a Normal paragraph
    let (mut ed, _) = editor(bytes);
    ed.set_sel(Pos::new(top(1), 5), Pos::new(top(1), 5));
    ed.enter();
    let p = ed.doc.para(top(2)).unwrap();
    assert_eq!(p.props.style, None);
    assert_eq!(
        ed.doc.para(top(1)).unwrap().props.style.as_deref(),
        Some("berschrift1")
    );
}

#[test]
fn line_break_and_tab_become_elements() {
    let (mut ed, _) = editor(build(BODY, true));
    ed.set_sel(Pos::new(top(9), 4), Pos::new(top(9), 4));
    ed.line_break();
    ed.insert("\t");
    let out = write::document_xml(&ed.doc);
    assert!(
        out.contains(
            r#"<w:r><w:t>Last</w:t><w:br/><w:tab/><w:t xml:space="preserve"> line</w:t></w:r>"#
        ),
        "{out}"
    );
    let (ed2, _) = editor({
        let pkg = open_package(build(BODY, true)).unwrap();
        write::package(&pkg.bytes, &pkg.main, &out, &BTreeMap::new()).unwrap()
    });
    assert_eq!(texts(&ed2).last().unwrap(), "Last\n\t line");
}

#[test]
fn undo_redo_restore_exact_xml() {
    let bytes = build(BODY, true);
    let (mut ed, _) = editor(bytes.clone());
    let src = write::document_xml(&ed.doc);
    let v0 = ed.version;
    ed.set_sel(Pos::new(top(9), 0), Pos::new(top(9), 0));
    for c in ["a", "b", "c"] {
        ed.insert(c);
    }
    ed.enter();
    let edited = write::document_xml(&ed.doc);
    assert_ne!(edited, src);
    assert!(ed.undo()); // the Enter
    assert!(ed.undo()); // the typing (one step)
    assert_eq!(write::document_xml(&ed.doc), src);
    assert_eq!(ed.version, v0, "back to the saved version = not dirty");
    assert!(!ed.undo());
    assert!(ed.redo());
    assert!(ed.redo());
    assert_eq!(write::document_xml(&ed.doc), edited);
    assert!(!ed.redo());
}

#[test]
fn table_cell_edit_and_tab() {
    let bytes = build(BODY, true);
    let (mut ed, pkg) = editor(bytes.clone());
    let src = String::from_utf8(entry(&bytes, "word/document.xml").unwrap()).unwrap();
    let a1 = Path {
        b: 6,
        cell: Some((0, 0, 0)),
    };
    ed.set_sel(Pos::new(a1, 2), Pos::new(a1, 2));
    ed.insert("x");
    assert!(ed.cell_step(false));
    assert_eq!(ed.head.path.cell, Some((0, 1, 0)));
    ed.insert("Z");
    let out = write::document_xml(&ed.doc);
    // structure (tblPr, grid, tcPr, row tags) kept verbatim around the cells
    assert!(out.contains(r#"<w:tc><w:tcPr><w:tcW w:w="3000" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>A1x</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Z</w:t></w:r></w:p></w:tc></w:tr>"#), "{out}");
    assert!(out.contains(r#"<w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc>"#));
    let t = body_span(&src, 6);
    assert_eq!(&out[..t.start], &src[..t.start]);
    let (ed2, _) = editor(save(&ed, &pkg));
    let tx = texts(&ed2);
    assert!(tx.contains(&"A1x".to_string()) && tx.contains(&"Z".to_string()));
    // Enter inside a cell adds a paragraph in that cell
    let (mut ed, _) = editor(bytes);
    ed.set_sel(Pos::new(a1, 1), Pos::new(a1, 1));
    ed.enter();
    assert_eq!(ed.head.path.cell, Some((0, 0, 1)));
    let out = write::document_xml(&ed.doc);
    assert!(out.contains(r#"</w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p><w:p><w:r><w:t>1</w:t></w:r></w:p></w:tc>"#), "{out}");
}

#[test]
fn images_and_rels_survive_edits_next_to_them() {
    let bytes = build(BODY, true);
    let (mut ed, pkg) = editor(bytes.clone());
    // caret right after the image object (model "Before " + U+FFFC)
    let p = top(4);
    let off = "Before ".len() + super::model::OBJ_LEN;
    ed.set_sel(Pos::new(p, off), Pos::new(p, off));
    ed.insert("!");
    let out = save(&ed, &pkg);
    let xml = String::from_utf8(entry(&out, "word/document.xml").unwrap()).unwrap();
    assert!(xml.contains(r#"<w:r><w:drawing><wp:inline><wp:extent cx="952500" cy="476250"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:t>! after</w:t></w:r>"#), "{xml}");
    for name in [
        "word/media/image1.png",
        "word/_rels/document.xml.rels",
        "[Content_Types].xml",
        "word/styles.xml",
        "word/numbering.xml",
        "_rels/.rels",
    ] {
        assert_eq!(entry(&out, name), entry(&bytes, name), "{name}");
    }
    let pkg2 = open_package(out).unwrap();
    assert!(pkg2.images.contains_key("rId9"));
    // deleting across the image removes it as one unit
    let (mut ed, _) = editor(bytes);
    ed.set_sel(Pos::new(p, 6), Pos::new(p, off + 1));
    ed.backspace(false);
    let xml = write::document_xml(&ed.doc);
    assert!(!xml.contains("rId9"));
    assert_eq!(texts(&ed)[4], "Beforeafter");
}

#[test]
fn hyperlink_is_an_atomic_object() {
    let (mut ed, _) = editor(build(BODY, true));
    let p = top(5);
    // Right from the start jumps over the whole link
    ed.set_sel(Pos::new(p, 0), Pos::new(p, 0));
    ed.motion(Motion::Right, false);
    assert_eq!(ed.head.off, super::model::OBJ_LEN);
    ed.insert("+");
    let xml = write::document_xml(&ed.doc);
    assert!(xml.contains(r#"<w:hyperlink r:id="rId5"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>a link</w:t></w:r></w:hyperlink><w:r><w:t>+ and &amp; text &lt;ok&gt;</w:t></w:r>"#), "{xml}");
}

#[test]
fn multi_paragraph_selection_delete_and_paste() {
    let bytes = build(BODY, true);
    let (mut ed, pkg) = editor(bytes.clone());
    let n = ed.flow.len();
    // from "Doc |Title" to "Hel|lo"
    ed.set_sel(Pos::new(top(0), 4), Pos::new(top(2), 3));
    assert_eq!(ed.selected_text(), "Title\nIntro\nHel");
    ed.insert("one\ntwo\nthree");
    let t = texts(&ed);
    assert_eq!(t[0], "Doc one");
    assert_eq!(t[1], "two");
    assert_eq!(t[2], "threelo brave world.");
    assert_eq!(ed.flow.len(), n);
    let (ed2, _) = editor(save(&ed, &pkg));
    assert_eq!(texts(&ed2)[2], "threelo brave world.");
    // a selection spanning into the table clears text but keeps the table
    let (mut ed, _) = editor(bytes);
    let b1 = Path {
        b: 6,
        cell: Some((0, 1, 0)),
    };
    ed.set_sel(Pos::new(top(5), 2), Pos::new(b1, 1));
    ed.backspace(false);
    let t = texts(&ed);
    assert!(t.contains(&"1".to_string()), "{t:?}");
    assert!(t.contains(&"A2".to_string()));
}

#[test]
fn styles_alignment_and_lists() {
    let (mut ed, pkg) = editor(build(BODY, true));
    ed.set_sel(Pos::new(top(9), 0), Pos::new(top(9), 0));
    ed.set_kind(Kind::Heading(1));
    ed.set_align(Align::Center);
    let xml = write::document_xml(&ed.doc);
    assert!(xml.contains(r#"<w:p><w:pPr><w:pStyle w:val="berschrift1"/><w:jc w:val="center"/></w:pPr><w:r><w:t>Last line</w:t></w:r></w:p>"#), "{xml}");
    assert_eq!(ed.kind_at(), Kind::Heading(1));
    assert_eq!(ed.align_at(), Align::Center);
    // bullets reuse the document's bullet definition (numId 1) and slot
    // numPr between pStyle and jc
    let id = ed.doc.numbering.find(true).unwrap();
    assert_eq!(id, "1");
    ed.toggle_list(true, &id);
    let xml = write::document_xml(&ed.doc);
    assert!(xml.contains(r#"<w:pPr><w:pStyle w:val="berschrift1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:jc w:val="center"/></w:pPr>"#), "{xml}");
    assert_eq!(ed.list_at(), Some(true));
    // numbers
    assert_eq!(ed.doc.numbering.find(false).as_deref(), Some("2"));
    ed.toggle_list(false, "2");
    assert_eq!(ed.list_at(), Some(false));
    ed.toggle_list(false, "2");
    assert_eq!(ed.list_at(), None);
    ed.set_kind(Kind::Normal);
    ed.set_align(Align::Left);
    let xml = write::document_xml(&ed.doc);
    assert!(
        xml.contains(r#"<w:p><w:r><w:t>Last line</w:t></w:r></w:p>"#),
        "{xml}"
    );
    let _ = save(&ed, &pkg);
}

#[test]
fn list_definition_is_added_when_missing() {
    let body = r#"<w:p><w:r><w:t>item</w:t></w:r></w:p><w:sectPr/>"#;
    let bytes = build(body, false);
    let pkg = open_package(bytes.clone()).unwrap();
    assert!(pkg.numbering_xml.is_none());
    let opened = Opened {
        path: std::env::temp_dir().join("never-written.docx"),
        pkg,
    };
    let (mut st, _) = State::new(opened);
    let _ = st.update(Msg::ToggleEdit);
    let _ = st.update(Msg::List(true));
    assert_eq!(st.ed.list_at(), Some(true));
    assert!(st.is_dirty());
    let out = st.package_bytes().unwrap();
    let num = String::from_utf8(entry(&out, "word/numbering.xml").unwrap()).unwrap();
    assert!(num.contains(r#"<w:numFmt w:val="bullet"/>"#));
    let ct = String::from_utf8(entry(&out, "[Content_Types].xml").unwrap()).unwrap();
    assert!(ct.contains(r#"PartName="/word/numbering.xml""#));
    let rels = String::from_utf8(entry(&out, "word/_rels/document.xml.rels").unwrap()).unwrap();
    assert!(rels.contains(r#"Target="numbering.xml""#));
    let pkg2 = open_package(out).unwrap();
    let ed2 = Editor::new(pkg2.doc);
    assert_eq!(ed2.list_at(), Some(true));
    // a second list kind appends to the new part
    let _ = st.update(Msg::List(false));
    assert_eq!(st.ed.list_at(), Some(false));
    let out = st.package_bytes().unwrap();
    let num = String::from_utf8(entry(&out, "word/numbering.xml").unwrap()).unwrap();
    assert_eq!(num.matches("<w:num ").count(), 2, "{num}");
    assert!(
        num.find("<w:abstractNum w:abstractNumId=\"2\"").unwrap() < num.find("<w:num ").unwrap()
    );
}

#[test]
fn empty_list_item_enter_ends_the_list() {
    let (mut ed, _) = editor(build(BODY, true));
    let end = ed.doc.para(top(3)).unwrap().len();
    ed.set_sel(Pos::new(top(3), end), Pos::new(top(3), end));
    ed.enter();
    assert_eq!(ed.list_at(), Some(true), "new item continues the list");
    ed.enter();
    assert_eq!(ed.list_at(), None);
}

#[tokio::test]
async fn state_edit_save_cycle_in_temp_dir() {
    let dir = std::env::temp_dir().join(format!("ogx-docx-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.docx");
    let bytes = build(BODY, true);
    std::fs::write(&path, &bytes).unwrap();
    let opened = super::open(path.clone()).await.unwrap();
    let (mut st, _) = State::new(opened);
    assert_eq!(st.is_editing(), Some(false));
    assert!(!st.is_dirty());
    // read-only: edits are ignored, selection works
    let _ = st.update(Msg::Act(Act::Insert("nope".into())));
    assert!(!st.is_dirty());
    let _ = st.update(Msg::Act(Act::SelectAll));
    assert!(st.ed.has_sel());
    let _ = st.toggle_edit();
    assert_eq!(st.is_editing(), Some(true));
    let _ = st.update(Msg::Act(Act::Move(Motion::DocEnd, false)));
    let _ = st.update(Msg::Act(Act::Insert(" — edited".into())));
    assert!(st.is_dirty());
    assert!(st.words > 5);
    // leaving edit mode with changes is refused
    let (_, ev) = st.update(Msg::ToggleEdit);
    assert!(matches!(ev, Some(super::Event::Status(s)) if s.contains("Unsaved")));
    assert_eq!(st.is_editing(), Some(true));
    // save (what State::save's task does)
    let out = st.package_bytes().unwrap();
    crate::fsops::write_bytes(path.clone(), out).await.unwrap();
    let _ = st.update(Msg::Saved(Ok((st.ed.version, st.parts_gen))));
    assert!(!st.is_dirty());
    let reopened = super::open(path.clone()).await.unwrap();
    let (st2, _) = State::new(reopened);
    assert_eq!(texts(&st2.ed).last().unwrap(), "Last line — edited");
    // undo after save makes it dirty again; redo returns to the saved state
    let _ = st.update(Msg::Act(Act::Undo));
    assert!(st.is_dirty());
    let _ = st.update(Msg::Act(Act::Redo));
    assert!(!st.is_dirty());
    let _ = st.view();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn list_labels_count_per_list() {
    let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>a</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>b</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>c</w:t></w:r></w:p>"#;
    let (ed, _) = editor(build(body, true));
    let labels = super::list_labels(&ed);
    let get = |b: usize| labels.get(&ed.doc.para(top(b)).unwrap().id).cloned();
    assert_eq!(get(0).as_deref(), Some("1."));
    assert_eq!(get(1).as_deref(), Some("2."));
    assert_eq!(get(2).as_deref(), Some("•"));
}

#[test]
fn unsupported_documents_fail_cleanly() {
    assert!(open_package(b"not a zip".to_vec()).is_err());
    // unprefixed WordprocessingML: refuse (falls back to the reader)
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    z.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    z.write_all(br#"<document xmlns="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><body><p/></body></document>"#)
        .unwrap();
    let bytes = z.finish().unwrap().into_inner();
    assert!(open_package(bytes).is_err());
}

/// Optional: LibreOffice accepts what we write (run with --ignored).
#[test]
#[ignore]
fn soffice_converts_edited_document() {
    let Ok(st) = std::process::Command::new("soffice")
        .arg("--version")
        .output()
    else {
        return;
    };
    if !st.status.success() {
        return;
    }
    let (mut ed, pkg) = editor(build(BODY, true));
    ed.set_sel(Pos::new(top(2), 0), Pos::new(top(2), 5));
    ed.toggle(Attr::Bold);
    ed.set_sel(Pos::new(top(9), 0), Pos::new(top(9), 0));
    ed.set_kind(Kind::Heading(2));
    ed.toggle_list(true, "1");
    ed.insert("New ");
    ed.enter();
    ed.insert("and a new paragraph");
    let dir = std::env::temp_dir().join(format!("ogx-docx-soffice-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("edited.docx");
    std::fs::write(&file, save(&ed, &pkg)).unwrap();
    let out = std::process::Command::new("soffice")
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
        .arg(&dir)
        .arg(&file)
        .env("HOME", &dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pdf = std::fs::metadata(dir.join("edited.pdf")).unwrap();
    assert!(pdf.len() > 1000);
    std::fs::remove_dir_all(&dir).ok();
}

/// Round-trip real documents listed in OGX_DOCX_FILES (colon-separated):
/// an unedited save must reproduce document.xml exactly and an edited one
/// must re-parse. Only reads those files (run with --ignored).
#[test]
#[ignore]
fn round_trip_files_from_env() {
    let Ok(list) = std::env::var("OGX_DOCX_FILES") else {
        return;
    };
    for f in list.split(':').filter(|f| !f.is_empty()) {
        let bytes = std::fs::read(f).unwrap();
        let src = String::from_utf8(entry(&bytes, "word/document.xml").unwrap()).unwrap();
        let (mut ed, pkg) = editor(bytes);
        assert_eq!(write::document_xml(&ed.doc), src, "{f}");
        ed.select_all();
        let n = ed.flow.len();
        ed.toggle(Attr::Bold);
        ed.motion(Motion::DocEnd, false);
        ed.insert("\nadded");
        let (ed2, _) = editor(save(&ed, &pkg));
        assert_eq!(ed2.flow.len(), n + 1, "{f}");
        eprintln!("{f}: {n} paragraphs ok");
    }
}
