//! Notification text comes from arbitrary third-party apps (Discord,
//! Signal, browser push, ...) and can contain pretty much any script or
//! symbol — iced has no automatic system-fontconfig glyph fallback (a
//! codepoint outside the *chosen* font's cmap just tofu's, regardless of
//! what else is loaded; see og-bar's `icon_font.rs` for the same note),
//! so leaving text on whatever narrow font iced falls back to by default
//! is what was producing boxes on some notifications. Resolving the
//! system's actual configured sans-serif via `fc-match` (same mechanism
//! `icon_font.rs` uses for the nerd-font icon glyphs, just pointed at a
//! generic family instead of a specific one) gets much broader coverage —
//! still not literally every script/emoji, but every ordinary Latin/
//! Cyrillic/Greek/symbol case that was almost certainly the actual gap.
pub fn load_default_font() -> Option<(&'static str, Vec<u8>)> {
    let output = std::process::Command::new("fc-match")
        .args(["sans-serif", "-f", "%{family[0]}\n%{file}"])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let family = lines.next()?.trim();
    let path = lines.next()?.trim();
    if family.is_empty() || path.is_empty() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    // Leaked once at startup, for the process's whole lifetime — the only
    // way to get the `&'static str` `Font::with_name` requires out of a
    // name resolved at runtime, and there's exactly one of these per run.
    let family: &'static str = Box::leak(family.to_string().into_boxed_str());
    Some((family, bytes))
}
