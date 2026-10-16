//! PDF viewer + page editor.
//!
//! View mode: page thumbnails, zoom, page navigation, and non-destructive
//! exports (extract selected pages, split every N) that write new files.
//! Edit mode (Ctrl+E): rotate, delete, reorder, insert pages from another
//! PDF; Save writes over the original, Discard reloads it.
//!
//! Every page operation edits a *plan* — a list of (source, page, rotation)
//! — and nothing touches disk until Save/export, when `compose::build`
//! assembles the plan into a real PDF. Rendering is `engine` (hayro).
//!
//! Tools panel (`toolpanel`): the Stirling-PDF tool set. A tool runs on the
//! current document (unsaved page edits included). "Apply" (edit mode only)
//! makes the result the working copy — Save writes it over the file;
//! "Save as new file" writes `<name>-<tool>.pdf` beside it. Tools that
//! produce other files write them beside the PDF; reports show in the panel.

pub(crate) mod compose;
mod engine;
mod toolpanel;
pub(crate) mod tools;

use super::Event;
use crate::fsops;
use crate::theme;
use engine::Engine;
use iced::widget::{
    button, checkbox, column, container, image, mouse_area, row, scrollable, stack, text,
    text_input, Space,
};
use iced::{Element, Length, Point, Radians, Rotation, Task};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toolpanel::{Action, Form, FormMsg, JobOut, Tool};
use tools::server::ServerConfig;
use tools::Output;

const THUMB_W: f32 = 110.0;
// render a bit above display size so text stays crisp on HiDPI screens
const OVERSAMPLE: f32 = 1.5;
const ZOOMS: [f32; 10] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 6.0];

#[derive(Debug, Clone)]
pub struct Source {
    path: PathBuf,
    bytes: Arc<Vec<u8>>,
    engine: Engine,
    sizes: Vec<(f32, f32)>,
}

pub async fn open(path: PathBuf) -> Result<Source, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    open_bytes(path, bytes).await
}

