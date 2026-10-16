//! Text-shaped targets: plain text, Markdown, HTML (+ the XHTML body EPUB
//! reuses), RTF and the structured XML dump.

use super::super::extract::{Align, Item, Kind, Layout, Line, Picture, Span, Table};
use super::{colored, esc, half, hex, pictures, slots, Slot};
use base64::Engine as _;
use std::collections::HashMap;
use std::fmt::Write as _;

// ------------------------------------------------------------------ text

fn list_prefix(kind: &Kind) -> String {
    match kind {
        Kind::List {
            ordered,
            marker,
            level,
        } => {
            let m = if *ordered { marker.as_str() } else { "•" };
            format!("{}{m} ", "  ".repeat(*level as usize))
        }
        _ => String::new(),
    }
}

pub fn txt(layout: &Layout) -> String {
    let mut pages = Vec::new();
    for p in &layout.pages {
        let mut s = String::new();
        let mut prev_list = false;
        for it in &p.items {
            match it {
                Item::Para(pa) => {
                    let is_list = matches!(pa.kind, Kind::List { .. });
                    if !s.is_empty() && !(is_list && prev_list) {
                        s.push('\n');
                    }
                    s.push_str(&list_prefix(&pa.kind));
                    s.push_str(pa.text().trim());
                    s.push('\n');
                    prev_list = is_list;
                }
                Item::Table(t) => {
                    if !s.is_empty() {
                        s.push('\n');
                    }
                    for row in &t.rows {
                        let cells: Vec<String> = row
                            .iter()
                            .map(|c| c.text().replace(['\t', '\n'], " "))
                            .collect();
                        s.push_str(cells.join("\t").trim_end());
                        s.push('\n');
                    }
                    prev_list = false;
                }
                Item::Picture(_) => {}
            }
        }
        pages.push(s);
    }
    pages.join("\u{c}")
}

// -------------------------------------------------------------- markdown

fn md_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '|') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

fn md_runs(runs: &[Span]) -> String {
    let mut o = String::new();
    for r in runs {
        let t = r.text.as_str();
        let core = t.trim();
        if core.is_empty() {
            o.push_str(if t.is_empty() { "" } else { " " });
            continue;
        }
        let lead = &t[..t.len() - t.trim_start().len()];
        let trail = &t[t.trim_end().len()..];
        let mut body = if r.mono {
            format!("`{}`", core.replace('`', "'"))
        } else {
            md_escape(core)
        };
        if r.italic {
            body = format!("*{body}*");
        }
        if r.bold {
            body = format!("**{body}**");
        }
        o.push_str(lead);
        o.push_str(&body);
        o.push_str(trail);
    }
    // block-level markers at line start would change the meaning
    let t = o.trim_start();
    if t.starts_with(['#', '+', '-', '=']) {
        return format!("\\{}", t);
    }
    if let Some(i) = t.find(|c: char| !c.is_ascii_digit()) {
        let rest = &t[i..];
        if i > 0 && rest.starts_with(['.', ')']) && (rest.len() == 1 || rest[1..].starts_with(' '))
        {
            return format!("{}\\{}", &t[..i], &t[i..]);
        }
    }
    t.to_string()
}

fn md_cell(runs: &[Span], plain: bool) -> String {
    if plain {
        let t: String = runs.iter().map(|r| r.text.as_str()).collect();
        return md_escape(t.trim()).replace('\n', " ");
    }
    md_runs(runs).replace('\n', " ")
}

