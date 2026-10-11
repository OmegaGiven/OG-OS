//! Plain-text / code / markdown on iced's text_editor.
//!
//! Opens read-only. Edit mode is explicit (button or Ctrl+E); Save writes
//! over the original file (atomically), Discard reloads it from disk.
//! Code is syntax-highlighted by file extension (iced's syntect-backed
//! highlighter); plain text stays unhighlighted.

use super::Event;
use crate::filetype::Kind;
use crate::fsops;
use crate::theme;
use iced::highlighter;
use iced::widget::text::Wrapping;
use iced::widget::{button, column, row, text, text_editor, Space};
use iced::{Element, Font, Length, Task};
use std::path::{Path, PathBuf};

/// Docs that are really text. PDF/DOCX/EPUB go to the placeholder until the
/// doc backend exists.
pub fn supported(path: &Path, kind: Kind) -> bool {
    kind == Kind::Text
        || matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("md" | "markdown" | "txt")
        )
}

/// Dark theme for highlighted code: warm browns that sit well on the app's
/// near-black background.
const CODE_THEME: highlighter::Theme = highlighter::Theme::Base16Mocha;

/// syntect token for the highlighter (resolved against syntect's default
/// syntax set by extension/name), or None for plain text. Languages that set
/// lacks map to the closest one it has.
fn syntax_token(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?;
    match name {
        "Makefile" | "makefile" | "GNUmakefile" => return Some("make"),
        "PKGBUILD" | ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".xinitrc" => {
            return Some("sh")
        }
        "Cargo.lock" => return Some("properties"),
        _ => {}
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rs",
        "py" | "pyw" | "pyi" => "py",
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => "js",
        "json" | "jsonc" | "geojson" => "json",
        // no TOML/INI grammar in syntect's defaults: key = value + # comments
        "toml" | "ini" | "cfg" | "conf" | "desktop" | "properties" | "service" => "properties",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" | "fish" => "sh",
        "c" => "c",
        "h" | "hpp" | "hh" | "hxx" | "cpp" | "cc" | "cxx" | "c++" => "cpp",
        "cs" => "cs",
        "go" => "go",
        "java" => "java",
        "html" | "htm" | "xhtml" => "html",
        "css" | "scss" => "css",
        "xml" | "svg" | "xsd" | "xsl" | "xslt" => "xml",
        "md" | "markdown" => "md",
        "sql" => "sql",
        "lua" => "lua",
        "rb" => "rb",
        "php" => "php",
        "pl" | "pm" => "pl",
        "hs" => "hs",
        "ml" | "mli" => "ml",
        "scala" => "scala",
        "tex" | "sty" => "tex",
        "diff" | "patch" => "diff",
        "mk" | "mak" => "make",
        "bat" | "cmd" => "bat",
        "r" => "r",
        "clj" => "clj",
        "erl" => "erl",
        "groovy" | "gradle" => "groovy",
        "d" => "d",
        "m" => "m",
        "lisp" | "el" | "scm" => "lisp",
        "dot" | "gv" => "dot",
        "tcl" => "tcl",
        "rst" => "rst",
        _ => return None,
    })
}

pub struct State {
    path: PathBuf,
    content: text_editor::Content,
    pub editing: bool,
    pub dirty: bool,
    saving: bool,
    syntax: Option<&'static str>,
    wrap: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Edit(text_editor::Action),
    ToggleEdit,
    Save,
    Saved(Result<(), String>),
    Discard,
    Reloaded(Result<String, String>),
    ToggleWrap,
}

impl State {
    pub fn new(path: PathBuf, s: &str) -> Self {
        Self {
            syntax: syntax_token(&path),
            path,
            content: text_editor::Content::with_text(s),
            editing: false,
            dirty: false,
            saving: false,
            wrap: true,
        }
    }

