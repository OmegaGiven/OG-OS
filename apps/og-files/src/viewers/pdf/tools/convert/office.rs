//! Office targets: Word (.docx), OpenDocument text (.odt) and PowerPoint
//! (.pptx), all written from scratch as plain XML parts in a zip.

use super::super::extract::{
    Align, BBox, Item, Kind, Layout, PageLayout, Para, Picture, Span, Table,
};
use super::super::raster;
use super::{colored, esc, half, hex, pictures, slots, zip, Slot};
use std::collections::HashMap;
use std::fmt::Write as _;

const EMU: f32 = 12_700.0; // per point

/// Page margins (pt) from where the content sits: median over pages.
fn margins(layout: &Layout) -> (f32, f32, f32, f32) {
    let mut ls = Vec::new();
    let mut rs = Vec::new();
    let mut ts = Vec::new();
    let mut bs = Vec::new();
    for p in &layout.pages {
        let b = p.items.iter().map(item_box).reduce(|a, b| BBox {
            x0: a.x0.min(b.x0),
            y0: a.y0.min(b.y0),
            x1: a.x1.max(b.x1),
            y1: a.y1.max(b.y1),
        });
        if let Some(b) = b {
            ls.push(b.x0);
            rs.push(p.width - b.x1);
            ts.push(b.y0);
            bs.push(p.height - b.y1);
        }
    }
    let med = |mut v: Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2)
            .copied()
            .unwrap_or(72.0)
            .clamp(28.0, 108.0)
    };
    (med(ls), med(rs), med(ts), med(bs))
}

fn item_box(i: &Item) -> BBox {
    match i {
        Item::Para(p) => p.bbox,
        Item::Table(t) => t.bbox,
        Item::Picture(p) => p.bbox,
    }
}

/// Most used font family, weighted by characters.
fn body_font(layout: &Layout) -> String {
    let mut hist: HashMap<&str, usize> = HashMap::new();
    for p in &layout.pages {
        for i in &p.items {
            if let Item::Para(pa) = i {
                for l in &pa.lines {
                    for s in &l.spans {
                        *hist.entry(&s.font).or_default() += s.text.len();
                    }
                }
            }
        }
    }
    hist.into_iter()
        .max_by_key(|&(f, n)| (n, std::cmp::Reverse(f)))
        .map(|(f, _)| f.to_string())
        .unwrap_or_else(|| "Times New Roman".into())
}

/// (numFmt, lvlText with %1, start) for an ordered marker like "3." "(b)" "iv)".
fn marker_format(marker: &str) -> (&'static str, String, u32) {
    let tok: String = marker.chars().filter(|c| c.is_alphanumeric()).collect();
    let pattern = marker.replacen(&tok, "%1", 1);
    if let Ok(n) = tok.parse::<u32>() {
        return ("decimal", pattern, n.max(1));
    }
    let roman = |s: &str| -> Option<u32> {
        let v = |c| match c {
            'i' => Some(1),
            'v' => Some(5),
            'x' => Some(10),
            'l' => Some(50),
            'c' => Some(100),
            _ => None,
        };
        let ds: Vec<u32> = s.chars().map(v).collect::<Option<_>>()?;
        let mut t = 0i64;
        for (k, d) in ds.iter().enumerate() {
            if ds.get(k + 1).is_some_and(|n| n > d) {
                t -= *d as i64;
            } else {
                t += *d as i64;
            }
        }
        (t > 0).then_some(t as u32)
    };
    if tok.len() == 1 && tok.chars().all(|c| c.is_ascii_lowercase()) && !"ivx".contains(&*tok) {
        let c = tok.chars().next().unwrap_or('a');
        return ("lowerLetter", pattern, c as u32 - 'a' as u32 + 1);
    }
    if let Some(n) = roman(&tok) {
        return ("lowerRoman", pattern, n);
    }
    ("decimal", "%1.".into(), 1)
}

/// List numbering: one shared bullet definition, a fresh numbered one for
/// every ordered list (so each restarts where the PDF's did).
struct Numbering {
    /// per ordered list: (fmt, pattern, start)
    ordered: Vec<(&'static str, String, u32)>,
}

impl Numbering {
    /// numId for each paragraph in document order (None = not a list)
    fn assign(layout: &Layout) -> (Numbering, Vec<Option<u32>>) {
        let mut n = Numbering { ordered: vec![] };
        let mut ids = Vec::new();
        let mut cur: Option<u32> = None;
        for p in &layout.pages {
            for it in &p.items {
                match it {
                    Item::Para(Para {
                        kind:
                            Kind::List {
                                ordered: true,
                                marker,
                                ..
                            },
                        ..
                    }) => {
                        let id = *cur.get_or_insert_with(|| {
                            n.ordered.push(marker_format(marker));
                            n.ordered.len() as u32 + 1
                        });
                        ids.push(Some(id));
                    }
                    Item::Para(Para {
                        kind: Kind::List { ordered: false, .. },
                        ..
                    }) => ids.push(Some(1)),
                    Item::Para(_) => {
                        cur = None;
                        ids.push(None);
                    }
                    _ => cur = None,
                }
            }
        }
        (n, ids)
    }
}

// ------------------------------------------------------------------ docx

const W_NS: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"";

fn w_runs(runs: &[Span], body_font: &str, body_size: f32, o: &mut String) {
    for r in runs {
        o.push_str("<w:r><w:rPr>");
        if *r.font != *body_font {
            let f = esc(&r.font);
            let _ = write!(
                o,
                "<w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:cs=\"{f}\"/>"
            );
        }
        if r.bold {
            o.push_str("<w:b/>");
        }
        if r.italic {
            o.push_str("<w:i/>");
        }
        if r.invisible {
            // OCR text layer: keep it searchable but don't print it twice
            // over the scan
            o.push_str("<w:vanish/>");
        }
        if colored(r.color) {
            let _ = write!(o, "<w:color w:val=\"{}\"/>", hex(r.color));
        }
        let sz = (half(r.size) * 2.0) as u32;
        if (half(r.size) - half(body_size)).abs() > 0.1 {
            let _ = write!(o, "<w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/>");
        }
        o.push_str("</w:rPr>");
        let _ = write!(
            o,
            "<w:t xml:space=\"preserve\">{}</w:t></w:r>",
            esc(&r.text)
        );
    }
}

fn w_jc(a: Align) -> &'static str {
    match a {
        Align::Left => "",
        Align::Center => "<w:jc w:val=\"center\"/>",
        Align::Right => "<w:jc w:val=\"right\"/>",
        Align::Justify => "<w:jc w:val=\"both\"/>",
    }
}

