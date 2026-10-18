use super::super::extract::tests::fixture;
use super::*;
use std::io::Read;

fn well_formed(name: &str, xml: &str) {
    let mut r = quick_xml::Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("{name}: not well-formed at {}: {e}", r.buffer_position()),
        }
    }
}

fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>, zip::CompressionMethod)> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().to_string(), b, f.compression())
        })
        .collect()
}

/// every .xml/.rels/.opf/.ncx/.xhtml part parses
fn check_parts(name: &str, parts: &[(String, Vec<u8>, zip::CompressionMethod)]) {
    for (n, b, _) in parts {
        if [".xml", ".rels", ".opf", ".ncx", ".xhtml"]
            .iter()
            .any(|e| n.ends_with(e))
        {
            well_formed(&format!("{name}:{n}"), std::str::from_utf8(b).unwrap());
        }
    }
}

fn part<'a>(parts: &'a [(String, Vec<u8>, zip::CompressionMethod)], n: &str) -> &'a str {
    let p = parts
        .iter()
        .find(|p| p.0 == n)
        .unwrap_or_else(|| panic!("{n} missing"));
    std::str::from_utf8(&p.1).unwrap()
}

fn run(t: Target) -> Vec<(String, Vec<u8>)> {
    convert(&fixture(), t, "report").unwrap_or_else(|e| panic!("{t}: {e}"))
}

fn single(t: Target) -> Vec<u8> {
    let out = run(t);
    assert_eq!(out[0].0, format!("converted.{}", t.ext()));
    out.into_iter().next().unwrap().1
}

#[test]
fn text_reading_order_and_pages() {
    let s = String::from_utf8(single(Target::Text)).unwrap();
    assert_eq!(s.matches('\u{c}').count(), 2, "{s}");
    let left7 = s.find("Left column sentence number 7").unwrap();
    let right0 = s.find("Right column sentence number 0").unwrap();
    assert!(left7 < right0, "columns interleaved:\n{s}");
    assert!(s.contains("• First item\n• Second item"), "{s}");
    assert!(s.contains("Apples\t3\t1.20"), "{s}");
    assert!(s.contains("Gamma\t333\t3,330.00"), "{s}");
    assert!(s.contains("Hi fi"), "CID/ToUnicode text: {s}");
    // the paragraph is reflowed into one line
    assert!(s.contains("explains the results of the quarter"), "{s}");
}

#[test]
fn docx_parts_and_reopens() {
    let b = single(Target::Docx);
    let parts = unzip(&b);
    check_parts("docx", &parts);
    let doc = part(&parts, "word/document.xml");
    assert!(doc.contains("<w:pStyle w:val=\"Heading1\"/>"));
    assert!(doc.contains("Quarterly Report"));
    assert!(doc.contains("<w:tbl>") && doc.contains("TableGrid"));
    assert!(doc.contains("<w:numPr>"));
    assert!(doc.contains("w:type=\"page\""));
    assert!(doc.contains("r:embed=\"rIdImg1\""));
    assert!(parts.iter().any(|p| p.0 == "word/media/image1.png"));
    assert!(part(&parts, "word/numbering.xml").contains("w:numFmt w:val=\"bullet\""));
    // the app's own docx editor opens it
    let pkg = crate::viewers::docx::parse::open_package(b).expect("og-files docx parser");
    assert!(!pkg.doc.body.is_empty());
}

#[test]
fn odt_parts() {
    let parts = unzip(&single(Target::Odt));
    assert_eq!(parts[0].0, "mimetype");
    assert_eq!(parts[0].2, zip::CompressionMethod::Stored);
    check_parts("odt", &parts);
    let c = part(&parts, "content.xml");
    assert!(c.contains("text:outline-level=\"1\""));
    assert!(c.contains("<table:table "));
    assert!(c.contains("<text:list "));
    assert!(c.contains("fo:break-before=\"page\""));
    assert!(c.contains("Pictures/image1.png"));
    assert!(part(&parts, "META-INF/manifest.xml").contains("Pictures/image1.png"));
}

#[test]
fn pptx_parts() {
    let parts = unzip(&single(Target::Pptx));
    check_parts("pptx", &parts);
    for n in 1..=3 {
        let s = part(&parts, &format!("ppt/slides/slide{n}.xml"));
        assert!(s.contains("<p:pic>"), "slide {n} has no page picture");
        assert!(parts
            .iter()
            .any(|p| p.0 == format!("ppt/media/page{n}.jpg")));
    }
    let s1 = part(&parts, "ppt/slides/slide1.xml");
    assert!(s1.contains("Quarterly Report"));
    assert!(s1.contains("<a:t>Apples</a:t>"));
    // text was removed from the background, so the boxes are visible
    assert!(!s1.contains("<a:alpha val=\"0\"/>"));
    assert!(part(&parts, "ppt/presentation.xml").contains("<p:sldId id=\"258\""));
}