/// Returns the Markdown and the image files (`NN.ext`, linked as
/// `<img_prefix>NN.ext`).
pub fn markdown(layout: &Layout, img_prefix: &str) -> (String, Vec<(String, Vec<u8>)>) {
    let mut names: HashMap<usize, String> = HashMap::new();
    let mut files = Vec::new();
    for (k, p) in pictures(layout).into_iter().enumerate() {
        let n = format!("{:02}.{}", k + 1, p.ext);
        names.insert(p.id, n.clone());
        files.push((n, p.data.to_vec()));
    }
    let mut o = String::new();
    for (pi, p) in layout.pages.iter().enumerate() {
        if pi > 0 {
            let _ = write!(o, "\n<!-- page {} -->\n\n", p.number);
        }
        let mut prev_list = false;
        for it in &p.items {
            match it {
                Item::Para(pa) => {
                    let text = md_runs(&pa.runs());
                    match &pa.kind {
                        Kind::Heading(l) => {
                            if prev_list {
                                o.push('\n');
                            }
                            let plain = pa.text();
                            let _ = writeln!(
                                o,
                                "{} {}\n",
                                "#".repeat(*l as usize),
                                md_escape(plain.trim())
                            );
                            prev_list = false;
                        }
                        Kind::List {
                            ordered,
                            marker,
                            level,
                        } => {
                            let m = if *ordered {
                                let n: String =
                                    marker.chars().filter(char::is_ascii_digit).collect();
                                if n.is_empty() {
                                    "1.".to_string()
                                } else {
                                    format!("{n}.")
                                }
                            } else {
                                "-".to_string()
                            };
                            let _ = writeln!(o, "{}{m} {text}", "   ".repeat(*level as usize));
                            prev_list = true;
                        }
                        Kind::Body => {
                            if prev_list {
                                o.push('\n');
                            }
                            let _ = writeln!(o, "{text}\n");
                            prev_list = false;
                        }
                    }
                }
                Item::Table(t) => {
                    if prev_list {
                        o.push('\n');
                    }
                    md_table(t, &mut o);
                    prev_list = false;
                }
                Item::Picture(pic) => {
                    if prev_list {
                        o.push('\n');
                    }
                    if let Some(n) = names.get(&pic.id) {
                        let _ = writeln!(o, "![]({img_prefix}{n})\n");
                    }
                    prev_list = false;
                }
            }
        }
        if prev_list {
            o.push('\n');
        }
    }
    (o.trim_end().to_string() + "\n", files)
}

fn md_table(t: &Table, o: &mut String) {
    let nc = t.cols();
    if nc == 0 {
        return;
    }
    for (r, row) in t.rows.iter().enumerate() {
        o.push('|');
        for c in row {
            let _ = write!(o, " {} |", md_cell(&c.runs(), r == 0));
        }
        o.push('\n');
        if r == 0 {
            o.push('|');
            for _ in 0..nc {
                o.push_str(" --- |");
            }
            o.push('\n');
        }
    }
    o.push('\n');
}

// ------------------------------------------------------------------ html

/// `strong` = false where the element is bold anyway (headings, th).
fn html_runs_b(runs: &[Span], strong: bool) -> String {
    let mut o = String::new();
    for r in runs {
        let mut t = esc(&r.text);
        if r.mono {
            t = format!("<code>{t}</code>");
        }
        if r.italic {
            t = format!("<i>{t}</i>");
        }
        if r.bold && strong {
            t = format!("<b>{t}</b>");
        }
        if colored(r.color) && !r.invisible {
            t = format!("<span style=\"color:#{}\">{t}</span>", hex(r.color));
        }
        o.push_str(&t);
    }
    o
}

fn html_runs(runs: &[Span]) -> String {
    html_runs_b(runs, true)
}

fn align_css(a: Align) -> &'static str {
    match a {
        Align::Left => "",
        Align::Center => " style=\"text-align:center\"",
        Align::Right => " style=\"text-align:right\"",
        Align::Justify => " style=\"text-align:justify\"",
    }
}

