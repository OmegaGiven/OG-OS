//! Every input type converted from a tiny fixture generated here (temp
//! dirs only; no user files). Each output must parse with lopdf, have the
//! expected page count and contain the expected text.

use super::ir::{self, Block, Cell, Doc, Inline, Page, Style, Table};
use super::*;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let d = std::env::temp_dir().join(format!(
            "og-files-topdf-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }

    fn file(&self, name: &str, data: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Parses with lopdf, checks the page count and that every needle occurs
/// (whitespace-insensitive) in the extracted text. Returns the document.
fn check(pdf: &[u8], pages: usize, needles: &[&str]) -> lopdf::Document {
    // eyeballing hook: OGX_TOPDF_DEMO_DIR=/tmp/x cargo test topdf
    if let Some(dir) = std::env::var_os("OGX_TOPDF_DEMO_DIR") {
        let name = needles
            .first()
            .map_or("out".to_string(), |n| squash(n).replace('/', "_"));
        let _ = std::fs::write(Path::new(&dir).join(format!("{name}.pdf")), pdf);
    }
    let doc = lopdf::Document::load_mem(pdf).expect("output parses as PDF");
    assert_eq!(doc.get_pages().len(), pages, "page count");
    let text = squash(&structure::page_texts(pdf).unwrap().join("\n"));
    for n in needles {
        assert!(text.contains(&squash(n)), "missing {n:?} in {text:?}");
    }
    doc
}

use super::super::structure;

fn page_size(doc: &lopdf::Document, n: u32) -> (f32, f32) {
    let id = doc.get_pages()[&n];
    let page = doc.get_dictionary(id).unwrap();
    let mb = page.get(b"MediaBox").unwrap().as_array().unwrap();
    let f = |o: &lopdf::Object| o.as_float().unwrap_or_else(|_| o.as_i64().unwrap() as f32);
    (f(&mb[2]) - f(&mb[0]), f(&mb[3]) - f(&mb[1]))
}

fn image_count(doc: &lopdf::Document) -> usize {
    doc.objects
        .values()
        .filter(|o| {
            o.as_stream()
                .ok()
                .and_then(|s| s.dict.get(b"Subtype").ok())
                .and_then(|t| t.as_name().ok())
                == Some(b"Image".as_slice())
        })
        .count()
}

fn has_outline(doc: &lopdf::Document) -> bool {
    doc.catalog().is_ok_and(|c| c.get(b"Outlines").is_ok())
}

fn title_of(doc: &lopdf::Document) -> Option<String> {
    let info = doc.trailer.get(b"Info").ok()?;
    let dict = match info {
        lopdf::Object::Reference(id) => doc.get_dictionary(*id).ok()?,
        lopdf::Object::Dictionary(d) => d,
        _ => return None,
    };
    let t = dict.get(b"Title").ok()?;
    lopdf::decode_text_string(t).ok()
}

fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb(rgb));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn zip_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

// ------------------------------------------------------------- emitter

#[test]
fn literal_text_is_never_markup() {
    let nasty = r##"#set page(width: 1pt) $x^2$ *b* _i_ [x] {y} "q" \ <l> @ref = // c /* d"##;
    let mut d = Doc::new(Page::A4);
    d.blocks.push(Block::Heading(
        1,
        vec![Inline::Text(nasty.into(), Style::default())],
    ));
    d.blocks.push(Block::plain(nasty));
    d.blocks.push(Block::Code {
        text: nasty.into(),
        shaded: true,
    });
    let src = d.source();
    assert!(src.contains(&ir::lit(nasty)));
    let pdf = d.to_pdf().unwrap();
    let doc = check(&pdf, 1, &["$x^2$ *b* _i_ [x] {y}", "@ref = // c /* d"]);
    assert_eq!(page_size(&doc, 1).0.round(), 595.0);
}

#[test]
fn table_rows_are_padded_and_spans_respected() {
    let c = |s: &str| Cell::text(s, Style::default());
    let mut wide = c("wide");
    wide.colspan = 2;
    let mut tall = c("tall");
    tall.rowspan = 2;
    let t = Table {
        header: 1,
        rows: vec![
            vec![c("H1"), c("H2"), c("H3")],
            vec![tall, wide],
            vec![c("x")],
            vec![c("only")],
        ],
        borders: true,
        ..Default::default()
    };
    let mut d = Doc::new(Page::A4);
    d.blocks.push(Block::Table(t));
    let src = d.source();
    assert!(src.contains("table.header(repeat: true"));
    assert!(src.contains("rowspan: 2") && src.contains("colspan: 2"));
    let pdf = d.to_pdf().unwrap();
    check(&pdf, 1, &["H1", "tall", "wide", "only"]);
}

// ----------------------------------------------------------------- text

#[test]
fn plain_text_monospace_and_long() {
    let t = Tmp::new();
    let mut s = String::from("Title line\n\tindented with a tab\n");
    for i in 0..150 {
        s.push_str(&format!("line {i:03} ##### [x] *y*\n"));
    }
    let p = t.file("notes.txt", &s);
    let pdf = file_to_pdf(&p).unwrap();
    let doc = lopdf::Document::load_mem(&pdf).unwrap();
    assert!(doc.get_pages().len() >= 2, "150 lines span pages");
    let all = squash(&structure::page_texts(&pdf).unwrap().join(""));
    assert!(all.contains("Titleline") && all.contains("line149#####[x]*y*"));
    assert_eq!(title_of(&doc).as_deref(), Some("notes"));
    // proportional option reflows
    let prop = office::text(b"one\ntwo\n\nthree", false);
    assert_eq!(prop.blocks.len(), 2);
}

#[test]
fn markdown_full() {
    let t = Tmp::new();
    t.file("pic.png", png(40, 20, [10, 120, 200]));
    let md = "# Main Heading\n\nSome **bold** and *italic* and `code` text with a [link](https://example.org).\n\n\
              - first\n- second\n  1. nested one\n\n> quoted words\n\n```rust\nfn main() {}\n```\n\n\
              | Col A | Col B |\n|---|---|\n| a1 | b1 |\n\n![a picture](pic.png)\n\n## Sub Heading\n\n~~gone~~ end\n";
    let p = t.file("readme.md", md);
    let pdf = file_to_pdf(&p).unwrap();
    let doc = check(
        &pdf,
        1,
        &[
            "Main Heading",
            "bold",
            "first",
            "nested one",
            "quoted words",
            "fn main() {}",
            "Col A",
            "b1",
            "Sub Heading",
            "gone end",
        ],
    );
    assert_eq!(image_count(&doc), 1, "relative image embedded");
    assert!(has_outline(&doc), "headings become bookmarks");
    assert_eq!(title_of(&doc).as_deref(), Some("readme"));
}

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100"><rect width="200" height="100" fill="#3a7"/><circle cx="50" cy="50" r="30" fill="#fff"/></svg>"##;

#[test]
fn html_semantics_css_images_tables() {
    let t = Tmp::new();
    t.file("img.png", png(30, 30, [200, 0, 0]));
    let data = format!(
        "data:image/png;base64,{}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            png(8, 8, [0, 200, 0])
        )
    );
    let html = format!(
        r#"<!doctype html><html><head><title>My Page</title><style>p {{ color: red }}</style><script>document.write("NOPE")</script></head>
<body><h1>Welcome Home</h1>
<p style="text-align:center;color:#336699">Centred <b>bold</b> <i>it</i> <u>under</u> <s>struck</s> H<sub>2</sub>O x<sup>2</sup></p>
<div style="display:none">HIDDEN TEXT</div>
<ul><li>apple</li><li>banana<ol start="3"><li>three</li></ol></li></ul>
<table border="1"><thead><tr><th>Name</th><th>Qty</th></tr></thead>
<tbody><tr><td colspan="2">merged cell</td></tr><tr><td rowspan="2">r</td><td>1</td></tr><tr><td>2</td></tr></tbody></table>
<pre>  pre   formatted</pre><blockquote>wise words</blockquote>
<p><img src="img.png" width="30"> <img src="{data}"> <img src="missing.png" alt="gone image"></p>
{SVG}
<dl><dt>Term</dt><dd>Definition</dd></dl><hr><p>a&amp;b &lt;tag&gt; &#169;</p></body></html>"#
    );
    let p = t.file("page.html", html);
    let pdf = file_to_pdf(&p).unwrap();
    let doc = check(
        &pdf,
        1,
        &[
            "Welcome Home",
            "Centred",
            "apple",
            "three",
            "Name",
            "merged cell",
            "pre   formatted",
            "wise words",
            "[gone image]",
            "Term",
            "Definition",
            "a&b <tag> ©",
        ],
    );
    let text = structure::page_texts(&pdf).unwrap().join("");
    assert!(!text.contains("HIDDEN") && !text.contains("NOPE"));
    assert_eq!(title_of(&doc).as_deref(), Some("My Page"));
    assert!(
        image_count(&doc) >= 2,
        "file + data: images (svg is vector)"
    );
}

#[test]
fn svg_page_sized_to_drawing() {
    let t = Tmp::new();
    let p = t.file("draw.svg", SVG);
    let pdf = file_to_pdf(&p).unwrap();
    let doc = check(&pdf, 1, &[]);
    let (w, h) = page_size(&doc, 1);
    assert_eq!((w.round(), h.round()), (150.0, 75.0));
    assert_eq!(image_count(&doc), 0, "vector, not rasterized");
    assert!(file_to_pdf(&t.file("bad.svg", "<svg nope")).is_err());
}

// --------------------------------------------------------------- office

fn docx_fixture() -> Vec<u8> {
    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture""#;
    let body = r#"
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Report Heading</w:t></w:r></w:p>
<w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:rPr><w:b/><w:color w:val="C00000"/><w:sz w:val="28"/></w:rPr><w:t xml:space="preserve">Bold red </w:t></w:r><w:r><w:rPr><w:i/><w:u w:val="single"/><w:strike/></w:rPr><w:t>styled run</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>numbered alpha</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>numbered beta</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>a bullet</w:t></w:r></w:p>
<w:p><w:hyperlink r:id="rId5"><w:r><w:t>web link</w:t></w:r></w:hyperlink></w:p>
<w:tbl><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
<w:tr><w:tc><w:p><w:r><w:t>Cell A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Cell B1</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Spanning row</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:t xml:space="preserve">Picture: </w:t></w:r><w:r><w:drawing><wp:inline><wp:extent cx="952500" cy="476250"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:p><w:r><w:br w:type="page"/></w:r><w:r><w:t>After the break</w:t></w:r></w:p>
<w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr>"#;
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}</w:body></w:document>"#
    );
    let styles = r#"<?xml version="1.0" encoding="UTF-8"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style></w:styles>"#;
    let numbering = r#"<?xml version="1.0" encoding="UTF-8"?><w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Quarterly &amp; Annual</dc:title></cp:coreProperties>"#;
    zip_of(&[
        ("[Content_Types].xml", b"<Types/>".to_vec()),
        ("_rels/.rels", root_rels.into()),
        ("word/document.xml", doc.into_bytes()),
        ("word/styles.xml", styles.into()),
        ("word/numbering.xml", numbering.into()),
        ("word/_rels/document.xml.rels", rels.into()),
        ("word/media/image1.png", png(10, 5, [0, 0, 255])),
        ("docProps/core.xml", core.into()),
    ])
}