struct DocxCtx<'a> {
    font: String,
    size: f32,
    text_width: f32,
    rel_of: HashMap<usize, (String, String)>, // pic id → (rId, file name)
    nums: std::slice::Iter<'a, Option<u32>>,
    pic_n: u32,
}

fn docx_para(p: &Para, num: Option<u32>, cx: &DocxCtx<'_>, o: &mut String) {
    o.push_str("<w:p><w:pPr>");
    match &p.kind {
        Kind::Heading(l) => {
            let _ = write!(o, "<w:pStyle w:val=\"Heading{l}\"/>");
        }
        Kind::List { .. } => o.push_str("<w:pStyle w:val=\"ListParagraph\"/>"),
        Kind::Body => {}
    }
    if let (Kind::List { level, .. }, Some(id)) = (&p.kind, num) {
        let _ = write!(
            o,
            "<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{id}\"/></w:numPr>",
            level.min(&3)
        );
    }
    o.push_str(w_jc(p.align));
    o.push_str("</w:pPr>");
    w_runs(&p.runs(), &cx.font, cx.size, o);
    o.push_str("</w:p>\n");
}

fn docx_picture(p: &Picture, cx: &mut DocxCtx<'_>, o: &mut String) {
    let Some((rid, name)) = cx.rel_of.get(&p.id).cloned() else {
        return;
    };
    let scale = (cx.text_width / p.bbox.w().max(1.0)).min(1.0);
    let (w, h) = (
        (p.bbox.w() * scale * EMU) as u64,
        (p.bbox.h() * scale * EMU) as u64,
    );
    cx.pic_n += 1;
    let n = cx.pic_n;
    let _ = write!(
        o,
        "<w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{w}\" cy=\"{h}\"/><wp:docPr id=\"{n}\" name=\"Picture {n}\"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect=\"1\"/></wp:cNvGraphicFramePr><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic><pic:nvPicPr><pic:cNvPr id=\"{n}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>\n"
    );
}

fn docx_table(t: &Table, cx: &DocxCtx<'_>, o: &mut String) {
    let sl = slots(t);
    let scale = (cx.text_width / t.widths.iter().sum::<f32>().max(1.0)).min(1.0);
    let tw: Vec<u32> = t
        .widths
        .iter()
        .map(|w| (w * scale * 20.0).max(200.0) as u32)
        .collect();
    o.push_str("<w:tbl><w:tblPr>");
    if t.ruled {
        o.push_str("<w:tblStyle w:val=\"TableGrid\"/>");
    }
    o.push_str("<w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblLayout w:type=\"fixed\"/><w:tblLook w:val=\"04A0\" w:firstRow=\"1\" w:lastRow=\"0\" w:firstColumn=\"1\" w:lastColumn=\"0\" w:noHBand=\"0\" w:noVBand=\"1\"/></w:tblPr><w:tblGrid>");
    for w in &tw {
        let _ = write!(o, "<w:gridCol w:w=\"{w}\"/>");
    }
    o.push_str("</w:tblGrid>\n");
    for (r, row) in t.rows.iter().enumerate() {
        o.push_str("<w:tr>");
        for (c, cell) in row.iter().enumerate() {
            let (span, vmerge, content) = match sl[r][c] {
                Slot::Skip => continue,
                Slot::Origin => (
                    cell.colspan.max(1),
                    if cell.rowspan > 1 {
                        "<w:vMerge w:val=\"restart\"/>"
                    } else {
                        ""
                    },
                    true,
                ),
                Slot::VCont { colspan } => (colspan, "<w:vMerge/>", false),
            };
            let width: u32 = tw[c..(c + span as usize).min(tw.len())].iter().sum();
            let _ = write!(o, "<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/>");
            if span > 1 {
                let _ = write!(o, "<w:gridSpan w:val=\"{span}\"/>");
            }
            o.push_str(vmerge);
            o.push_str("</w:tcPr><w:p>");
            if content {
                w_runs(&cell.runs(), &cx.font, cx.size, o);
            }
            o.push_str("</w:p></w:tc>");
        }
        o.push_str("</w:tr>\n");
    }
    o.push_str("</w:tbl>\n");
}

fn docx_numbering(n: &Numbering) -> String {
    let mut o = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering {W_NS}>"
    );
    // abstract 0: bullets
    o.push_str(
        "<w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/>",
    );
    for (l, b) in ["\u{2022}", "\u{25e6}", "\u{25aa}", "\u{2022}"]
        .iter()
        .enumerate()
    {
        let _ = write!(
            o,
            "<w:lvl w:ilvl=\"{l}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"{b}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr></w:lvl>",
            720 + 360 * l
        );
    }
    o.push_str("</w:abstractNum>");
    for (k, (fmt, pat, start)) in n.ordered.iter().enumerate() {
        let _ = write!(
            o,
            "<w:abstractNum w:abstractNumId=\"{}\"><w:multiLevelType w:val=\"hybridMultilevel\"/>",
            k + 1
        );
        for l in 0..4u32 {
            let (f, p, s) = if l == 0 {
                (*fmt, pat.clone(), *start)
            } else {
                (
                    ["decimal", "lowerLetter", "lowerRoman"][(l % 3) as usize],
                    format!("%{}.", l + 1),
                    1,
                )
            };
            let p = if l == 0 {
                p
            } else {
                p.replace("%1", &format!("%{}", l + 1))
            };
            let _ = write!(
                o,
                "<w:lvl w:ilvl=\"{l}\"><w:start w:val=\"{s}\"/><w:numFmt w:val=\"{f}\"/><w:lvlText w:val=\"{}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr></w:lvl>",
                esc(&p),
                720 + 360 * l
            );
        }
        o.push_str("</w:abstractNum>");
    }
    o.push_str("<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>");
    for k in 0..n.ordered.len() {
        let _ = write!(
            o,
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"{}\"/></w:num>",
            k + 2,
            k + 1
        );
    }
    o.push_str("</w:numbering>");
    o
}