/// XHTML for a run of items (shared by HTML and EPUB). Lists are nested
/// properly (`<ul>` inside the open `<li>`).
pub(super) fn xhtml_items<'a>(
    items: impl Iterator<Item = &'a Item>,
    img_src: &dyn Fn(&Picture) -> String,
    o: &mut String,
) {
    let mut stack: Vec<bool> = Vec::new(); // ordered?
    let close_all = |stack: &mut Vec<bool>, o: &mut String| {
        while let Some(ord) = stack.pop() {
            o.push_str(if ord { "</li></ol>\n" } else { "</li></ul>\n" });
        }
    };
    for it in items {
        match it {
            Item::Para(pa) => {
                if let Kind::List {
                    ordered,
                    marker,
                    level,
                } = &pa.kind
                {
                    let depth = (*level as usize + 1).min(stack.len() + 1);
                    while stack.len() > depth {
                        let ord = stack.pop().unwrap();
                        o.push_str(if ord { "</li></ol>\n" } else { "</li></ul>\n" });
                    }
                    let open = |o: &mut String| {
                        if *ordered {
                            let n: String = marker.chars().filter(char::is_ascii_digit).collect();
                            match n.parse::<u32>() {
                                Ok(k) if k > 1 => {
                                    let _ = write!(o, "<ol start=\"{k}\">");
                                }
                                _ => o.push_str("<ol>"),
                            }
                        } else {
                            o.push_str("<ul>");
                        }
                    };
                    if stack.len() == depth {
                        if stack.last() == Some(ordered) {
                            o.push_str("</li>\n");
                        } else {
                            let ord = stack.pop().unwrap();
                            o.push_str(if ord { "</li></ol>\n" } else { "</li></ul>\n" });
                            open(o);
                            stack.push(*ordered);
                        }
                    } else {
                        open(o);
                        stack.push(*ordered);
                    }
                    let _ = write!(o, "<li>{}", html_runs(&pa.runs()));
                    continue;
                }
                close_all(&mut stack, o);
                match pa.kind {
                    Kind::Heading(l) => {
                        let _ = writeln!(
                            o,
                            "<h{l}{}>{}</h{l}>",
                            align_css(pa.align),
                            html_runs_b(&pa.runs(), false)
                        );
                    }
                    _ => {
                        let _ =
                            writeln!(o, "<p{}>{}</p>", align_css(pa.align), html_runs(&pa.runs()));
                    }
                }
            }
            Item::Table(t) => {
                close_all(&mut stack, o);
                html_table(t, o);
            }
            Item::Picture(p) => {
                close_all(&mut stack, o);
                let w = (p.bbox.w() * 4.0 / 3.0).round().max(1.0);
                let _ = writeln!(
                    o,
                    "<p class=\"figure\"><img src=\"{}\" alt=\"\" style=\"width:{w}px\"/></p>",
                    esc(&img_src(p))
                );
            }
        }
    }
    close_all(&mut stack, o);
}

fn html_table(t: &Table, o: &mut String) {
    let sl = slots(t);
    let header = t.rows.len() > 1
        && t.rows[0]
            .iter()
            .all(|c| c.covered || c.bold() || c.lines.is_empty())
        && t.rows[0].iter().any(|c| c.bold());
    let _ = writeln!(
        o,
        "<table class=\"{}\">",
        if t.ruled { "ruled" } else { "plain" }
    );
    for (r, row) in t.rows.iter().enumerate() {
        o.push_str("<tr>");
        let tag = if header && r == 0 { "th" } else { "td" };
        for (c, cell) in row.iter().enumerate() {
            if sl[r][c] != Slot::Origin {
                continue;
            }
            let mut attrs = String::new();
            if cell.colspan > 1 {
                let _ = write!(attrs, " colspan=\"{}\"", cell.colspan);
            }
            if cell.rowspan > 1 {
                let _ = write!(attrs, " rowspan=\"{}\"", cell.rowspan);
            }
            let _ = write!(
                o,
                "<{tag}{attrs}>{}</{tag}>",
                html_runs_b(&cell.runs(), tag == "td")
            );
        }
        o.push_str("</tr>\n");
    }
    o.push_str("</table>\n");
}

pub(super) const CSS: &str = "body{font-family:Georgia,'Times New Roman',serif;line-height:1.45;max-width:46em;margin:2em auto;padding:0 1em;color:#111}\n\
h1,h2,h3{font-family:Helvetica,Arial,sans-serif;line-height:1.2}\n\
table{border-collapse:collapse;margin:1em 0}\n\
td,th{padding:.25em .6em;vertical-align:top;text-align:left}\n\
table.ruled td,table.ruled th{border:1px solid #888}\n\
img{max-width:100%;height:auto}\n\
p.figure{text-align:center}\n\
section.page{page-break-after:always}\n\
hr.page{border:0;border-top:1px dashed #bbb;margin:2em 0}\n";

pub fn html(layout: &Layout, title: &str) -> String {
    let mut o = String::new();
    let _ = write!(
        o,
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\"/>\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\n<title>{}</title>\n<style>\n{CSS}</style>\n</head>\n<body>\n",
        esc(title)
    );
    let src = |p: &Picture| {
        format!(
            "data:{};base64,{}",
            p.mime(),
            base64::engine::general_purpose::STANDARD.encode(p.data.as_slice())
        )
    };
    for (i, p) in layout.pages.iter().enumerate() {
        if i > 0 {
            o.push_str("<hr class=\"page\"/>\n");
        }
        let _ = writeln!(o, "<section class=\"page\" id=\"page-{}\">", p.number);
        xhtml_items(p.items.iter(), &src, &mut o);
        o.push_str("</section>\n");
    }
    o.push_str("</body>\n</html>\n");
    o
}