#[test]
fn docx_through_the_editor_model() {
    let t = Tmp::new();
    let p = t.file("report.docx", docx_fixture());
    let d = docx::convert(std::fs::read(&p).unwrap()).unwrap();
    let src = d.source();
    assert!(src.contains("#heading(level: 1)"), "{src}");
    assert!(
        src.contains("\"1.\"") && src.contains("\"2.\""),
        "Word numbering"
    );
    assert!(src.contains("weight: \"bold\"") && src.contains("fill: rgb(192, 0, 0)"));
    assert!(src.contains("#underline[#strike[") && src.contains("style: \"italic\""));
    assert!(src.contains("#align(center)"));
    assert!(src.contains("colspan: 2"));
    assert!(src.contains("#link(\"https://example.com/\")"));
    let pdf = file_to_pdf(&p).unwrap();
    let doc = check(
        &pdf,
        2,
        &[
            "Report Heading",
            "Bold red",
            "styled run",
            "1.",
            "numbered alpha",
            "numbered beta",
            "a bullet",
            "web link",
            "Cell A1",
            "Spanning row",
            "After the break",
        ],
    );
    let (w, h) = page_size(&doc, 1);
    assert_eq!((w.round(), h.round()), (792.0, 612.0), "sectPr page size");
    assert_eq!(image_count(&doc), 1);
    assert!(has_outline(&doc));
    assert_eq!(title_of(&doc).as_deref(), Some("Quarterly & Annual"));
}