fn docx_styles(font: &str, size: f32) -> String {
    let f = esc(font);
    let sz = (half(size) * 2.0) as u32;
    let mut o = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles {W_NS}><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"{f}\" w:hAnsi=\"{f}\" w:eastAsia=\"{f}\" w:cs=\"{f}\"/><w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/><w:lang w:val=\"en-US\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>"
    );
    for (l, s) in [(1u8, 32u32), (2, 26), (3, 24)] {
        let _ = write!(
            o,
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{l}\"><w:name w:val=\"heading {l}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:uiPriority w:val=\"9\"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"{}\" w:after=\"80\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{s}\"/><w:szCs w:val=\"{s}\"/></w:rPr></w:style>",
            if l == 1 { 360 } else { 200 },
            l - 1
        );
    }
    o.push_str("<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/><w:uiPriority w:val=\"34\"/><w:qFormat/><w:pPr><w:spacing w:after=\"40\"/><w:ind w:left=\"720\"/><w:contextualSpacing/></w:pPr></w:style>\
<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:uiPriority w:val=\"99\"/><w:semiHidden/><w:unhideWhenUsed/><w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>\
<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:basedOn w:val=\"TableNormal\"/><w:uiPriority w:val=\"39\"/><w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr><w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/></w:tblBorders></w:tblPr></w:style>\
</w:styles>");
    o
}

fn core_xml(title: &str) -> String {
    let now = super::now_utc();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:title>{}</dc:title><dc:creator>OG Files</dc:creator><dcterms:created xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{now}</dcterms:modified></cp:coreProperties>",
        esc(title)
    )
}

const APP_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Application>OG Files</Application></Properties>";

fn first_title(layout: &Layout) -> String {
    layout
        .pages
        .iter()
        .flat_map(|p| p.items.iter())
        .find_map(|i| match i {
            Item::Para(p) if matches!(p.kind, Kind::Heading(_)) => Some(p.text()),
            _ => None,
        })
        .unwrap_or_default()
}

pub fn docx(layout: &Layout) -> Result<Vec<u8>, String> {
    let font = body_font(layout);
    let (ml, mr, mt, mb) = margins(layout);
    let (pw, ph) = layout
        .pages
        .first()
        .map(|p| (p.width, p.height))
        .unwrap_or((612.0, 792.0));
    let (numbering, nums) = Numbering::assign(layout);
    let mut rels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/><Relationship Id=\"rIdNumbering\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>");
    let mut media = Vec::new();
    let mut rel_of = HashMap::new();
    for (k, p) in pictures(layout).into_iter().enumerate() {
        let name = format!("image{}.{}", k + 1, p.ext);
        let rid = format!("rIdImg{}", k + 1);
        let _ = write!(
            rels,
            "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{name}\"/>"
        );
        media.push((format!("word/media/{name}"), p.data.to_vec(), true));
        rel_of.insert(p.id, (rid, name));
    }
    rels.push_str("</Relationships>");

    let mut cx = DocxCtx {
        font: font.clone(),
        size: layout.body_size,
        text_width: (pw - ml - mr).max(144.0),
        rel_of,
        nums: nums.iter(),
        pic_n: 0,
    };
    let mut body = String::new();
    for (pi, p) in layout.pages.iter().enumerate() {
        if pi > 0 {
            body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>\n");
        }
        let n = p.items.len();
        for (k, it) in p.items.iter().enumerate() {
            match it {
                Item::Para(pa) => {
                    let num = cx.nums.next().copied().flatten();
                    docx_para(pa, num, &cx, &mut body);
                }
                Item::Table(t) => {
                    docx_table(t, &cx, &mut body);
                    // two tables in a row would merge; a document can't end
                    // with a table either
                    if k + 1 == n || matches!(p.items.get(k + 1), Some(Item::Table(_))) {
                        body.push_str("<w:p/>\n");
                    }
                }
                Item::Picture(pic) => docx_picture(pic, &mut cx, &mut body),
            }
        }
    }
    let tw = |v: f32| (v * 20.0).round() as u32;
    let orient = if pw > ph {
        " w:orient=\"landscape\""
    } else {
        ""
    };
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document {W_NS}><w:body>\n{body}<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"{orient}/><w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" w:header=\"708\" w:footer=\"708\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>",
        tw(pw),
        tw(ph),
        tw(mt),
        tw(mr),
        tw(mb),
        tw(ml)
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpg\" ContentType=\"image/jpeg\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/><Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/></Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>";
    let mut entries = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.as_bytes().to_vec(),
            false,
        ),
        ("_rels/.rels".into(), root_rels.as_bytes().to_vec(), false),
        ("word/document.xml".into(), document.into_bytes(), false),
        (
            "word/_rels/document.xml.rels".into(),
            rels.into_bytes(),
            false,
        ),
        (
            "word/styles.xml".into(),
            docx_styles(&font, layout.body_size).into_bytes(),
            false,
        ),
        (
            "word/numbering.xml".into(),
            docx_numbering(&numbering).into_bytes(),
            false,
        ),
        (
            "docProps/core.xml".into(),
            core_xml(&first_title(layout)).into_bytes(),
            false,
        ),
        (
            "docProps/app.xml".into(),
            APP_XML.as_bytes().to_vec(),
            false,
        ),
    ];
    entries.extend(media);
    zip(entries)
}

// ------------------------------------------------------------------- odt

const ODF_NS: &str = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:style=\"urn:oasis:names:tc:opendocument:xmlns:style:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" xmlns:fo=\"urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" xmlns:svg=\"urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0\" office:version=\"1.3\"";

fn pt(v: f32) -> String {
    format!("{:.2}pt", v)
}

/// Text with ODF whitespace rules (runs of spaces → text:s, tabs).
fn odf_text(s: &str, at_start: bool, o: &mut String) {
    let mut spaces = 0usize;
    let mut first = at_start;
    let flush = |n: &mut usize, first: bool, o: &mut String| {
        if *n == 0 {
            return;
        }
        if first {
            let _ = write!(o, "<text:s text:c=\"{}\"/>", *n);
        } else {
            o.push(' ');
            if *n > 1 {
                let _ = write!(o, "<text:s text:c=\"{}\"/>", *n - 1);
            }
        }
        *n = 0;
    };
    for c in s.chars() {
        match c {
            ' ' => spaces += 1,
            '\t' => {
                flush(&mut spaces, first, o);
                o.push_str("<text:tab/>");
                first = false;
            }
            '\n' => {
                flush(&mut spaces, first, o);
                o.push_str("<text:line-break/>");
                first = false;
            }
            c => {
                flush(&mut spaces, first, o);
                first = false;
                o.push_str(&esc(&c.to_string()));
            }
        }
    }
    flush(&mut spaces, first, o);
}