// ------------------------------------------------------------------- rtf

fn rtf_text(s: &str, o: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '{' => o.push_str("\\{"),
            '}' => o.push_str("\\}"),
            '\n' => o.push_str("\\line "),
            '\t' => o.push_str("\\tab "),
            c if (c as u32) < 0x20 => {}
            c if (c as u32) < 0x80 => o.push(c),
            c => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(o, "\\u{}?", *u as i16);
                }
            }
        }
    }
}

struct RtfTables {
    fonts: Vec<String>,
    colors: Vec<[u8; 3]>,
}

impl RtfTables {
    fn font(&mut self, f: &str) -> usize {
        match self.fonts.iter().position(|x| x == f) {
            Some(i) => i,
            None => {
                self.fonts.push(f.to_string());
                self.fonts.len() - 1
            }
        }
    }
    /// 1-based colour index (0 = auto)
    fn color(&mut self, c: [u8; 3]) -> usize {
        if !colored(c) {
            return 0;
        }
        match self.colors.iter().position(|x| *x == c) {
            Some(i) => i + 1,
            None => {
                self.colors.push(c);
                self.colors.len()
            }
        }
    }
}

fn rtf_runs(runs: &[Span], t: &mut RtfTables, o: &mut String) {
    for r in runs {
        let f = t.font(&r.font);
        let _ = write!(o, "{{\\f{f}\\fs{}", (half(r.size) * 2.0) as u32);
        if r.bold {
            o.push_str("\\b");
        }
        if r.italic {
            o.push_str("\\i");
        }
        let c = t.color(r.color);
        if c > 0 {
            let _ = write!(o, "\\cf{c}");
        }
        if r.invisible {
            o.push_str("\\v");
        }
        o.push(' ');
        rtf_text(&r.text, o);
        o.push('}');
    }
}

fn rtf_pict(p: &Picture, o: &mut String) {
    let kind = if p.ext == "jpg" {
        "jpegblip"
    } else {
        "pngblip"
    };
    let (tw, th) = ((p.bbox.w() * 20.0) as u32, (p.bbox.h() * 20.0) as u32);
    let _ = write!(
        o,
        "{{\\pict\\{kind}\\picw{}\\pich{}\\picwgoal{tw}\\pichgoal{th}\n",
        p.px.0, p.px.1
    );
    for chunk in p.data.chunks(64) {
        for b in chunk {
            let _ = write!(o, "{b:02x}");
        }
        o.push('\n');
    }
    o.push('}');
}