#[test]
fn odt_and_rtf() {
    let t = Tmp::new();
    let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
<office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style></office:automatic-styles>
<office:body><office:text>
<text:h text:outline-level="1">Odt Chapter</text:h>
<text:p>Some <text:span text:style-name="T1">strong</text:span> words</text:p>
<table:table><table:table-row><table:table-cell><text:p>t11</text:p></table:table-cell><table:table-cell><text:p>t12</text:p></table:table-cell></table:table-row></table:table>
</office:text></office:body></office:document-content>"#;
    let odt = zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text".to_vec(),
        ),
        ("content.xml", content.into()),
    ]);
    let pdf = file_to_pdf(&t.file("a.odt", odt)).unwrap();
    let doc = check(&pdf, 1, &["Odt Chapter", "Some strong words", "t11", "t12"]);
    assert!(has_outline(&doc));

    let rtf =
        r"{\rtf1\ansi{\fonttbl{\f0 Times;}}\f0\fs24 Hello {\b bold rtf} world\par Second para\par}";
    let pdf = file_to_pdf(&t.file("b.rtf", rtf)).unwrap();
    check(&pdf, 1, &["Hello bold rtf world", "Second para"]);
}

#[test]
fn spreadsheets_csv_xlsx_ods() {
    let t = Tmp::new();
    let pdf = file_to_pdf(&t.file(
        "data.csv",
        "Name,Amount,Note\nalpha,12.5,first\nbeta,7,second\n",
    ))
    .unwrap();
    let doc = check(&pdf, 1, &["Name", "Amount", "alpha", "12.5", "second"]);
    let (w, h) = page_size(&doc, 1);
    assert!(w < h, "narrow table stays portrait");

    let pdf = file_to_pdf(&t.file("tabs.tsv", "k\tv\nx\t1\n")).unwrap();
    check(&pdf, 1, &["k", "x"]);

    // two sheets, one of them wide → landscape, one section each
    let mut wb = rust_xlsxwriter::Workbook::new();
    let ws = wb.add_worksheet();
    ws.set_name("Summary").unwrap();
    ws.write_string(0, 0, "Region").unwrap();
    ws.write_string(0, 1, "Total").unwrap();
    ws.write_string(1, 0, "North").unwrap();
    ws.write_number(1, 1, 42.0).unwrap();
    let ws2 = wb.add_worksheet();
    ws2.set_name("Wide").unwrap();
    for c in 0..14u16 {
        ws2.write_string(0, c, format!("Column header {c}"))
            .unwrap();
        ws2.write_string(1, c, format!("value {c}")).unwrap();
    }
    let p = t.file("book.xlsx", wb.save_to_buffer().unwrap());
    let pdf = file_to_pdf(&p).unwrap();
    let doc = lopdf::Document::load_mem(&pdf).unwrap();
    assert!(doc.get_pages().len() >= 2);
    let text = squash(&structure::page_texts(&pdf).unwrap().join(""));
    for n in [
        "Summary",
        "Region",
        "North",
        "42",
        "Wide",
        "Columnheader13",
        "value0",
    ] {
        assert!(text.contains(n), "{n} in {text}");
    }
    let (w, h) = page_size(&doc, 1);
    assert!(w > h, "wide sheet → landscape");
    assert!(has_outline(&doc), "sheet names in the outline");

    // .ods through the same loaders
    let mut wb = spreadsheet_ods::WorkBook::new_empty();
    let mut sh = spreadsheet_ods::Sheet::new("Ods Sheet");
    sh.set_value(0, 0, "odscell");
    sh.set_value(1, 0, 5.0);
    wb.push_sheet(sh);
    let p = t.file(
        "c.ods",
        spreadsheet_ods::write_ods_buf(&mut wb, Vec::new()).unwrap(),
    );
    check(&file_to_pdf(&p).unwrap(), 1, &["Ods Sheet", "odscell", "5"]);
}

