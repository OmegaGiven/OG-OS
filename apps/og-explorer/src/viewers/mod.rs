//! Viewer/editor registry. Each family is a module with its own State, Msg
//! and view; `Viewer` dispatches by enum (see SPEC.md §5 for the contract
//! every viewer follows and the backend planned for each placeholder).
//!
//! Loading is two-step so Messages stay Clone: an async loader produces a
//! plain-data `Loaded`, then `Viewer::from_loaded` builds the live state
//! (which may hold non-Clone things like a text_editor::Content).

pub mod archive;
pub mod audio;
pub mod convert;
pub mod doc;
pub mod hex;
pub mod image;
pub mod model3d;
pub mod pdf;
pub mod sheet;
pub mod video;
pub mod placeholder;
pub mod table;
pub mod text;

use crate::filetype::Kind;
use crate::fsops;
use iced::widget::{center, text as label};
use iced::{Element, Task};
use std::path::PathBuf;

pub const TEXT_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum Loaded {
    Text(String),
    Table(table::Data),
    Image(PathBuf),
    Pdf(pdf::Source),
    Locked(PathBuf),
    Placeholder(Kind),
}

pub enum Viewer {
    Loading,
    Text(text::State),
    Image(image::State),
    Pdf(pdf::State),
    Locked(convert::Locked),
    Table(table::State),
    Placeholder(placeholder::State),
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Ready(Result<Loaded, String>),
    Text(text::Msg),
    Image(image::Msg),
    Pdf(pdf::Msg),
    Locked(convert::LockedMsg),
    /// "Convert to PDF" from the image/placeholder views
    Convert,
    Converted(Result<PathBuf, String>),
}

/// What a viewer asks its tab / the app to do.
pub enum Event {
    Status(String),
    /// the file was renamed on disk (auto-rename)
    Renamed(PathBuf),
    /// open another file in its own tab (e.g. an archive entry extracted to
    /// the cache)
    OpenFile(PathBuf),
}

pub fn load(path: PathBuf, kind: Kind) -> Task<Msg> {
    let fut = async move {
        match kind {
            Kind::Image => Ok(Loaded::Image(path)),
            Kind::Doc if is_pdf(&path) => match pdf::open(path.clone()).await {
                // hayro can't decrypt without the password: offer the unlock screen
                Err(e) if e.contains("Decryption") => Ok(Loaded::Locked(path)),
                r => r.map(Loaded::Pdf),
            },
            Kind::Table if table::supported(&path) => {
                let delim = table::delimiter(&path);
                fsops::read_text(path, TEXT_LIMIT)
                    .await
                    .map(|s| Loaded::Table(table::parse(&s, delim)))
            }
            Kind::Text | Kind::Doc if text::supported(&path, kind) => {
                match fsops::read_text(path, TEXT_LIMIT).await {
                    Ok(s) => Ok(Loaded::Text(s)),
                    // not UTF-8 → binary placeholder (hex view later)
                    Err(e) if e == "not UTF-8 text" => Ok(Loaded::Placeholder(Kind::Binary)),
                    Err(e) => Err(e),
                }
            }
            other => Ok(Loaded::Placeholder(other)),
        }
    };
    Task::perform(fut, Msg::Ready)
}

impl Viewer {
    fn from_loaded(path: &std::path::Path, l: Loaded) -> (Viewer, Task<Msg>) {
        let v = match l {
            Loaded::Text(s) => Viewer::Text(text::State::new(path.to_path_buf(), &s)),
            Loaded::Table(d) => Viewer::Table(table::State::new(d)),
            Loaded::Image(p) => Viewer::Image(image::State::new(p)),
            Loaded::Pdf(src) => {
                let (st, t) = pdf::State::new(src);
                return (Viewer::Pdf(st), t.map(Msg::Pdf));
            }
            Loaded::Locked(p) => Viewer::Locked(convert::Locked::new(p)),
            Loaded::Placeholder(k) => {
                Viewer::Placeholder(placeholder::State::new(path.to_path_buf(), k))
            }
        };
        (v, Task::none())
    }

    pub fn is_dirty(&self) -> bool {
        match self {
            Viewer::Text(t) => t.dirty,
            Viewer::Pdf(p) => p.dirty,
            _ => false,
        }
    }

    /// Files open read-only; editing is an explicit mode (button / Ctrl+E)
    /// and saving overwrites the original. Viewers without an editor yet
    /// return None.
    pub fn is_editing(&self) -> Option<bool> {
        match self {
            Viewer::Text(t) => Some(t.editing),
            Viewer::Pdf(p) => Some(p.editing),
            _ => None,
        }
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        match self {
            Viewer::Text(s) => s.toggle_edit().map(Msg::Text),
            Viewer::Pdf(s) => s.toggle_edit().map(Msg::Pdf),
            _ => Task::none(),
        }
    }

    pub fn update(&mut self, path: &std::path::Path, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match (self, msg) {
            (this, Msg::Ready(r)) => {
                let (v, t) = match r {
                    Ok(l) => Viewer::from_loaded(path, l),
                    Err(e) => (Viewer::Error(e), Task::none()),
                };
                *this = v;
                (t, None)
            }
            (this @ Viewer::Locked(_), Msg::Locked(m)) => {
                let Viewer::Locked(l) = this else {
                    unreachable!()
                };
                let (t, opened) = l.update(m);
                match opened {
                    Some(src) => {
                        let (st, t2) = pdf::State::new(src);
                        *this = Viewer::Pdf(st);
                        (t2.map(Msg::Pdf), Some(Event::Status("Unlocked".into())))
                    }
                    None => (t.map(Msg::Locked), None),
                }
            }
            (_, Msg::Convert) | (Viewer::Image(_), Msg::Image(image::Msg::ToPdf)) => (
                Task::perform(convert::to_pdf(path.to_path_buf()), Msg::Converted),
                Some(Event::Status("Converting to PDF…".into())),
            ),
            (Viewer::Image(_), Msg::Image(image::Msg::SplitScan)) => (
                Task::perform(convert::split_scan(path.to_path_buf()), Msg::Converted),
                Some(Event::Status("Splitting scanned photos…".into())),
            ),
            (_, Msg::Converted(r)) => (Task::none(), Some(convert::status(r))),
            (Viewer::Pdf(s), Msg::Pdf(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Pdf), ev)
            }
            (Viewer::Text(s), Msg::Text(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Text), ev)
            }
            (Viewer::Image(s), Msg::Image(m)) => {
                s.update(m);
                (Task::none(), None)
            }
            _ => (Task::none(), None),
        }
    }

    /// Ctrl+S from the app. Viewers that can't save ignore it.
    pub fn save(&mut self) -> Task<Msg> {
        match self {
            Viewer::Text(s) => s.save().map(Msg::Text),
            Viewer::Pdf(s) => s.save().map(Msg::Pdf),
            _ => Task::none(),
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        match self {
            Viewer::Loading => center(label("Loading…")).into(),
            Viewer::Error(e) => center(label(format!("Can't open: {e}"))).into(),
            Viewer::Text(s) => s.view().map(Msg::Text),
            Viewer::Image(s) => s.view().map(Msg::Image),
            Viewer::Pdf(s) => s.view().map(Msg::Pdf),
            Viewer::Table(s) => s.view(),
            Viewer::Placeholder(s) => s.view(Msg::Convert),
            Viewer::Locked(s) => s.view().map(Msg::Locked),
        }
    }
}

fn is_pdf(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}
