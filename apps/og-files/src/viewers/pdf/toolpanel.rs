//! The PDF Tools panel: a catalog of every tool (Stirling-PDF parity, see
//! SPEC.md §5a), a form per tool, and the glue that turns a form into a
//! blocking job over the current document bytes.
//!
//! Form values live in two small string/bool maps keyed by field name, so a
//! tool's form is just a list of `field(...)` calls — no per-tool state.

use super::tools::{
    self, convert, layout, pages, raster, server, structure, Anchor, Output, Paper,
};
use crate::theme;
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::{
    button, checkbox, column, container, pick_list, row, text, text_editor, text_input, Space,
};
use iced::{Element, Length, Point, Rectangle, Renderer, Theme};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    // organize
    SplitRanges,
    SplitBookmarks,
    SplitSize,
    SelectPages,
    RemoveBlank,
    Crop,
    Nup,
    Booklet,
    ScalePaper,
    ScaleContent,
    SplitSections,
    SinglePage,
    Overlay,
    InsertImages,
    InsertUrl,
    // add content
    PageNumbers,
    TextStamp,
    Watermark,
    ImageStamp,
    Signature,
    Attachments,
    Bookmarks,
    Metadata,
    // security
    AddPassword,
    RemovePassword,
    Sanitize,
    RedactText,
    RedactAreas,
    RemoveAnnotations,
    RemoveImages,
    FlattenForms,
    FlattenAll,
    UnlockForms,
    RemoveSignatures,
    ShowJavaScript,
    CertSign,
    ValidateSignature,
    // convert & extract
    ToImages,
    ExtractImages,
    ExtractScans,
    ExtractAttachments,
    ConvertTo,
    Info,
    Compare,
    AutoRename,
    // optimize & colour
    CompressLossless,
    CompressServer,
    Repair,
    Ocr,
    PdfA,
    AutoSplit,
    Grayscale,
    Invert,
    AdjustColors,
    FakeScan,
}

use Tool::*;

pub const CATEGORIES: [(&str, &[Tool]); 5] = [
    (
        "Organize",
        &[
            SplitRanges,
            SplitBookmarks,
            SplitSize,
            AutoSplit,
            SelectPages,
            RemoveBlank,
            Crop,
            Nup,
            Booklet,
            ScalePaper,
            ScaleContent,
            SplitSections,
            SinglePage,
            Overlay,
            InsertImages,
            InsertUrl,
        ],
    ),
    (
        "Add & edit",
        &[
            PageNumbers,
            TextStamp,
            Watermark,
            ImageStamp,
            Signature,
            Attachments,
            Bookmarks,
            Metadata,
        ],
    ),
    (
        "Security",
        &[
            AddPassword,
            RemovePassword,
            Sanitize,
            RedactText,
            RedactAreas,
            RemoveAnnotations,
            RemoveImages,
            FlattenForms,
            FlattenAll,
            UnlockForms,
            RemoveSignatures,
            ShowJavaScript,
            CertSign,
            ValidateSignature,
        ],
    ),
    (
        "Convert & extract",
        &[
            ToImages,
            ExtractImages,
            ExtractScans,
            ExtractAttachments,
            ConvertTo,
            Info,
            Compare,
            AutoRename,
        ],
    ),
    (
        "Optimize & colour",
        &[
            CompressLossless,
            CompressServer,
            Repair,
            Ocr,
            PdfA,
            Grayscale,
            Invert,
            AdjustColors,
            FakeScan,
        ],
    ),
];

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            SplitRanges => "Split by ranges",
            SplitBookmarks => "Split by bookmarks",
            SplitSize => "Split by file size",
            SelectPages => "Select pages",
            RemoveBlank => "Remove blank pages",
            Crop => "Crop",
            Nup => "Multiple pages per sheet",
            Booklet => "Booklet",
            ScalePaper => "Fit to paper size",
            ScaleContent => "Scale pages",
            SplitSections => "Split pages into sections",
            SinglePage => "Combine into one long page",
            Overlay => "Overlay another PDF",
            InsertImages => "Insert images as pages",
            InsertUrl => "Insert web page",
            PageNumbers => "Page numbers",
            TextStamp => "Text stamp",
            Watermark => "Watermark",
            ImageStamp => "Image stamp / add image",
            Signature => "Draw signature",
            Attachments => "Add attachments",
            Bookmarks => "Edit bookmarks",
            Metadata => "Edit metadata",
            AddPassword => "Add password & permissions",
            RemovePassword => "Remove password",
            Sanitize => "Sanitize",
            RedactText => "Redact text",
            RedactAreas => "Redact areas",
            RemoveAnnotations => "Remove annotations",
            RemoveImages => "Remove images",
            FlattenForms => "Flatten forms",
            FlattenAll => "Flatten to images",
            UnlockForms => "Unlock form fields",
            RemoveSignatures => "Remove digital signatures",
            ShowJavaScript => "Show JavaScript",
            CertSign => "Sign with certificate",
            ValidateSignature => "Validate signatures",
            ToImages => "Pages to images",
            ExtractImages => "Extract images",
            ExtractScans => "Split scanned photos",
            ExtractAttachments => "Extract attachments",
            ConvertTo => "Convert PDF to…",
            Info => "Document info",
            Compare => "Compare with another PDF",
            AutoRename => "Auto-rename from content",
            CompressLossless => "Compress (lossless)",
            CompressServer => "Compress (strong)",
            Repair => "Repair",
            Ocr => "OCR (make text searchable)",
            PdfA => "Convert to PDF/A",
            AutoSplit => "Auto-split at QR separator pages",
            Grayscale => "Grayscale",
            Invert => "Invert colours",
            AdjustColors => "Adjust colours",
            FakeScan => "Make it look scanned",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            SplitRanges => "One new file per group, e.g. 1-3, 4-8, 9-",
            SplitBookmarks => "One new file per top-level bookmark.",
            SplitSize => "New files no bigger than the size given.",
            SelectPages => "Tick pages by range, e.g. odd or 1-5,9.",
            RemoveBlank => "Drops pages with almost no ink.",
            Crop => "Trims margins (points, 72 per inch) by setting the crop box.",
            Nup => "Puts 2–16 pages on each sheet.",
            Booklet => "2-up sheets in fold-and-staple order.",
            ScalePaper => "Fits pages onto a standard paper size.",
            ScaleContent => "Enlarges or shrinks pages by a factor.",
            SplitSections => "Cuts each page into a grid (2×1 splits a two-page scan).",
            SinglePage => "Stacks every page into one tall page.",
            Overlay => "Draws another PDF's pages over or under these.",
            InsertImages => "Adds image files as pages after the current page.",
            InsertUrl => "Renders a web page to PDF (server) and inserts it after the current page.",
            PageNumbers => "Placeholders: {n}, {total}, {filename}.",
            TextStamp => "Stamps a line of text on the chosen pages.",
            Watermark => "Large faint text on every page, centred or tiled.",
            ImageStamp => "Places an image file on the chosen pages.",
            Signature => "Draw below, click the page where it goes, then apply.",
            Attachments => "Embeds files inside the PDF.",
            Bookmarks => "One per line: indent two spaces per level, end with @ page.",
            Metadata => "Title, author and other document properties.",
            AddPassword => "AES-256. Leave the open password empty to only restrict permissions.",
            RemovePassword => "Needs the current password.",
            Sanitize => "Strips active or hidden content.",
            RedactText => "Removes the words from the page text. Works with simple fonts; check the result.",
            RedactAreas => "Drag boxes on the page. Covered pages become images, so the content is really gone.",
            RemoveAnnotations => "Deletes comments, highlights and other annotations.",
            RemoveImages => "Deletes every image.",
            FlattenForms => "Bakes form fields into the page so they can't be edited.",
            FlattenAll => "Turns pages into images (no selectable text afterwards).",
            UnlockForms => "Makes read-only form fields editable.",
            RemoveSignatures => "Removes digital signatures.",
            ShowJavaScript => "Lists all scripts in the document.",
            CertSign => "Signs with a .p12/.pfx certificate (server).",
            ValidateSignature => "Checks digital signatures (server).",
            ToImages => "Writes each page as an image next to the PDF.",
            ExtractImages => "Saves embedded images next to the PDF.",
            ExtractScans => "Several photos scanned on one page → separate, straightened images.",
            ExtractAttachments => "Saves embedded files next to the PDF.",
            ConvertTo => "Word, Excel, PowerPoint, HTML, Markdown and more.",
            Info => "Pages, fonts, images, security, metadata.",
            Compare => "Text differences, page by page.",
            AutoRename => "Renames the file from its title or first line.",
            CompressLossless => "Smaller file without touching image quality.",
            CompressServer => "Stronger compression incl. images (server).",
            Repair => "Rebuilds a damaged file.",
            Ocr => "Adds an invisible, searchable text layer to scans (offline, Latin script).",
            PdfA => "Archival PDF/A-2b (server).",
            AutoSplit => "Splits at separator pages carrying a QR code; drops those pages.",
            Grayscale => "Converts to greyscale (pages become images).",
            Invert => "Dark pages, light text (pages become images).",
            AdjustColors => "Brightness, contrast, saturation (pages become images).",
            FakeScan => "Slight tilt, noise and paper tone.",
        }
    }

    pub fn needs_server(self) -> bool {
        matches!(
            self,
            InsertUrl
                | CertSign
                | ValidateSignature
                | CompressServer
                | PdfA
        )
    }
}