#[test]
fn xlsx_tables_as_sheets() {
    use calamine::{Data, Reader, Xlsx};
    let b = single(Target::Xlsx);
    let mut wb: Xlsx<_> = Xlsx::new(Cursor::new(b)).expect("calamine opens it");
    let names = wb.sheet_names().to_vec();
    assert_eq!(names, vec!["Page 1 Table 1", "Page 2 Table 1"]);
    let r = wb.worksheet_range("Page 1 Table 1").unwrap();
    assert_eq!(r.get_value((1, 0)), Some(&Data::String("Apples".into())));
    assert_eq!(r.get_value((2, 1)), Some(&Data::Float(12.0)));
    let r = wb.worksheet_range("Page 2 Table 1").unwrap();
    assert_eq!(r.get_value((3, 2)), Some(&Data::Float(3330.0)));
}

#[test]
fn csv_one_file_per_table() {
    let out = run(Target::Csv);
    let names: Vec<&str> = out.iter().map(|f| f.0.as_str()).collect();
    assert_eq!(names, vec!["page01-table1.csv", "page02-table1.csv"]);
    let s = String::from_utf8(out[1].1.clone()).unwrap();
    assert!(s.starts_with("Name,Count,Total\n"), "{s}");
    assert!(s.contains("Gamma,333,\"3,330.00\""), "{s}");
}

#[test]
fn markdown_with_image_files() {
    let out = run(Target::Markdown);
    let md = String::from_utf8(out[0].1.clone()).unwrap();
    assert!(md.starts_with("# Quarterly Report\n"), "{md}");
    assert!(
        md.contains("| Item | Qty | Price |\n| --- | --- | --- |"),
        "{md}"
    );
    assert!(md.contains("- First item\n- Second item"), "{md}");
    assert!(md.contains("![](report-converted-img-01.png)"), "{md}");
    assert_eq!(out[1].0, "converted-img-01.png");
    assert!(out[1].1.starts_with(b"\x89PNG"));
}

#[test]
fn html_semantic_self_contained() {
    let s = String::from_utf8(single(Target::Html)).unwrap();
    assert!(s.contains("<title>Quarterly Report</title>"));
    assert!(s.contains("<h1>Quarterly Report</h1>"));
    assert!(
        s.contains("<ul><li>First item</li>\n<li>Second item</li></ul>"),
        "{s}"
    );
    assert!(s.contains("<th>Item</th>"), "{s}");
    assert!(s.contains("src=\"data:image/png;base64,"));
    // the body is XHTML-clean
    let body = &s[s.find("<body>").unwrap()..s.find("</html>").unwrap()];
    well_formed("html body", body);
}

