//! What styles.xml and numbering.xml say, reduced to what the editor needs:
//! the look of each paragraph/character style (for display), heading/title
//! detection by style *name* (ids are localized), list formats, and the ids
//! of the document's own heading styles (the toolbar reuses them).

use super::model::Align;
use super::parse::{tree, Node};
use std::collections::HashMap;

/// Run look; `None` = inherit.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Look {
    pub b: Option<bool>,
    pub i: Option<bool>,
    pub u: Option<bool>,
    pub s: Option<bool>,
    /// points
    pub size: Option<f32>,
    pub color: Option<[u8; 3]>,
}

impl Look {
    /// `self` with every unset field taken from `base`.
    pub fn over(self, base: Look) -> Look {
        Look {
            b: self.b.or(base.b),
            i: self.i.or(base.i),
            u: self.u.or(base.u),
            s: self.s.or(base.s),
            size: self.size.or(base.size),
            color: self.color.or(base.color),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StyleDef {
    pub name: String,
    pub para: bool,
    pub based: Option<String>,
    pub look: Look,
    pub num: Option<(String, u8)>,
    pub align: Option<Align>,
    pub before: Option<f32>,
    pub after: Option<f32>,
    pub ind_left: Option<f32>,
    pub outline: Option<u8>,
}

/// A paragraph style as the toolbar names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Title,
    Subtitle,
    Heading(u8),
    Quote,
    /// any other style
    Other,
}

impl Kind {
    pub const MENU: [Kind; 8] = [
        Kind::Normal,
        Kind::Title,
        Kind::Subtitle,
        Kind::Heading(1),
        Kind::Heading(2),
        Kind::Heading(3),
        Kind::Heading(4),
        Kind::Quote,
    ];

    pub fn label(self) -> String {
        match self {
            Kind::Normal => "Normal".into(),
            Kind::Title => "Title".into(),
            Kind::Subtitle => "Subtitle".into(),
            Kind::Heading(n) => format!("Heading {n}"),
            Kind::Quote => "Quote".into(),
            Kind::Other => "Custom style".into(),
        }
    }

    /// The built-in style name Word uses (styles are matched by name).
    fn style_name(self) -> Option<String> {
        match self {
            Kind::Normal | Kind::Other => None,
            Kind::Title => Some("title".into()),
            Kind::Subtitle => Some("subtitle".into()),
            Kind::Heading(n) => Some(format!("heading {n}")),
            Kind::Quote => Some("quote".into()),
        }
    }

    /// Conventional style id when the document doesn't define the style.
    fn fallback_id(self) -> Option<String> {
        match self {
            Kind::Normal | Kind::Other => None,
            Kind::Title => Some("Title".into()),
            Kind::Subtitle => Some("Subtitle".into()),
            Kind::Heading(n) => Some(format!("Heading{n}")),
            Kind::Quote => Some("Quote".into()),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

/// Fully resolved paragraph style (basedOn chain applied).
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub look: Look,
    pub num: Option<(String, u8)>,
    pub align: Option<Align>,
    pub before: Option<f32>,
    pub after: Option<f32>,
    pub ind_left: Option<f32>,
    pub heading: Option<u8>,
    pub title: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Styles {
    pub defs: HashMap<String, StyleDef>,
    /// docDefaults run look
    pub base: Look,
    pub base_after: Option<f32>,
    pub base_before: Option<f32>,
    pub default_para: Option<String>,
}

pub fn heading_from_name(name: &str) -> Option<u8> {
    let l = name.trim().to_ascii_lowercase();
    let rest = l.strip_prefix("heading")?.trim();
    rest.parse::<u8>()
        .ok()
        .filter(|n| (1..=9).contains(n))
        .map(|n| n.min(6))
}

pub fn hex_color(v: &str) -> Option<[u8; 3]> {
    if v.len() != 6 || v.eq_ignore_ascii_case("auto") {
        return None;
    }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn on(src: &str, n: &Node) -> bool {
    n.attr(src, "val")
        .is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "off" | "none"))
}

/// Run look from an rPr node.
pub fn look_of(src: &str, rpr: &Node) -> Look {
    let mut l = Look::default();
    for k in &rpr.kids {
        match &*k.name {
            "b" => l.b = Some(on(src, k)),
            "i" => l.i = Some(on(src, k)),
            "strike" => l.s = Some(on(src, k)),
            "u" => l.u = Some(k.attr(src, "val").is_none_or(|v| v != "none")),
            "sz" => {
                l.size = k
                    .attr(src, "val")
                    .and_then(|v| v.parse::<f32>().ok())
                    .map(|v| v / 2.0)
            }
            "color" => l.color = k.attr(src, "val").and_then(|v| hex_color(&v)),
            _ => {}
        }
    }
    l
}

pub fn align_of(v: &str) -> Option<Align> {
    Some(match v {
        "left" | "start" => Align::Left,
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "distribute" => Align::Justify,
        _ => return None,
    })
}

fn twips(n: &Node, src: &str, names: &[&str]) -> Option<f32> {
    names
        .iter()
        .find_map(|a| n.attr(src, a))
        .and_then(|v| v.parse::<f32>().ok())
}

impl Styles {
    pub fn parse(src: &str) -> Styles {
        let mut st = Styles::default();
        let Ok(nodes) = tree(src) else { return st };
        let Some(root) = nodes.first() else { return st };
        for n in &root.kids {
            match &*n.name {
                "docDefaults" => {
                    if let Some(r) = n.find("rPr") {
                        st.base = look_of(src, r);
                    }
                    if let Some(sp) = n.find("spacing") {
                        st.base_after = twips(sp, src, &["after"]);
                        st.base_before = twips(sp, src, &["before"]);
                    }
                }
                "style" => {
                    let Some(id) = n.attr(src, "styleId") else {
                        continue;
                    };
                    let mut d = StyleDef {
                        para: n.attr(src, "type").as_deref() == Some("paragraph"),
                        ..Default::default()
                    };
                    if d.para
                        && n.attr(src, "default")
                            .is_some_and(|v| v == "1" || v == "true")
                    {
                        st.default_para = Some(id.clone());
                    }
                    for k in &n.kids {
                        match &*k.name {
                            "name" => d.name = k.attr(src, "val").unwrap_or_default(),
                            "basedOn" => d.based = k.attr(src, "val"),
                            "rPr" => d.look = look_of(src, k),
                            "pPr" => {
                                let p = ppr_bits(src, k);
                                d.num = p.num;
                                d.align = p.align;
                                d.before = p.before;
                                d.after = p.after;
                                d.ind_left = p.ind_left;
                                d.outline = p.outline;
                            }
                            _ => {}
                        }
                    }
                    st.defs.insert(id, d);
                }
                _ => {}
            }
        }
        st
    }

    /// Resolve a paragraph style (None = the default paragraph style).
    pub fn resolve(&self, id: Option<&str>) -> Resolved {
        let mut r = Resolved::default();
        let mut cur = id.map(str::to_string).or_else(|| self.default_para.clone());
        let mut depth = 0;
        let mut first = true;
        while let (Some(c), true) = (cur.take(), depth < 12) {
            depth += 1;
            let Some(d) = self.defs.get(&c) else {
                if first {
                    // undefined id: still honour "Heading2"-style ids
                    r.heading = heading_from_name(&c);
                    r.title = c.eq_ignore_ascii_case("title");
                }
                break;
            };
            if first {
                r.heading = heading_from_name(&d.name)
                    .or_else(|| heading_from_name(&c))
                    .or(d.outline.filter(|o| *o < 9).map(|o| (o + 1).min(6)));
                r.title = d.name.eq_ignore_ascii_case("title");
            }
            first = false;
            r.look = r.look.over(d.look);
            r.num = r.num.take().or_else(|| d.num.clone());
            r.align = r.align.or(d.align);
            r.before = r.before.or(d.before);
            r.after = r.after.or(d.after);
            r.ind_left = r.ind_left.or(d.ind_left);
            cur = d.based.clone();
        }
        r.look = r.look.over(self.base);
        r.before = r.before.or(self.base_before);
        r.after = r.after.or(self.base_after);
        r
    }

    /// Character style look (rStyle), chain applied.
    pub fn char_look(&self, id: &str) -> Look {
        let mut l = Look::default();
        let mut cur = Some(id.to_string());
        let mut depth = 0;
        while let (Some(c), true) = (cur.take(), depth < 12) {
            depth += 1;
            let Some(d) = self.defs.get(&c) else { break };
            l = l.over(d.look);
            cur = d.based.clone();
        }
        l
    }

    /// What the toolbar calls this paragraph style.
    pub fn kind(&self, id: Option<&str>) -> Kind {
        let Some(id) = id else { return Kind::Normal };
        if Some(id) == self.default_para.as_deref() {
            return Kind::Normal;
        }
        let name = self
            .defs
            .get(id)
            .map(|d| d.name.to_ascii_lowercase())
            .unwrap_or_else(|| id.to_ascii_lowercase());
        if let Some(h) = heading_from_name(&name) {
            return Kind::Heading(h);
        }
        match name.as_str() {
            "normal" => Kind::Normal,
            "title" => Kind::Title,
            "subtitle" => Kind::Subtitle,
            "quote" => Kind::Quote,
            _ => Kind::Other,
        }
    }

    /// Paragraph style id for `kind`: the document's own style with that
    /// name, else the conventional id. None = Normal (no pStyle).
    pub fn id_for(&self, kind: Kind) -> Option<String> {
        let name = kind.style_name()?;
        let mut hits: Vec<&String> = self
            .defs
            .iter()
            .filter(|(_, d)| d.para && d.name.eq_ignore_ascii_case(&name))
            .map(|(id, _)| id)
            .collect();
        hits.sort();
        hits.first()
            .map(|s| s.to_string())
            .or_else(|| kind.fallback_id())
    }
}

/// Paragraph properties read from a pPr node.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PprBits {
    pub style: Option<String>,
    pub num: Option<(String, u8)>,
    pub align: Option<Align>,
    pub ind_left: Option<f32>,
    pub before: Option<f32>,
    pub after: Option<f32>,
    pub outline: Option<u8>,
}

pub fn ppr_bits(src: &str, ppr: &Node) -> PprBits {
    let mut p = PprBits::default();
    for k in &ppr.kids {
        match &*k.name {
            "pStyle" => p.style = k.attr(src, "val"),
            "numPr" => {
                let id = k.child("numId").and_then(|n| n.attr(src, "val"));
                let lvl = k
                    .child("ilvl")
                    .and_then(|n| n.attr(src, "val"))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                p.num = id.map(|id| (id, lvl));
            }
            "jc" => p.align = k.attr(src, "val").and_then(|v| align_of(&v)),
            "ind" => p.ind_left = twips(k, src, &["left", "start"]),
            "spacing" => {
                p.before = twips(k, src, &["before"]);
                p.after = twips(k, src, &["after"]);
            }
            "outlineLvl" => p.outline = k.attr(src, "val").and_then(|v| v.parse().ok()),
            _ => {}
        }
    }
    p
}

// -------------------------------------------------------------- numbering

#[derive(Debug, Clone, Default)]
pub struct Level {
    pub fmt: String,
    pub text: String,
    pub start: u32,
    pub ind_left: Option<f32>,
}

#[derive(Debug, Clone, Default)]
pub struct Numbering {
    /// numId → abstractNumId (document order)
    pub nums: Vec<(String, String)>,
    pub levels: HashMap<String, HashMap<u8, Level>>,
    /// numId → level → start override
    pub overrides: HashMap<String, HashMap<u8, u32>>,
}

impl Numbering {
    pub fn parse(src: &str) -> Numbering {
        let mut n = Numbering::default();
        let Ok(nodes) = tree(src) else { return n };
        let Some(root) = nodes.first() else { return n };
        for k in &root.kids {
            match &*k.name {
                "abstractNum" => {
                    let Some(a) = k.attr(src, "abstractNumId") else {
                        continue;
                    };
                    let lv = n.levels.entry(a).or_default();
                    for l in k.kids.iter().filter(|l| &*l.name == "lvl") {
                        let Some(i) = l.attr(src, "ilvl").and_then(|v| v.parse::<u8>().ok()) else {
                            continue;
                        };
                        let val = |name: &str| l.child(name).and_then(|c| c.attr(src, "val"));
                        lv.insert(
                            i,
                            Level {
                                fmt: val("numFmt").unwrap_or_else(|| "decimal".into()),
                                text: val("lvlText").unwrap_or_default(),
                                start: val("start").and_then(|v| v.parse().ok()).unwrap_or(1),
                                ind_left: l
                                    .find("ind")
                                    .and_then(|i| twips(i, src, &["left", "start"])),
                            },
                        );
                    }
                }
                "num" => {
                    let (Some(id), Some(a)) = (
                        k.attr(src, "numId"),
                        k.child("abstractNumId").and_then(|c| c.attr(src, "val")),
                    ) else {
                        continue;
                    };
                    for o in k.kids.iter().filter(|o| &*o.name == "lvlOverride") {
                        if let (Some(i), Some(s)) = (
                            o.attr(src, "ilvl").and_then(|v| v.parse::<u8>().ok()),
                            o.child("startOverride")
                                .and_then(|s| s.attr(src, "val"))
                                .and_then(|v| v.parse::<u32>().ok()),
                        ) {
                            n.overrides.entry(id.clone()).or_default().insert(i, s);
                        }
                    }
                    n.nums.push((id, a));
                }
                _ => {}
            }
        }
        n
    }

    pub fn level(&self, num: &str, lvl: u8) -> Option<&Level> {
        let a = &self.nums.iter().find(|(id, _)| id == num)?.1;
        self.levels.get(a)?.get(&lvl)
    }

    pub fn is_bullet(&self, num: &str, lvl: u8) -> bool {
        self.level(num, lvl)
            .is_none_or(|l| matches!(l.fmt.as_str(), "bullet" | "none"))
    }

    /// First numId whose top level is a bullet (or a number).
    pub fn find(&self, bullet: bool) -> Option<String> {
        self.nums
            .iter()
            .find(|(id, _)| {
                self.level(id, 0).is_some() && self.is_bullet(id, 0) == bullet && id != "0"
            })
            .map(|(id, _)| id.clone())
    }
}

/// "3", "c", "iii"… for a list counter.
pub fn format_counter(fmt: &str, n: u32) -> String {
    fn roman(mut n: u32) -> String {
        const T: [(u32, &str); 13] = [
            (1000, "m"),
            (900, "cm"),
            (500, "d"),
            (400, "cd"),
            (100, "c"),
            (90, "xc"),
            (50, "l"),
            (40, "xl"),
            (10, "x"),
            (9, "ix"),
            (5, "v"),
            (4, "iv"),
            (1, "i"),
        ];
        let mut s = String::new();
        for (v, r) in T {
            while n >= v {
                s.push_str(r);
                n -= v;
            }
        }
        s
    }
    fn letters(n: u32) -> String {
        let n = n.max(1) - 1;
        let c = (b'a' + (n % 26) as u8) as char;
        c.to_string().repeat(n as usize / 26 + 1)
    }
    match fmt {
        "lowerLetter" => letters(n),
        "upperLetter" => letters(n).to_ascii_uppercase(),
        "lowerRoman" => roman(n),
        "upperRoman" => roman(n).to_ascii_uppercase(),
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}