#[derive(Default)]
struct OdtStyles {
    /// text look key → T name
    text: Vec<(String, String)>,
    /// (parent, align, page break) → P name
    para: Vec<((String, Align, bool), String)>,
    fonts: Vec<String>,
    auto: String,
}

impl OdtStyles {
    fn font(&mut self, f: &str) {
        if !self.fonts.iter().any(|x| x == f) {
            self.fonts.push(f.to_string());
        }
    }

    fn text_style(&mut self, s: &Span, body_font: &str, body_size: f32) -> Option<String> {
        let mut props = String::new();
        if s.bold {
            props.push_str(" fo:font-weight=\"bold\" style:font-weight-asian=\"bold\" style:font-weight-complex=\"bold\"");
        }
        if s.italic {
            props.push_str(" fo:font-style=\"italic\" style:font-style-asian=\"italic\" style:font-style-complex=\"italic\"");
        }
        if (half(s.size) - half(body_size)).abs() > 0.1 {
            let _ = write!(props, " fo:font-size=\"{}pt\"", half(s.size));
        }
        if colored(s.color) {
            let _ = write!(props, " fo:color=\"#{}\"", hex(s.color));
        }
        if *s.font != *body_font {
            self.font(&s.font);
            let _ = write!(props, " style:font-name=\"{}\"", esc(&s.font));
        }
        if s.invisible {
            props.push_str(" text:display=\"none\"");
        }
        if props.is_empty() {
            return None;
        }
        if let Some((_, n)) = self.text.iter().find(|(k, _)| *k == props) {
            return Some(n.clone());
        }
        let n = format!("T{}", self.text.len() + 1);
        let _ = write!(
            self.auto,
            "<style:style style:name=\"{n}\" style:family=\"text\"><style:text-properties{props}/></style:style>"
        );
        self.text.push((props, n.clone()));
        Some(n)
    }

    fn para_style(&mut self, parent: &str, align: Align, brk: bool) -> String {
        let key = (parent.to_string(), align, brk);
        if let Some((_, n)) = self.para.iter().find(|(k, _)| *k == key) {
            return n.clone();
        }
        let n = format!("P{}", self.para.len() + 1);
        let a = match align {
            Align::Left => "",
            Align::Center => " fo:text-align=\"center\"",
            Align::Right => " fo:text-align=\"end\"",
            Align::Justify => " fo:text-align=\"justify\"",
        };
        let b = if brk { " fo:break-before=\"page\"" } else { "" };
        let _ = write!(
            self.auto,
            "<style:style style:name=\"{n}\" style:family=\"paragraph\" style:parent-style-name=\"{parent}\"><style:paragraph-properties{a}{b}/></style:style>"
        );
        self.para.push((key, n.clone()));
        n
    }
}

fn odt_runs(runs: &[Span], st: &mut OdtStyles, font: &str, size: f32, o: &mut String) {
    for (k, r) in runs.iter().enumerate() {
        match st.text_style(r, font, size) {
            Some(n) => {
                let _ = write!(o, "<text:span text:style-name=\"{n}\">");
                odf_text(&r.text, k == 0, o);
                o.push_str("</text:span>");
            }
            None => odf_text(&r.text, k == 0, o),
        }
    }
}

