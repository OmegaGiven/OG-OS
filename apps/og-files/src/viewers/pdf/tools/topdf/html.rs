//! HTML → document blocks. This is a document converter, not a browser:
//! semantic tags (headings, paragraphs, lists, tables, quotes, code,
//! images, links) and the inline `style="…"` basics (colour, weight,
//! italics, decoration, size, text-align, display:none) are honoured;
//! stylesheets, scripts, floats and positioning are not.
//!
//! Shared by HTML files, web pages, Markdown (rendered to HTML first),
//! e-mail bodies, EPUB/MOBI chapters and FB2.

use super::ir::{Align, Block, Cell, Files, Img, Inline, Para, Style, Table};
use base64::Engine as _;
use scraper::{ElementRef, Html, Node};

/// Maps an image reference (relative path, URL, cid:…) to its bytes.
pub type Fetch<'a> = dyn FnMut(&str) -> Option<Vec<u8>> + 'a;

pub struct Out {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

/// nesting depth of lists / quotes / tables kept as such; deeper content is
/// flattened (Typst and readers alike choke on absurd nesting)
const MAX_NEST: usize = 14;
const MAX_IMAGES: usize = 2000;
/// element depth walked at all (stack guard)
const MAX_DEPTH: usize = 400;

struct Cx<'a, 'f> {
    files: &'a mut Files,
    fetch: &'a mut Fetch<'f>,
    /// absolute base for relative links (web pages)
    link_base: Option<url::Url>,
    base_size: f32,
    nest: usize,
    depth: usize,
    images: usize,
}

#[derive(Clone)]
struct Inh {
    st: Style,
    pre: bool,
    align: Align,
}

#[derive(Default)]
struct Flow {
    blocks: Vec<Block>,
    inl: Vec<Inline>,
    align: Align,
}

fn ends_blank(inl: &[Inline]) -> bool {
    match inl.last() {
        None | Some(Inline::Break) => true,
        Some(Inline::Text(t, _)) => t.ends_with(' ') || t.ends_with('\n'),
        Some(Inline::Link(_, inner)) => ends_blank(inner) && !inner.is_empty(),
        Some(Inline::Image(_)) => false,
    }
}

fn trim_end(inl: &mut Vec<Inline>) {
    while let Some(last) = inl.last_mut() {
        match last {
            Inline::Text(t, _) => {
                let n = t.trim_end_matches([' ', '\n']).len();
                t.truncate(n);
                if t.is_empty() {
                    inl.pop();
                    continue;
                }
            }
            Inline::Break => {
                inl.pop();
                continue;
            }
            _ => {}
        }
        break;
    }
}

impl Flow {
    fn text(&mut self, s: &str, st: &Style, pre: bool) {
        let t = if pre {
            s.replace("\r\n", "\n")
        } else {
            let mut o = String::with_capacity(s.len());
            let mut ws = false;
            for c in s.chars() {
                if c.is_whitespace() && c != '\u{a0}' {
                    ws = true;
                } else {
                    if ws {
                        o.push(' ');
                    }
                    ws = false;
                    o.push(c);
                }
            }
            if ws {
                o.push(' ');
            }
            if o.starts_with(' ') && ends_blank(&self.inl) {
                o.remove(0);
            }
            o
        };
        if t.is_empty() {
            return;
        }
        if let Some(Inline::Text(prev, pst)) = self.inl.last_mut() {
            if pst == st {
                prev.push_str(&t);
                return;
            }
        }
        self.inl.push(Inline::Text(t, st.clone()));
    }

    fn flush(&mut self) {
        trim_end(&mut self.inl);
        if self.inl.is_empty() {
            return;
        }
        let inl = std::mem::take(&mut self.inl);
        self.blocks.push(Block::Para(Para {
            inl,
            align: self.align,
            ..Default::default()
        }));
    }

    fn push(&mut self, b: Block) {
        self.flush();
        self.blocks.push(b);
    }