/// How a finished job should be applied.
#[derive(Debug, Clone)]
pub enum JobOut {
    Out(Output),
    /// pages to insert after the current page (images, web page)
    Insert(Vec<u8>),
    /// new file name (no directory) for auto-rename
    Rename(String),
    /// tick these pages
    Select(Vec<usize>),
    /// prefill form fields (metadata, bookmarks)
    Prefill(Vec<(&'static str, String)>),
}

// ---------------------------------------------------------------- form ----

#[derive(Default)]
pub struct Form {
    text: HashMap<&'static str, String>,
    flag: HashMap<&'static str, bool>,
    pub bookmarks: text_editor::Content,
    pub strokes: Vec<Vec<(f32, f32)>>,
    /// signature anchor on the page, 0..1 from top-left
    pub sig_at: Option<(f32, f32)>,
    /// redaction boxes: (plan page, x0, y0, x1, y1) in 0..1, top-left origin
    pub regions: Vec<(usize, f32, f32, f32, f32)>,
}

#[derive(Debug, Clone)]
pub enum FormMsg {
    Text(&'static str, String),
    Flag(&'static str, bool),
    Bookmarks(text_editor::Action),
    Stroke(Vec<(f32, f32)>),
    ClearStrokes,
    ClearRegions,
}

impl Form {
    pub fn update(&mut self, m: FormMsg) {
        match m {
            FormMsg::Text(k, v) => {
                self.text.insert(k, v);
            }
            FormMsg::Flag(k, v) => {
                self.flag.insert(k, v);
            }
            FormMsg::Bookmarks(a) => self.bookmarks.perform(a),
            FormMsg::Stroke(s) if s.len() > 1 => self.strokes.push(s),
            FormMsg::Stroke(_) => {}
            FormMsg::ClearStrokes => {
                self.strokes.clear();
                self.sig_at = None;
            }
            FormMsg::ClearRegions => self.regions.clear(),
        }
    }

    pub fn prefill(&mut self, values: Vec<(&'static str, String)>) {
        for (k, v) in values {
            if k == "bookmarks" {
                self.bookmarks = text_editor::Content::with_text(&v);
            } else {
                self.text.insert(k, v);
            }
        }
    }

    pub fn get_text(&self, k: &'static str) -> String {
        self.t(k, "")
    }

    fn t(&self, k: &'static str, default: &str) -> String {
        self.text
            .get(k)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    fn num(&self, k: &'static str, default: f32) -> Result<f32, String> {
        let s = self.t(k, "");
        if s.trim().is_empty() {
            return Ok(default);
        }
        s.trim()
            .parse::<f32>()
            .map_err(|_| format!("'{s}' isn't a number"))
    }

    fn f(&self, k: &'static str, default: bool) -> bool {
        self.flag.get(k).copied().unwrap_or(default)
    }

    fn paper(&self) -> Paper {
        let s = self.t("paper", "A4");
        Paper::ALL
            .into_iter()
            .find(|p| p.to_string() == s)
            .unwrap_or(Paper::A4)
    }

    fn anchor(&self, default: Anchor) -> Anchor {
        let s = self.t("anchor", &default.to_string());
        Anchor::ALL
            .into_iter()
            .find(|a| a.to_string() == s)
            .unwrap_or(default)
    }

    fn font(&self) -> layout::Font {
        let s = self.t("font", "Helvetica");
        layout::Font::ALL
            .into_iter()
            .find(|f| f.to_string() == s)
            .unwrap_or(layout::Font::Helvetica)
    }

    fn color(&self, default: &str) -> Result<tools::Rgb, String> {
        let s = self.t("color", default);
        let h = s.trim().trim_start_matches('#');
        if h.len() != 6 {
            return Err(format!("colour '{s}' should look like #ff0000"));
        }
        let c = |i: usize| {
            u8::from_str_radix(&h[i..i + 2], 16)
                .map(|v| v as f32 / 255.0)
                .map_err(|_| format!("bad colour '{s}'"))
        };
        Ok([c(0)?, c(2)?, c(4)?])
    }

    fn style(
        &self,
        size: f32,
        opacity: f32,
        rotation: f32,
        color: &str,
    ) -> Result<layout::TextStyle, String> {
        Ok(layout::TextStyle {
            font: self.font(),
            size: self.num("size", size)?,
            color: self.color(color)?,
            opacity: (self.num("opacity", opacity * 100.0)? / 100.0).clamp(0.0, 1.0),
            rotation: self.num("rotation", rotation)?,
        })
    }

    fn place(&self, anchor: Anchor) -> Result<layout::Placement, String> {
        Ok(layout::Placement {
            anchor: self.anchor(anchor),
            margin: self.num("margin", 24.0)?,
            offset: (0.0, 0.0),
        })
    }
}

// ---------------------------------------------------------------- jobs ----

/// Everything a job needs, captured by value so it can run off-thread.
pub type Job = Box<dyn FnOnce(Arc<Vec<u8>>) -> Result<JobOut, String> + Send>;

pub struct JobInput {
    pub page_count: usize,
    pub current: usize,
    pub selected: Vec<usize>,
    pub file_name: String,
    pub server: Option<server::ServerConfig>,
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    let p = expand_home(path.trim());
    std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
}

fn read_many(list: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let files: Vec<(String, Vec<u8>)> = list
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|p| {
            let name = FsPath::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.into());
            read(p).map(|b| (name, b))
        })
        .collect::<Result<_, _>>()?;
    if files.is_empty() {
        return Err("give at least one file path".into());
    }
    Ok(files)
}

pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => PathBuf::from(p),
    }
}

fn reply_out(r: server::Reply, suffix: &str) -> JobOut {
    if r.ext == "pdf" {
        JobOut::Out(Output::Pdf(r.bytes))
    } else {
        JobOut::Out(Output::Files(vec![(
            format!("{suffix}.{}", r.ext),
            r.bytes,
        )]))
    }
}

fn split_files(
    doc: &[u8],
    groups: Vec<Vec<usize>>,
    label: impl Fn(usize) -> String,
) -> Result<JobOut, String> {
    let src = [Arc::new(doc.to_vec())];
    let files = groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let plan: Vec<_> = g.iter().map(|&p| (0, p, 0)).collect();
            super::compose::build(&src, &plan).map(|b| (format!("{}.pdf", label(i)), b))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(JobOut::Out(Output::Files(files)))
}

/// "Split scanned photos" form → detector settings (Stirling's knobs).
fn scan_options(form: &Form) -> Result<tools::scans::Options, String> {
    let d = tools::scans::Options::default();
    let whole = |k: &'static str, def: u32, max: f32| -> Result<u32, String> {
        let v = form.num(k, def as f32)?;
        if !(0.0..=max).contains(&v) {
            return Err(format!("'{v}' is out of range (0–{max})"));
        }
        Ok(v.round() as u32)
    };
    Ok(tools::scans::Options {
        angle_threshold: form.num("angle", d.angle_threshold)?.clamp(0.0, 45.0),
        tolerance: whole("tolerance", d.tolerance as u32, 255.0)? as u8,
        min_area: whole("min_area", d.min_area, 1e9)?,
        min_contour_area: whole("min_contour", d.min_contour_area, 1e9)?,
        border: whole("border", d.border, 1000.0)?,
    })
}

fn slug(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_lowercase();
    s.chars().take(40).collect()
}

/// Parse the bookmark editor: two spaces per level, "Title @ page".
fn parse_bookmarks(src: &str, page_count: usize) -> Result<Vec<structure::Bookmark>, String> {
    src.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let level = (l.len() - l.trim_start().len()) / 2;
            let (title, page) = l
                .trim()
                .rsplit_once('@')
                .ok_or_else(|| format!("'{}' needs '@ page'", l.trim()))?;
            let page: usize = page
                .trim()
                .parse()
                .map_err(|_| format!("bad page in '{}'", l.trim()))?;
            if page == 0 || page > page_count {
                return Err(format!("page {page} is outside 1–{page_count}"));
            }
            Ok(structure::Bookmark {
                title: title.trim().to_string(),
                page: page - 1,
                level,
            })
        })
        .collect()
}