fn pptx_fixture() -> Vec<u8> {
    let ns = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
    let pres = format!(
        r#"<?xml version="1.0"?><p:presentation {ns}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst><p:sldSz cx="9144000" cy="5143500"/></p:presentation>"#
    );
    let pres_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/></Relationships>"#;
    let layout = format!(
        r#"<?xml version="1.0"?><p:sldLayout {ns}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="228600"/><a:ext cx="8229600" cy="857250"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"#
    );
    let slide1 = format!(
        r#"<?xml version="1.0"?><p:sld {ns}><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFF2CC"/></a:solidFill></p:bgPr></p:bg><p:spTree>
<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:rPr lang="en-US" b="1"/><a:t>Deck Title Slide</a:t></a:r></a:p></p:txBody></p:sp>
<p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="1371600"/><a:ext cx="4000000" cy="2000000"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:p><a:r><a:rPr sz="2400" i="1"><a:solidFill><a:srgbClr val="1F4E79"/></a:solidFill></a:rPr><a:t>First point</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:t>Sub point</a:t></a:r></a:p></p:txBody></p:sp>
<p:pic><p:nvPicPr><p:cNvPr id="4" name="Pic"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="5000000" y="1371600"/><a:ext cx="2000000" cy="1000000"/></a:xfrm></p:spPr></p:pic>
</p:spTree></p:cSld></p:sld>"#
    );
    let slide2 = format!(
        r#"<?xml version="1.0"?><p:sld {ns}><p:cSld><p:spTree>
<p:grpSp><p:grpSpPr><a:xfrm><a:off x="1000000" y="1000000"/><a:ext cx="2000000" cy="1000000"/><a:chOff x="0" y="0"/><a:chExt cx="2000000" cy="1000000"/></a:xfrm></p:grpSpPr>
<p:sp><p:nvSpPr><p:cNvPr id="5" name="Box"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="500000"/></a:xfrm><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill></p:spPr><p:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:t>Grouped box</a:t></a:r></a:p></p:txBody></p:sp></p:grpSp>
<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="6" name="Table"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="1000000" y="3000000"/><a:ext cx="4000000" cy="600000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblGrid><a:gridCol w="2000000"/><a:gridCol w="2000000"/></a:tblGrid><a:tr h="300000"><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>tcell1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>tcell2</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>
</p:spTree></p:cSld></p:sld>"#
    );
    let s1_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#;
    zip_of(&[
        ("ppt/presentation.xml", pres.into_bytes()),
        ("ppt/_rels/presentation.xml.rels", pres_rels.into()),
        ("ppt/slides/slide1.xml", slide1.into_bytes()),
        ("ppt/slides/slide2.xml", slide2.into_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", s1_rels.into()),
        ("ppt/slideLayouts/slideLayout1.xml", layout.into_bytes()),
        ("ppt/media/image1.png", png(20, 10, [255, 128, 0])),
    ])
}

#[test]
fn pptx_slides_positioned() {
    let t = Tmp::new();
    let p = t.file("deck.pptx", pptx_fixture());
    let d = slides::pptx(&std::fs::read(&p).unwrap()).unwrap();
    let src = d.source();
    // title placeholder got the layout's position (36pt, 18pt)
    assert!(src.contains("dx: 36.00pt, dy: 18.00pt"), "{src}");
    // grouped shape at group offset (1000000 EMU = 78.74pt)
    assert!(src.contains("dx: 78.74pt, dy: 78.74pt"));
    assert!(src.contains("fill: rgb(255, 242, 204)"), "slide background");
    assert!(src.contains("size: 24.00pt"));
    let pdf = file_to_pdf(&p).unwrap();
    let doc = check(
        &pdf,
        2,
        &[
            "Deck Title Slide",
            "First point",
            "Sub point",
            "Grouped box",
            "tcell1",
            "tcell2",
        ],
    );
    let (w, h) = page_size(&doc, 2);
    assert_eq!((w.round(), h.round()), (720.0, 405.0));
    assert_eq!(image_count(&doc), 1);
    assert_eq!(title_of(&doc).as_deref(), Some("Deck Title Slide"));
}

#[test]
fn odp_slides() {
    let t = Tmp::new();
    let content = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink">
<office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold" fo:font-size="30pt"/></style:style></office:automatic-styles>
<office:body><office:presentation>
<draw:page draw:name="p1"><draw:frame presentation:class="title" svg:x="2cm" svg:y="1cm" svg:width="20cm" svg:height="3cm"><draw:text-box><text:p><text:span text:style-name="T1">Odp Title</text:span></text:p></draw:text-box></draw:frame>
<draw:frame presentation:class="outline" svg:x="2cm" svg:y="5cm" svg:width="20cm" svg:height="8cm"><draw:text-box><text:list><text:list-item><text:p>odp bullet</text:p></text:list-item></text:list></draw:text-box></draw:frame></draw:page>
<draw:page draw:name="p2"><draw:frame svg:x="1in" svg:y="1in" svg:width="2in" svg:height="1in"><draw:image xlink:href="Pictures/a.png"/></draw:frame><draw:custom-shape svg:x="10cm" svg:y="10cm" svg:width="5cm" svg:height="2cm"><text:p>shape words</text:p></draw:custom-shape></draw:page>
</office:presentation></office:body></office:document-content>"#;
    let styles = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"><office:automatic-styles><style:page-layout style:name="PM1"><style:page-layout-properties fo:page-width="28cm" fo:page-height="15.75cm"/></style:page-layout></office:automatic-styles></office:document-styles>"#;
    let odp = zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation".to_vec(),
        ),
        ("content.xml", content.into()),
        ("styles.xml", styles.into()),
        ("Pictures/a.png", png(16, 8, [9, 9, 200])),
    ]);
    let pdf = file_to_pdf(&t.file("talk.odp", odp)).unwrap();
    let doc = check(&pdf, 2, &["Odp Title", "odp bullet", "shape words"]);
    let (w, h) = page_size(&doc, 1);
    assert_eq!((w.round(), h.round()), (794.0, 446.0));
    assert_eq!(image_count(&doc), 1);
    assert_eq!(title_of(&doc).as_deref(), Some("Odp Title"));
}