/// A document that only exists in memory (tool result, inserted pages);
/// `path` is the file it will be saved over.
pub async fn open_bytes(path: PathBuf, bytes: Vec<u8>) -> Result<Source, String> {
    let bytes = Arc::new(bytes);
    let (engine, sizes) = engine::open(bytes.clone()).await?;
    if sizes.is_empty() {
        return Err("PDF has no pages".into());
    }
    Ok(Source {
        path,
        bytes,
        engine,
        sizes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Pg {
    src: usize,
    page: usize,
    rot: u16,
}

type PageKey = (usize, usize, u32);

pub struct State {
    path: PathBuf,
    sources: Vec<Source>,
    plan: Vec<Pg>,
    current: usize,
    selected: BTreeSet<usize>,
    zoom: usize, // index into ZOOMS
    thumbs: HashMap<(usize, usize), image::Handle>,
    page: Option<(PageKey, image::Handle)>,
    page_gen: u64,
    pub editing: bool,
    pub dirty: bool,
    busy: bool,
    insert_path: String,
    split_n: String,
    tools_open: bool,
    tool: Option<Tool>,
    form: Form,
    report: Option<(Tool, String)>,
    server: Option<ServerConfig>,
    // last pointer position over the page image, in image pixels
    mouse: (f32, f32),
    // redaction box being dragged: start point, 0..1
    drag: Option<(f32, f32)>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Thumb(usize, usize, image::Handle),
    Page(u64, PageKey, Result<image::Handle, String>),
    Go(usize),
    Prev,
    Next,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleSelect(usize),
    SelectAll,
    SelectNone,
    ToggleEdit,
    RotateLeft,
    RotateRight,
    Delete,
    MoveUp,
    MoveDown,
    InsertPath(String),
    Insert,
    Inserted(Result<Source, String>),
    Save,
    Saved(Result<(), String>),
    Discard,
    Reopened(Result<Source, String>),
    Extract,
    SplitN(String),
    Split,
    Exported(Result<Vec<PathBuf>, String>),
    ToggleTools,
    PickTool(Option<Tool>),
    Form(FormMsg),
    RunTool(bool),
    ToolDone(Tool, bool, Result<JobOut, String>),
    Prefilled(Result<JobOut, String>),
    WorkingLoaded(Tool, Result<Source, String>),
    Renamed(Result<PathBuf, String>),
    PageMove(Point),
    PagePress,
    PageRelease,
    Duplicate,
    InsertBlank,
    SaveCopy,
    SaveReport,
    ServerLoaded(Option<ServerConfig>),
    ServerSave,
    ServerSaved(Result<ServerConfig, String>),
    ServerTest,
    ServerTested(Result<String, String>),
}

fn thumbs_task(src: usize, s: &Source) -> Task<Msg> {
    let engine = s.engine.clone();
    let sizes = s.sizes.clone();
    let stream = iced::stream::channel(4, move |mut out| async move {
        use iced::futures::SinkExt;
        for (p, (w, _)) in sizes.iter().enumerate() {
            if let Ok(h) = engine.render(p, THUMB_W * OVERSAMPLE / w.max(1.0)).await {
                if out.send(Msg::Thumb(src, p, h)).await.is_err() {
                    return;
                }
            }
        }
    });
    Task::run(stream, |m| m)
}

impl State {
    pub fn new(source: Source) -> (Self, Task<Msg>) {
        let mut s = Self {
            path: source.path.clone(),
            sources: Vec::new(),
            plan: Vec::new(),
            current: 0,
            selected: BTreeSet::new(),
            zoom: 3,
            thumbs: HashMap::new(),
            page: None,
            page_gen: 0,
            editing: false,
            dirty: false,
            busy: false,
            insert_path: String::new(),
            split_n: "1".into(),
            tools_open: false,
            tool: None,
            form: Form::default(),
            report: None,
            server: None,
            mouse: (0.0, 0.0),
            drag: None,
        };
        // test/screenshot hook: OGX_TOOL="" opens the tool list,
        // OGX_TOOL="Watermark" (a tool label) opens that tool
        if let Ok(want) = std::env::var("OGX_TOOL") {
            s.tools_open = true;
            s.tool = toolpanel::CATEGORIES
                .iter()
                .flat_map(|(_, t)| t.iter())
                .copied()
                .find(|t| t.label() == want);
        }
        let t = s.reset(source);
        let srv = Task::perform(
            async {
                tokio::task::spawn_blocking(ServerConfig::load)
                    .await
                    .ok()
                    .flatten()
            },
            Msg::ServerLoaded,
        );
        (s, Task::batch([t, srv]))
    }

    /// Fresh plan = the file as it is on disk.
    fn reset(&mut self, source: Source) -> Task<Msg> {
        self.plan = (0..source.sizes.len())
            .map(|page| Pg {
                src: 0,
                page,
                rot: 0,
            })
            .collect();
        self.thumbs.clear();
        self.page = None;
        self.selected.clear();
        self.current = self.current.min(self.plan.len() - 1);
        let thumbs = thumbs_task(0, &source);
        self.sources = vec![source];
        Task::batch([thumbs, self.request_page()])
    }

    fn zoom(&self) -> f32 {
        ZOOMS[self.zoom]
    }

    fn request_page(&mut self) -> Task<Msg> {
        let Some(pg) = self.plan.get(self.current).copied() else {
            return Task::none();
        };
        let key = (pg.src, pg.page, (self.zoom() * 100.0) as u32);
        if self.page.as_ref().is_some_and(|(k, _)| *k == key) {
            return Task::none();
        }
        self.page_gen += 1;
        let gen = self.page_gen;
        let fut = self.sources[pg.src]
            .engine
            .render(pg.page, self.zoom() * OVERSAMPLE);
        Task::perform(fut, move |r| Msg::Page(gen, key, r))
    }

    /// Selected pages, or the current one if nothing is selected.
    fn targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            vec![self.current]
        } else {
            self.selected.iter().copied().collect()
        }
    }

    fn edited(&mut self) -> Task<Msg> {
        self.dirty = true;
        self.current = self.current.min(self.plan.len().saturating_sub(1));
        self.request_page()
    }

    fn plan_tuples(&self, idx: &[usize]) -> Vec<compose::PlanPage> {
        idx.iter()
            .map(|&i| self.plan[i])
            .map(|p| (p.src, p.page, p.rot))
            .collect()
    }

    fn sources_bytes(&self) -> Vec<Arc<Vec<u8>>> {
        self.sources.iter().map(|s| s.bytes.clone()).collect()
    }

    pub fn save(&mut self) -> Task<Msg> {
        if !self.dirty || self.busy {
            return Task::none();
        }
        self.busy = true;
        let (srcs, plan, path) = (
            self.sources_bytes(),
            self.plan_tuples(&(0..self.plan.len()).collect::<Vec<_>>()),
            self.path.clone(),
        );
        Task::perform(
            async move {
                let bytes = tokio::task::spawn_blocking(move || compose::build(&srcs, &plan))
                    .await
                    .map_err(|e| e.to_string())??;
                fsops::write_bytes(path, bytes).await
            },
            Msg::Saved,
        )
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        self.update(Msg::ToggleEdit).0
    }

    fn export(&mut self, parts: Vec<(String, Vec<usize>)>) -> Task<Msg> {
        self.busy = true;
        let srcs = self.sources_bytes();
        let parts: Vec<(String, Vec<compose::PlanPage>)> = parts
            .into_iter()
            .map(|(suffix, idx)| (suffix, self.plan_tuples(&idx)))
            .collect();
        let base = self.path.clone();
        Task::perform(
            async move {
                let mut written = Vec::new();
                for (suffix, plan) in parts {
                    let srcs = srcs.clone();
                    let bytes = tokio::task::spawn_blocking(move || compose::build(&srcs, &plan))
                        .await
                        .map_err(|e| e.to_string())??;
                    let out = unique_sibling(&base, &format!("{suffix}.pdf")).await;
                    fsops::write_bytes(out.clone(), bytes).await?;
                    written.push(out);
                }
                Ok(written)
            },
            Msg::Exported,
        )
    }

    /// The current document as the jobs see it: original bytes when the
    /// plan is untouched, otherwise assembled from the plan (off-thread).
    fn doc_thunk(&self) -> impl FnOnce() -> Result<Arc<Vec<u8>>, String> + Send + 'static {
        let identity = self.sources.len() == 1
            && self.plan.len() == self.sources[0].sizes.len()
            && self
                .plan
                .iter()
                .enumerate()
                .all(|(i, p)| p.src == 0 && p.page == i && p.rot == 0);
        let srcs = self.sources_bytes();
        let plan = self.plan_tuples(&(0..self.plan.len()).collect::<Vec<_>>());
        move || {
            if identity {
                Ok(srcs[0].clone())
            } else {
                compose::build(&srcs, &plan).map(Arc::new)
            }
        }
    }

    fn run_tool(&mut self, new_file: bool) -> (Task<Msg>, Option<Event>) {
        let Some(tool) = self.tool else {
            return (Task::none(), None);
        };
        if self.busy {
            return (Task::none(), None);
        }
        let input = toolpanel::JobInput {
            page_count: self.plan.len(),
            current: self.current,
            selected: self.selected.iter().copied().collect(),
            file_name: self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            server: self.server.clone(),
        };
        let job = match toolpanel::job(tool, &self.form, input) {
            Ok(j) => j,
            Err(e) => {
                return (
                    Task::none(),
                    Some(Event::Status(format!("{}: {e}", tool.label()))),
                )
            }
        };
        self.busy = true;
        let doc = self.doc_thunk();
        let task = Task::perform(
            async move {
                tokio::task::spawn_blocking(move || job(doc()?))
                    .await
                    .map_err(|_| "the tool crashed".to_string())?
            },
            move |r| Msg::ToolDone(tool, new_file, r),
        );
        (
            task,
            Some(Event::Status(format!("Running {}…", tool.label()))),
        )
    }

    fn write_files(&self, files: Vec<(String, Vec<u8>)>) -> Task<Msg> {
        let base = self.path.clone();
        Task::perform(
            async move {
                let mut written = Vec::new();
                for (suffix, bytes) in files {
                    let out = unique_sibling(&base, &suffix).await;
                    fsops::write_bytes(out.clone(), bytes).await?;
                    written.push(out);
                }
                Ok(written)
            },
            Msg::Exported,
        )
    }

    /// Page-local pointer position as 0..1 of the displayed page, if the
    /// page is shown unrotated (box tools need a 1:1 mapping).
    fn pointer_norm(&self) -> Option<(f32, f32)> {
        let pg = self.plan.get(self.current)?;
        if pg.rot != 0 {
            return None;
        }
        let (w, h) = self.sources[pg.src].sizes[pg.page];
        let (dw, dh) = (w * self.zoom(), h * self.zoom());
        Some((
            (self.mouse.0 / dw).clamp(0.0, 1.0),
            (self.mouse.1 / dh).clamp(0.0, 1.0),
        ))
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let none = (Task::none(), None);
        match msg {
            Msg::Thumb(src, page, h) => {
                self.thumbs.insert((src, page), h);
                none
            }
            Msg::Page(gen, key, r) if gen == self.page_gen => match r {
                Ok(h) => {
                    self.page = Some((key, h));
                    none
                }
                Err(e) => (Task::none(), Some(Event::Status(e))),
            },
            Msg::Page(..) => none, // stale render
            Msg::Go(i) => {
                self.current = i.min(self.plan.len() - 1);
                (self.request_page(), None)
            }
            Msg::Prev => self.update(Msg::Go(self.current.saturating_sub(1))),
            Msg::Next => self.update(Msg::Go(self.current + 1)),
            Msg::ZoomIn => {
                self.zoom = (self.zoom + 1).min(ZOOMS.len() - 1);
                (self.request_page(), None)
            }
            Msg::ZoomOut => {
                self.zoom = self.zoom.saturating_sub(1);
                (self.request_page(), None)
            }
            Msg::ZoomReset => {
                self.zoom = 2;
                (self.request_page(), None)
            }
            Msg::ToggleSelect(i) => {
                if !self.selected.remove(&i) {
                    self.selected.insert(i);
                }
                none
            }
            Msg::SelectAll => {
                self.selected = (0..self.plan.len()).collect();
                none
            }
            Msg::SelectNone => {
                self.selected.clear();
                none
            }
            Msg::ToggleEdit => {
                if self.editing && self.dirty {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Unsaved page edits: Save or Discard first".into(),
                        )),
                    );
                }
                self.editing = !self.editing;
                none
            }
            // ---- edit-mode operations (buttons only exist in edit mode) ----
            Msg::RotateLeft | Msg::RotateRight if self.editing => {
                let step = if matches!(msg, Msg::RotateRight) {
                    90
                } else {
                    270
                };
                for i in self.targets() {
                    self.plan[i].rot = (self.plan[i].rot + step) % 360;
                }
                (self.edited(), None)
            }
            Msg::Delete if self.editing => {
                let t = self.targets();
                if t.len() >= self.plan.len() {
                    return (
                        Task::none(),
                        Some(Event::Status("Can't delete every page".into())),
                    );
                }
                for &i in t.iter().rev() {
                    self.plan.remove(i);
                }
                self.selected.clear();
                self.current = t[0].min(self.plan.len() - 1);
                (self.edited(), None)
            }
            Msg::MoveUp | Msg::MoveDown if self.editing => {
                let i = self.current;
                let j = if matches!(msg, Msg::MoveUp) {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|&j| j < self.plan.len())
                };
                let Some(j) = j else { return none };
                self.plan.swap(i, j);
                // selection marks follow the pages they were on
                let (si, sj) = (self.selected.remove(&i), self.selected.remove(&j));
                if si {
                    self.selected.insert(j);
                }
                if sj {
                    self.selected.insert(i);
                }
                self.current = j;
                (self.edited(), None)
            }
            Msg::InsertPath(p) => {
                self.insert_path = p;
                none
            }
            Msg::Insert if self.editing && !self.insert_path.trim().is_empty() => {
                let p = expand_home(self.insert_path.trim());
                self.busy = true;
                (Task::perform(open(p), Msg::Inserted), None)
            }
            Msg::Inserted(r) => {
                self.busy = false;
                match r {
                    Ok(src) => {
                        let s = self.sources.len();
                        let n = src.sizes.len();
                        let at = (self.current + 1).min(self.plan.len());
                        self.plan.splice(
                            at..at,
                            (0..n).map(|page| Pg {
                                src: s,
                                page,
                                rot: 0,
                            }),
                        );
                        let name = src.path.display().to_string();
                        let thumbs = thumbs_task(s, &src);
                        self.sources.push(src);
                        self.insert_path.clear();
                        self.selected.clear();
                        self.current = at;
                        let t = self.edited();
                        (
                            Task::batch([thumbs, t]),
                            Some(Event::Status(format!("Inserted {n} pages from {name}"))),
                        )
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!("Insert failed: {e}"))),
                    ),
                }
            }
            Msg::Save => (self.save(), None),
            Msg::Saved(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        self.dirty = false;
                        // reload so the plan matches the new file (rotations baked in)
                        let t = Task::perform(open(self.path.clone()), Msg::Reopened);
                        (
                            t,
                            Some(Event::Status(format!("Saved over {}", self.path.display()))),
                        )
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!("Save failed: {e}"))),
                    ),
                }
            }
            Msg::Discard => {
                self.dirty = false;
                self.editing = false;
                (
                    Task::perform(open(self.path.clone()), Msg::Reopened),
                    Some(Event::Status("Page edits discarded".into())),
                )
            }
            Msg::Reopened(r) => match r {
                Ok(src) => (self.reset(src), None),
                Err(e) => (
                    Task::none(),
                    Some(Event::Status(format!("Reload failed: {e}"))),
                ),
            },
            // ---- exports: never modify the original, work in any mode ----
            Msg::Extract if !self.busy => {
                let t = self.targets();
                let suffix = format!("pages-{}", ranges(&t));
                (self.export(vec![(suffix, t)]), None)
            }
            Msg::SplitN(s) => {
                self.split_n = s.chars().filter(|c| c.is_ascii_digit()).collect();
                none
            }
            Msg::Split if !self.busy => {
                let n: usize = self.split_n.parse().unwrap_or(0);
                if n == 0 || n >= self.plan.len() {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Split size must be between 1 and the page count".into(),
                        )),
                    );
                }
                let all: Vec<usize> = (0..self.plan.len()).collect();
                let width = all.len().div_ceil(n).to_string().len();
                let parts = all
                    .chunks(n)
                    .enumerate()
                    .map(|(k, c)| (format!("part-{:0width$}", k + 1), c.to_vec()))
                    .collect();
                (self.export(parts), None)
            }
            Msg::Exported(r) => {
                self.busy = false;
                let status = match r {
                    Ok(files) if files.len() == 1 => format!("Exported {}", files[0].display()),
                    Ok(files) => format!("Exported {} files next to the original", files.len()),
                    Err(e) => format!("Export failed: {e}"),
                };
                (Task::none(), Some(Event::Status(status)))
            }
            Msg::ToggleTools => {
                self.tools_open = !self.tools_open;
                none
            }
            Msg::PickTool(t) => {
                self.tool = t;
                self.report = None;
                self.drag = None;
                let Some(tool) = t else { return none };
                match toolpanel::prefill_job(tool) {
                    Some(job) => {
                        let doc = self.doc_thunk();
                        let task = Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || job(doc()?))
                                    .await
                                    .map_err(|_| "crashed".to_string())?
                            },
                            Msg::Prefilled,
                        );
                        (task, None)
                    }
                    None => none,
                }
            }
            Msg::Form(m) => {
                self.form.update(m);
                none
            }
            Msg::Prefilled(r) => {
                if let Ok(JobOut::Prefill(v)) = r {
                    self.form.prefill(v);
                }
                none
            }
            Msg::RunTool(new_file) => self.run_tool(new_file),
            Msg::ToolDone(tool, new_file, r) => {
                self.busy = false;
                let r = match r {
                    Ok(r) => r,
                    Err(e) => {
                        return (
                            Task::none(),
                            Some(Event::Status(format!("{}: {e}", tool.label()))),
                        )
                    }
                };
                let slug = tool
                    .label()
                    .to_lowercase()
                    .replace(|c: char| !c.is_alphanumeric(), "-");
                let slug = slug.trim_matches('-').replace("--", "-");
                // a document plus a report: show the report, then treat it
                // like any other new document version
                let r = match r {
                    JobOut::PdfReport(bytes, report) => {
                        self.report = Some((tool, report));
                        JobOut::Out(Output::Pdf(bytes))
                    }
                    r => r,
                };
                match r {
                    JobOut::Out(Output::Pdf(bytes)) | JobOut::Insert(bytes)
                        if new_file || !self.editing =>
                    {
                        self.busy = true;
                        (self.write_files(vec![(format!("{slug}.pdf"), bytes)]), None)
                    }
                    JobOut::Out(Output::Pdf(bytes)) => {
                        self.busy = true;
                        let task = Task::perform(open_bytes(self.path.clone(), bytes), move |r| {
                            Msg::WorkingLoaded(tool, r)
                        });
                        (task, None)
                    }
                    JobOut::Insert(bytes) => {
                        self.busy = true;
                        let p = self.path.with_file_name(format!("{slug}.pdf"));
                        (Task::perform(open_bytes(p, bytes), Msg::Inserted), None)
                    }
                    JobOut::Out(Output::Files(files)) => {
                        self.busy = true;
                        (self.write_files(files), None)
                    }
                    JobOut::Out(Output::Report(r)) => {
                        self.report = Some((tool, r));
                        (
                            Task::none(),
                            Some(Event::Status(format!("{} done", tool.label()))),
                        )
                    }
                    JobOut::Select(p) => {
                        let n = p.len();
                        self.selected = p.into_iter().collect();
                        (
                            Task::none(),
                            Some(Event::Status(format!("{n} pages selected"))),
                        )
                    }
                    JobOut::Prefill(v) => {
                        self.form.prefill(v);
                        none
                    }
                    // converted to Out(Pdf) above
                    JobOut::PdfReport(..) => none,
                    JobOut::Rename(name) => {
                        if self.dirty {
                            return (
                                Task::none(),
                                Some(Event::Status(
                                    "Save or discard edits before renaming".into(),
                                )),
                            );
                        }
                        let from = self.path.clone();
                        let task = Task::perform(
                            async move {
                                let dir = from.parent().unwrap_or(Path::new(".")).to_path_buf();
                                let mut to = dir.join(&name);
                                let mut n = 2;
                                while tokio::fs::try_exists(&to).await.unwrap_or(false) {
                                    let stem = name.trim_end_matches(".pdf");
                                    to = dir.join(format!("{stem} ({n}).pdf"));
                                    n += 1;
                                }
                                tokio::fs::rename(&from, &to)
                                    .await
                                    .map_err(|e| e.to_string())?;
                                Ok(to)
                            },
                            Msg::Renamed,
                        );
                        (task, None)
                    }
                }
            }
            Msg::WorkingLoaded(tool, r) => {
                self.busy = false;
                match r {
                    Ok(src) => {
                        let t = self.reset(src);
                        self.dirty = true;
                        self.form.regions.clear();
                        (
                            t,
                            Some(Event::Status(format!(
                                "{} applied — Save to keep it, Discard to undo",
                                tool.label()
                            ))),
                        )
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!(
                            "{}: result didn't load: {e}",
                            tool.label()
                        ))),
                    ),
                }
            }
            Msg::Renamed(r) => match r {
                Ok(p) => {
                    self.path = p.clone();
                    if let Some(s) = self.sources.first_mut() {
                        s.path = p.clone();
                    }
                    (Task::none(), Some(Event::Renamed(p)))
                }
                Err(e) => (
                    Task::none(),
                    Some(Event::Status(format!("Rename failed: {e}"))),
                ),
            },
            Msg::PageMove(p) => {
                self.mouse = (p.x, p.y);
                none
            }
            Msg::PagePress => match (self.tool, self.pointer_norm()) {
                (Some(Tool::RedactAreas), Some(pt)) => {
                    self.drag = Some(pt);
                    none
                }
                (Some(Tool::Signature), Some(pt)) => {
                    self.form.sig_at = Some(pt);
                    none
                }
                (Some(Tool::RedactAreas | Tool::Signature), None) => (
                    Task::none(),
                    Some(Event::Status(
                        "Save pending rotations first, then place boxes".into(),
                    )),
                ),
                _ => none,
            },
            Msg::PageRelease => {
                if let (Some((x0, y0)), Some((x1, y1))) = (self.drag.take(), self.pointer_norm()) {
                    if (x1 - x0).abs() > 0.005 && (y1 - y0).abs() > 0.005 {
                        self.form.regions.push((
                            self.current,
                            x0.min(x1),
                            y0.min(y1),
                            x0.max(x1),
                            y0.max(y1),
                        ));
                    }
                }
                none
            }
            Msg::Duplicate if self.editing => {
                let t = self.targets();
                let at = t.last().copied().unwrap_or(self.current) + 1;
                let copies: Vec<Pg> = t.iter().map(|&i| self.plan[i]).collect();
                let n = copies.len();
                self.plan.splice(at..at, copies);
                self.selected.clear();
                self.current = at;
                (
                    self.edited(),
                    Some(Event::Status(format!("Duplicated {n} page(s)"))),
                )
            }
            Msg::InsertBlank if self.editing => {
                let pg = self.plan[self.current];
                let (w, h) = self.sources[pg.src].sizes[pg.page];
                let p = self.path.with_file_name("blank.pdf");
                self.busy = true;
                let task = Task::perform(
                    async move {
                        let bytes =
                            tokio::task::spawn_blocking(move || tools::layout::blank_page(w, h))
                                .await
                                .map_err(|e| e.to_string())??;
                        open_bytes(p, bytes).await
                    },
                    Msg::Inserted,
                );
                (task, None)
            }
            Msg::SaveCopy if !self.busy => {
                self.busy = true;
                let doc = self.doc_thunk();
                let base = self.path.clone();
                let task = Task::perform(
                    async move {
                        let bytes = tokio::task::spawn_blocking(doc)
                            .await
                            .map_err(|e| e.to_string())??;
                        let out = unique_sibling(&base, "copy.pdf").await;
                        fsops::write_bytes(out.clone(), bytes.to_vec()).await?;
                        Ok(vec![out])
                    },
                    Msg::Exported,
                );
                (task, None)
            }
            Msg::SaveReport => match &self.report {
                Some((tool, r)) => {
                    let slug = tool
                        .label()
                        .to_lowercase()
                        .replace(|c: char| !c.is_alphanumeric(), "-");
                    self.busy = true;
                    (
                        self.write_files(vec![(
                            format!("{}.txt", slug.trim_matches('-')),
                            r.clone().into_bytes(),
                        )]),
                        None,
                    )
                }
                None => none,
            },
            Msg::ServerLoaded(cfg) => {
                if let Some(c) = &cfg {
                    self.form.prefill(vec![
                        ("srv_url", c.url.clone()),
                        ("srv_key", c.api_key.clone()),
                    ]);
                }
                self.server = cfg;
                none
            }
            Msg::ServerSave => {
                let cfg = ServerConfig {
                    url: self
                        .form_text("srv_url")
                        .trim()
                        .trim_end_matches('/')
                        .to_string(),
                    api_key: self.form_text("srv_key").trim().to_string(),
                };
                let task = Task::perform(
                    async move {
                        let path = crate::config::config_dir().join("stirling.json");
                        let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
                        if let Some(d) = path.parent() {
                            tokio::fs::create_dir_all(d)
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        tokio::fs::write(&path, json)
                            .await
                            .map_err(|e| e.to_string())?;
                        // the key is a secret: keep the file private
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = tokio::fs::set_permissions(
                                &path,
                                std::fs::Permissions::from_mode(0o600),
                            )
                            .await;
                        }
                        Ok(cfg)
                    },
                    Msg::ServerSaved,
                );
                (task, None)
            }
            Msg::ServerSaved(r) => match r {
                Ok(c) => {
                    self.server = (!c.url.is_empty()).then_some(c);
                    (
                        Task::none(),
                        Some(Event::Status("Server settings saved".into())),
                    )
                }
                Err(e) => (
                    Task::none(),
                    Some(Event::Status(format!("Couldn't save server settings: {e}"))),
                ),
            },
            Msg::ServerTest => match self.server.clone() {
                Some(c) => (
                    Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || tools::server::ping(&c))
                                .await
                                .map_err(|e| e.to_string())?
                        },
                        Msg::ServerTested,
                    ),
                    None,
                ),
                None => (
                    Task::none(),
                    Some(Event::Status("Save the server settings first".into())),
                ),
            },
            Msg::ServerTested(r) => (
                Task::none(),
                Some(Event::Status(match r {
                    Ok(s) => format!("Stirling server OK: {s}"),
                    Err(e) => format!("Stirling server: {e}"),
                })),
            ),
            _ => none,
        }
    }

    fn form_text(&self, k: &'static str) -> String {
        self.form.get_text(k)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let small = |s: &'static str| text(s).size(12);
        let nav = row![
            button(small("Prev"))
                .on_press(Msg::Prev)
                .style(button::secondary),
            text(format!("{} / {}", self.current + 1, self.plan.len())).size(12),
            button(small("Next"))
                .on_press(Msg::Next)
                .style(button::secondary),
            Space::with_width(12),
            button(small("-"))
                .on_press(Msg::ZoomOut)
                .style(button::secondary),
            text(format!("{:.0}%", self.zoom() * 100.0))
                .size(12)
                .width(44),
            button(small("+"))
                .on_press(Msg::ZoomIn)
                .style(button::secondary),
            button(small("100%"))
                .on_press(Msg::ZoomReset)
                .style(button::text),
            Space::with_width(12),
            button(
                text(if self.selected.is_empty() {
                    "Extract page".to_string()
                } else {
                    format!("Extract {} selected", self.selected.len())
                })
                .size(12)
            )
            .on_press_maybe((!self.busy).then_some(Msg::Extract))
            .style(button::secondary),
            text("Split every").size(12),
            text_input("1", &self.split_n)
                .on_input(Msg::SplitN)
                .width(40)
                .size(12),
            button(small("Split"))
                .on_press_maybe((!self.busy).then_some(Msg::Split))
                .style(button::secondary),
            button(small("Save copy"))
                .on_press_maybe((!self.busy).then_some(Msg::SaveCopy))
                .style(button::secondary),
            button(small("Tools"))
                .on_press(Msg::ToggleTools)
                .style(if self.tools_open {
                    button::primary
                } else {
                    button::secondary
                }),
            if self.editing {
                button(small("Done"))
                    .on_press_maybe((!self.dirty).then_some(Msg::ToggleEdit))
                    .style(button::secondary)
            } else {
                button(small("Edit pages  Ctrl+E")).on_press(Msg::ToggleEdit)
            },
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        // wraps onto a second line in a narrow viewer instead of crushing labels
        .wrap();

        let mut bars = column![nav].spacing(6);
        if self.editing {
            let on = |m: Msg| (!self.busy).then_some(m);
            bars = bars.push(
                row![
                    text(if self.dirty {
                        "EDITING  * modified"
                    } else {
                        "EDITING"
                    })
                    .size(12)
                    .color(iced::Color::from_rgb8(0xff, 0xc8, 0x6b)),
                    button(small("Rotate left"))
                        .on_press_maybe(on(Msg::RotateLeft))
                        .style(button::secondary),
                    button(small("Rotate right"))
                        .on_press_maybe(on(Msg::RotateRight))
                        .style(button::secondary),
                    button(small("Delete"))
                        .on_press_maybe(on(Msg::Delete))
                        .style(button::danger),
                    button(small("Move up"))
                        .on_press_maybe(on(Msg::MoveUp))
                        .style(button::secondary),
                    button(small("Move down"))
                        .on_press_maybe(on(Msg::MoveDown))
                        .style(button::secondary),
                    button(small("Duplicate"))
                        .on_press_maybe(on(Msg::Duplicate))
                        .style(button::secondary),
                    button(small("Blank page"))
                        .on_press_maybe(on(Msg::InsertBlank))
                        .style(button::secondary),
                    text_input(
                        "insert PDF after this page: /path/to/file.pdf",
                        &self.insert_path
                    )
                    .on_input(Msg::InsertPath)
                    .on_submit(Msg::Insert)
                    .size(12)
                    .width(Length::Fill),
                    button(small("Insert"))
                        .on_press_maybe(on(Msg::Insert))
                        .style(button::secondary),
                    button(small("Discard"))
                        .on_press_maybe(self.dirty.then_some(Msg::Discard))
                        .style(button::secondary),
                    button(small("Save  Ctrl+S"))
                        .on_press_maybe((self.dirty && !self.busy).then_some(Msg::Save)),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center)
                .wrap(),
            );
        }
        bars = bars.push(
            row![
                text(format!(
                    "{}{}",
                    if self.selected.is_empty() {
                        "Tick pages to act on several at once"
                    } else {
                        ""
                    },
                    if self.busy { "  ·  working…" } else { "" }
                ))
                .size(11)
                .color(theme::MUTED),
                Space::with_width(Length::Fill),
                button(text("Select all").size(11))
                    .on_press(Msg::SelectAll)
                    .style(button::text),
                button(text("Select none").size(11))
                    .on_press(Msg::SelectNone)
                    .style(button::text),
            ]
            .align_y(iced::Alignment::Center),
        );

        // TODO(phase 2): virtualize for 500+ page documents
        let strip = column(self.plan.iter().enumerate().map(|(i, pg)| {
            let (w, h) = self.sources[pg.src].sizes[pg.page];
            let thumb: Element<'_, Msg> = match self.thumbs.get(&(pg.src, pg.page)) {
                Some(hd) => image(hd.clone())
                    .width(THUMB_W)
                    .rotation(rot(pg.rot))
                    .into(),
                None => container(text("...").color(theme::MUTED))
                    .center_x(THUMB_W)
                    .center_y(THUMB_W * h / w.max(1.0))
                    .into(),
            };
            let label = if pg.src == 0 {
                format!("{}", i + 1)
            } else {
                format!("{}  (+)", i + 1)
            };
            column![
                button(thumb)
                    .on_press(Msg::Go(i))
                    .style(if i == self.current {
                        button::primary
                    } else {
                        button::text
                    })
                    .padding(3),
                row![
                    checkbox("", self.selected.contains(&i))
                        .on_toggle(move |_| Msg::ToggleSelect(i))
                        .size(14),
                    text(label).size(11)
                ]
                .spacing(2)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(2)
            .into()
        }))
        .spacing(8)
        .padding(4);

        let pg = self.plan[self.current];
        let (w, h) = self.sources[pg.src].sizes[pg.page];
        let (dw, dh) = (w * self.zoom(), h * self.zoom());
        let mut page: Element<'_, Msg> = match &self.page {
            // show the last render of this page (even at the old zoom) while a new one comes in
            Some(((s, p, _), hd)) if *s == pg.src && *p == pg.page => image(hd.clone())
                .width(Length::Fixed(dw))
                .rotation(rot(pg.rot))
                .into(),
            _ => text("Rendering…").color(theme::MUTED).into(),
        };
        // box tools: track the pointer over the page and draw marks on top
        if matches!(self.tool, Some(Tool::RedactAreas | Tool::Signature)) && self.tools_open {
            let mut layers = vec![page];
            let boxed = |x0: f32,
                         y0: f32,
                         x1: f32,
                         y1: f32,
                         fill: iced::Color,
                         border: iced::Color|
             -> Element<'_, Msg> {
                container(
                    container(Space::new(
                        Length::Fixed((x1 - x0) * dw),
                        Length::Fixed((y1 - y0) * dh),
                    ))
                    .style(move |_| container::Style {
                        background: Some(fill.into()),
                        border: iced::Border {
                            color: border,
                            width: 1.5,
                            radius: 2.0.into(),
                        },
                        ..Default::default()
                    }),
                )
                .padding(iced::Padding {
                    top: y0 * dh,
                    left: x0 * dw,
                    ..Default::default()
                })
                .into()
            };
            let black = iced::Color {
                a: 0.7,
                ..iced::Color::BLACK
            };
            for &(p, x0, y0, x1, y1) in &self.form.regions {
                if p == self.current {
                    layers.push(boxed(
                        x0,
                        y0,
                        x1,
                        y1,
                        black,
                        iced::Color::from_rgb8(0xff, 0x40, 0x40),
                    ));
                }
            }
            if let (Some((x0, y0)), Some((x1, y1))) = (self.drag, self.pointer_norm()) {
                layers.push(boxed(
                    x0.min(x1),
                    y0.min(y1),
                    x0.max(x1),
                    y0.max(y1),
                    iced::Color {
                        a: 0.3,
                        ..iced::Color::BLACK
                    },
                    iced::Color::WHITE,
                ));
            }
            if let (Some(Tool::Signature), Some((x, y))) = (self.tool, self.form.sig_at) {
                let sw = self
                    .form_text("width")
                    .trim()
                    .parse::<f32>()
                    .unwrap_or(160.0)
                    / w;
                let sh = sw * w * toolpanel::SIG_ASPECT / h;
                layers.push(boxed(
                    (x - sw / 2.0).max(0.0),
                    (y - sh / 2.0).max(0.0),
                    (x + sw / 2.0).min(1.0),
                    (y + sh / 2.0).min(1.0),
                    iced::Color::from_rgba8(0x10, 0x20, 0x6a, 0.15),
                    iced::Color::from_rgb8(0x10, 0x20, 0x6a),
                ));
            }
            page = mouse_area(stack(layers))
                .on_move(Msg::PageMove)
                .on_press(Msg::PagePress)
                .on_release(Msg::PageRelease)
                .interaction(iced::mouse::Interaction::Crosshair)
                .into();
        }

        let mut body = row![
            scrollable(strip).width(THUMB_W + 30.0).height(Length::Fill),
            scrollable(container(page).padding(12))
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(8);
        if self.tools_open {
            body = body.push(
                container(self.tools_view())
                    .width(340)
                    .height(Length::Fill)
                    .style(theme::panel),
            );
        }

        column![bars, body].spacing(8).into()
    }

    fn tools_view(&self) -> Element<'_, Msg> {
        let inner: Element<'_, Msg> = match self.tool {
            None => {
                let mut list = column![].spacing(4);
                for (cat, tools) in toolpanel::CATEGORIES {
                    list = list.push(text(cat).size(14)).push(Space::with_height(2));
                    for &t in tools {
                        let srv = t.needs_server();
                        let label = if srv {
                            format!("{}  · server", t.label())
                        } else {
                            t.label().to_string()
                        };
                        list = list.push(
                            button(text(label).size(12).color(if srv && self.server.is_none() {
                                theme::MUTED
                            } else {
                                iced::Color::WHITE
                            }))
                            .on_press(Msg::PickTool(Some(t)))
                            .style(button::text)
                            .padding([2, 6])
                            .width(Length::Fill),
                        );
                    }
                    list = list.push(Space::with_height(8));
                }
                // Stirling server settings
                let srv_field =
                    |label: &'static str, key: &'static str, secure: bool| -> Element<'_, Msg> {
                        column![
                            text(label).size(11).color(theme::MUTED),
                            text_input("", &self.form_text(key))
                                .on_input(move |v| Msg::Form(FormMsg::Text(key, v)))
                                .secure(secure)
                                .size(12),
                        ]
                        .spacing(2)
                        .into()
                    };
                list = list
                    .push(text("Server settings (Stirling-PDF)").size(14))
                    .push(
                        text(match &self.server {
                            Some(c) => format!("Using {}", c.url),
                            None => "Not set up. OCR, strong compression, Office conversion, PDF/A and certificate signing need it.".into(),
                        })
                        .size(11)
                        .color(theme::MUTED),
                    )
                    .push(srv_field("URL", "srv_url", false))
                    .push(srv_field("API key (Stirling: Account settings → API key)", "srv_key", true))
                    .push(
                        row![
                            button(text("Save").size(12)).on_press(Msg::ServerSave),
                            button(text("Test connection").size(12)).on_press(Msg::ServerTest).style(button::secondary),
                        ]
                        .spacing(6),
                    );
                list.into()
            }
            Some(tool) => {
                let mut col = column![
                    button(text("< All tools").size(12))
                        .on_press(Msg::PickTool(None))
                        .style(button::text),
                    text(tool.label()).size(16),
                    text(tool.help()).size(12).color(theme::MUTED),
                    Space::with_height(4),
                    toolpanel::view_form(
                        tool,
                        &self.form,
                        self.editing,
                        self.server.is_some(),
                        Msg::Form,
                        |a| match a {
                            Action::Run => Msg::RunTool(false),
                            Action::RunNewFile => Msg::RunTool(true),
                        }
                    ),
                ]
                .spacing(6);
                if self.busy {
                    col = col.push(text("working…").size(12).color(theme::MUTED));
                }
                if let Some((t, r)) = &self.report {
                    if *t == tool {
                        col = col
                            .push(Space::with_height(6))
                            .push(
                                row![
                                    text("Result").size(14),
                                    Space::with_width(Length::Fill),
                                    button(text("Save as .txt").size(11))
                                        .on_press(Msg::SaveReport)
                                        .style(button::secondary),
                                ]
                                .align_y(iced::Alignment::Center),
                            )
                            .push(
                                container(text(r.clone()).size(12).font(iced::Font::MONOSPACE))
                                    .padding(6)
                                    .style(theme::panel),
                            );
                    }
                }
                col.into()
            }
        };
        scrollable(container(inner).padding(10))
            .height(Length::Fill)
            .into()
    }
}