#[test]
fn rtf_balanced() {
    let s = String::from_utf8(single(Target::Rtf)).unwrap();
    assert!(s.starts_with("{\\rtf1"));
    let mut depth = 0i32;
    let mut prev = ' ';
    for c in s.chars() {
        if prev != '\\' {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        assert!(depth >= 0);
        prev = if prev == '\\' && c == '\\' { ' ' } else { c };
    }
    assert_eq!(depth, 0);
    assert!(s.contains("Quarterly Report"));
    assert!(s.contains("\\trowd") && s.contains("\\pngblip") && s.contains("\\page"));
}

#[test]
fn xml_dump() {
    let s = String::from_utf8(single(Target::Xml)).unwrap();
    well_formed("xml", &s);
    assert!(s.contains("<table rows=\"4\" columns=\"3\" ruled=\"true\""));
    assert!(s.contains("<heading level=\"1\""));
    assert!(s.contains("<list-item ordered=\"false\" marker=\"•\""));
    assert!(s.contains("font=\"Arial\" size=\"20\""));
    assert!(s.contains("bold=\"true\""));
    assert!(s.contains("<image id=\"1\" format=\"png\""));
}

#[test]
fn epub_structure() {
    let parts = unzip(&single(Target::Epub));
    assert_eq!(parts[0].0, "mimetype");
    assert_eq!(parts[0].1, b"application/epub+zip");
    assert_eq!(parts[0].2, zip::CompressionMethod::Stored);
    check_parts("epub", &parts);
    let opf = part(&parts, "OEBPS/content.opf");
    assert!(opf.contains("properties=\"nav\""));
    assert!(opf.contains("dcterms:modified"));
    // one heading-1 → chapters per page
    assert_eq!(
        parts
            .iter()
            .filter(|p| p.0.ends_with(".xhtml") && p.0.contains("chapter"))
            .count(),
        3
    );
    assert!(part(&parts, "OEBPS/nav.xhtml").contains("Quarterly Report"));
    assert!(parts.iter().any(|p| p.0 == "OEBPS/images/img1.png"));
}

#[test]
fn opened_by_og_files_readers() {
    // the app's own document viewer reads what we write
    let dir = std::env::temp_dir().join(format!("ogx-pdfconv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for t in [Target::Docx, Target::Odt, Target::Rtf, Target::Epub] {
        let p = dir.join(format!("x.{}", t.ext()));
        std::fs::write(&p, single(t)).unwrap();
        let d = crate::viewers::doc::debug_load(&p).unwrap_or_else(|e| panic!("{t}: {e}"));
        assert!(d.contains("Quarterly Report"), "{t}: {d}");
        assert!(d.contains("Apples"), "{t}: {d}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn errors_not_panics() {
    assert!(convert(b"not a pdf", Target::Docx, "x").is_err());
    // a page without text
    let mut doc = lopdf::Document::with_version("1.5");
    let pages = doc.new_object_id();
    let c = doc.add_object(lopdf::Stream::new(
        lopdf::dictionary! {},
        b"0 0 m 10 10 l S".to_vec(),
    ));
    let page = doc.add_object(lopdf::dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => c, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()] });
    doc.objects.insert(
        pages,
        lopdf::Object::Dictionary(
            lopdf::dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let cat = doc.add_object(lopdf::dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", cat);
    let mut b = Vec::new();
    doc.save_to(&mut b).unwrap();
    let e = convert(&b, Target::Text, "x").unwrap_err();
    assert!(e.contains("no text"), "{e}");
    // pptx still works (picture-only slides)
    assert!(convert(&b, Target::Pptx, "x").is_ok());
}

#[test]
fn numbers_for_spreadsheets() {
    use super::sheet::as_number;
    assert_eq!(as_number("1,234.50"), Some(1234.5));
    assert_eq!(as_number("-12"), Some(-12.0));
    assert_eq!(as_number("(7)"), Some(-7.0));
    assert_eq!(as_number("007"), None);
    assert_eq!(as_number("1,23"), None);
    assert_eq!(as_number("12a"), None);
    assert_eq!(as_number("."), None);
}

/// Real files: `OGX_PDF=file.pdf OGX_DEMO_DIR=/tmp/out cargo test -p
/// og-files convert_real_file -- --ignored` writes every target.
#[test]
#[ignore]
fn convert_real_file() {
    let Ok(p) = std::env::var("OGX_PDF") else {
        return;
    };
    let dir = std::env::var("OGX_DEMO_DIR").unwrap_or_else(|_| "/tmp/ogx-pdfconv".into());
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = std::fs::read(&p).unwrap();
    for t in Target::ALL {
        let t0 = std::time::Instant::now();
        match convert(&pdf, t, "demo") {
            Ok(files) => {
                for (n, b) in files {
                    std::fs::write(format!("{dir}/demo-{n}"), b).unwrap();
                }
                eprintln!("{t}: ok in {:?}", t0.elapsed());
            }
            Err(e) => eprintln!("{t}: {e}"),
        }
    }
}

/// With LibreOffice installed: every office output converts to PDF.
#[test]
#[ignore]
fn soffice_opens_outputs() {
    let Ok(st) = std::process::Command::new("soffice")
        .arg("--version")
        .output()
    else {
        return;
    };
    if !st.status.success() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("ogx-soffice-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for t in [
        Target::Docx,
        Target::Odt,
        Target::Pptx,
        Target::Xlsx,
        Target::Rtf,
        Target::Html,
    ] {
        let src = dir.join(format!("in-{}.{}", t.ext(), t.ext()));
        std::fs::write(&src, single(t)).unwrap();
        let out = std::process::Command::new("soffice")
            .arg(format!(
                "-env:UserInstallation=file://{}/profile",
                dir.display()
            ))
            .args(["--headless", "--convert-to", "pdf", "--outdir"])
            .arg(&dir)
            .arg(&src)
            .output()
            .unwrap();
        let pdf = dir.join(format!("in-{}.pdf", t.ext()));
        let ok = std::fs::metadata(&pdf)
            .map(|m| m.len() > 500)
            .unwrap_or(false);
        assert!(
            ok,
            "{t}: soffice failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