// ----------------------------------------------------------------- mail

#[test]
fn eml_headers_body_attachments() {
    let t = Tmp::new();
    let img = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        png(6, 6, [1, 2, 3]),
    );
    let att = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        b"report contents",
    );
    let eml = format!(
        "From: Alice Example <alice@example.org>\r\nTo: bob@example.org\r\nSubject: Status update\r\nDate: Tue, 1 Sep 2026 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"XX\"\r\n\r\n\
--XX\r\nContent-Type: multipart/related; boundary=\"YY\"\r\n\r\n\
--YY\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Hello <b>Bob</b>, see the chart:</p><img src=\"cid:chart1\"></body></html>\r\n\
--YY\r\nContent-Type: image/png\r\nContent-ID: <chart1>\r\nContent-Transfer-Encoding: base64\r\n\r\n{img}\r\n--YY--\r\n\
--XX\r\nContent-Type: text/plain; name=\"report.txt\"\r\nContent-Disposition: attachment; filename=\"report.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{att}\r\n--XX--\r\n"
    );
    let pdf = file_to_pdf(&t.file("mail.eml", eml)).unwrap();
    let doc = check(
        &pdf,
        1,
        &[
            "Status update",
            "From",
            "Alice Example <alice@example.org>",
            "bob@example.org",
            "Hello Bob",
            "Attachments",
            "report.txt",
        ],
    );
    assert_eq!(title_of(&doc).as_deref(), Some("Status update"));
    assert_eq!(image_count(&doc), 1, "cid: image inline, not repeated");

    let plain = "From: x@y.z\r\nSubject: Plain one\r\n\r\nLine one\r\nLine two\r\n\r\nNew para\r\n";
    let pdf = file_to_pdf(&t.file("p.eml", plain)).unwrap();
    check(&pdf, 1, &["Plain one", "Line one", "Line two", "New para"]);
}