    /// Everything as one run of inlines (headings, captions).
    fn into_inline(mut self) -> (Vec<Inline>, Vec<Block>) {
        self.flush();
        let mut inl = Vec::new();
        let mut rest = Vec::new();
        for b in self.blocks {
            match b {
                Block::Para(p) => {
                    if !inl.is_empty() {
                        inl.push(Inline::Text(" ".into(), Style::default()));
                    }
                    inl.extend(p.inl);
                }
                other => rest.push(other),
            }
        }
        (inl, rest)
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        self.blocks
    }
}

// ------------------------------------------------------------------ CSS

const NAMED: [(&str, [u8; 3]); 18] = [
    ("black", [0, 0, 0]),
    ("white", [255, 255, 255]),
    ("red", [255, 0, 0]),
    ("green", [0, 128, 0]),
    ("blue", [0, 0, 255]),
    ("yellow", [255, 255, 0]),
    ("orange", [255, 165, 0]),
    ("purple", [128, 0, 128]),
    ("gray", [128, 128, 128]),
    ("grey", [128, 128, 128]),
    ("silver", [192, 192, 192]),
    ("maroon", [128, 0, 0]),
    ("navy", [0, 0, 128]),
    ("teal", [0, 128, 128]),
    ("olive", [128, 128, 0]),
    ("lime", [0, 255, 0]),
    ("aqua", [0, 255, 255]),
    ("fuchsia", [255, 0, 255]),
];

pub fn parse_color(v: &str) -> Option<[u8; 3]> {
    let v = v
        .trim()
        .trim_end_matches("!important")
        .trim()
        .to_ascii_lowercase();
    if let Some(h) = v.strip_prefix('#') {
        let h: String = h.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let p = |s: &str| u8::from_str_radix(s, 16).ok();
        return match h.len() {
            3 | 4 => {
                let d: Vec<u8> = h
                    .chars()
                    .filter_map(|c| c.to_digit(16))
                    .map(|d| d as u8 * 17)
                    .collect();
                Some([d[0], d[1], d[2]])
            }
            6 | 8 => Some([p(&h[0..2])?, p(&h[2..4])?, p(&h[4..6])?]),
            _ => None,
        };
    }
    if let Some(args) = v.strip_prefix("rgb(").or_else(|| v.strip_prefix("rgba(")) {
        let nums: Vec<f32> = args
            .trim_end_matches(')')
            .split([',', ' ', '/'])
            .filter(|s| !s.is_empty())
            .take(3)
            .filter_map(|s| {
                if let Some(p) = s.strip_suffix('%') {
                    p.parse::<f32>().ok().map(|x| x * 2.55)
                } else {
                    s.parse::<f32>().ok()
                }
            })
            .collect();
        if nums.len() == 3 {
            let c = |x: f32| x.clamp(0.0, 255.0) as u8;
            return Some([c(nums[0]), c(nums[1]), c(nums[2])]);
        }
        return None;
    }
    NAMED.iter().find(|(n, _)| *n == v).map(|(_, c)| *c)
}

fn luma(c: [u8; 3]) -> f32 {
    (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0
}

/// CSS length → points, relative to `cur` (the current font size).
fn length(v: &str, cur: f32) -> Option<f32> {
    let v = v.trim().to_ascii_lowercase();
    let num = |s: &str| s.trim().parse::<f32>().ok().filter(|x| x.is_finite());
    let pts = if let Some(n) = v.strip_suffix("px") {
        num(n)? * 0.75
    } else if let Some(n) = v.strip_suffix("pt") {
        num(n)?
    } else if let Some(n) = v.strip_suffix("rem") {
        num(n)? * 11.0
    } else if let Some(n) = v.strip_suffix("em") {
        num(n)? * cur
    } else if let Some(n) = v.strip_suffix('%') {
        num(n)? / 100.0 * cur
    } else {
        match v.as_str() {
            "xx-small" => 7.0,
            "x-small" => 8.0,
            "small" | "smaller" => cur * 0.85,
            "medium" => 11.0,
            "large" | "larger" => cur * 1.2,
            "x-large" => 16.0,
            "xx-large" => 20.0,
            _ => return None,
        }
    };
    Some(pts.clamp(4.0, 96.0))
}

#[derive(Default)]
struct Css {
    hidden: bool,
    align: Option<Align>,
    bg: Option<[u8; 3]>,
}

fn apply_css(decl: &str, st: &mut Style, base: f32) -> Css {
    let mut css = Css::default();
    for d in decl.split(';') {
        let Some((k, v)) = d.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_end_matches("!important").trim();
        let lv = v.to_ascii_lowercase();
        match k.as_str() {
            "display" if lv == "none" => css.hidden = true,
            "visibility" if lv == "hidden" => css.hidden = true,
            "color" => {
                // the page is white: drop near-white text meant for dark
                // backgrounds we don't paint
                st.color = parse_color(v).filter(|c| luma(*c) < 0.8);
            }
            "background-color" | "background" => {
                css.bg = v
                    .split_whitespace()
                    .find_map(parse_color)
                    .or_else(|| parse_color(v));
            }
            "font-weight" => {
                st.bold = matches!(lv.as_str(), "bold" | "bolder")
                    || lv.parse::<u32>().is_ok_and(|w| w >= 600);
            }
            "font-style" => st.italic = lv.starts_with("italic") || lv.starts_with("oblique"),
            "text-decoration" | "text-decoration-line" => {
                if lv.contains("none") {
                    st.underline = false;
                    st.strike = false;
                }
                if lv.contains("underline") {
                    st.underline = true;
                }
                if lv.contains("line-through") {
                    st.strike = true;
                }
            }
            "text-align" => {
                css.align = match lv.as_str() {
                    "center" => Some(Align::Center),
                    "right" | "end" => Some(Align::Right),
                    "justify" => Some(Align::Justify),
                    "left" | "start" => Some(Align::Left),
                    _ => None,
                }
            }
            "font-size" => {
                if let Some(p) = length(&lv, st.size.unwrap_or(base)) {
                    st.size = Some(p);
                }
            }
            "font-family" => {
                if ["mono", "courier", "consolas", "menlo"]
                    .iter()
                    .any(|m| lv.contains(m))
                {
                    st.mono = true;
                }
            }
            "vertical-align" => match lv.as_str() {
                "super" => st.sup = true,
                "sub" => st.sub = true,
                _ => {}
            },
            _ => {}
        }
    }
    css
}

fn align_attr(v: Option<&str>) -> Option<Align> {
    match v?.trim().to_ascii_lowercase().as_str() {
        "center" | "middle" => Some(Align::Center),
        "right" => Some(Align::Right),
        "justify" => Some(Align::Justify),
        "left" => Some(Align::Left),
        _ => None,
    }
}

/// `data:` URI payload.
pub fn data_uri(src: &str) -> Option<Vec<u8>> {
    use base64::engine::{
        general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig,
    };
    let rest = src.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if meta.to_ascii_lowercase().ends_with(";base64") {
        let clean: String = percent_decode(data)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let engine = GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
        );
        engine.decode(clean).ok()
    } else {
        Some(percent_decode(data).into_bytes())
    }
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ walk

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "body"
            | "center"
            | "details"
            | "dialog"
            | "div"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "header"
            | "hgroup"
            | "html"
            | "legend"
            | "main"
            | "nav"
            | "p"
            | "section"
            | "summary"
            | "caption"
            | "tbody"
            | "thead"
            | "tfoot"
            // FB2 / EPUB leftovers
            | "poem"
            | "stanza"
            | "v"
            | "epigraph"
            | "text-author"
    )
}