/// Build the off-thread job for `tool` from the form. Validation errors
/// come back immediately, before anything runs.
pub fn job(tool: Tool, form: &Form, input: JobInput) -> Result<Job, String> {
    let n = input.page_count;
    // page fields default to the ticked pages, else the whole document
    let default_pages = if input.selected.is_empty() {
        "all".to_string()
    } else {
        input
            .selected
            .iter()
            .map(|p| (p + 1).to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let range = |k: &'static str| -> Result<Vec<usize>, String> {
        let spec = form.t(k, &default_pages);
        let spec = if spec.trim().is_empty() {
            default_pages.clone()
        } else {
            spec
        };
        pages::parse(&spec, n)
    };
    let srv = || {
        input.server.clone().ok_or_else(|| {
            "Set up the Stirling server first (see Settings in this panel)".to_string()
        })
    };
    let out = |r: Result<Vec<u8>, String>| r.map(|b| JobOut::Out(Output::Pdf(b)));

    Ok(match tool {
        SplitRanges => {
            let groups = pages::parse_groups(&form.t("groups", ""), n)?;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                split_files(&pdf, groups, |i| format!("part-{:02}", i + 1))
            })
        }
        SplitBookmarks => Box::new(move |pdf: Arc<Vec<u8>>| {
            let groups = structure::bookmark_groups(&pdf)?;
            if groups.is_empty() {
                return Err("this PDF has no bookmarks".into());
            }
            let names: Vec<String> = groups
                .iter()
                .enumerate()
                .map(|(i, (t, _))| format!("{:02}-{}", i + 1, slug(t)))
                .collect();
            split_files(
                &pdf,
                groups.into_iter().map(|(_, g)| g).collect(),
                move |i| names[i].clone(),
            )
        }),
        SplitSize => {
            let mb = form.num("mb", 10.0)?;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                let groups = structure::size_groups(&pdf, (mb * 1024.0 * 1024.0) as u64)?;
                split_files(&pdf, groups, |i| format!("part-{:02}", i + 1))
            })
        }
        SelectPages => {
            let p = range("pages")?;
            Box::new(move |_| Ok(JobOut::Select(p)))
        }
        RemoveBlank => {
            let ink = form.num("ink", 0.5)? / 100.0;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                let blank = raster::blank_pages(&pdf, ink)?;
                if blank.is_empty() {
                    return Err("no blank pages found".into());
                }
                if blank.len() == n {
                    return Err("every page looks blank — nothing removed".into());
                }
                let keep: Vec<_> = (0..n)
                    .filter(|p| !blank.contains(p))
                    .map(|p| (0, p, 0))
                    .collect();
                out(super::compose::build(&[pdf.clone()], &keep))
            })
        }
        Crop => {
            let p = range("pages")?;
            let m = [
                form.num("left", 36.0)?,
                form.num("bottom", 36.0)?,
                form.num("right", 36.0)?,
                form.num("top", 36.0)?,
            ];
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::crop(&pdf, &p, m)))
        }
        Nup => {
            let per: usize = form
                .t("per", "4")
                .parse()
                .map_err(|_| "pages per sheet must be 2, 4, 6, 9 or 16")?;
            let (paper, border) = (form.paper(), form.f("border", false));
            Box::new(move |pdf: Arc<Vec<u8>>| out(layout::nup(&pdf, per, paper, border)))
        }
        Booklet => {
            let paper = form.paper();
            Box::new(move |pdf: Arc<Vec<u8>>| out(layout::booklet(&pdf, paper)))
        }
        ScalePaper => {
            let (p, paper) = (range("pages")?, form.paper());
            Box::new(move |pdf: Arc<Vec<u8>>| out(layout::scale_to_paper(&pdf, &p, paper)))
        }
        ScaleContent => {
            let (p, f) = (range("pages")?, form.num("factor", 100.0)? / 100.0);
            Box::new(move |pdf: Arc<Vec<u8>>| out(layout::scale_content(&pdf, &p, f)))
        }
        SplitSections => {
            let p = range("pages")?;
            let (c, r) = (
                form.num("cols", 2.0)? as usize,
                form.num("rows", 1.0)? as usize,
            );
            Box::new(move |pdf: Arc<Vec<u8>>| out(layout::split_sections(&pdf, &p, c, r)))
        }
        SinglePage => Box::new(move |pdf: Arc<Vec<u8>>| out(layout::single_page(&pdf))),
        Overlay => {
            let (path, under) = (form.t("other", ""), form.f("under", false));
            let mode = if under {
                layout::Overlay::Background
            } else {
                layout::Overlay::Foreground
            };
            Box::new(move |pdf: Arc<Vec<u8>>| out(layout::overlay(&pdf, &read(&path)?, mode)))
        }
        InsertImages => {
            let (list, fit) = (form.t("files", ""), form.f("fit", true));
            let (paper, margin) = (form.paper(), form.num("margin", 24.0)?);
            Box::new(move |_| {
                let imgs: Vec<Vec<u8>> = read_many(&list)?.into_iter().map(|(_, b)| b).collect();
                layout::images_to_pdf(&imgs, fit.then_some(paper), margin).map(JobOut::Insert)
            })
        }
        InsertUrl => {
            let (cfg, url) = (srv()?, form.t("url", ""));
            Box::new(move |_| {
                let r = server::url_to_pdf(&cfg, url.trim())?;
                if r.ext != "pdf" {
                    return Err(format!("server returned .{} instead of a PDF", r.ext));
                }
                Ok(JobOut::Insert(r.bytes))
            })
        }
        PageNumbers => {
            let p = range("pages")?;
            let (fmt, start) = (form.t("format", "{n}"), form.num("start", 1.0)? as usize);
            let (style, place) = (
                form.style(10.0, 1.0, 0.0, "#000000")?,
                form.place(Anchor::Bottom)?,
            );
            let name = input.file_name.clone();
            Box::new(move |pdf: Arc<Vec<u8>>| {
                out(layout::page_numbers(
                    &pdf, &p, &fmt, start, &name, &style, place,
                ))
            })
        }
        TextStamp => {
            let p = range("pages")?;
            let txt = form.t("text", "");
            if txt.trim().is_empty() {
                return Err("enter the stamp text".into());
            }
            let (style, place) = (
                form.style(18.0, 1.0, 0.0, "#c00000")?,
                form.place(Anchor::TopRight)?,
            );
            Box::new(move |pdf: Arc<Vec<u8>>| {
                out(layout::text_stamp(&pdf, &p, &txt, &style, place))
            })
        }
        Watermark => {
            let txt = form.t("text", "CONFIDENTIAL");
            let style = form.style(64.0, 0.15, 45.0, "#808080")?;
            let (tiled, spacing) = (form.f("tiled", false), form.num("spacing", 120.0)?);
            Box::new(move |pdf: Arc<Vec<u8>>| {
                out(layout::watermark(&pdf, &txt, &style, tiled, spacing))
            })
        }
        ImageStamp => {
            let (p, path) = (range("pages")?, form.t("image", ""));
            let (w, op, place) = (
                form.num("width", 120.0)?,
                form.num("opacity", 100.0)? / 100.0,
                form.place(Anchor::BottomRight)?,
            );
            Box::new(move |pdf: Arc<Vec<u8>>| {
                out(layout::image_stamp(&pdf, &p, &read(&path)?, w, op, place))
            })
        }
        Signature => {
            if form.strokes.is_empty() {
                return Err("draw a signature in the box first".into());
            }
            let (x, y) = form
                .sig_at
                .ok_or("click on the page where the signature should go")?;
            let w = form.num("width", 160.0)?;
            let (strokes, page, color) =
                (form.strokes.clone(), input.current, form.color("#10206a")?);
            Box::new(move |pdf: Arc<Vec<u8>>| {
                let doc = lopdf_page_size(&pdf, page)?;
                let h = w * SIG_ASPECT;
                let rect = (x * doc.0 - w / 2.0, (1.0 - y) * doc.1 - h / 2.0, w, h);
                out(layout::draw_signature(
                    &pdf, page, &strokes, rect, color, 1.6,
                ))
            })
        }
        Attachments => {
            let list = form.t("files", "");
            Box::new(move |pdf: Arc<Vec<u8>>| {
                out(structure::attachments_add(&pdf, &read_many(&list)?))
            })
        }
        Bookmarks => {
            let marks = parse_bookmarks(&form.bookmarks.text(), n)?;
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::bookmarks_set(&pdf, &marks)))
        }
        Metadata => {
            let meta = structure::Metadata {
                title: form.t("title", ""),
                author: form.t("author", ""),
                subject: form.t("subject", ""),
                keywords: form.t("keywords", ""),
                creator: form.t("creator", ""),
                producer: form.t("producer", ""),
                creation_date: form.t("created", ""),
                mod_date: form.t("modified", ""),
            };
            let wipe = form.f("wipe", false);
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::metadata_set(&pdf, &meta, wipe)))
        }
        AddPassword => {
            let (user, owner) = (form.t("user_pw", ""), form.t("owner_pw", ""));
            if user.is_empty() && owner.is_empty() {
                return Err("set an open password, an owner password, or both".into());
            }
            let perms = structure::Permissions {
                print: form.f("p_print", true),
                print_high_quality: form.f("p_print", true),
                modify: form.f("p_modify", true),
                copy: form.f("p_copy", true),
                annotate: form.f("p_annotate", true),
                fill_forms: form.f("p_forms", true),
                accessibility: true,
                assemble: form.f("p_modify", true),
            };
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::encrypt(&pdf, &user, &owner, perms)))
        }
        RemovePassword => {
            let pw = form.t("password", "");
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::decrypt(&pdf, &pw)))
        }
        Sanitize => {
            let what = structure::Sanitize {
                javascript: form.f("s_js", true),
                embedded_files: form.f("s_files", true),
                xmp_metadata: form.f("s_xmp", false),
                document_info: form.f("s_info", false),
                links: form.f("s_links", false),
                fonts: form.f("s_fonts", false),
            };
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::sanitize(&pdf, what)))
        }
        RedactText => {
            let terms: Vec<String> = form
                .t("terms", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if terms.is_empty() {
                return Err("enter the words to redact, comma-separated".into());
            }
            let case = form.f("case", false);
            Box::new(move |pdf: Arc<Vec<u8>>| {
                let (b, hits) = structure::redact_text(&pdf, &terms, case)?;
                if hits == 0 {
                    return Err("none of those words were found in removable text".into());
                }
                Ok(JobOut::Out(Output::Pdf(b)))
            })
        }
        RedactAreas => {
            if form.regions.is_empty() {
                return Err("drag boxes on the page first".into());
            }
            let (regions, dpi) = (form.regions.clone(), form.num("dpi", 200.0)?);
            Box::new(move |pdf: Arc<Vec<u8>>| out(raster::redact_regions(&pdf, &regions, dpi)))
        }
        RemoveAnnotations => {
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::remove_annotations(&pdf)))
        }
        RemoveImages => Box::new(move |pdf: Arc<Vec<u8>>| out(structure::remove_images(&pdf))),
        FlattenForms => Box::new(move |pdf: Arc<Vec<u8>>| out(structure::flatten_forms(&pdf))),
        FlattenAll => {
            let (p, dpi) = (range("pages")?, form.num("dpi", 200.0)?);
            Box::new(move |pdf: Arc<Vec<u8>>| out(raster::rasterize(&pdf, &p, dpi, 85)))
        }
        UnlockForms => Box::new(move |pdf: Arc<Vec<u8>>| out(structure::unlock_forms(&pdf))),
        RemoveSignatures => {
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::remove_signatures(&pdf)))
        }
        ShowJavaScript => Box::new(move |pdf: Arc<Vec<u8>>| {
            structure::javascript_report(&pdf).map(|r| JobOut::Out(Output::Report(r)))
        }),
        CertSign => {
            let (cfg, cert, pw) = (srv()?, form.t("cert", ""), form.t("cert_pw", ""));
            let (reason, loc, vis) = (
                form.t("reason", ""),
                form.t("location", ""),
                form.f("visible", false),
            );
            Box::new(move |pdf: Arc<Vec<u8>>| {
                server::cert_sign(&cfg, &pdf, &read(&cert)?, &pw, &reason, &loc, vis)
                    .map(|r| reply_out(r, "signed"))
            })
        }
        ValidateSignature => {
            let cfg = srv()?;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                server::validate_signature(&cfg, &pdf).map(|r| JobOut::Out(Output::Report(r)))
            })
        }
        ToImages => {
            let (p, dpi) = (range("pages")?, form.num("dpi", 150.0)?);
            let fmt = if form.f("jpeg", false) {
                raster::ImageFormat::Jpeg(88)
            } else {
                raster::ImageFormat::Png
            };
            Box::new(move |pdf: Arc<Vec<u8>>| {
                raster::render_pages(&pdf, &p, dpi, fmt).map(|f| JobOut::Out(Output::Files(f)))
            })
        }
        ExtractImages => {
            let p = range("pages")?;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                let f = structure::extract_images(&pdf, &p)?;
                if f.is_empty() {
                    return Err("no extractable images on those pages".into());
                }
                Ok(JobOut::Out(Output::Files(f)))
            })
        }
        ExtractScans => {
            let p = range("pages")?;
            let opts = scan_options(form)?;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                tools::scans::split_pdf(&pdf, &p, opts).map(|f| JobOut::Out(Output::Files(f)))
            })
        }
        ExtractAttachments => Box::new(move |pdf: Arc<Vec<u8>>| {
            let f = structure::attachments_extract(&pdf)?;
            if f.is_empty() {
                return Err("this PDF has no attachments".into());
            }
            Ok(JobOut::Out(Output::Files(
                f.into_iter()
                    .map(|(name, b)| (format!("attachment-{name}"), b))
                    .collect(),
            )))
        }),
        ConvertTo => {
            let t = form.t("target", &convert::Target::Docx.to_string());
            let target = convert::Target::ALL
                .into_iter()
                .find(|x| x.to_string() == t)
                .unwrap_or(convert::Target::Docx);
            let stem = std::path::Path::new(&input.file_name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".into());
            Box::new(move |pdf: Arc<Vec<u8>>| {
                convert::convert(&pdf, target, &stem).map(|f| JobOut::Out(Output::Files(f)))
            })
        }
        Info => Box::new(move |pdf: Arc<Vec<u8>>| {
            structure::info_report(&pdf).map(|r| JobOut::Out(Output::Report(r)))
        }),
        Compare => {
            let path = form.t("other", "");
            Box::new(move |pdf: Arc<Vec<u8>>| {
                structure::compare_text(&pdf, &read(&path)?).map(|r| JobOut::Out(Output::Report(r)))
            })
        }
        AutoRename => Box::new(
            move |pdf: Arc<Vec<u8>>| match structure::suggest_title(&pdf)? {
                Some(t) => Ok(JobOut::Rename(format!("{t}.pdf"))),
                None => Err("couldn't find a usable title in this PDF".into()),
            },
        ),
        CompressLossless => {
            Box::new(move |pdf: Arc<Vec<u8>>| out(structure::compress_lossless(&pdf)))
        }
        CompressServer => {
            let (cfg, level) = (srv()?, form.num("level", 5.0)?.clamp(1.0, 9.0) as u8);
            Box::new(move |pdf: Arc<Vec<u8>>| {
                server::compress(&cfg, &pdf, level).map(|r| reply_out(r, "compressed"))
            })
        }
        Repair => {
            let use_server = form.f("server", false);
            let cfg = if use_server { Some(srv()?) } else { None };
            Box::new(move |pdf: Arc<Vec<u8>>| match cfg {
                Some(c) => server::repair(&c, &pdf).map(|r| reply_out(r, "repaired")),
                None => out(structure::repair(&pdf)),
            })
        }
        Ocr => {
            let langs: Vec<String> = form
                .t("langs", "eng")
                .split([',', ' '])
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            tools::ocr::check_languages(&langs)?;
            let p = range("pages")?;
            let mode = if form.f("force", false) {
                tools::ocr::Mode::Force
            } else {
                tools::ocr::Mode::Skip
            };
            Box::new(move |pdf: Arc<Vec<u8>>| out(tools::ocr::ocr_pdf(&pdf, &p, mode)))
        }
        PdfA => {
            let cfg = srv()?;
            Box::new(move |pdf: Arc<Vec<u8>>| {
                server::to_pdfa(&cfg, &pdf).map(|r| reply_out(r, "pdfa"))
            })
        }
        AutoSplit => {
            let opts = tools::qrsplit::Options {
                any_qr: form.f("any_qr", false),
                duplex: form.f("duplex", false),
            };
            Box::new(move |pdf: Arc<Vec<u8>>| {
                tools::qrsplit::auto_split(&pdf, opts).map(|f| JobOut::Out(Output::Files(f)))
            })
        }
        Grayscale => {
            let dpi = form.num("dpi", 150.0)?;
            Box::new(move |pdf: Arc<Vec<u8>>| out(raster::grayscale(&pdf, dpi)))
        }
        Invert => {
            let dpi = form.num("dpi", 150.0)?;
            Box::new(move |pdf: Arc<Vec<u8>>| out(raster::invert(&pdf, dpi)))
        }
        AdjustColors => {
            let a = raster::Adjust {
                brightness: form.num("bright", 0.0)? / 100.0,
                contrast: form.num("contrast", 0.0)? / 100.0,
                saturation: form.num("sat", 0.0)? / 100.0,
            };
            let dpi = form.num("dpi", 150.0)?;
            Box::new(move |pdf: Arc<Vec<u8>>| out(raster::adjust_colors(&pdf, a, dpi)))
        }
        FakeScan => {
            let o = raster::FakeScan {
                grayscale: form.f("gray", true),
                ..Default::default()
            };
            let dpi = form.num("dpi", 150.0)?;
            Box::new(move |pdf: Arc<Vec<u8>>| out(raster::fake_scan(&pdf, o, dpi)))
        }
    })
}