pub fn odt(layout: &Layout) -> Result<Vec<u8>, String> {
    let font = body_font(layout);
    let size = layout.body_size;
    let (ml, mr, mt, mb) = margins(layout);
    let (pw, ph) = layout
        .pages
        .first()
        .map(|p| (p.width, p.height))
        .unwrap_or((612.0, 792.0));
    let text_width = (pw - ml - mr).max(144.0);
    let mut st = OdtStyles::default();
    st.font(&font);
    let (numbering, nums) = Numbering::assign(layout);
    // list styles: L1 bullets, L{k+2} ordered list k
    let mut list_styles = String::from("<text:list-style style:name=\"L1\">");
    for (l, b) in ["\u{2022}", "\u{25e6}", "\u{25aa}", "\u{2022}"]
        .iter()
        .enumerate()
    {
        let _ = write!(
            list_styles,
            "<text:list-level-style-bullet text:level=\"{}\" text:bullet-char=\"{b}\"><style:list-level-properties text:list-level-position-and-space-mode=\"label-alignment\"><style:list-level-label-alignment text:label-followed-by=\"listtab\" fo:text-indent=\"-0.25in\" fo:margin-left=\"{:.2}in\"/></style:list-level-properties></text:list-level-style-bullet>",
            l + 1,
            0.5 + 0.25 * l as f32
        );
    }
    list_styles.push_str("</text:list-style>");
    for (k, (fmt, pat, _)) in numbering.ordered.iter().enumerate() {
        let f = match *fmt {
            "lowerLetter" => "a",
            "lowerRoman" => "i",
            _ => "1",
        };
        let (prefix, suffix) = pat.split_once("%1").unwrap_or(("", "."));
        let _ = write!(list_styles, "<text:list-style style:name=\"L{}\">", k + 2);
        for l in 0..4 {
            let (f, prefix, suffix) = if l == 0 {
                (f, prefix, suffix)
            } else {
                (["1", "a", "i"][l % 3], "", ".")
            };
            let _ = write!(
                list_styles,
                "<text:list-level-style-number text:level=\"{}\" style:num-format=\"{f}\" style:num-prefix=\"{}\" style:num-suffix=\"{}\"><style:list-level-properties text:list-level-position-and-space-mode=\"label-alignment\"><style:list-level-label-alignment text:label-followed-by=\"listtab\" fo:text-indent=\"-0.25in\" fo:margin-left=\"{:.2}in\"/></style:list-level-properties></text:list-level-style-number>",
                l + 1,
                esc(prefix),
                esc(suffix),
                0.5 + 0.25 * l as f32
            );
        }
        list_styles.push_str("</text:list-style>");
    }

    let mut body = String::new();
    let mut media = Vec::new();
    let mut href: HashMap<usize, String> = HashMap::new();
    for (k, p) in pictures(layout).into_iter().enumerate() {
        let name = format!("Pictures/image{}.{}", k + 1, p.ext);
        media.push((name.clone(), p.data.to_vec(), true, p.mime()));
        href.insert(p.id, name);
    }
    let mut nums_it = nums.iter();
    let mut table_n = 0;
    let mut pic_n = 0;
    // open lists: (list style, depth)
    let mut open: Vec<u32> = Vec::new();
    let close_lists = |open: &mut Vec<u32>, o: &mut String| {
        while open.pop().is_some() {
            o.push_str("</text:list-item></text:list>");
        }
    };
    for (pi, page) in layout.pages.iter().enumerate() {
        let mut brk = pi > 0;
        for it in &page.items {
            match it {
                Item::Para(pa) => {
                    let num = nums_it.next().copied().flatten();
                    if let (Kind::List { level, .. }, Some(id)) = (&pa.kind, num) {
                        let depth = (*level as usize + 1).min(open.len() + 1);
                        while open.len() > depth {
                            open.pop();
                            body.push_str("</text:list-item></text:list>");
                        }
                        if open.len() == depth {
                            if open.last() == Some(&id) {
                                body.push_str("</text:list-item><text:list-item>");
                            } else {
                                open.pop();
                                let _ = write!(body, "</text:list-item></text:list><text:list text:style-name=\"L{id}\"><text:list-item>");
                                open.push(id);
                            }
                        } else {
                            let _ = write!(
                                body,
                                "<text:list text:style-name=\"L{id}\"><text:list-item>"
                            );
                            open.push(id);
                        }
                        let ps = st.para_style("List_20_Paragraph", pa.align, brk);
                        let _ = write!(body, "<text:p text:style-name=\"{ps}\">");
                        odt_runs(&pa.runs(), &mut st, &font, size, &mut body);
                        body.push_str("</text:p>");
                        brk = false;
                        continue;
                    }
                    close_lists(&mut open, &mut body);
                    match pa.kind {
                        Kind::Heading(l) => {
                            let ps = st.para_style(&format!("Heading_20_{l}"), pa.align, brk);
                            let _ = write!(
                                body,
                                "<text:h text:style-name=\"{ps}\" text:outline-level=\"{l}\">"
                            );
                            odt_runs(&pa.runs(), &mut st, &font, size, &mut body);
                            body.push_str("</text:h>\n");
                        }
                        _ => {
                            let ps = st.para_style("Standard", pa.align, brk);
                            let _ = write!(body, "<text:p text:style-name=\"{ps}\">");
                            odt_runs(&pa.runs(), &mut st, &font, size, &mut body);
                            body.push_str("</text:p>\n");
                        }
                    }
                    brk = false;
                }
                Item::Table(t) => {
                    close_lists(&mut open, &mut body);
                    table_n += 1;
                    odt_table(t, table_n, brk, text_width, &mut st, &font, size, &mut body);
                    brk = false;
                }
                Item::Picture(p) => {
                    close_lists(&mut open, &mut body);
                    let Some(h) = href.get(&p.id) else { continue };
                    pic_n += 1;
                    let scale = (text_width / p.bbox.w().max(1.0)).min(1.0);
                    let ps = st.para_style("Standard", Align::Center, brk);
                    let _ = write!(
                        body,
                        "<text:p text:style-name=\"{ps}\"><draw:frame draw:style-name=\"fr1\" draw:name=\"Image{pic_n}\" text:anchor-type=\"as-char\" svg:width=\"{}\" svg:height=\"{}\" draw:z-index=\"0\"><draw:image xlink:href=\"{h}\" xlink:type=\"simple\" xlink:show=\"embed\" xlink:actuate=\"onLoad\"/></draw:frame></text:p>\n",
                        pt(p.bbox.w() * scale),
                        pt(p.bbox.h() * scale)
                    );
                    brk = false;
                }
            }
        }
    }
    close_lists(&mut open, &mut body);

    let fq = |f: &str| {
        if f.contains(' ') {
            format!("'{}'", esc(f))
        } else {
            esc(f)
        }
    };
    let mut faces = String::new();
    for f in &st.fonts {
        let _ = write!(
            faces,
            "<style:font-face style:name=\"{}\" svg:font-family=\"{}\"/>",
            esc(f),
            fq(f)
        );
    }
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-content {ODF_NS}><office:font-face-decls>{faces}</office:font-face-decls><office:automatic-styles>{}<style:style style:name=\"fr1\" style:family=\"graphic\"><style:graphic-properties style:vertical-pos=\"top\" style:vertical-rel=\"baseline\"/></style:style>{list_styles}</office:automatic-styles><office:body><office:text>\n{body}</office:text></office:body></office:document-content>",
        st.auto
    );
    let f = esc(&font);
    let mut heads = String::new();
    for (l, s) in [(1, 16.0), (2, 13.0), (3, 12.0)] {
        let _ = write!(
            heads,
            "<style:style style:name=\"Heading_20_{l}\" style:display-name=\"Heading {l}\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:next-style-name=\"Standard\" style:default-outline-level=\"{l}\" style:class=\"text\"><style:paragraph-properties fo:margin-top=\"{}pt\" fo:margin-bottom=\"4pt\" fo:keep-with-next=\"always\"/><style:text-properties fo:font-size=\"{s}pt\" fo:font-weight=\"bold\" style:font-weight-asian=\"bold\" style:font-weight-complex=\"bold\"/></style:style>",
            if l == 1 { 18 } else { 10 }
        );
    }
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-styles {ODF_NS}><office:font-face-decls><style:font-face style:name=\"{f}\" svg:font-family=\"{}\"/></office:font-face-decls><office:styles><style:default-style style:family=\"paragraph\"><style:paragraph-properties fo:margin-bottom=\"6pt\"/><style:text-properties style:font-name=\"{f}\" fo:font-size=\"{}pt\" fo:language=\"en\" fo:country=\"US\"/></style:default-style><style:style style:name=\"Standard\" style:family=\"paragraph\" style:class=\"text\"/>{heads}<style:style style:name=\"List_20_Paragraph\" style:display-name=\"List Paragraph\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"list\"><style:paragraph-properties fo:margin-bottom=\"2pt\"/></style:style><style:style style:name=\"Table_20_Contents\" style:display-name=\"Table Contents\" style:family=\"paragraph\" style:parent-style-name=\"Standard\" style:class=\"extra\"><style:paragraph-properties fo:margin-bottom=\"0pt\"/></style:style></office:styles><office:automatic-styles><style:page-layout style:name=\"pm1\"><style:page-layout-properties fo:page-width=\"{}\" fo:page-height=\"{}\" style:print-orientation=\"{}\" fo:margin-top=\"{}\" fo:margin-bottom=\"{}\" fo:margin-left=\"{}\" fo:margin-right=\"{}\"/></style:page-layout></office:automatic-styles><office:master-styles><style:master-page style:name=\"Standard\" style:page-layout-name=\"pm1\"/></office:master-styles></office:document-styles>",
        fq(&font),
        half(size),
        pt(pw),
        pt(ph),
        if pw > ph { "landscape" } else { "portrait" },
        pt(mt),
        pt(mb),
        pt(ml),
        pt(mr)
    );
    let meta = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-meta {ODF_NS}><office:meta><meta:generator>OG Files</meta:generator><dc:title>{}</dc:title><dc:date>{}</dc:date></office:meta></office:document-meta>",
        esc(&first_title(layout)),
        super::now_utc()
    );
    let mut manifest = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" manifest:version=\"1.3\"><manifest:file-entry manifest:full-path=\"/\" manifest:version=\"1.3\" manifest:media-type=\"application/vnd.oasis.opendocument.text\"/><manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"styles.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"meta.xml\" manifest:media-type=\"text/xml\"/>");
    for (name, _, _, mime) in &media {
        let _ = write!(
            manifest,
            "<manifest:file-entry manifest:full-path=\"{name}\" manifest:media-type=\"{mime}\"/>"
        );
    }
    manifest.push_str("</manifest:manifest>");
    let mut entries = vec![
        (
            "mimetype".to_string(),
            b"application/vnd.oasis.opendocument.text".to_vec(),
            true,
        ),
        ("META-INF/manifest.xml".into(), manifest.into_bytes(), false),
        ("content.xml".into(), content.into_bytes(), false),
        ("styles.xml".into(), styles.into_bytes(), false),
        ("meta.xml".into(), meta.into_bytes(), false),
    ];
    entries.extend(media.into_iter().map(|(n, d, s, _)| (n, d, s)));
    zip(entries)
}

