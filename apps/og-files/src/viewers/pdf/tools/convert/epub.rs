//! EPUB 3 (with an NCX for EPUB 2 readers): chapters start at level-1
//! headings when the document has at least two, otherwise one chapter per
//! page. Images go to `OEBPS/images/`.

use super::super::extract::{Item, Kind, Layout, Picture};
use super::text::{xhtml_items, CSS};
use super::{esc, pictures, uuid_of, zip};
use std::collections::HashMap;
use std::fmt::Write as _;

struct Chapter<'a> {
    title: String,
    items: Vec<&'a Item>,
}

fn chapters(layout: &Layout) -> Vec<Chapter<'_>> {
    let all: Vec<&Item> = layout.pages.iter().flat_map(|p| p.items.iter()).collect();
    let is_h1 = |i: &Item| matches!(i, Item::Para(p) if p.kind == Kind::Heading(1));
    let h1s = all.iter().filter(|i| is_h1(i)).count();
    if h1s >= 2 {
        let mut out: Vec<Chapter<'_>> = Vec::new();
        for it in all {
            if is_h1(it) || out.is_empty() {
                let title = match it {
                    Item::Para(p) if is_h1(it) => p.text().trim().to_string(),
                    _ => "Start".to_string(),
                };
                out.push(Chapter {
                    title,
                    items: vec![],
                });
            }
            out.last_mut().unwrap().items.push(it);
        }
        out
    } else {
        layout
            .pages
            .iter()
            .map(|p| Chapter {
                title: p
                    .items
                    .iter()
                    .find_map(|i| match i {
                        Item::Para(pa) if matches!(pa.kind, Kind::Heading(_)) => {
                            Some(pa.text().trim().to_string())
                        }
                        _ => None,
                    })
                    .filter(|t| !t.is_empty() && t.chars().count() <= 80)
                    .unwrap_or_else(|| format!("Page {}", p.number)),
                items: p.items.iter().collect(),
            })
            .collect()
    }
}

pub fn epub(layout: &Layout, title: &str, source: &[u8]) -> Result<Vec<u8>, String> {
    let id = format!("urn:uuid:{}", uuid_of(source));
    let mut names: HashMap<usize, String> = HashMap::new();
    let mut manifest = String::new();
    let mut files: Vec<(String, Vec<u8>, bool)> = Vec::new();
    for (k, p) in pictures(layout).into_iter().enumerate() {
        let n = format!("images/img{}.{}", k + 1, p.ext);
        let _ = write!(
            manifest,
            "<item id=\"img{}\" href=\"{n}\" media-type=\"{}\"/>",
            k + 1,
            p.mime()
        );
        files.push((format!("OEBPS/{n}"), p.data.to_vec(), true));
        names.insert(p.id, n);
    }
    let chs = chapters(layout);
    let mut spine = String::new();
    let mut nav = String::new();
    let mut ncx = String::new();
    for (k, ch) in chs.iter().enumerate() {
        let n = k + 1;
        let file = format!("chapter{n:03}.xhtml");
        let mut body = String::new();
        let src = |p: &Picture| names.get(&p.id).cloned().unwrap_or_default();
        xhtml_items(ch.items.iter().copied(), &src, &mut body);
        let doc = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n<head>\n<meta charset=\"utf-8\"/>\n<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n</head>\n<body>\n{body}</body>\n</html>\n",
            esc(&ch.title)
        );
        files.push((format!("OEBPS/{file}"), doc.into_bytes(), false));
        let _ = write!(
            manifest,
            "<item id=\"ch{n}\" href=\"{file}\" media-type=\"application/xhtml+xml\"/>"
        );
        let _ = write!(spine, "<itemref idref=\"ch{n}\"/>");
        let _ = write!(nav, "<li><a href=\"{file}\">{}</a></li>", esc(&ch.title));
        let _ = write!(
            ncx,
            "<navPoint id=\"np{n}\" playOrder=\"{n}\"><navLabel><text>{}</text></navLabel><content src=\"{file}\"/></navPoint>",
            esc(&ch.title)
        );
    }
    let t = esc(title);
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\"><metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"bookid\">{id}</dc:identifier><dc:title>{t}</dc:title><dc:language>en</dc:language><dc:creator>OG Files</dc:creator><meta property=\"dcterms:modified\">{}</meta></metadata><manifest><item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/><item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/><item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>{manifest}</manifest><spine toc=\"ncx\">{spine}</spine></package>",
        super::now_utc()
    );
    let nav_doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\n<head><meta charset=\"utf-8\"/><title>{t}</title></head>\n<body><nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1><ol>{nav}</ol></nav></body>\n</html>\n"
    );
    let ncx_doc = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\"><head><meta name=\"dtb:uid\" content=\"{id}\"/><meta name=\"dtb:depth\" content=\"1\"/><meta name=\"dtb:totalPageCount\" content=\"0\"/><meta name=\"dtb:maxPageNumber\" content=\"0\"/></head><docTitle><text>{t}</text></docTitle><navMap>{ncx}</navMap></ncx>"
    );
    let container = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>";
    let mut entries = vec![
        (
            "mimetype".to_string(),
            b"application/epub+zip".to_vec(),
            true,
        ),
        (
            "META-INF/container.xml".into(),
            container.as_bytes().to_vec(),
            false,
        ),
        ("OEBPS/content.opf".into(), opf.into_bytes(), false),
        ("OEBPS/nav.xhtml".into(), nav_doc.into_bytes(), false),
        ("OEBPS/toc.ncx".into(), ncx_doc.into_bytes(), false),
        ("OEBPS/style.css".into(), CSS.as_bytes().to_vec(), false),
    ];
    entries.extend(files);
    zip(entries)
}
