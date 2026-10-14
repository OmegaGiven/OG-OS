//! Word (.docx) viewer/editor.
//!
//! View mode shows the document as pages of rich text (selectable,
//! copyable); edit mode (Ctrl+E) adds a caret, typing, a formatting toolbar
//! and undo/redo, and Save (Ctrl+S) writes over the original atomically.
//!
//! Round-trip guarantee: document.xml is rebuilt from the original text:
//! paragraphs/tables/objects that weren't edited are copied byte for byte,
//! edited paragraphs are regenerated reusing their original pPr and run
//! rPr; all other parts of the package are copied raw. Saving an unedited
//! document reproduces document.xml exactly (see tests).

pub mod model;
pub mod parse;
pub mod styles;
pub mod widget;
pub mod write;

#[cfg(test)]
mod tests;

use super::Event;
use crate::widgets::icon;
use crate::{fsops, theme};
use iced::widget::{button, column, container, pick_list, row, text, tooltip, Space};
use iced::{Element, Length, Task, Theme};
use iced_fonts::Bootstrap;
use model::{Align, Attr, Editor};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use styles::{format_counter, Kind};
pub use widget::Act;

const EDIT_COLOR: iced::Color =
    iced::Color::from_rgb(1.0, 0xc8 as f32 / 255.0, 0x6b as f32 / 255.0);

pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
}

#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    pkg: parse::Package,
}

/// Read and parse off the UI thread.
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    let p = path.clone();
    let pkg = tokio::task::spawn_blocking(move || {
        let len = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
        if len > parse::FILE_LIMIT {
            return Err("document too large to edit".to_string());
        }
        let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
        parse::open_package(bytes)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Opened { path, pkg })
}

#[derive(Debug, Clone)]
pub enum Msg {
    Act(Act),
    Style(Kind),
    List(bool),
    Align(Align),
    ToggleEdit,
    Save,
    /// (content version, package-parts generation) that were written
    Saved(Result<(u64, u64), String>),
    Discard,
    Reloaded(Result<Opened, String>),
}

pub struct State {
    path: PathBuf,
    pkg: Arc<Vec<u8>>,
    main: String,
    ed: Editor,
    images: HashMap<String, iced::widget::image::Handle>,
    labels: HashMap<u64, String>,
    words: usize,
    /// ed.version the labels/word count were computed for
    counted: u64,
    editing: bool,
    saving: bool,
    saved_version: u64,
    /// package parts replaced/added by the editor (list definitions)
    parts: BTreeMap<String, Vec<u8>>,
    parts_gen: u64,
    saved_parts_gen: u64,
    numbering_part: Option<String>,
    numbering_xml: Option<String>,
    focus_gen: u64,
}