#[allow(clippy::too_many_arguments)]
fn odt_table(
    t: &Table,
    n: usize,
    brk: bool,
    text_width: f32,
    st: &mut OdtStyles,
    font: &str,
    size: f32,
    o: &mut String,
) {
    let total: f32 = t.widths.iter().sum::<f32>().max(1.0);
    let scale = (text_width / total).min(1.0);
    let name = format!("Table{n}");
    let _ = write!(
        st.auto,
        "<style:style style:name=\"{name}\" style:family=\"table\"><style:table-properties style:width=\"{}\" table:align=\"left\"{}/></style:style>",
        pt(total * scale),
        if brk { " fo:break-before=\"page\"" } else { "" }
    );
    for (c, w) in t.widths.iter().enumerate() {
        let _ = write!(
            st.auto,
            "<style:style style:name=\"{name}.C{c}\" style:family=\"table-column\"><style:table-column-properties style:column-width=\"{}\"/></style:style>",
            pt(w * scale)
        );
    }
    let border = if t.ruled {
        "0.5pt solid #000000"
    } else {
        "none"
    };
    let _ = write!(
        st.auto,
        "<style:style style:name=\"{name}.cell\" style:family=\"table-cell\"><style:table-cell-properties fo:padding=\"2pt\" fo:border=\"{border}\"/></style:style>"
    );
    let _ = write!(
        o,
        "<table:table table:name=\"{name}\" table:style-name=\"{name}\">"
    );
    for c in 0..t.widths.len() {
        let _ = write!(o, "<table:table-column table:style-name=\"{name}.C{c}\"/>");
    }
    for row in &t.rows {
        o.push_str("<table:table-row>");
        for cell in row {
            if cell.covered {
                o.push_str("<table:covered-table-cell/>");
                continue;
            }
            let mut a = String::new();
            if cell.colspan > 1 {
                let _ = write!(a, " table:number-columns-spanned=\"{}\"", cell.colspan);
            }
            if cell.rowspan > 1 {
                let _ = write!(a, " table:number-rows-spanned=\"{}\"", cell.rowspan);
            }
            let _ = write!(
                o,
                "<table:table-cell table:style-name=\"{name}.cell\" office:value-type=\"string\"{a}><text:p text:style-name=\"Table_20_Contents\">"
            );
            odt_runs(&cell.runs(), st, font, size, o);
            o.push_str("</text:p></table:table-cell>");
        }
        o.push_str("</table:table-row>");
    }
    o.push_str("</table:table>\n");
}

// ------------------------------------------------------------------ pptx

const P_NS: &str = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"";
const GRP: &str = "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>";

/// The same PDF with all text-showing operators removed (page content and
/// form XObjects), so the rendered page is the background under editable
/// text boxes. None if the file can't be rewritten (encrypted…).
fn strip_text(pdf: &[u8]) -> Option<Vec<u8>> {
    use lopdf::content::Content;
    use lopdf::{Document, Object};
    let mut doc = Document::load_mem(pdf).ok()?;
    if doc.is_encrypted() {
        return None;
    }
    let keep = |c: Content| Content::<Vec<lopdf::content::Operation>> {
        operations: c
            .operations
            .into_iter()
            .filter(|op| !matches!(op.operator.as_str(), "Tj" | "TJ" | "'" | "\""))
            .collect(),
    };
    let pages: Vec<lopdf::ObjectId> = doc.get_pages().values().copied().collect();
    for id in pages {
        let content = doc.get_page_content_with_limit(id, 256 << 20).ok()?;
        let c = Content::decode(&content).ok()?;
        let bytes = keep(c).encode().ok()?;
        doc.change_page_content(id, bytes).ok()?;
    }
    for obj in doc.objects.values_mut() {
        if let Object::Stream(s) = obj {
            let form = s
                .dict
                .get(b"Subtype")
                .and_then(|v| v.as_name())
                .is_ok_and(|n| n == b"Form");
            if !form {
                continue;
            }
            let data = s
                .decompressed_content()
                .unwrap_or_else(|_| s.content.clone());
            if let Ok(c) = Content::decode(&data) {
                if let Ok(b) = keep(c).encode() {
                    s.set_plain_content(b);
                    let _ = s.compress();
                }
            }
        }
    }
    let mut out = Vec::new();
    doc.save_to(&mut out).ok()?;
    Some(out)
}

