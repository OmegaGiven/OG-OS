//! Reads just the `notif_fx_*` keys out of og-settings' shared
//! config file. Deliberately not the full `Config` struct from the main
//! binary (that would need a workspace lib split just for five fields) —
//! any other key in the file is simply ignored here.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Rain,
    Glow,
    Wind,
    Sparkle,
}

pub struct FxConfig {
    pub enabled: bool,
    pub effect: EffectKind,
    pub color: (u8, u8, u8),
    pub duration_ms: u32,
}

pub fn load() -> Option<FxConfig> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{home}/.config/sway-power/config.json");
    let content = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;

    let color_hex = v.get("notif_fx_color").and_then(|x| x.as_str()).unwrap_or("#ff7800");
    let effect = match v.get("notif_fx_effect").and_then(|x| x.as_str()) {
        Some("glow") => EffectKind::Glow,
        Some("wind") => EffectKind::Wind,
        Some("sparkle") => EffectKind::Sparkle,
        _ => EffectKind::Rain,
    };

    Some(FxConfig {
        enabled: v.get("notif_fx_enabled").and_then(|x| x.as_bool()).unwrap_or(false),
        effect,
        color: parse_hex(color_hex).unwrap_or((255, 120, 0)),
        duration_ms: v.get("notif_fx_duration_ms").and_then(|x| x.as_u64()).unwrap_or(1600) as u32,
    })
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}