/// Bullet / number text for every list paragraph (Word counts per numId).
fn list_labels(ed: &Editor) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let mut counters: HashMap<String, [u32; 9]> = HashMap::new();
    let num = &ed.doc.numbering;
    for &path in &ed.flow {
        let Some(p) = ed.doc.para(path) else { continue };
        let Some((id, lvl, bullet)) = ed.list_of(p) else {
            continue;
        };
        let lvl = usize::from(lvl.min(8));
        if bullet {
            out.insert(p.id, ["•", "◦", "▪"][lvl % 3].to_string());
            continue;
        }
        let start = |l: usize| {
            num.overrides
                .get(&id)
                .and_then(|o| o.get(&(l as u8)))
                .copied()
                .or_else(|| num.level(&id, l as u8).map(|lv| lv.start))
                .unwrap_or(1)
        };
        let c = counters.entry(id.clone()).or_insert([0; 9]);
        c[lvl] = if c[lvl] == 0 { start(lvl) } else { c[lvl] + 1 };
        for d in c.iter_mut().skip(lvl + 1) {
            *d = 0;
        }
        let snapshot = *c;
        let pattern = num
            .level(&id, lvl as u8)
            .map(|l| l.text.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("%{}.", lvl + 1));
        let mut s = pattern;
        for k in (1..=9).rev() {
            let tag = format!("%{k}");
            if s.contains(&tag) {
                let v = if snapshot[k - 1] == 0 {
                    start(k - 1)
                } else {
                    snapshot[k - 1]
                };
                let fmt = num
                    .level(&id, (k - 1) as u8)
                    .map(|l| l.fmt.clone())
                    .unwrap_or_default();
                s = s.replace(&tag, &format_counter(&fmt, v));
            }
        }
        out.insert(p.id, s);
    }
    out
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let pkg = o.pkg;
        let images = pkg
            .images
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    iced::widget::image::Handle::from_bytes(v.as_ref().clone()),
                )
            })
            .collect();
        let ed = Editor::new(pkg.doc);
        let mut st = State {
            path: o.path,
            pkg: pkg.bytes,
            main: pkg.main,
            saved_version: ed.version,
            ed,
            images,
            labels: HashMap::new(),
            words: 0,
            counted: 0,
            editing: false,
            saving: false,
            parts: BTreeMap::new(),
            parts_gen: 0,
            saved_parts_gen: 0,
            numbering_part: pkg.numbering_part,
            numbering_xml: pkg.numbering_xml,
            focus_gen: 0,
        };
        st.recount();
        (st, Task::none())
    }

    fn recount(&mut self) {
        if self.counted != self.ed.version {
            self.counted = self.ed.version;
            self.labels = list_labels(&self.ed);
            self.words = self.ed.word_count();
        }
    }

    fn status(s: impl Into<String>) -> (Task<Msg>, Option<Event>) {
        (Task::none(), Some(Event::Status(s.into())))
    }

    /// A list definition to use: the document's own, else add one.
    fn list_id(&mut self, bullet: bool) -> Result<String, String> {
        if let Some(id) = self.ed.doc.numbering.find(bullet) {
            return Ok(id);
        }
        let part = self.numbering_part.clone().unwrap_or_else(|| {
            format!("{}/numbering.xml", parse::dir_of(&self.main))
                .trim_start_matches('/')
                .to_string()
        });
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(self.pkg.as_slice()))
            .map_err(|e| e.to_string())?;
        let rels_name = parse::rels_path(&self.main);
        let text_of = |z: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
                       parts: &BTreeMap<String, Vec<u8>>,
                       name: &str| {
            parts
                .get(name)
                .and_then(|b| String::from_utf8(b.clone()).ok())
                .or_else(|| parse::part_text(z, name))
        };
        let rels = text_of(&mut z, &self.parts, &rels_name);
        let types = text_of(&mut z, &self.parts, "[Content_Types].xml");
        let added = write::add_list(
            bullet,
            &part,
            self.numbering_xml.as_deref(),
            (&rels_name, rels.as_deref()),
            types.as_deref(),
        )
        .ok_or("this document's package can't take a list definition")?;
        self.parts.extend(added.parts);
        self.parts_gen += 1;
        self.parts
            .insert(part.clone(), added.numbering_xml.clone().into_bytes());
        self.ed
            .set_numbering(styles::Numbering::parse(&added.numbering_xml));
        self.numbering_xml = Some(added.numbering_xml);
        self.numbering_part = Some(part);
        Ok(added.num_id)
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let out = self.update_inner(msg);
        self.recount();
        out
    }

    fn update_inner(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let editing = self.editing;
        match msg {
            Msg::Act(a) => {
                let ed = &mut self.ed;
                match a {
                    Act::Select(a, h) => ed.set_sel(a, h),
                    Act::SelectAll => ed.select_all(),
                    Act::Move(m, ext) => ed.motion(m, ext),
                    _ if !editing => {}
                    Act::Insert(s) => ed.insert(&s),
                    Act::Enter => ed.enter(),
                    Act::LineBreak => ed.line_break(),
                    Act::Backspace(w) => ed.backspace(w),
                    Act::Delete(w) => ed.delete(w),
                    Act::Tab(back) => {
                        if !ed.cell_step(back) && !back {
                            ed.insert("\t");
                        }
                    }
                    Act::Toggle(attr) => {
                        ed.toggle(attr);
                        self.focus_gen += 1;
                    }
                    Act::Undo => {
                        ed.undo();
                        self.focus_gen += 1;
                    }
                    Act::Redo => {
                        ed.redo();
                        self.focus_gen += 1;
                    }
                    Act::Cut => {
                        if ed.has_sel() {
                            ed.backspace(false);
                        }
                    }
                }
                (Task::none(), None)
            }
            Msg::Style(k) if editing => {
                self.ed.set_kind(k);
                self.focus_gen += 1;
                (Task::none(), None)
            }
            Msg::Align(a) if editing => {
                self.ed.set_align(a);
                self.focus_gen += 1;
                (Task::none(), None)
            }
            Msg::List(bullet) if editing => {
                self.focus_gen += 1;
                // removing a list needs no definition
                let removing = self.ed.sel_paths().iter().all(|&p| {
                    self.ed
                        .doc
                        .para(p)
                        .and_then(|p| self.ed.list_of(p))
                        .is_some_and(|l| l.2 == bullet)
                });
                let id = if removing {
                    String::new()
                } else {
                    match self.list_id(bullet) {
                        Ok(id) => id,
                        Err(e) => return Self::status(format!("Can't add a list: {e}")),
                    }
                };
                self.ed.toggle_list(bullet, &id);
                (Task::none(), None)
            }
            Msg::Style(_) | Msg::Align(_) | Msg::List(_) => (Task::none(), None),
            Msg::ToggleEdit => {
                if self.editing && self.is_dirty() {
                    return Self::status("Unsaved changes: Save or Discard first");
                }
                self.editing = !self.editing;
                self.focus_gen += 1;
                (Task::none(), None)
            }
            Msg::Save => {
                self.focus_gen += 1;
                (self.save(), None)
            }
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok((v, g)) => {
                        self.saved_version = v;
                        self.saved_parts_gen = g;
                        Self::status(format!("Saved over {}", self.path.display()))
                    }
                    Err(e) => Self::status(format!("Save failed: {e}")),
                }
            }
            Msg::Discard => (Task::perform(open(self.path.clone()), Msg::Reloaded), None),
            Msg::Reloaded(r) => match r {
                Ok(o) => {
                    let (st, _) = State::new(o);
                    *self = st;
                    Self::status("Changes discarded")
                }
                Err(e) => Self::status(format!("Reload failed: {e}")),
            },
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.ed.version != self.saved_version || self.parts_gen != self.saved_parts_gen
    }

    pub fn is_editing(&self) -> Option<bool> {
        Some(self.editing)
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        if self.editing && self.is_dirty() {
            return Task::done(Msg::ToggleEdit);
        }
        self.update(Msg::ToggleEdit).0
    }

    /// The bytes a save would write (document.xml regenerated, rest copied).
    #[cfg(test)]
    pub fn package_bytes(&self) -> Result<Vec<u8>, String> {
        let xml = write::document_xml(&self.ed.doc);
        write::package(&self.pkg, &self.main, &xml, &self.parts)
    }

    pub fn save(&mut self) -> Task<Msg> {
        if self.saving || !self.is_dirty() {
            return Task::none();
        }
        self.saving = true;
        let doc = self.ed.doc.clone();
        let pkg = self.pkg.clone();
        let main = self.main.clone();
        let parts = self.parts.clone();
        let version = (self.ed.version, self.parts_gen);
        let path = self.path.clone();
        Task::perform(
            async move {
                let bytes = tokio::task::spawn_blocking(move || {
                    let xml = write::document_xml(&doc);
                    write::package(&pkg, &main, &xml, &parts)
                })
                .await
                .map_err(|e| e.to_string())??;
                fsops::write_bytes(path, bytes).await?;
                Ok(version)
            },
            Msg::Saved,
        )
    }

    // ------------------------------------------------------------- view

    fn toolbar(&self) -> Element<'_, Msg> {
        let ed = &self.ed;
        let tb = |b: Bootstrap, on: bool, msg: Msg, tip: &'static str| -> Element<'_, Msg> {
            tooltip(
                button(icon(b).size(14))
                    .padding([4, 7])
                    .on_press(msg)
                    .style(if on {
                        button::primary
                    } else {
                        button::secondary
                    }),
                container(text(tip).size(12))
                    .padding([3, 6])
                    .style(container::rounded_box),
                tooltip::Position::Bottom,
            )
            .into()
        };
        let kind = ed.kind_at();
        let mut kinds: Vec<Kind> = Kind::MENU.to_vec();
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
        let list = ed.list_at();
        let align = ed.align_at();
        let sep = || {
            container(Space::new(1, 18)).style(|t: &Theme| container::Style {
                background: Some(t.extended_palette().background.strong.color.into()),
                ..Default::default()
            })
        };
        row![
            pick_list(kinds, Some(kind), Msg::Style)
                .text_size(12)
                .width(130),
            sep(),
            tb(
                Bootstrap::TypeBold,
                ed.attr_on(Attr::Bold),
                Msg::Act(Act::Toggle(Attr::Bold)),
                "Bold  Ctrl+B"
            ),
            tb(
                Bootstrap::TypeItalic,
                ed.attr_on(Attr::Italic),
                Msg::Act(Act::Toggle(Attr::Italic)),
                "Italic  Ctrl+I"
            ),
            tb(
                Bootstrap::TypeUnderline,
                ed.attr_on(Attr::Underline),
                Msg::Act(Act::Toggle(Attr::Underline)),
                "Underline  Ctrl+U"
            ),
            tb(
                Bootstrap::TypeStrikethrough,
                ed.attr_on(Attr::Strike),
                Msg::Act(Act::Toggle(Attr::Strike)),
                "Strikethrough"
            ),
            sep(),
            tb(
                Bootstrap::ListUl,
                list == Some(true),
                Msg::List(true),
                "Bulleted list"
            ),
            tb(
                Bootstrap::ListOl,
                list == Some(false),
                Msg::List(false),
                "Numbered list"
            ),
            sep(),
            tb(
                Bootstrap::TextLeft,
                align == Align::Left,
                Msg::Align(Align::Left),
                "Align left"
            ),
            tb(
                Bootstrap::TextCenter,
                align == Align::Center,
                Msg::Align(Align::Center),
                "Center"
            ),
            tb(
                Bootstrap::TextRight,
                align == Align::Right,
                Msg::Align(Align::Right),
                "Align right"
            ),
            tb(
                Bootstrap::Justify,
                align == Align::Justify,
                Msg::Align(Align::Justify),
                "Justify"
            ),
            sep(),
            tooltip(
                button(icon(Bootstrap::ArrowCounterclockwise).size(14))
                    .padding([4, 7])
                    .on_press_maybe(ed.can_undo().then_some(Msg::Act(Act::Undo)))
                    .style(button::secondary),
                container(text("Undo  Ctrl+Z").size(12))
                    .padding([3, 6])
                    .style(container::rounded_box),
                tooltip::Position::Bottom,
            ),
            tooltip(
                button(icon(Bootstrap::ArrowClockwise).size(14))
                    .padding([4, 7])
                    .on_press_maybe(ed.can_redo().then_some(Msg::Act(Act::Redo)))
                    .style(button::secondary),
                container(text("Redo  Ctrl+Shift+Z").size(12))
                    .padding([3, 6])
                    .style(container::rounded_box),
                tooltip::Position::Bottom,
            ),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let dirty = self.is_dirty();
        let words = text(format!(
            "{} word{}",
            self.words,
            if self.words == 1 { "" } else { "s" }
        ))
        .size(12)
        .color(theme::MUTED);
        let mut bar = row![].spacing(6).align_y(iced::Alignment::Center);
        bar = if self.editing {
            bar.push(
                text(if dirty {
                    "EDITING  * modified"
                } else {
                    "EDITING"
                })
                .size(12)
                .color(EDIT_COLOR),
            )
        } else {
            bar.push(
                text("Word document · read-only")
                    .size(12)
                    .color(theme::MUTED),
            )
        };
        bar = bar.push(Space::with_width(Length::Fill)).push(words);
        bar = if self.editing {
            bar.push(
                button(text("Discard").size(12))
                    .on_press_maybe(dirty.then_some(Msg::Discard))
                    .style(button::secondary),
            )
            .push(button(text("Save  Ctrl+S").size(12)).on_press_maybe(dirty.then_some(Msg::Save)))
            .push(
                button(text("Done").size(12))
                    .on_press_maybe((!dirty).then_some(Msg::ToggleEdit))
                    .style(button::secondary),
            )
        } else {
            bar.push(button(text("Edit  Ctrl+E").size(12)).on_press(Msg::ToggleEdit))
        };
        let page = widget::Page {
            ed: &self.ed,
            images: &self.images,
            labels: &self.labels,
            editing: self.editing,
            focus_gen: self.focus_gen,
        };
        let mut col = column![bar].spacing(6);
        if self.editing {
            col = col.push(self.toolbar());
        }
        col.push(container(page).width(Length::Fill).height(Length::Fill))
            .into()
    }
}
