//! Icon glyphs (workspace app icons, launcher/power/network/bluetooth/
//! pulseaudio icons) are nerd-font private-use-area codepoints. iced has no
//! GTK-style fontconfig fallback — a codepoint outside the loaded font's
//! cmap just tofu's ("?" boxes), which is exactly what happened before this
//! existed. Every icon-bearing Text widget must explicitly opt into this
//! font; nothing else (workspace numbers, clock, CPU%, etc) should, since
//! Symbols Nerd Font has zero basic-Latin coverage — not even a space.

pub const FONT_NAME: &str = "Symbols Nerd Font";

pub fn nerd_font() -> iced::Font {
    iced::Font::with_name(FONT_NAME)
}

/// User-configurable launcher icons can be either plain text ("./") or a
/// nerd-font glyph ("") — this picks the right font per string instead of
/// forcing one or the other. Nerd-font glyphs live well above ASCII, so
/// "any non-ASCII char present" is a reliable enough signal without needing
/// to check the font's actual cmap at runtime.
pub fn font_for(s: &str) -> iced::Font {
    if s.chars().any(|c| c as u32 > 0x7f) {
        nerd_font()
    } else {
        iced::Font::default()
    }
}

/// Resolved via `fc-match` rather than a hardcoded path so a font package
/// update or a different machine doesn't silently break icon rendering.
pub fn load_font_bytes() -> Option<Vec<u8>> {
    let output = std::process::Command::new("fc-match")
        .args([FONT_NAME, "-f", "%{file}"])
        .output()
        .ok()?;
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    std::fs::read(path).ok()
}