/// Visible page size in points for signature placement.
fn lopdf_page_size(pdf: &[u8], page: usize) -> Result<(f32, f32), String> {
    let p =
        hayro::hayro_syntax::Pdf::new(pdf.to_vec()).map_err(|_| "can't read PDF".to_string())?;
    p.pages()
        .get(page)
        .map(|pg| pg.render_dimensions())
        .ok_or_else(|| "no such page".into())
}

/// Tools whose form has prefill data to load when opened.
pub fn prefill_job(tool: Tool) -> Option<Job> {
    match tool {
        Metadata => Some(Box::new(move |doc: Arc<Vec<u8>>| {
            let m = structure::metadata_get(&doc)?;
            Ok(JobOut::Prefill(vec![
                ("title", m.title),
                ("author", m.author),
                ("subject", m.subject),
                ("keywords", m.keywords),
                ("creator", m.creator),
                ("producer", m.producer),
                ("created", m.creation_date),
                ("modified", m.mod_date),
            ]))
        })),
        Bookmarks => Some(Box::new(move |doc: Arc<Vec<u8>>| {
            let marks = structure::bookmarks_get(&doc)?;
            let text = marks
                .iter()
                .map(|b| format!("{}{} @ {}", "  ".repeat(b.level), b.title, b.page + 1))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(JobOut::Prefill(vec![("bookmarks", text)]))
        })),
        _ => None,
    }
}