// --------------------------------------------------------------- ebooks

#[test]
fn epub_chapters_images_toc() {
    let t = Tmp::new();
    let container = r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
    let opf = r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>The Test Book</dc:title></metadata>
<manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/><item id="c2" href="text/ch%202.xhtml" media-type="application/xhtml+xml"/><item id="i1" href="img/p.png" media-type="image/png"/></manifest>
<spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#;
    let nav = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><nav><ol><li><a href="text/ch1.xhtml">One</a></li><li><a href="text/ch%202.xhtml#start">Second Part</a></li></ol></nav></body></html>"#;
    let ch1 = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>c1</title></head><body><h1>Chapter One</h1><p>It was a <em>dark</em> night.</p><p><img src="../img/p.png" alt="pic"/></p></body></html>"#;
    let ch2 = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><p id="start">No heading here, just prose.</p></body></html>"#;
    let epub = zip_of(&[
        ("mimetype", b"application/epub+zip".to_vec()),
        ("META-INF/container.xml", container.into()),
        ("OEBPS/content.opf", opf.into()),
        ("OEBPS/nav.xhtml", nav.into()),
        ("OEBPS/text/ch1.xhtml", ch1.into()),
        ("OEBPS/text/ch 2.xhtml", ch2.into()),
        ("OEBPS/img/p.png", png(12, 12, [50, 50, 50])),
    ]);
    let pdf = file_to_pdf(&t.file("book.epub", epub)).unwrap();
    let doc = check(
        &pdf,
        2,
        &[
            "Chapter One",
            "It was a dark night.",
            "Second Part",
            "No heading here",
        ],
    );
    assert_eq!(image_count(&doc), 1);
    assert!(has_outline(&doc));
    assert_eq!(title_of(&doc).as_deref(), Some("The Test Book"));
}