pub fn rtf(layout: &Layout) -> String {
    let mut tabs = RtfTables {
        fonts: vec!["Times New Roman".into()],
        colors: vec![],
    };
    let mut body = String::new();
    for (pi, p) in layout.pages.iter().enumerate() {
        if pi > 0 {
            body.push_str("\\page\n");
        }
        for it in &p.items {
            match it {
                Item::Para(pa) => {
                    body.push_str("\\pard\\plain\\sa120");
                    match &pa.kind {
                        Kind::Heading(l) => {
                            let _ = write!(body, "\\s{l}\\outlinelevel{}\\keepn\\sb240", l - 1);
                        }
                        Kind::List { level, .. } => {
                            let _ = write!(body, "\\fi-360\\li{}", 360 * (*level as u32 + 1) + 360);
                        }
                        Kind::Body => {}
                    }
                    body.push_str(match pa.align {
                        Align::Left => "\\ql",
                        Align::Center => "\\qc",
                        Align::Right => "\\qr",
                        Align::Justify => "\\qj",
                    });
                    body.push(' ');
                    if let Kind::List {
                        ordered, marker, ..
                    } = &pa.kind
                    {
                        let m = if *ordered {
                            marker.as_str()
                        } else {
                            "\u{2022}"
                        };
                        rtf_text(m, &mut body);
                        body.push_str("\\tab ");
                    }
                    rtf_runs(&pa.runs(), &mut tabs, &mut body);
                    body.push_str("\\par\n");
                }
                Item::Table(t) => rtf_table(t, &mut tabs, &mut body),
                Item::Picture(pic) => {
                    body.push_str("\\pard\\plain\\qc ");
                    rtf_pict(pic, &mut body);
                    body.push_str("\\par\n");
                }
            }
        }
    }
    let (w, h) = layout
        .pages
        .first()
        .map(|p| (p.width, p.height))
        .unwrap_or((612.0, 792.0));
    let mut o = String::from("{\\rtf1\\ansi\\ansicpg1252\\deff0\\uc1\n{\\fonttbl");
    for (i, f) in tabs.fonts.iter().enumerate() {
        let _ = write!(o, "{{\\f{i}\\fnil ");
        rtf_text(f, &mut o);
        o.push_str(";}");
    }
    o.push_str("}\n{\\colortbl ;");
    for c in &tabs.colors {
        let _ = write!(o, "\\red{}\\green{}\\blue{};", c[0], c[1], c[2]);
    }
    o.push_str("}\n{\\stylesheet{\\s0 Normal;}");
    for (l, sz) in [(1, 32), (2, 28), (3, 24)] {
        let _ = write!(
            o,
            "{{\\s{l}\\outlinelevel{}\\b\\fs{sz} heading {l};}}",
            l - 1
        );
    }
    let _ = write!(
        o,
        "}}\n\\paperw{}\\paperh{}\\margl1080\\margr1080\\margt1080\\margb1080\n",
        (w * 20.0) as u32,
        (h * 20.0) as u32
    );
    o.push_str(&body);
    o.push_str("}\n");
    o
}

fn rtf_table(t: &Table, tabs: &mut RtfTables, o: &mut String) {
    let sl = slots(t);
    let scale = table_scale(t);
    for (r, row) in t.rows.iter().enumerate() {
        o.push_str("\\trowd\\trgaph70\\trleft0");
        let mut x = 0.0f32;
        for (c, _) in row.iter().enumerate() {
            x += t.widths.get(c).copied().unwrap_or(72.0) * scale;
            match sl[r][c] {
                Slot::Origin => {
                    if t.rows[r][c].rowspan > 1 {
                        o.push_str("\\clvmgf");
                    }
                    if t.rows[r][c].colspan > 1 {
                        o.push_str("\\clmgf");
                    }
                }
                Slot::VCont { .. } => o.push_str("\\clvmrg"),
                Slot::Skip => o.push_str("\\clmrg"),
            }
            if t.ruled {
                o.push_str("\\clbrdrt\\brdrs\\brdrw10\\clbrdrl\\brdrs\\brdrw10\\clbrdrb\\brdrs\\brdrw10\\clbrdrr\\brdrs\\brdrw10");
            }
            let _ = write!(o, "\\cellx{}", (x * 20.0) as u32);
        }
        o.push('\n');
        for (c, cell) in row.iter().enumerate() {
            o.push_str("\\pard\\plain\\intbl ");
            if sl[r][c] == Slot::Origin {
                rtf_runs(&cell.runs(), tabs, o);
            }
            o.push_str("\\cell\n");
        }
        o.push_str("\\row\n");
    }
    o.push_str("\\pard\\plain\\par\n");
}

/// Shrink tables wider than a printable 6.5in line.
pub(super) fn table_scale(t: &Table) -> f32 {
    let w: f32 = t.widths.iter().sum();
    if w > 468.0 {
        468.0 / w
    } else {
        1.0
    }
}

// ------------------------------------------------------------------- xml

fn f1(v: f32) -> String {
    let r = (v * 10.0).round() / 10.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        format!("{r:.1}")
    }
}

