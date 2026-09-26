//! Identical to og-hotkeys'/og-search's own `fonts.rs` — resolves the
//! system's configured sans-serif via `fc-match` so arbitrary transcript/
//! answer text (Claude's output can contain any script) doesn't tofu on
//! iced's narrow built-in font.
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
    let family: &'static str = Box::leak(family.to_string().into_boxed_str());
    Some((family, bytes))
}