/// A minimal uncompressed MOBI 6 file: record 0 (PalmDOC + MOBI header +
/// title), one text record, one image record.
fn mobi_fixture(text: &str, image: &[u8]) -> Vec<u8> {
    let title = b"Mobi Fixture Title";
    let mut rec0 = Vec::new();
    // PalmDOC header: no compression, text length, 1 record, 4096, no DRM
    rec0.extend(1u16.to_be_bytes());
    rec0.extend(0u16.to_be_bytes());
    rec0.extend((text.len() as u32).to_be_bytes());
    rec0.extend(1u16.to_be_bytes());
    rec0.extend(4096u16.to_be_bytes());
    rec0.extend(0u16.to_be_bytes());
    rec0.extend(0u16.to_be_bytes());
    let mut m = vec![0u8; 232];
    let put = |m: &mut Vec<u8>, at: usize, v: u32| m[at..at + 4].copy_from_slice(&v.to_be_bytes());
    m[0..4].copy_from_slice(b"MOBI");
    put(&mut m, 4, 232);
    put(&mut m, 8, 2); // book
    put(&mut m, 12, 65001); // UTF-8
    put(&mut m, 64, 2); // first non-book record
    put(&mut m, 68, 16 + 232); // full name offset (in record 0)
    put(&mut m, 72, title.len() as u32);
    put(&mut m, 92, 2); // first image record
    put(&mut m, 112, 0); // no EXTH
    m[176..178].copy_from_slice(&1u16.to_be_bytes()); // first content record
    m[178..180].copy_from_slice(&1u16.to_be_bytes());
    rec0.extend(m);
    rec0.extend(title);
    rec0.extend([0, 0]);
    let records = [rec0, text.as_bytes().to_vec(), image.to_vec()];
    let mut out = Vec::new();
    let mut name = [0u8; 32];
    name[..4].copy_from_slice(b"mobi");
    out.extend(name);
    out.extend([0u8; 4]); // attributes, version
    out.extend([0u8; 24]); // dates, modnum, appinfo, sortinfo
    out.extend(b"BOOKMOBI");
    out.extend([0u8; 8]);
    out.extend((records.len() as u16).to_be_bytes());
    let mut offset = 78 + 8 * records.len() + 2;
    for (i, r) in records.iter().enumerate() {
        out.extend((offset as u32).to_be_bytes());
        out.extend((i as u32 * 2).to_be_bytes());
        offset += r.len();
    }
    out.extend([0, 0]);
    for r in &records {
        out.extend(r);
    }
    out
}

#[test]
fn mobi_text_and_images() {
    let t = Tmp::new();
    let text = r#"<html><head><guide></guide></head><body><h1>Mobi Chapter</h1><p>Hello from <b>mobi</b>.</p><mbp:pagebreak/><p><img recindex="00001"/></p></body></html>"#;
    let p = t.file("b.mobi", mobi_fixture(text, &png(9, 9, [7, 7, 7])));
    let pdf = file_to_pdf(&p).unwrap();
    let doc = check(&pdf, 1, &["Mobi Chapter", "Hello from mobi."]);
    assert_eq!(image_count(&doc), 1);
    assert_eq!(title_of(&doc).as_deref(), Some("Mobi Fixture Title"));
    assert!(file_to_pdf(&t.file("x.azw3", b"not a book")).is_err());
}

#[test]
fn fb2_sections_and_binaries() {
    let t = Tmp::new();
    let img = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        png(5, 5, [100, 0, 100]),
    );
    let fb2 = format!(
        r##"<?xml version="1.0" encoding="utf-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><book-title>Fb Two Book</book-title></title-info></description>
<body><section><title><p>Part One</p></title><p>Plain <emphasis>em</emphasis> and <strong>strong</strong> &amp; more.</p><image l:href="#pic1"/>
<poem><stanza><v>Roses are red</v></stanza></poem><epigraph><p>Said someone</p></epigraph></section></body>
<binary id="pic1" content-type="image/png">{img}</binary></FictionBook>"##
    );
    let pdf = file_to_pdf(&t.file("b.fb2", fb2)).unwrap();
    let doc = check(
        &pdf,
        1,
        &[
            "Part One",
            "Plain em and strong & more.",
            "Roses are red",
            "Said someone",
        ],
    );
    assert_eq!(image_count(&doc), 1);
    assert!(has_outline(&doc));
    assert_eq!(title_of(&doc).as_deref(), Some("Fb Two Book"));
}

// --------------------------------------------------------------- comics