fn rot(deg: u16) -> Rotation {
    Rotation::Solid(Radians((deg as f32).to_radians()))
}

/// 0-based indices → "1-3_7_9-10" (1-based) for export file names.
fn ranges(idx: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < idx.len() {
        let start = idx[i];
        let mut end = start;
        while i + 1 < idx.len() && idx[i + 1] == end + 1 {
            i += 1;
            end = idx[i];
        }
        out.push(if start == end {
            format!("{}", start + 1)
        } else {
            format!("{}-{}", start + 1, end + 1)
        });
        i += 1;
    }
    let s = out.join("_");
    if s.len() > 40 {
        format!("{}-pages", idx.len())
    } else {
        s
    }
}

/// `<dir>/<stem>-<suffix>`, adding " (2)", " (3)"… before the extension
/// instead of overwriting. `suffix` includes the extension ("pages-1-3.pdf").
async fn unique_sibling(base: &Path, suffix: &str) -> PathBuf {
    let dir = base.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into());
    let mut n = 1;
    loop {
        let (body, ext) = match suffix.rsplit_once('.') {
            Some((b, e)) if !e.is_empty() && e.len() <= 5 => (b, format!(".{e}")),
            _ => (suffix, String::new()),
        };
        let name = if n == 1 {
            format!("{stem}-{body}{ext}")
        } else {
            format!("{stem}-{body} ({n}){ext}")
        };
        let p = dir.join(name);
        if !tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return p;
        }
        n += 1;
    }
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => crate::config::home().join(rest),
        None => PathBuf::from(p),
    }
}