fn a_runs(runs: &[Span], scale: f32, force_invisible: bool, o: &mut String) {
    for r in runs {
        if r.text.is_empty() {
            continue;
        }
        let sz = ((r.size * scale * 100.0).round() as u32).clamp(100, 400_000);
        let _ = write!(o, "<a:r><a:rPr lang=\"en-US\" sz=\"{sz}\"");
        if r.bold {
            o.push_str(" b=\"1\"");
        }
        if r.italic {
            o.push_str(" i=\"1\"");
        }
        o.push_str(" dirty=\"0\">");
        if r.invisible || force_invisible {
            let _ = write!(
                o,
                "<a:solidFill><a:srgbClr val=\"{}\"><a:alpha val=\"0\"/></a:srgbClr></a:solidFill>",
                hex(r.color)
            );
        } else {
            let _ = write!(
                o,
                "<a:solidFill><a:srgbClr val=\"{}\"/></a:solidFill>",
                hex(r.color)
            );
        }
        let f = esc(&r.font);
        let _ = write!(
            o,
            "<a:latin typeface=\"{f}\"/><a:cs typeface=\"{f}\"/></a:rPr><a:t>{}</a:t></a:r>",
            esc(&r.text)
        );
    }
}

struct SlideCtx {
    scale: f32,
    next_id: u32,
    /// text was not removed from the background: keep boxes transparent
    transparent: bool,
}

fn text_box(
    lines: &[super::super::extract::Line],
    align: Align,
    cx: &mut SlideCtx,
    o: &mut String,
) {
    let Some(first) = lines.first() else { return };
    let b = lines.iter().skip(1).fold(first.bbox, |a, l| BBox {
        x0: a.x0.min(l.bbox.x0),
        y0: a.y0.min(l.bbox.y0),
        x1: a.x1.max(l.bbox.x1),
        y1: a.y1.max(l.bbox.y1),
    });
    let s = cx.scale * EMU;
    // a little slack so the substitute font doesn't wrap/clip
    let (x, y, w, h) = (
        (b.x0 * s) as i64,
        (b.y0 * s) as i64,
        ((b.w() * 1.08 + 4.0) * s) as i64,
        ((b.h() + 2.0) * s) as i64,
    );
    cx.next_id += 1;
    let id = cx.next_id;
    let algn = match align {
        Align::Center => "ctr",
        Align::Right => "r",
        Align::Justify | Align::Left => "l",
    };
    let pitch = if lines.len() > 1 {
        (lines.last().unwrap().baseline - first.baseline) / (lines.len() - 1) as f32
    } else {
        0.0
    };
    let _ = write!(
        o,
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"Text {id}\"/><p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap=\"none\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\" rtlCol=\"0\" anchor=\"t\"><a:noAutofit/></a:bodyPr><a:lstStyle/>"
    );
    let all_invisible = lines.iter().all(|l| l.spans.iter().all(|s| s.invisible));
    for l in lines {
        let _ = write!(o, "<a:p><a:pPr algn=\"{algn}\">");
        if pitch > 0.5 {
            let _ = write!(
                o,
                "<a:lnSpc><a:spcPts val=\"{}\"/></a:lnSpc>",
                ((pitch * cx.scale * 100.0) as u32).clamp(100, 158_400)
            );
        }
        o.push_str("<a:spcBef><a:spcPts val=\"0\"/></a:spcBef><a:spcAft><a:spcPts val=\"0\"/></a:spcAft></a:pPr>");
        let mut spans = l.spans.clone();
        if let Some(f) = spans.first_mut() {
            f.text = f.text.trim_start().to_string();
        }
        a_runs(&spans, cx.scale, cx.transparent || all_invisible, o);
        o.push_str("</a:p>");
    }
    o.push_str("</p:txBody></p:sp>");
}

fn slide_xml(page: &PageLayout, cx: &mut SlideCtx, sw: i64, sh: i64, has_bg: bool) -> String {
    let mut o = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:sld {P_NS}><p:cSld><p:spTree>{GRP}");
    cx.next_id = 1;
    if has_bg {
        cx.next_id += 1;
        let _ = write!(
            o,
            "<p:pic><p:nvPicPr><p:cNvPr id=\"{}\" name=\"Page {}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId2\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{sw}\" cy=\"{sh}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>",
            cx.next_id, page.number
        );
    }
    for it in &page.items {
        match it {
            Item::Para(pa) => {
                let mut lines = pa.lines.clone();
                if let Kind::List {
                    ordered, marker, ..
                } = &pa.kind
                {
                    // the marker was taken off the text; put it back
                    if let Some(s) = lines.first_mut().and_then(|l| l.spans.first_mut()) {
                        let m = if *ordered {
                            marker.as_str()
                        } else {
                            "\u{2022}"
                        };
                        s.text = format!("{m} {}", s.text);
                    }
                }
                text_box(&lines, pa.align, cx, &mut o)
            }
            Item::Table(t) => {
                for row in &t.rows {
                    for c in row {
                        for l in &c.lines {
                            text_box(std::slice::from_ref(l), Align::Left, cx, &mut o);
                        }
                    }
                }
            }
            Item::Picture(_) => {}
        }
    }
    o.push_str("</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>");
    o
}

const THEME: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Office Theme\"><a:themeElements><a:clrScheme name=\"Office\"><a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1><a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1><a:dk2><a:srgbClr val=\"44546A\"/></a:dk2><a:lt2><a:srgbClr val=\"E7E6E6\"/></a:lt2><a:accent1><a:srgbClr val=\"4472C4\"/></a:accent1><a:accent2><a:srgbClr val=\"ED7D31\"/></a:accent2><a:accent3><a:srgbClr val=\"A5A5A5\"/></a:accent3><a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4><a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5><a:accent6><a:srgbClr val=\"70AD47\"/></a:accent6><a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink><a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink></a:clrScheme><a:fontScheme name=\"Office\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont><a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme><a:fmtScheme name=\"Office\"><a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w=\"6350\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln w=\"12700\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln><a:ln w=\"19050\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>";