    pub fn save(&mut self) -> Task<Msg> {
        if self.saving || !self.dirty {
            return Task::none();
        }
        self.saving = true;
        Task::perform(
            fsops::write_text(self.path.clone(), self.content.text()),
            Msg::Saved,
        )
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        let (t, _) = self.update(Msg::ToggleEdit);
        t
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Edit(a) => {
                // read-only: selection and cursor moves only, no edits
                if self.editing || !a.is_edit() {
                    self.dirty |= a.is_edit();
                    self.content.perform(a);
                }
                (Task::none(), None)
            }
            Msg::ToggleEdit => {
                if self.editing && self.dirty {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Unsaved changes: Save or Discard first".into(),
                        )),
                    );
                }
                self.editing = !self.editing;
                (Task::none(), None)
            }
            Msg::ToggleWrap => {
                self.wrap = !self.wrap;
                (Task::none(), None)
            }
            Msg::Save => (self.save(), None),
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok(()) => {
                        self.dirty = false;
                        (
                            Task::none(),
                            Some(Event::Status(format!("Saved over {}", self.path.display()))),
                        )
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!("Save failed: {e}"))),
                    ),
                }
            }
            Msg::Discard => (
                Task::perform(
                    fsops::read_text(self.path.clone(), super::TEXT_LIMIT),
                    Msg::Reloaded,
                ),
                None,
            ),
            Msg::Reloaded(r) => match r {
                Ok(s) => {
                    self.content = text_editor::Content::with_text(&s);
                    self.dirty = false;
                    self.editing = false;
                    (
                        Task::none(),
                        Some(Event::Status("Changes discarded".into())),
                    )
                }
                Err(e) => (
                    Task::none(),
                    Some(Event::Status(format!("Reload failed: {e}"))),
                ),
            },
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let (line, col) = self.content.cursor_position();
        let status = row![
            text(format!("Ln {}, Col {}", line + 1, col + 1))
                .size(12)
                .color(theme::MUTED),
            button(text("Wrap").size(12))
                .on_press(Msg::ToggleWrap)
                .style(if self.wrap {
                    button::primary
                } else {
                    button::secondary
                }),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let bar: Element<'_, Msg> = if self.editing {
            row![
                text(if self.dirty {
                    "EDITING  * modified"
                } else {
                    "EDITING"
                })
                .size(12)
                .color(iced::Color::from_rgb8(0xff, 0xc8, 0x6b)),
                Space::with_width(Length::Fill),
                status,
                button(text("Discard").size(12))
                    .on_press_maybe(self.dirty.then_some(Msg::Discard))
                    .style(button::secondary),
                button(text("Save  Ctrl+S").size(12))
                    .on_press_maybe(self.dirty.then_some(Msg::Save)),
                button(text("Done").size(12))
                    .on_press_maybe((!self.dirty).then_some(Msg::ToggleEdit))
                    .style(button::secondary),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![
                text("Read-only").size(12).color(theme::MUTED),
                Space::with_width(Length::Fill),
                status,
                button(text("Edit  Ctrl+E").size(12)).on_press(Msg::ToggleEdit),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        };
        // always interactive so read-only text can still be selected and
        // copied; `update` drops edit actions unless in edit mode
        let editor = text_editor(&self.content)
            .font(Font::MONOSPACE)
            .height(Length::Fill)
            .wrapping(if self.wrap {
                Wrapping::WordOrGlyph
            } else {
                Wrapping::None
            })
            .on_action(Msg::Edit);
        let editor: Element<'_, Msg> = match self.syntax {
            Some(token) => editor.highlight(token, CODE_THEME).into(),
            None => editor.into(),
        };
        column![bar, editor].spacing(6).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_maps_to_highlighter_language() {
        let tok = |p: &str| syntax_token(Path::new(p));
        assert_eq!(tok("main.rs"), Some("rs"));
        assert_eq!(tok("a/b/script.PY"), Some("py"));
        assert_eq!(tok("app.tsx"), Some("js"));
        assert_eq!(tok("Cargo.toml"), Some("properties"));
        assert_eq!(tok("x.yml"), Some("yaml"));
        assert_eq!(tok("foo.h"), Some("cpp"));
        assert_eq!(tok("Makefile"), Some("make"));
        assert_eq!(tok("PKGBUILD"), Some("sh"));
        assert_eq!(tok("notes.txt"), None);
        assert_eq!(tok("README"), None);
    }

    #[test]
    fn every_token_resolves_in_syntect_defaults() {
        // the same set iced_highlighter loads; an unknown token would
        // silently fall back to plain text
        let set = syntect::parsing::SyntaxSet::load_defaults_nonewlines();
        let exts = [
            "rs", "py", "js", "json", "toml", "yaml", "sh", "c", "cpp", "cs", "go", "java", "html",
            "css", "xml", "md", "sql", "lua", "rb", "php", "pl", "hs", "ml", "scala", "tex",
            "diff", "mk", "bat", "r", "clj", "erl", "groovy", "d", "m", "lisp", "dot", "tcl",
            "rst",
        ];
        for e in exts {
            let t = syntax_token(Path::new(&format!("f.{e}"))).unwrap();
            assert!(set.find_syntax_by_token(t).is_some(), "{e} -> {t}");
        }
    }
}