// ---------------------------------------------------------------- view ----

pub const SIG_ASPECT: f32 = 0.375; // pad is 8:3

pub enum Action {
    Run,
    RunNewFile,
}

/// The form for `tool`. `editing` controls whether "Apply" (replace the
/// working copy) is available; new-file output always is.
pub fn view_form<'a, M: Clone + 'a>(
    tool: Tool,
    form: &'a Form,
    editing: bool,
    has_server: bool,
    on_form: impl Fn(FormMsg) -> M + Copy + 'a,
    on_action: impl Fn(Action) -> M + 'a,
) -> Element<'a, M> {
    let field =
        |label: &'static str, key: &'static str, placeholder: &'static str| -> Element<'a, M> {
            column![
                text(label).size(12).color(theme::MUTED),
                text_input(
                    placeholder,
                    form.text.get(key).map(String::as_str).unwrap_or("")
                )
                .on_input(move |v| on_form(FormMsg::Text(key, v)))
                .size(13),
            ]
            .spacing(2)
            .into()
        };
    let secret = |label: &'static str, key: &'static str| -> Element<'a, M> {
        column![
            text(label).size(12).color(theme::MUTED),
            text_input("", form.text.get(key).map(String::as_str).unwrap_or(""))
                .on_input(move |v| on_form(FormMsg::Text(key, v)))
                .secure(true)
                .size(13),
        ]
        .spacing(2)
        .into()
    };
    let flag = |label: &'static str, key: &'static str, default: bool| -> Element<'a, M> {
        checkbox(label, form.f(key, default))
            .on_toggle(move |v| on_form(FormMsg::Flag(key, v)))
            .size(15)
            .text_size(13)
            .into()
    };
    fn pick<'a, M: Clone + 'a, T: ToString + PartialEq + Clone + 'a>(
        label: &'static str,
        key: &'static str,
        options: Vec<T>,
        current: String,
        on_form: impl Fn(FormMsg) -> M + 'a,
    ) -> Element<'a, M> {
        let opts: Vec<String> = options.iter().map(T::to_string).collect();
        column![
            text(label).size(12).color(theme::MUTED),
            pick_list(opts, Some(current), move |v: String| on_form(
                FormMsg::Text(key, v)
            ))
            .text_size(13),
        ]
        .spacing(2)
        .into()
    }
    let paper = || {
        pick(
            "Paper",
            "paper",
            Paper::ALL.to_vec(),
            form.t("paper", "A4"),
            on_form,
        )
    };
    let anchor = |d: Anchor| {
        pick(
            "Position",
            "anchor",
            Anchor::ALL.to_vec(),
            form.t("anchor", &d.to_string()),
            on_form,
        )
    };
    let font = || {
        pick(
            "Font",
            "font",
            layout::Font::ALL.to_vec(),
            form.t("font", "Helvetica"),
            on_form,
        )
    };
    let pages_f = || field("Pages", "pages", "all · 1-3,7 · odd · 10-");
    let row2 =
        |a: Element<'a, M>, b: Element<'a, M>| -> Element<'a, M> { row![a, b].spacing(8).into() };

    let fields: Vec<Element<'a, M>> = match tool {
        SplitRanges => vec![field("Groups", "groups", "1-3, 4-8, 9-")],
        SplitSize => vec![field("Max size per file (MB)", "mb", "10")],
        SelectPages => vec![field("Pages to tick", "pages", "odd · 1-5,9 · 10-")],
        RemoveBlank => vec![field("Max ink per page (%)", "ink", "0.5")],
        Crop => vec![
            pages_f(),
            row2(field("Left", "left", "36"), field("Right", "right", "36")),
            row2(field("Top", "top", "36"), field("Bottom", "bottom", "36")),
        ],
        Nup => vec![
            pick(
                "Pages per sheet",
                "per",
                vec!["2", "4", "6", "9", "16"],
                form.t("per", "4"),
                on_form,
            ),
            paper(),
            flag("Border around pages", "border", false),
        ],
        Booklet => vec![paper()],
        ScalePaper => vec![pages_f(), paper()],
        ScaleContent => vec![pages_f(), field("Scale (%)", "factor", "100")],
        SplitSections => vec![
            pages_f(),
            row2(field("Columns", "cols", "2"), field("Rows", "rows", "1")),
        ],
        Overlay => vec![
            field("Other PDF", "other", "/path/to/overlay.pdf"),
            flag("Put it underneath instead", "under", false),
        ],
        InsertImages => vec![
            field(
                "Image files (one per line or ; separated)",
                "files",
                "~/Pictures/a.jpg; ~/Pictures/b.png",
            ),
            flag("Fit onto paper (else page = image size)", "fit", true),
            paper(),
        ],
        InsertUrl => vec![field("Web address", "url", "https://…")],
        PageNumbers => vec![
            pages_f(),
            field("Format", "format", "{n} · Page {n} of {total}"),
            row2(field("Start at", "start", "1"), field("Size", "size", "10")),
            anchor(Anchor::Bottom),
            font(),
        ],
        TextStamp => vec![
            pages_f(),
            field("Text", "text", "APPROVED"),
            anchor(Anchor::TopRight),
            row2(
                field("Size", "size", "18"),
                field("Colour", "color", "#c00000"),
            ),
            row2(
                field("Opacity (%)", "opacity", "100"),
                field("Rotation (°)", "rotation", "0"),
            ),
            font(),
        ],
        Watermark => vec![
            field("Text", "text", "CONFIDENTIAL"),
            row2(
                field("Size", "size", "64"),
                field("Colour", "color", "#808080"),
            ),
            row2(
                field("Opacity (%)", "opacity", "15"),
                field("Rotation (°)", "rotation", "45"),
            ),
            flag("Tile across the page", "tiled", false),
            font(),
        ],
        ImageStamp => vec![
            pages_f(),
            field("Image file", "image", "~/Pictures/logo.png"),
            row2(
                field("Width (pt)", "width", "120"),
                field("Opacity (%)", "opacity", "100"),
            ),
            anchor(Anchor::BottomRight),
        ],
        Signature => vec![
            container(
                Canvas::new(Pad {
                    strokes: &form.strokes,
                    on_stroke: move |s| on_form(FormMsg::Stroke(s)),
                })
                .width(Length::Fill)
                .height(120),
            )
            .style(|_: &Theme| container::Style {
                background: Some(iced::Color::WHITE.into()),
                border: iced::Border {
                    radius: 6.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into(),
            row![
                button(text("Clear").size(12))
                    .on_press(on_form(FormMsg::ClearStrokes))
                    .style(button::secondary),
                text(match form.sig_at {
                    Some(_) => "placed — click the page again to move it",
                    None => "then click on the page to place it",
                })
                .size(12)
                .color(theme::MUTED),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
            row2(
                field("Width (pt)", "width", "160"),
                field("Ink colour", "color", "#10206a"),
            ),
        ],
        Attachments => vec![field(
            "Files (one per line or ; separated)",
            "files",
            "~/Documents/invoice.xlsx",
        )],
        Bookmarks => vec![text_editor(&form.bookmarks)
            .on_action(move |a| on_form(FormMsg::Bookmarks(a)))
            .height(220)
            .font(iced::Font::MONOSPACE)
            .into()],
        Metadata => vec![
            field("Title", "title", ""),
            field("Author", "author", ""),
            field("Subject", "subject", ""),
            field("Keywords", "keywords", ""),
            row2(
                field("Creator", "creator", ""),
                field("Producer", "producer", ""),
            ),
            row2(
                field("Created", "created", "D:20261011…"),
                field("Modified", "modified", ""),
            ),
            flag("Remove all metadata instead", "wipe", false),
        ],
        AddPassword => vec![
            secret("Password to open (optional)", "user_pw"),
            secret("Owner password (to change permissions)", "owner_pw"),
            flag("Allow printing", "p_print", true),
            flag("Allow editing & page assembly", "p_modify", true),
            flag("Allow copying text", "p_copy", true),
            flag("Allow comments", "p_annotate", true),
            flag("Allow filling forms", "p_forms", true),
        ],
        RemovePassword => vec![secret("Current password", "password")],
        Sanitize => vec![
            flag("JavaScript", "s_js", true),
            flag("Embedded files", "s_files", true),
            flag("XMP metadata", "s_xmp", false),
            flag("Document properties", "s_info", false),
            flag("Links", "s_links", false),
            flag("Fonts (text may stop rendering)", "s_fonts", false),
        ],
        RedactText => vec![
            field(
                "Words or phrases (comma-separated)",
                "terms",
                "John Smith, 555-0100",
            ),
            flag("Match case", "case", false),
        ],
        RedactAreas => vec![
            text(format!(
                "{} box(es) marked. Drag on the page to add more.",
                form.regions.len()
            ))
            .size(12)
            .into(),
            button(text("Clear boxes").size(12))
                .on_press(on_form(FormMsg::ClearRegions))
                .style(button::secondary)
                .into(),
            field("Quality (dpi)", "dpi", "200"),
        ],
        FlattenAll => vec![pages_f(), field("Quality (dpi)", "dpi", "200")],
        CertSign => vec![
            field("Certificate (.p12 / .pfx)", "cert", "~/certs/me.p12"),
            secret("Certificate password", "cert_pw"),
            row2(
                field("Reason", "reason", ""),
                field("Location", "location", ""),
            ),
            flag("Visible signature", "visible", false),
        ],
        ToImages => vec![
            pages_f(),
            field("Resolution (dpi)", "dpi", "150"),
            flag("JPEG instead of PNG", "jpeg", false),
        ],
        ExtractImages => vec![pages_f()],
        ConvertTo => vec![pick(
            "Format",
            "target",
            convert::Target::ALL.to_vec(),
            form.t("target", &convert::Target::Docx.to_string()),
            on_form,
        )],
        Compare => vec![field("Other PDF", "other", "/path/to/other.pdf")],
        CompressServer => vec![field("Level 1 (light) – 9 (strong)", "level", "5")],
        Repair => vec![flag(
            "Use the server (Ghostscript/qpdf) instead",
            "server",
            false,
        )],
        Ocr => vec![
            pages_f(),
            field(
                "Languages — Latin script only (eng, deu, fra, spa…); accents are dropped",
                "langs",
                "eng",
            ),
            flag("Redo pages that already have text", "force", false),
        ],
        AutoSplit => vec![
            flag(
                "Split at any QR code (not only Stirling's separator sheet)",
                "any_qr",
                false,
            ),
            flag(
                "Duplex scan: also drop the blank back of each separator",
                "duplex",
                false,
            ),
        ],
        ExtractScans => vec![
            pages_f(),
            row2(
                field("Straighten tilts over (°)", "angle", "1"),
                field("Background tolerance (0–255)", "tolerance", "30"),
            ),
            row2(
                field("Min photo area (px²)", "min_area", "8000"),
                field("Ignore specks under (px²)", "min_contour", "500"),
            ),
            field("Trim edges (px)", "border", "1"),
        ],
        Grayscale | Invert => vec![field("Resolution (dpi)", "dpi", "150")],
        AdjustColors => vec![
            row2(
                field("Brightness (-100…100)", "bright", "0"),
                field("Contrast", "contrast", "0"),
            ),
            row2(
                field("Saturation", "sat", "0"),
                field("Resolution (dpi)", "dpi", "150"),
            ),
        ],
        FakeScan => vec![
            flag("Greyscale", "gray", true),
            field("Resolution (dpi)", "dpi", "150"),
        ],
        _ => vec![],
    };

    let blocked = tool.needs_server() && !has_server;
    let produces_report = matches!(tool, ShowJavaScript | Info | Compare | ValidateSignature);
    let produces_files = matches!(
        tool,
        SplitRanges
            | SplitBookmarks
            | SplitSize
            | ToImages
            | ExtractImages
            | ExtractScans
            | ExtractAttachments
            | AutoSplit
            | ConvertTo
    );
    let modifies = !(produces_report || produces_files || matches!(tool, SelectPages | AutoRename));

    let mut actions = row![].spacing(6);
    if blocked {
        actions = actions.push(
            text("Needs the Stirling server — see Server settings at the bottom of the tool list.")
                .size(12)
                .color(theme::MUTED),
        );
    } else if modifies {
        let inserts = matches!(tool, InsertImages | InsertUrl);
        actions = actions
            .push(
                button(text(if inserts { "Insert" } else { "Apply" }).size(13))
                    .on_press_maybe(editing.then(|| on_action(Action::Run))),
            )
            .push(
                button(text("Save as new file").size(13))
                    .on_press(on_action(Action::RunNewFile))
                    .style(button::secondary),
            );
    } else {
        let label = match tool {
            SelectPages => "Select",
            AutoRename => "Rename file",
            _ if produces_report => "Run",
            _ => "Save files",
        };
        actions = actions.push(button(text(label).size(13)).on_press(on_action(Action::Run)));
    }

    let mut col = column(fields).spacing(8);
    col = col.push(Space::with_height(4)).push(actions);
    if modifies && !editing && !blocked {
        col = col.push(
            text("Turn on Edit pages to apply to this file; or save the result as a new file.")
                .size(11)
                .color(theme::MUTED),
        );
    }
    col.into()
}

// ------------------------------------------------------- signature pad ----

struct Pad<'a, F> {
    strokes: &'a [Vec<(f32, f32)>],
    on_stroke: F,
}

impl<'a, M, F: Fn(Vec<(f32, f32)>) -> M> canvas::Program<M> for Pad<'a, F> {
    type State = Option<Vec<(f32, f32)>>;

    fn update(
        &self,
        state: &mut Self::State,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<M>) {
        let pos = cursor
            .position_in(bounds)
            .map(|p| (p.x / bounds.width, p.y / bounds.height));
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = pos {
                    *state = Some(vec![p]);
                    return (canvas::event::Status::Captured, None);
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some(s), Some(p)) = (state.as_mut(), pos) {
                    s.push(p);
                    return (canvas::event::Status::Captured, None);
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(s) = state.take() {
                    return (canvas::event::Status::Captured, Some((self.on_stroke)(s)));
                }
            }
            _ => {}
        }
        (canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let ink = Stroke::default()
            .with_width(2.0)
            .with_color(iced::Color::from_rgb8(0x10, 0x20, 0x6a));
        let to = |(x, y): (f32, f32)| Point::new(x * bounds.width, y * bounds.height);
        for s in self.strokes.iter().chain(state.iter()) {
            if s.len() < 2 {
                continue;
            }
            let path = Path::new(|b| {
                b.move_to(to(s[0]));
                for &p in &s[1..] {
                    b.line_to(to(p));
                }
            });
            frame.stroke(&path, ink);
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    /// 4-page text PDF with a bookmark, made with lopdf.
    fn sample() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
        );
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::new();
        for i in 0..4 {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 24.into()]),
                    Operation::new("Td", vec![72.into(), 700.into()]),
                    Operation::new(
                        "Tj",
                        vec![Object::string_literal(format!(
                            "Secret report page {}",
                            i + 1
                        ))],
                    ),
                    Operation::new("ET", vec![]),
                ],
            };
            let c = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            kids.push(
                doc.add_object(dictionary! {
                    "Type" => "Page", "Parent" => pages_id, "Contents" => c, "Resources" => resources,
                    "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                })
                .into(),
            );
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 4 }),
        );
        let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", cat);
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn png(dir: &std::path::Path) -> String {
        let p = dir.join("stamp.png");
        image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 30, 30, 160]))
            .save(&p)
            .unwrap();
        p.display().to_string()
    }

    /// Every native (non-server) tool, driven exactly like the UI does:
    /// form values → `job()` → run on the document → check the output.
    #[test]
    fn every_native_tool_runs_end_to_end() {
        let dir = std::env::temp_dir().join(format!("ogx-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let doc = Arc::new(sample());
        let other = dir.join("other.pdf");
        std::fs::write(&other, sample()).unwrap();
        let other = other.display().to_string();
        let img = png(&dir);
        let att = dir.join("notes.txt");
        std::fs::write(&att, b"attached").unwrap();
        let att = att.display().to_string();

        let cases: Vec<(Tool, Vec<(&'static str, &str)>)> = vec![
            (SplitRanges, vec![("groups", "1-2, 3-")]),
            (SplitSize, vec![("mb", "1")]),
            (SelectPages, vec![("pages", "odd")]),
            (Crop, vec![]),
            (Nup, vec![("per", "4")]),
            (Booklet, vec![]),
            (ScalePaper, vec![("paper", "A4")]),
            (ScaleContent, vec![("factor", "50")]),
            (SplitSections, vec![("cols", "2"), ("rows", "1")]),
            (SinglePage, vec![]),
            (Overlay, vec![("other", other.as_str())]),
            (InsertImages, vec![("files", img.as_str())]),
            (PageNumbers, vec![("format", "Page {n} of {total}")]),
            (TextStamp, vec![("text", "APPROVED")]),
            (Watermark, vec![]),
            (ImageStamp, vec![("image", img.as_str())]),
            (Attachments, vec![("files", att.as_str())]),
            (Metadata, vec![("title", "Quarterly"), ("author", "OG")]),
            (AddPassword, vec![("user_pw", "pw"), ("owner_pw", "owner")]),
            (Sanitize, vec![]),
            (RedactText, vec![("terms", "Secret")]),
            (RemoveAnnotations, vec![]),
            (RemoveImages, vec![]),
            (FlattenForms, vec![]),
            (FlattenAll, vec![("pages", "1"), ("dpi", "72")]),
            (UnlockForms, vec![]),
            (RemoveSignatures, vec![]),
            (ShowJavaScript, vec![]),
            (ToImages, vec![("pages", "1-2"), ("dpi", "50")]),
            (ConvertTo, vec![]),
            (ConvertTo, vec![("target", "Excel (.xlsx)")]),
            (ConvertTo, vec![("target", "PowerPoint (.pptx)")]),
            (Info, vec![]),
            (Compare, vec![("other", other.as_str())]),
            (AutoRename, vec![]),
            (CompressLossless, vec![]),
            (Repair, vec![]),
            (Grayscale, vec![("dpi", "50")]),
            (Invert, vec![("dpi", "50")]),
            (AdjustColors, vec![("bright", "10"), ("dpi", "50")]),
            (FakeScan, vec![("dpi", "50")]),
        ];
        let mut failures = Vec::new();
        for (tool, fields) in cases {
            let mut form = Form::default();
            for (k, v) in fields {
                form.update(FormMsg::Text(k, v.to_string()));
            }
            let input = JobInput {
                page_count: 4,
                current: 0,
                selected: vec![],
                file_name: "sample.pdf".into(),
                server: None,
            };
            let result = job(tool, &form, input).and_then(|j| j(doc.clone()));
            let check = match result {
                Err(e) => Err(e),
                Ok(JobOut::Out(Output::Pdf(b))) | Ok(JobOut::Insert(b)) => {
                    // password-protected output can't be parsed without the password
                    if tool == AddPassword {
                        Document::load_mem(&b)
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    } else {
                        hayro::hayro_syntax::Pdf::new(b)
                            .map(|_| ())
                            .map_err(|e| format!("output doesn't parse: {e:?}"))
                    }
                }
                Ok(JobOut::Out(Output::Files(f))) if f.is_empty() => Err("no files".into()),
                Ok(JobOut::Out(Output::Report(r))) if r.trim().is_empty() => {
                    Err("empty report".into())
                }
                Ok(_) => Ok(()),
            };
            match check {
                // correct answer for a document without forms/signatures
                Err(e)
                    if e.contains("has no form fields")
                        || e.contains("has no signature fields") => {}
                Err(e) => failures.push(format!("{}: {e}", tool.label())),
                Ok(()) => {}
            }
        }
        // signature needs strokes + a click position
        let mut form = Form::default();
        form.update(FormMsg::Stroke(vec![(0.1, 0.5), (0.5, 0.2), (0.9, 0.6)]));
        form.sig_at = Some((0.5, 0.8));
        let input = JobInput {
            page_count: 4,
            current: 1,
            selected: vec![],
            file_name: "s.pdf".into(),
            server: None,
        };
        if let Err(e) = job(Signature, &form, input).and_then(|j| j(doc.clone())) {
            failures.push(format!("Draw signature: {e}"));
        }
        // redaction boxes
        let mut form = Form::default();
        form.regions.push((0, 0.1, 0.05, 0.6, 0.15));
        form.update(FormMsg::Text("dpi", "72".into()));
        let input = JobInput {
            page_count: 4,
            current: 0,
            selected: vec![],
            file_name: "s.pdf".into(),
            server: None,
        };
        if let Err(e) = job(RedactAreas, &form, input).and_then(|j| j(doc.clone())) {
            failures.push(format!("Redact areas: {e}"));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            failures.is_empty(),
            "tools failed:\n{}",
            failures.join("\n")
        );
    }

    /// Visual check: `OGX_PDF=in.pdf OGX_DEMO_DIR=/tmp/out cargo test -p og-files demo_outputs`
    /// writes a handful of tool results for eyeballing. Skipped otherwise.
    #[test]
    fn demo_outputs() {
        let (Some(pdf), Some(out)) = (
            std::env::var_os("OGX_PDF"),
            std::env::var_os("OGX_DEMO_DIR"),
        ) else {
            return;
        };
        let doc = Arc::new(std::fs::read(pdf).unwrap());
        let n = hayro::hayro_syntax::Pdf::new(doc.to_vec())
            .unwrap()
            .pages()
            .len();
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let run = |tool: Tool, form: Form, name: &str| {
            let input = JobInput {
                page_count: n,
                current: 0,
                selected: vec![],
                file_name: "demo.pdf".into(),
                server: None,
            };
            match job(tool, &form, input)
                .and_then(|j| j(doc.clone()))
                .unwrap()
            {
                JobOut::Out(Output::Pdf(b)) => std::fs::write(out.join(name), b).unwrap(),
                _ => panic!("{name}: not a pdf"),
            }
        };
        let with = |kv: &[(&'static str, &str)]| {
            let mut f = Form::default();
            for (k, v) in kv {
                f.update(FormMsg::Text(k, v.to_string()));
            }
            f
        };
        run(Watermark, with(&[("text", "DRAFT")]), "watermark.pdf");
        run(
            PageNumbers,
            with(&[("format", "Page {n} of {total}")]),
            "numbers.pdf",
        );
        run(Nup, with(&[("per", "4")]), "nup.pdf");
        let mut f = with(&[]);
        f.update(FormMsg::Stroke(vec![
            (0.05, 0.7),
            (0.2, 0.2),
            (0.35, 0.8),
            (0.5, 0.3),
            (0.7, 0.75),
            (0.95, 0.4),
        ]));
        f.sig_at = Some((0.7, 0.85));
        run(Signature, f, "signature.pdf");
        let mut f = with(&[("dpi", "110")]);
        f.regions.push((0, 0.12, 0.08, 0.75, 0.14));
        run(RedactAreas, f, "redacted.pdf");
    }

    #[test]
    fn bookmark_editor_parses_levels() {
        let m = parse_bookmarks("Intro @ 1\n  Detail @ 2\nEnd @ 4", 4).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!((m[1].level, m[1].page), (1, 1));
        assert!(parse_bookmarks("Nope", 4).is_err());
        assert!(parse_bookmarks("Far @ 9", 4).is_err());
    }
}