pub fn pptx(pdf: &[u8], layout: &Layout) -> Result<Vec<u8>, String> {
    let (pw, ph) = layout
        .pages
        .iter()
        .map(|p| (p.width, p.height))
        .next()
        .unwrap_or((612.0, 792.0));
    // slide size limits: 1–56 inches per side
    let scale = {
        let s_max = 4032.0 / pw.max(ph).max(1.0);
        let s_min = 72.0 / pw.min(ph).max(1.0);
        1.0f32.min(s_max).max(s_min)
    };
    let (sw, sh) = ((pw * scale * EMU) as i64, (ph * scale * EMU) as i64);
    let stripped = strip_text(pdf);
    let transparent = stripped.is_none();
    let all: Vec<usize> = (0..layout.pages.len()).collect();
    let bgs = raster::render_pages(
        stripped.as_deref().unwrap_or(pdf),
        &all,
        144.0,
        raster::ImageFormat::Jpeg(85),
    )
    .unwrap_or_default();

    let mut entries: Vec<(String, Vec<u8>, bool)> = Vec::new();
    let mut ct_slides = String::new();
    let mut pres_rels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"theme/theme1.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps\" Target=\"presProps.xml\"/><Relationship Id=\"rId4\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/viewProps\" Target=\"viewProps.xml\"/><Relationship Id=\"rId5\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles\" Target=\"tableStyles.xml\"/>");
    let mut sld_ids = String::new();
    for (k, page) in layout.pages.iter().enumerate() {
        let n = k + 1;
        // each slide scales its own page to the slide (pages may differ)
        let page_scale =
            (sw as f32 / EMU / page.width.max(1.0)).min(sh as f32 / EMU / page.height.max(1.0));
        let mut cx = SlideCtx {
            scale: page_scale,
            next_id: 1,
            transparent,
        };
        let bg = bgs.get(k).map(|(_, b)| b.clone());
        let mut srels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>");
        let (pw_e, ph_e) = (
            (page.width * page_scale * EMU) as i64,
            (page.height * page_scale * EMU) as i64,
        );
        if let Some(b) = bg {
            let _ = write!(
                srels,
                "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/page{n}.jpg\"/>"
            );
            entries.push((format!("ppt/media/page{n}.jpg"), b, true));
        }
        srels.push_str("</Relationships>");
        let has_bg = entries
            .iter()
            .any(|e| e.0 == format!("ppt/media/page{n}.jpg"));
        entries.push((
            format!("ppt/slides/slide{n}.xml"),
            slide_xml(page, &mut cx, pw_e, ph_e, has_bg).into_bytes(),
            false,
        ));
        entries.push((
            format!("ppt/slides/_rels/slide{n}.xml.rels"),
            srels.into_bytes(),
            false,
        ));
        let _ = write!(
            ct_slides,
            "<Override PartName=\"/ppt/slides/slide{n}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"
        );
        let _ = write!(
            pres_rels,
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide{n}.xml\"/>",
            n + 5
        );
        let _ = write!(
            sld_ids,
            "<p:sldId id=\"{}\" r:id=\"rId{}\"/>",
            255 + n,
            n + 5
        );
    }
    pres_rels.push_str("</Relationships>");
    let presentation = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:presentation {P_NS} saveSubsetFonts=\"1\"><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst>{sld_ids}</p:sldIdLst><p:sldSz cx=\"{sw}\" cy=\"{sh}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/><p:defaultTextStyle><a:defPPr><a:defRPr lang=\"en-US\"/></a:defPPr></p:defaultTextStyle></p:presentation>"
    );
    let master = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:sldMaster {P_NS}><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg><p:spTree>{GRP}</p:spTree></p:cSld><p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/><p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr><a:defRPr sz=\"4400\"/></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr><a:defRPr sz=\"1800\"/></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz=\"1800\"/></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"
    );
    let layout_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:sldLayout {P_NS} type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\"><p:spTree>{GRP}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"
    );
    let rels = |items: &[(&str, &str)]| {
        let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
        for (k, (ty, target)) in items.iter().enumerate() {
            let _ = write!(
                s,
                "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/{ty}\" Target=\"{target}\"/>",
                k + 1
            );
        }
        s.push_str("</Relationships>");
        s
    };
    let content_types = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"jpg\" ContentType=\"image/jpeg\"/><Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/><Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/><Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/><Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/><Override PartName=\"/ppt/presProps.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presProps+xml\"/><Override PartName=\"/ppt/viewProps.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml\"/><Override PartName=\"/ppt/tableStyles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/><Override PartName=\"/docProps/app.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.extended-properties+xml\"/>{ct_slides}</Types>"
    );
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"ppt/presentation.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties\" Target=\"docProps/app.xml\"/></Relationships>";
    let pres_props = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:presentationPr {P_NS}/>"
    );
    let view_props = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:viewPr {P_NS}><p:normalViewPr><p:restoredLeft sz=\"15620\"/><p:restoredTop sz=\"94660\"/></p:normalViewPr><p:gridSpacing cx=\"76200\" cy=\"76200\"/></p:viewPr>");
    let table_styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<a:tblStyleLst xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" def=\"{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}\"/>";
    let mut out = vec![
        (
            "[Content_Types].xml".to_string(),
            content_types.into_bytes(),
            false,
        ),
        ("_rels/.rels".into(), root_rels.as_bytes().to_vec(), false),
        (
            "docProps/core.xml".into(),
            core_xml(&first_title(layout)).into_bytes(),
            false,
        ),
        (
            "docProps/app.xml".into(),
            APP_XML.as_bytes().to_vec(),
            false,
        ),
        (
            "ppt/presentation.xml".into(),
            presentation.into_bytes(),
            false,
        ),
        (
            "ppt/_rels/presentation.xml.rels".into(),
            pres_rels.into_bytes(),
            false,
        ),
        ("ppt/presProps.xml".into(), pres_props.into_bytes(), false),
        ("ppt/viewProps.xml".into(), view_props.into_bytes(), false),
        (
            "ppt/tableStyles.xml".into(),
            table_styles.as_bytes().to_vec(),
            false,
        ),
        (
            "ppt/theme/theme1.xml".into(),
            THEME.as_bytes().to_vec(),
            false,
        ),
        (
            "ppt/slideMasters/slideMaster1.xml".into(),
            master.into_bytes(),
            false,
        ),
        (
            "ppt/slideMasters/_rels/slideMaster1.xml.rels".into(),
            rels(&[
                ("slideLayout", "../slideLayouts/slideLayout1.xml"),
                ("theme", "../theme/theme1.xml"),
            ])
            .into_bytes(),
            false,
        ),
        (
            "ppt/slideLayouts/slideLayout1.xml".into(),
            layout_xml.into_bytes(),
            false,
        ),
        (
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels".into(),
            rels(&[("slideMaster", "../slideMasters/slideMaster1.xml")]).into_bytes(),
            false,
        ),
    ];
    out.extend(entries);
    zip(out)
}
