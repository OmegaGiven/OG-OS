//! Viewer/editor registry. Each family is a module with its own State, Msg
//! and view; `Viewer` dispatches by enum (see SPEC.md §5 for the contract
//! every viewer follows and the backend planned for each placeholder).
//!
//! Loading is two-step so Messages stay Clone: an async loader produces a
//! plain-data `Loaded`, then `Viewer::from_loaded` builds the live state
//! (which may hold non-Clone things like a text_editor::Content).
//!
//! Phase 2 viewers (audio, video, sheet, archive, hex, doc, 3D) share one
//! contract — `supported`, `open`, `State::{new, update, view, is_dirty,
//! is_editing, toggle_edit, save}` — so they're wired below uniformly.

pub mod archive;
pub mod audio;
pub mod convert;
pub mod doc;
pub mod hex;
pub mod image;
pub mod model3d;
pub mod pdf;
pub mod placeholder;
pub mod sheet;
pub mod table;
pub mod text;
pub mod video;

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
    Audio(audio::Opened),
    Video(video::Opened),
    Sheet(sheet::Opened),
    Archive(archive::Opened),
    Hex(hex::Opened),
    Doc(doc::Opened),
    Model3d(model3d::Opened),
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
    Audio(audio::State),
    Video(video::State),
    Sheet(sheet::State),
    Archive(archive::State),
    Hex(hex::State),
    Doc(doc::State),
    Model3d(model3d::State),
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
    Audio(audio::Msg),
    Video(video::Msg),
    Sheet(sheet::Msg),
    Archive(archive::Msg),
    Hex(hex::Msg),
    Doc(doc::Msg),
    Model3d(model3d::Msg),
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
        // specialised viewers claim their formats first; order matters where
        // formats overlap (doc's Markdown preview beats the plain text editor)
        macro_rules! claim {
            ($m:ident, $v:ident) => {
                if !is_pdf(&path) && $m::supported(&path) {
                    return $m::open(path).await.map(Loaded::$v);
                }
            };
        }
        claim!(sheet, Sheet);
        claim!(doc, Doc);
        claim!(audio, Audio);
        claim!(video, Video);
        claim!(archive, Archive);
        claim!(model3d, Model3d);
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
                match fsops::read_text(path.clone(), TEXT_LIMIT).await {
                    Ok(s) => Ok(Loaded::Text(s)),
                    // not UTF-8 → bytes
                    Err(e) if e == "not UTF-8 text" => hex_or_placeholder(path, Kind::Binary).await,
                    Err(e) => Err(e),
                }
            }
            Kind::Binary => hex_or_placeholder(path, Kind::Binary).await,
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
            Loaded::Audio(o) => {
                let (st, t) = audio::State::new(o);
                return (Viewer::Audio(st), t.map(Msg::Audio));
            }
            Loaded::Video(o) => {
                let (st, t) = video::State::new(o);
                return (Viewer::Video(st), t.map(Msg::Video));
            }
            Loaded::Sheet(o) => {
                let (st, t) = sheet::State::new(o);
                return (Viewer::Sheet(st), t.map(Msg::Sheet));
            }
            Loaded::Archive(o) => {
                let (st, t) = archive::State::new(o);
                return (Viewer::Archive(st), t.map(Msg::Archive));
            }
            Loaded::Hex(o) => {
                let (st, t) = hex::State::new(o);
                return (Viewer::Hex(st), t.map(Msg::Hex));
            }
            Loaded::Doc(o) => {
                let (st, t) = doc::State::new(o);
                return (Viewer::Doc(st), t.map(Msg::Doc));
            }
            Loaded::Model3d(o) => {
                let (st, t) = model3d::State::new(o);
                return (Viewer::Model3d(st), t.map(Msg::Model3d));
            }
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
            Viewer::Audio(s) => s.is_dirty(),
            Viewer::Video(s) => s.is_dirty(),
            Viewer::Sheet(s) => s.is_dirty(),
            Viewer::Archive(s) => s.is_dirty(),
            Viewer::Hex(s) => s.is_dirty(),
            Viewer::Doc(s) => s.is_dirty(),
            Viewer::Model3d(s) => s.is_dirty(),
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
            Viewer::Audio(s) => s.is_editing(),
            Viewer::Video(s) => s.is_editing(),
            Viewer::Sheet(s) => s.is_editing(),
            Viewer::Archive(s) => s.is_editing(),
            Viewer::Hex(s) => s.is_editing(),
            Viewer::Doc(s) => s.is_editing(),
            Viewer::Model3d(s) => s.is_editing(),
            _ => None,
        }
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        match self {
            Viewer::Text(s) => s.toggle_edit().map(Msg::Text),
            Viewer::Pdf(s) => s.toggle_edit().map(Msg::Pdf),
            Viewer::Audio(s) => s.toggle_edit().map(Msg::Audio),
            Viewer::Video(s) => s.toggle_edit().map(Msg::Video),
            Viewer::Sheet(s) => s.toggle_edit().map(Msg::Sheet),
            Viewer::Archive(s) => s.toggle_edit().map(Msg::Archive),
            Viewer::Hex(s) => s.toggle_edit().map(Msg::Hex),
            Viewer::Doc(s) => s.toggle_edit().map(Msg::Doc),
            Viewer::Model3d(s) => s.toggle_edit().map(Msg::Model3d),
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
            (Viewer::Audio(s), Msg::Audio(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Audio), ev)
            }
            (Viewer::Video(s), Msg::Video(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Video), ev)
            }
            (Viewer::Sheet(s), Msg::Sheet(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Sheet), ev)
            }
            (Viewer::Archive(s), Msg::Archive(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Archive), ev)
            }
            (Viewer::Hex(s), Msg::Hex(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Hex), ev)
            }
            (Viewer::Doc(s), Msg::Doc(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Doc), ev)
            }
            (Viewer::Model3d(s), Msg::Model3d(m)) => {
                let (t, ev) = s.update(m);
                (t.map(Msg::Model3d), ev)
            }

            _ => (Task::none(), None),
        }
    }

    /// Keyboard input while the viewer pane has focus. None = not consumed
    /// (the app falls back to its own shortcuts).
    pub fn key(
        &mut self,
        key: &iced::keyboard::Key,
        mods: iced::keyboard::Modifiers,
    ) -> Option<Task<Msg>> {
        let _ = (key, mods);
        match self {
            _ => None,
        }
    }

    /// The viewer wants the window fullscreen (video).
    pub fn fullscreen(&self) -> bool {
        match self {
            _ => false,
        }
    }

    /// The app left/entered fullscreen on its own (e.g. Escape).
    pub fn set_fullscreen(&mut self, on: bool) {
        let _ = on;
    }

    /// Ctrl+S from the app. Viewers that can't save ignore it.
    pub fn save(&mut self) -> Task<Msg> {
        match self {
            Viewer::Text(s) => s.save().map(Msg::Text),
            Viewer::Pdf(s) => s.save().map(Msg::Pdf),
            Viewer::Audio(s) => s.save().map(Msg::Audio),
            Viewer::Video(s) => s.save().map(Msg::Video),
            Viewer::Sheet(s) => s.save().map(Msg::Sheet),
            Viewer::Archive(s) => s.save().map(Msg::Archive),
            Viewer::Hex(s) => s.save().map(Msg::Hex),
            Viewer::Doc(s) => s.save().map(Msg::Doc),
            Viewer::Model3d(s) => s.save().map(Msg::Model3d),
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
            Viewer::Audio(s) => s.view().map(Msg::Audio),
            Viewer::Video(s) => s.view().map(Msg::Video),
            Viewer::Sheet(s) => s.view().map(Msg::Sheet),
            Viewer::Archive(s) => s.view().map(Msg::Archive),
            Viewer::Hex(s) => s.view().map(Msg::Hex),
            Viewer::Doc(s) => s.view().map(Msg::Doc),
            Viewer::Model3d(s) => s.view().map(Msg::Model3d),
        }
    }
}

fn is_pdf(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Any file can be shown as bytes; fall back to the placeholder if the hex
/// viewer can't open it.
async fn hex_or_placeholder(path: PathBuf, kind: Kind) -> Result<Loaded, String> {
    if hex::supported(&path) {
        if let Ok(o) = hex::open(path).await {
            return Ok(Loaded::Hex(o));
        }
    }
    Ok(Loaded::Placeholder(kind))
}