const SKIP: [&str; 20] = [
    "head", "script", "style", "noscript", "template", "iframe", "object", "embed", "select",
    "option", "input", "textarea", "canvas", "audio", "video", "map", "meta", "link", "title",
    "button",
];

impl Cx<'_, '_> {
    fn walk(&mut self, e: ElementRef<'_>, inh: &Inh, flow: &mut Flow) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        for child in e.children() {
            match child.value() {
                Node::Text(t) => flow.text(t, &inh.st, inh.pre),
                Node::Element(_) => {
                    if let Some(el) = ElementRef::wrap(child) {
                        self.element(el, inh, flow);
                    }
                }
                _ => {}
            }
        }
        self.depth -= 1;
    }

    fn image(&mut self, src: &str) -> Option<Img> {
        if self.images >= MAX_IMAGES {
            return None;
        }
        let src = src.trim();
        let bytes = if src.starts_with("data:") {
            data_uri(src)?
        } else {
            (self.fetch)(src)?
        };
        let img = self.files.add(bytes)?;
        self.images += 1;
        Some(img)
    }

    fn sized(img: Img, e: ElementRef<'_>) -> Img {
        let px = |a: &str| {
            e.attr(a)
                .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
        };
        let mut img = img;
        match (px("width"), px("height")) {
            (Some(w), _) => {
                let w = w * 0.75;
                img.h = img.h * w / img.w.max(0.01);
                img.w = w;
            }
            (None, Some(h)) => {
                let h = h * 0.75;
                img.w = img.w * h / img.h.max(0.01);
                img.h = h;
            }
            _ => {}
        }
        img
    }

    fn link(&self, href: &str) -> Option<String> {
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
            return None;
        }
        if let Some(base) = &self.link_base {
            return base.join(href).ok().map(|u| u.to_string());
        }
        let lower = href.to_ascii_lowercase();
        (lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("mailto:"))
        .then(|| href.to_string())
    }

    fn element(&mut self, e: ElementRef<'_>, inh: &Inh, flow: &mut Flow) {
        let name = e.value().name().to_ascii_lowercase();
        let name = name.as_str();
        if SKIP.contains(&name) || e.attr("hidden").is_some() {
            return;
        }
        let mut inh2 = inh.clone();
        let cur = inh.st.size.unwrap_or(self.base_size);
        let css = e
            .attr("style")
            .map(|s| apply_css(s, &mut inh2.st, self.base_size))
            .unwrap_or_default();
        if css.hidden {
            return;
        }
        let st = &mut inh2.st;
        match name {
            "b" | "strong" => st.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" | "emphasis" => st.italic = true,
            "u" | "ins" => st.underline = true,
            "s" | "strike" | "del" | "strikethrough" => st.strike = true,
            "code" | "kbd" | "samp" | "tt" => st.mono = true,
            "sup" => st.sup = true,
            "sub" => st.sub = true,
            "mark" => st.highlight = Some([255, 240, 120]),
            "small" => st.size = Some(cur * 0.85),
            "big" => st.size = Some(cur * 1.2),
            "font" => {
                if let Some(c) = e
                    .attr("color")
                    .and_then(parse_color)
                    .filter(|c| luma(*c) < 0.8)
                {
                    st.color = Some(c);
                }
                if let Some(n) = e.attr("size").and_then(|s| s.trim().parse::<i32>().ok()) {
                    let px = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
                    st.size = Some(px[(n.clamp(1, 7) - 1) as usize] * 0.75);
                }
            }
            "span" | "a" => {
                if let Some(bg) = css.bg.filter(|c| luma(*c) < 0.97) {
                    st.highlight = Some(bg);
                }
            }
            _ => {}
        }
        if let Some(a) = css.align.or_else(|| align_attr(e.attr("align"))) {
            inh2.align = a;
        }
        match name {
            "br" => flow.inl.push(Inline::Break),
            "empty-line" => flow.flush(),
            "hr" => flow.push(Block::Rule),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "title-fb2" => {
                let level = name[1..].parse::<u8>().unwrap_or(2);
                flow.flush();
                let mut tmp = Flow::default();
                self.walk(e, &inh2, &mut tmp);
                let (inl, rest) = tmp.into_inline();
                if !inl.is_empty() {
                    flow.blocks.push(Block::Heading(level, inl));
                }
                flow.blocks.extend(rest);
            }
            "pre" | "listing" | "xmp" | "plaintext" => {
                flow.flush();
                let mut text: String = e.text().collect();
                if text.starts_with('\n') {
                    text.remove(0);
                }
                let text = text.trim_end().replace("\r\n", "\n");
                if !text.is_empty() {
                    flow.blocks.push(Block::Code { text, shaded: true });
                }
            }
            "blockquote" if self.nest < MAX_NEST => {
                flow.flush();
                self.nest += 1;
                let mut tmp = Flow {
                    align: inh2.align,
                    ..Default::default()
                };
                self.walk(e, &inh2, &mut tmp);
                self.nest -= 1;
                let bs = tmp.finish();
                if !bs.is_empty() {
                    flow.blocks.push(Block::Quote(bs));
                }
            }
            "ul" | "ol" | "menu" | "dir" if self.nest < MAX_NEST => {
                flow.flush();
                self.nest += 1;
                let mut items: Vec<Vec<Block>> = Vec::new();
                for child in e.children() {
                    let Some(el) = ElementRef::wrap(child) else {
                        if let Node::Text(t) = child.value() {
                            if !t.trim().is_empty() {
                                items.push(vec![Block::plain(t.trim())]);
                            }
                        }
                        continue;
                    };
                    let mut tmp = Flow {
                        align: inh2.align,
                        ..Default::default()
                    };
                    if el.value().name().eq_ignore_ascii_case("li") {
                        self.walk(el, &inh2, &mut tmp);
                        items.push(tmp.finish());
                    } else {
                        // stray nested list etc.: belongs to the previous item
                        self.element(el, &inh2, &mut tmp);
                        let bs = tmp.finish();
                        match items.last_mut() {
                            Some(last) => last.extend(bs),
                            None if !bs.is_empty() => items.push(bs),
                            None => {}
                        }
                    }
                }
                self.nest -= 1;
                if !items.is_empty() {
                    let start = e
                        .attr("start")
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .unwrap_or(1);
                    flow.blocks.push(Block::List {
                        ordered: name == "ol",
                        start,
                        items,
                    });
                }
            }
            "li" => {
                // outside a list
                flow.flush();
                flow.inl.push(Inline::Text("• ".into(), Style::default()));
                self.walk(e, &inh2, flow);
                flow.flush();
            }
            "dl" => {
                flow.flush();
                self.walk(e, &inh2, flow);
                flow.flush();
            }
            "dt" => {
                flow.flush();
                inh2.st.bold = true;
                self.walk(e, &inh2, flow);
                flow.flush();
            }
            "dd" if self.nest < MAX_NEST => {
                flow.flush();
                self.nest += 1;
                let mut tmp = Flow::default();
                self.walk(e, &inh2, &mut tmp);
                self.nest -= 1;
                let bs = tmp.finish();
                if !bs.is_empty() {
                    flow.blocks.push(Block::Indent(18.0, bs));
                }
            }
            "table" => self.table(e, &inh2, flow),
            "img" | "image" => {
                let src = e
                    .attr("src")
                    .or_else(|| e.attr("data-src"))
                    .or_else(|| e.attr("href"))
                    .or_else(|| e.attr("xlink:href"))
                    .or_else(|| {
                        e.value()
                            .attrs()
                            .find(|(k, _)| k.ends_with("href"))
                            .map(|(_, v)| v)
                    });
                match src.and_then(|s| self.image(s)) {
                    Some(img) => flow.inl.push(Inline::Image(Self::sized(img, e))),
                    None => {
                        if let Some(alt) = e.attr("alt").map(str::trim).filter(|a| !a.is_empty()) {
                            let st = Style {
                                italic: true,
                                color: Some([110, 110, 110]),
                                ..inh2.st.clone()
                            };
                            flow.text(&format!("[{alt}]"), &st, false);
                        }
                    }
                }
            }
            "svg" => self.svg(e, flow),
            "a" => {
                let href = e.attr("href").or_else(|| {
                    e.value()
                        .attrs()
                        .find(|(k, _)| k.ends_with("href"))
                        .map(|(_, v)| v)
                });
                match href.and_then(|h| self.link(h)) {
                    Some(url) => {
                        let saved = std::mem::take(&mut flow.inl);
                        self.walk(e, &inh2, flow);
                        trim_end(&mut flow.inl);
                        let inner = std::mem::replace(&mut flow.inl, saved);
                        if !inner.is_empty() {
                            flow.inl.push(Inline::Link(url, inner));
                        }
                    }
                    None => self.walk(e, &inh2, flow),
                }
            }
            "q" => {
                flow.text("“", &inh2.st, false);
                self.walk(e, &inh2, flow);
                flow.text("”", &inh2.st, false);
            }
            // block containers, and the nesting tags above once too deep
            n if is_block(n) || matches!(n, "blockquote" | "ul" | "ol" | "menu" | "dir" | "dd") => {
                flow.flush();
                let saved = flow.align;
                flow.align = inh2.align;
                if n == "figcaption" || n == "caption" {
                    inh2.st.italic = true;
                }
                self.walk(e, &inh2, flow);
                flow.flush();
                flow.align = saved;
            }
            _ => self.walk(e, &inh2, flow),
        }
    }

    /// Inline `<svg>`: kept vector as an image of its own. An SVG that only
    /// wraps a bitmap (EPUB cover pages) becomes that bitmap.
    fn svg(&mut self, e: ElementRef<'_>, flow: &mut Flow) {
        let kids: Vec<ElementRef> = e.descendent_elements().skip(1).collect();
        if let [only] = kids.as_slice() {
            if only.value().name().eq_ignore_ascii_case("image") {
                let href = only
                    .value()
                    .attrs()
                    .find(|(k, _)| k.ends_with("href"))
                    .map(|(_, v)| v.to_string());
                if let Some(img) = href.and_then(|h| self.image(&h)) {
                    flow.inl.push(Inline::Image(img));
                }
                return;
            }
        }
        let mut s = e.html();
        if !s.contains("xmlns=") {
            s = s.replacen("<svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"", 1);
        }
        if self.images < MAX_IMAGES {
            if let Some(img) = self.files.add(s.into_bytes()) {
                self.images += 1;
                flow.inl.push(Inline::Image(img));
            }
        }
    }

    fn table(&mut self, e: ElementRef<'_>, inh: &Inh, flow: &mut Flow) {
        flow.flush();
        // rows in document order, with thead rows first
        let mut head: Vec<ElementRef> = Vec::new();
        let mut body: Vec<ElementRef> = Vec::new();
        let mut caption = None;
        for c in e.child_elements() {
            match c.value().name().to_ascii_lowercase().as_str() {
                "caption" => caption = Some(c),
                "thead" => head.extend(
                    c.child_elements()
                        .filter(|r| r.value().name().eq_ignore_ascii_case("tr")),
                ),
                "tbody" | "tfoot" => body.extend(
                    c.child_elements()
                        .filter(|r| r.value().name().eq_ignore_ascii_case("tr")),
                ),
                "tr" => body.push(c),
                _ => {}
            }
        }
        if let Some(c) = caption {
            let mut tmp = Flow {
                align: Align::Center,
                ..Default::default()
            };
            let mut i = inh.clone();
            i.st.italic = true;
            self.walk(c, &i, &mut tmp);
            flow.blocks.extend(tmp.finish());
        }
        fn cells_of<'a>(r: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
            r.child_elements()
                .filter(|c| {
                    let n = c.value().name();
                    n.eq_ignore_ascii_case("td") || n.eq_ignore_ascii_case("th")
                })
                .collect()
        }
        let all: Vec<ElementRef> = head.iter().chain(body.iter()).copied().collect();
        let ncells: usize = all.iter().map(|r| cells_of(r).len()).sum();
        let one_col = all.iter().all(|r| cells_of(r).len() <= 1);
        let has_th = all.iter().any(|r| {
            cells_of(r)
                .iter()
                .any(|c| c.value().name().eq_ignore_ascii_case("th"))
        });
        let border = e
            .attr("border")
            .and_then(|b| b.trim().parse::<u32>().ok())
            .is_some_and(|b| b > 0)
            || e.attr("style").is_some_and(|s| s.contains("border"))
            || has_th;
        // layout tables (single cell / single column, no borders): just
        // their content, in order
        if ncells == 0 || ((one_col || ncells == 1) && !border) || self.nest >= MAX_NEST {
            for r in &all {
                for c in cells_of(r) {
                    let saved = flow.align;
                    let mut i = inh.clone();
                    if let Some(a) = align_attr(c.attr("align")) {
                        i.align = a;
                    }
                    flow.align = i.align;
                    self.walk(c, &i, flow);
                    flow.flush();
                    flow.align = saved;
                }
            }
            return;
        }
        self.nest += 1;
        let mut rows = Vec::new();
        for r in &all {
            let mut row = Vec::new();
            for c in cells_of(r) {
                let mut i = inh.clone();
                let mut fill = c
                    .attr("bgcolor")
                    .and_then(parse_color)
                    .or_else(|| r.attr("bgcolor").and_then(parse_color));
                let css = c
                    .attr("style")
                    .map(|s| apply_css(s, &mut i.st, self.base_size))
                    .unwrap_or_default();
                if css.hidden {
                    continue;
                }
                fill = css.bg.or(fill).filter(|c| luma(*c) < 0.97);
                if c.value().name().eq_ignore_ascii_case("th") {
                    i.st.bold = true;
                }
                if let Some(a) = css.align.or_else(|| align_attr(c.attr("align"))) {
                    i.align = a;
                }
                let mut tmp = Flow {
                    align: i.align,
                    ..Default::default()
                };
                self.walk(c, &i, &mut tmp);
                let span = |a: &str| {
                    c.attr(a)
                        .and_then(|v| v.trim().parse::<u16>().ok())
                        .unwrap_or(1)
                        .clamp(1, 64)
                };
                row.push(Cell {
                    blocks: tmp.finish(),
                    colspan: span("colspan"),
                    rowspan: span("rowspan"),
                    fill,
                });
            }
            rows.push(row);
        }
        self.nest -= 1;
        let header = if !head.is_empty() {
            head.len()
        } else if all.len() > 1
            && cells_of(&all[0])
                .iter()
                .all(|c| c.value().name().eq_ignore_ascii_case("th"))
        {
            1
        } else {
            0
        };
        flow.blocks.push(Block::Table(Table {
            cols: Vec::new(),
            header,
            rows,
            size: None,
            borders: border,
        }));
    }
}

fn find_title(e: ElementRef<'_>) -> Option<String> {
    e.descendent_elements()
        .find(|d| d.value().name().eq_ignore_ascii_case("title"))
        .map(|t| {
            t.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|t| !t.is_empty())
}

/// Convert an HTML (or XHTML) document. `fetch` resolves image sources
/// that aren't `data:` URIs; `link_base` makes relative links absolute.
pub fn convert(
    html: &str,
    files: &mut Files,
    fetch: &mut Fetch<'_>,
    link_base: Option<url::Url>,
) -> Out {
    let doc = Html::parse_document(html);
    let root = doc.root_element();
    let mut cx = Cx {
        files,
        fetch,
        link_base,
        base_size: 11.0,
        nest: 0,
        depth: 0,
        images: 0,
    };
    let inh = Inh {
        st: Style::default(),
        pre: false,
        align: Align::Left,
    };
    let mut flow = Flow::default();
    cx.walk(root, &inh, &mut flow);
    Out {
        title: find_title(root),
        blocks: flow.finish(),
    }
}