fn xml_line(l: &Line, ind: &str, o: &mut String) {
    let _ = writeln!(
        o,
        "{ind}<line x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" baseline=\"{}\">",
        f1(l.bbox.x0),
        f1(l.bbox.y0),
        f1(l.bbox.w()),
        f1(l.bbox.h()),
        f1(l.baseline)
    );
    for s in &l.spans {
        let mut a = format!(
            "font=\"{}\" size=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
            esc(&s.font),
            f1(s.size),
            f1(s.bbox.x0),
            f1(s.bbox.y0),
            f1(s.bbox.w()),
            f1(s.bbox.h())
        );
        if s.bold {
            a.push_str(" bold=\"true\"");
        }
        if s.italic {
            a.push_str(" italic=\"true\"");
        }
        if s.mono {
            a.push_str(" monospace=\"true\"");
        }
        if s.color != [0, 0, 0] {
            let _ = write!(a, " color=\"#{}\"", hex(s.color));
        }
        if s.invisible {
            a.push_str(" invisible=\"true\"");
        }
        let _ = writeln!(o, "{ind}  <span {a}>{}</span>", esc(&s.text));
    }
    let _ = writeln!(o, "{ind}</line>");
}

pub fn xml(layout: &Layout, source: &str) -> String {
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        o,
        "<document source=\"{}.pdf\" pages=\"{}\" body-size=\"{}\" units=\"pt\" origin=\"top-left\">",
        esc(source),
        layout.pages.len(),
        f1(layout.body_size)
    );
    for p in &layout.pages {
        let _ = writeln!(
            o,
            "  <page number=\"{}\" width=\"{}\" height=\"{}\">",
            p.number,
            f1(p.width),
            f1(p.height)
        );
        for it in &p.items {
            match it {
                Item::Para(pa) => {
                    let b = pa.bbox;
                    let geo = format!(
                        "x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" align=\"{}\"",
                        f1(b.x0),
                        f1(b.y0),
                        f1(b.w()),
                        f1(b.h()),
                        match pa.align {
                            Align::Left => "left",
                            Align::Center => "center",
                            Align::Right => "right",
                            Align::Justify => "justify",
                        }
                    );
                    let (tag, extra) = match &pa.kind {
                        Kind::Body => ("paragraph", String::new()),
                        Kind::Heading(l) => ("heading", format!(" level=\"{l}\"")),
                        Kind::List {
                            ordered,
                            marker,
                            level,
                        } => (
                            "list-item",
                            format!(
                                " ordered=\"{ordered}\" marker=\"{}\" level=\"{level}\"",
                                esc(marker)
                            ),
                        ),
                    };
                    let _ = writeln!(o, "    <{tag}{extra} {geo}>");
                    let _ = writeln!(o, "      <text>{}</text>", esc(pa.text().trim()));
                    for l in &pa.lines {
                        xml_line(l, "      ", &mut o);
                    }
                    let _ = writeln!(o, "    </{tag}>");
                }
                Item::Table(t) => {
                    let b = t.bbox;
                    let _ = writeln!(
                        o,
                        "    <table rows=\"{}\" columns=\"{}\" ruled=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\">",
                        t.rows.len(),
                        t.cols(),
                        t.ruled,
                        f1(b.x0),
                        f1(b.y0),
                        f1(b.w()),
                        f1(b.h())
                    );
                    for row in &t.rows {
                        o.push_str("      <row>\n");
                        for c in row {
                            if c.covered {
                                o.push_str("        <cell covered=\"true\"/>\n");
                                continue;
                            }
                            let mut a = String::new();
                            if c.colspan > 1 {
                                let _ = write!(a, " colspan=\"{}\"", c.colspan);
                            }
                            if c.rowspan > 1 {
                                let _ = write!(a, " rowspan=\"{}\"", c.rowspan);
                            }
                            let _ = writeln!(o, "        <cell{a}>");
                            let _ = writeln!(o, "          <text>{}</text>", esc(&c.text()));
                            for l in &c.lines {
                                xml_line(l, "          ", &mut o);
                            }
                            o.push_str("        </cell>\n");
                        }
                        o.push_str("      </row>\n");
                    }
                    o.push_str("    </table>\n");
                }
                Item::Picture(pic) => {
                    let b = pic.bbox;
                    let _ = writeln!(
                        o,
                        "    <image id=\"{}\" format=\"{}\" pixel-width=\"{}\" pixel-height=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/>",
                        pic.id + 1,
                        if pic.ext == "jpg" { "jpeg" } else { "png" },
                        pic.px.0,
                        pic.px.1,
                        f1(b.x0),
                        f1(b.y0),
                        f1(b.w()),
                        f1(b.h())
                    );
                }
            }
        }
        o.push_str("  </page>\n");
    }
    o.push_str("</document>\n");
    o
}