/// Pages of distinct widths, stored out of order: "p10" must come last.
fn comic_pages() -> Vec<(String, Vec<u8>)> {
    vec![
        ("pages/p10.png".into(), png(30, 10, [1, 1, 1])),
        ("pages/p2.png".into(), png(20, 10, [2, 2, 2])),
        ("pages/p1.png".into(), png(10, 10, [3, 3, 3])),
        ("notes.txt".into(), b"skip me".to_vec()),
    ]
}

fn check_comic(pdf: &[u8]) {
    let doc = check(pdf, 3, &[]);
    let widths: Vec<f32> = (1..=3).map(|n| page_size(&doc, n).0.round()).collect();
    assert_eq!(widths, vec![10.0, 20.0, 30.0], "natural page order");
}

#[test]
fn comics_cbz_cbt_cb7_cbr() {
    let t = Tmp::new();
    let pages = comic_pages();
    // cbz
    let files: Vec<(&str, Vec<u8>)> = pages.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    check_comic(&file_to_pdf(&t.file("c.cbz", zip_of(&files))).unwrap());
    // cbt
    let mut tb = tar::Builder::new(Vec::new());
    for (n, b) in &pages {
        let mut h = tar::Header::new_gnu();
        h.set_size(b.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tb.append_data(&mut h, n, b.as_slice()).unwrap();
    }
    check_comic(&file_to_pdf(&t.file("c.cbt", tb.into_inner().unwrap())).unwrap());
    // cb7
    let mut w = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    for (n, b) in &pages {
        w.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file(n),
            Some(Cursor::new(b.clone())),
        )
        .unwrap();
    }
    let cb7 = w.finish().unwrap().into_inner();
    check_comic(&file_to_pdf(&t.file("c.cb7", cb7)).unwrap());
    // cbr (RAR 5, solid, built by the same pure-Rust crate)
    let mut rb = rars::Builder::new(rars::ArchiveVersion::Rar50).solid(true);
    for (n, b) in &pages {
        rb.add_bytes(n.as_bytes().to_vec(), b.clone(), None, None)
            .unwrap();
    }
    check_comic(&file_to_pdf(&t.file("c.cbr", rb.to_bytes().unwrap())).unwrap());
    // garbage
    assert!(file_to_pdf(&t.file("x.cbr", b"Rar! nope")).is_err());
    assert!(file_to_pdf(&t.file("e.cbz", zip_of(&[("a.txt", b"x".to_vec())]))).is_err());
}

// ------------------------------------------------------------------ web

/// Serves `routes` (path → (content type, body)) on 127.0.0.1 for a few
/// requests; no network access needed.
fn serve(routes: Vec<(&'static str, &'static str, Vec<u8>)>) -> String {
    use std::io::{BufRead, BufReader};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().take(8) {
            let Ok(mut s) = s else { continue };
            let mut line = String::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            if r.read_line(&mut line).is_err() {
                continue;
            }
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            match routes.iter().find(|(p, _, _)| *p == path) {
                Some((_, ct, body)) => {
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(body);
                }
                None => {
                    let _ = write!(
                        s,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                }
            }
        }
    });
    format!("http://{addr}")
}

#[test]
fn url_to_pdf_fetches_page_and_images() {
    let page = br#"<html><head><title>Served Page</title></head><body><h1>From the web</h1><p>Linked <a href="/other">relative</a>.</p><img src="/img.png"><img src="https://invalid.invalid/x.png"></body></html>"#;
    let base = serve(vec![
        ("/", "text/html; charset=utf-8", page.to_vec()),
        ("/img.png", "image/png", png(10, 10, [0, 0, 0])),
    ]);
    let pdf = url_to_pdf(&format!("{base}/")).unwrap();
    let doc = check(&pdf, 1, &["From the web", "Linked relative."]);
    assert_eq!(image_count(&doc), 1);
    assert_eq!(title_of(&doc).as_deref(), Some("Served Page"));
    assert!(url_to_pdf("ftp://example.org/").is_err());
    assert!(url_to_pdf("not a url").is_err());
    assert!(url_to_pdf(&format!("{base}/missing"))
        .unwrap_err()
        .contains("404"));
}

#[test]
fn unsupported_and_legacy_types_say_so() {
    let t = Tmp::new();
    assert!(file_to_pdf(&t.file("old.doc", b"\xd0\xcf\x11\xe0"))
        .unwrap_err()
        .contains(".docx"));
    assert!(!supported(Path::new("x.exe")));
    assert!(supported(Path::new("X.DOCX")));
}
