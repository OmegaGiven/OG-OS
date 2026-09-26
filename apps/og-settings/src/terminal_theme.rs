//! Regenerates the configured terminal's own color scheme from the app's
//! theme colors, for terminals with a plain declarative config file this
//! can be done safely and unattended:
//!
//! - Alacritty, foot, kitty — via a small companion file this module owns
//!   entirely, wired in with an `import`/`include` line added to the
//!   user's main config if it isn't already there.
//! - xfce4-terminal, terminator — patched in place (their config is a flat
//!   INI-ish format, safe to upsert known keys without touching anything
//!   else).
//! - Konsole — dropped as a new `.colorscheme` file; Konsole will just
//!   list it as a selectable scheme, since there's no single reliable way
//!   to find "the active profile" to switch automatically. One manual
//!   pick in Profile Settings the first time, matches every color-scheme
//!   sharing/theming tool for Konsole.
//!
//! Deliberately NOT supported: wezterm (config is executable Lua — no
//! text-upsert is safe against arbitrary user logic), gnome-terminal/tilix
//! (profiles live in dconf, not a file), urxvt/xterm (Xresources — a
//! different mechanism entirely), st (colors are compiled in, no config
//! file exists at all). Any terminal not recognized here is silently a
//! no-op — Apply & Save still succeeds, this is just extra.

use iced::Color;
use crate::config::Config;

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1.0 }
}

fn lighten(c: Color, t: f32) -> Color {
    mix(c, Color::WHITE, t)
}

fn to_hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}",
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8,
    )
}

fn to_hex_no_hash(c: Color) -> String {
    format!("{:02x}{:02x}{:02x}",
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8,
    )
}

fn to_rgb_triplet(c: Color) -> String {
    format!("{},{},{}", (c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8)
}

pub struct Palette {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    /// [black, red, green, yellow, blue, magenta, cyan, white]
    pub normal: [Color; 8],
    pub bright: [Color; 8],
}

/// Only six colors actually exist in the app's theme — everything else
/// here is procedurally derived from them so the terminal reads as part
/// of the same theme without needing a whole 16-slot color picker.
fn generate_palette(config: &Config) -> Palette {
    use crate::tabs::theme::hex_to_color;

    let bar_bg = hex_to_color(&config.bar_bg);
    let bar_text = hex_to_color(&config.bar_text);
    let accent = hex_to_color(&config.accent);
    let inactive = hex_to_color(&config.inactive_color);
    let urgent = hex_to_color(&config.urgent_color);
    let sec_bg = hex_to_color(&config.sec_bg);

    let green_base = Color::from_rgb8(0x4c, 0xaf, 0x50);
    let yellow_base = Color::from_rgb8(0xff, 0xc1, 0x07);
    let blue_base = Color::from_rgb8(0x21, 0x96, 0xf3);
    let magenta_base = Color::from_rgb8(0xe9, 0x1e, 0x63);
    let cyan_base = Color::from_rgb8(0x00, 0xbc, 0xd4);

    let black = inactive;
    let red = urgent;
    let green = mix(green_base, accent, 0.25);
    let yellow = mix(yellow_base, accent, 0.25);
    let blue = mix(blue_base, accent, 0.25);
    let magenta = mix(magenta_base, accent, 0.35);
    let cyan = mix(cyan_base, accent, 0.25);
    let white = bar_text;

    let normal = [black, red, green, yellow, blue, magenta, cyan, white];
    let bright = [
        lighten(black, 0.35),
        lighten(red, 0.25),
        lighten(green, 0.25),
        lighten(yellow, 0.25),
        lighten(blue, 0.25),
        lighten(magenta, 0.25),
        lighten(cyan, 0.25),
        lighten(white, 0.5),
    ];

    Palette {
        background: bar_bg,
        foreground: bar_text,
        cursor: accent,
        selection_bg: sec_bg,
        selection_fg: bar_text,
        normal,
        bright,
    }
}

/// Replaces the first line in `content` matching `section_header` exactly
/// (trimmed) as the start of a block, then finds/replaces `key`'s value
/// within that block (up to the next line starting with `[`), or inserts
/// it if missing. Creates the section at the end of the file if it
/// doesn't exist at all. Works for flat INI (`[section]`) and terminator's
/// bracket-depth sections (pass the literal `[[name]]` as the header) —
/// anything where "next line starting with `[`" ends the block.
fn upsert_ini_key(content: &str, section_header: &str, key: &str, value_line: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let section_pos = lines.iter().position(|l| l.trim() == section_header);

    match section_pos {
        Some(start) => {
            let mut end = lines.len();
            for (i, line) in lines.iter().enumerate().skip(start + 1) {
                if line.trim_start().starts_with('[') {
                    end = i;
                    break;
                }
            }
            let key_pos = lines[start + 1..end].iter().position(|l| {
                let t = l.trim_start();
                t.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='))
            }).map(|i| i + start + 1);

            match key_pos {
                Some(p) => lines[p] = value_line.to_string(),
                None => lines.insert(end, value_line.to_string()),
            }
        }
        None => {
            lines.push(section_header.to_string());
            lines.push(value_line.to_string());
        }
    }
    lines.join("\n") + "\n"
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn write(path: &str, content: &str) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, content);
}

// ── Alacritty ────────────────────────────────────────────────────────────

fn apply_alacritty(p: &Palette) {
    let dir = format!("{}/.config/alacritty", home());
    let colors_path = format!("{dir}/og-settings-colors.toml");
    let main_path = format!("{dir}/alacritty.toml");

    let toml = format!(
r#"# Auto-generated by og-settings — do not edit
[colors.primary]
background = "{}"
foreground = "{}"

[colors.cursor]
cursor = "{}"

[colors.selection]
background = "{}"
text = "{}"

[colors.normal]
black = "{}"
red = "{}"
green = "{}"
yellow = "{}"
blue = "{}"
magenta = "{}"
cyan = "{}"
white = "{}"

[colors.bright]
black = "{}"
red = "{}"
green = "{}"
yellow = "{}"
blue = "{}"
magenta = "{}"
cyan = "{}"
white = "{}"
"#,
        to_hex(p.background), to_hex(p.foreground),
        to_hex(p.cursor),
        to_hex(p.selection_bg), to_hex(p.selection_fg),
        to_hex(p.normal[0]), to_hex(p.normal[1]), to_hex(p.normal[2]), to_hex(p.normal[3]),
        to_hex(p.normal[4]), to_hex(p.normal[5]), to_hex(p.normal[6]), to_hex(p.normal[7]),
        to_hex(p.bright[0]), to_hex(p.bright[1]), to_hex(p.bright[2]), to_hex(p.bright[3]),
        to_hex(p.bright[4]), to_hex(p.bright[5]), to_hex(p.bright[6]), to_hex(p.bright[7]),
    );
    write(&colors_path, &toml);

    let main = read(&main_path);
    if main.contains("og-settings-colors.toml") {
        return;
    }
    let updated = if main.is_empty() {
        format!("[general]\nimport = [\"{colors_path}\"]\n")
    } else if let Some(pos) = main.find("import = [") {
        let insert_at = pos + "import = [".len();
        format!("{}\"{colors_path}\", {}", &main[..insert_at], &main[insert_at..])
    } else {
        format!("[general]\nimport = [\"{colors_path}\"]\n\n{main}")
    };
    write(&main_path, &updated);
}

// ── foot ─────────────────────────────────────────────────────────────────

fn apply_foot(p: &Palette) {
    let path = format!("{}/.config/foot/foot.ini", home());
    let mut content = read(&path);
    if content.is_empty() {
        content = "[main]\n\n[colors]\n".to_string();
    }
    let set = |c: &mut String, key: &str, color: Color| {
        *c = upsert_ini_key(c, "[colors]", key, &format!("{key}={}", to_hex_no_hash(color)));
    };
    set(&mut content, "background", p.background);
    set(&mut content, "foreground", p.foreground);
    set(&mut content, "selection-background", p.selection_bg);
    set(&mut content, "selection-foreground", p.selection_fg);
    let names = ["regular0", "regular1", "regular2", "regular3", "regular4", "regular5", "regular6", "regular7"];
    for (i, name) in names.iter().enumerate() {
        set(&mut content, name, p.normal[i]);
    }
    let bright_names = ["bright0", "bright1", "bright2", "bright3", "bright4", "bright5", "bright6", "bright7"];
    for (i, name) in bright_names.iter().enumerate() {
        set(&mut content, name, p.bright[i]);
    }
    write(&path, &content);
}

// ── kitty ────────────────────────────────────────────────────────────────

fn apply_kitty(p: &Palette) {
    let dir = format!("{}/.config/kitty", home());
    let colors_path = format!("{dir}/og-settings-colors.conf");
    let main_path = format!("{dir}/kitty.conf");

    let mut conf = String::from("# Auto-generated by og-settings — do not edit\n");
    conf += &format!("background {}\n", to_hex(p.background));
    conf += &format!("foreground {}\n", to_hex(p.foreground));
    conf += &format!("cursor {}\n", to_hex(p.cursor));
    conf += &format!("selection_background {}\n", to_hex(p.selection_bg));
    conf += &format!("selection_foreground {}\n", to_hex(p.selection_fg));
    for (i, c) in p.normal.iter().enumerate() {
        conf += &format!("color{} {}\n", i, to_hex(*c));
    }
    for (i, c) in p.bright.iter().enumerate() {
        conf += &format!("color{} {}\n", i + 8, to_hex(*c));
    }
    write(&colors_path, &conf);

    let main = read(&main_path);
    if main.contains("og-settings-colors.conf") {
        return;
    }
    let updated = format!("include og-settings-colors.conf\n\n{main}");
    write(&main_path, &updated);
}

// ── xfce4-terminal ───────────────────────────────────────────────────────

fn apply_xfce4_terminal(p: &Palette) {
    let path = format!("{}/.config/xfce4/terminal/terminalrc", home());
    let mut content = read(&path);
    if content.is_empty() {
        content = "[Configuration]\n".to_string();
    }
    let palette = {
        let mut all = p.normal.to_vec();
        all.extend_from_slice(&p.bright);
        all.iter().map(|c| to_hex(*c)).collect::<Vec<_>>().join(";")
    };
    let set = |c: &mut String, key: &str, value: &str| {
        *c = upsert_ini_key(c, "[Configuration]", key, &format!("{key}={value}"));
    };
    set(&mut content, "ColorForeground", &to_hex(p.foreground));
    set(&mut content, "ColorBackground", &to_hex(p.background));
    set(&mut content, "ColorCursor", &to_hex(p.cursor));
    set(&mut content, "ColorCursorForeground", &to_hex(p.background));
    set(&mut content, "ColorSelectionBackground", &to_hex(p.selection_bg));
    set(&mut content, "ColorPalette", &palette);
    write(&path, &content);
}

// ── terminator ───────────────────────────────────────────────────────────

fn apply_terminator(p: &Palette) {
    let path = format!("{}/.config/terminator/config", home());
    let mut content = read(&path);
    if content.is_empty() {
        content = "[profiles]\n  [[default]]\n".to_string();
    }
    let palette = {
        let mut all = p.normal.to_vec();
        all.extend_from_slice(&p.bright);
        all.iter().map(|c| to_hex(*c)).collect::<Vec<_>>().join(":")
    };
    let set = |c: &mut String, key: &str, value: &str| {
        *c = upsert_ini_key(c, "[[default]]", key, &format!("    {key} = {value}"));
    };
    set(&mut content, "background_color", &to_hex(p.background));
    set(&mut content, "foreground_color", &to_hex(p.foreground));
    set(&mut content, "cursor_color", &to_hex(p.cursor));
    set(&mut content, "palette", &palette);
    set(&mut content, "use_theme_colors", "False");
    write(&path, &content);
}

// ── Konsole ──────────────────────────────────────────────────────────────

fn apply_konsole(p: &Palette) {
    let path = format!("{}/.local/share/konsole/OGSettings.colorscheme", home());
    let color_section = |name: &str, c: Color| {
        format!("[{name}]\nColor={}\n", to_rgb_triplet(c))
    };
    let intense = |i: usize| lighten(p.normal[i], 0.25);

    let mut s = String::from("# Auto-generated by og-settings\n[General]\nDescription=OG Settings\n\n");
    s += &color_section("Background", p.background);
    s += &color_section("Foreground", p.foreground);
    for (i, c) in p.normal.iter().enumerate() {
        s += &color_section(&format!("Color{i}"), *c);
    }
    for i in 0..8 {
        s += &color_section(&format!("Color{i}Intense"), intense(i));
    }
    write(&path, &s);
}

/// Kept in sync with the `match` in `apply_terminal_theme` — used to tell
/// the user in the UI which terminals actually get themed.
pub const SUPPORTED_TERMINALS: &[&str] = &["alacritty", "foot", "kitty", "xfce4-terminal", "terminator", "konsole"];

/// Regenerates the color scheme/theme file for whichever terminal is
/// configured, if it's one of the supported ones. Silently does nothing
/// for anything else — this is best-effort extra, never a hard requirement.
pub fn apply_terminal_theme(config: &Config) {
    if config.terminal.is_empty() {
        return;
    }
    let palette = generate_palette(config);
    let name = config.terminal.rsplit('/').next().unwrap_or(&config.terminal);

    match name {
        "alacritty" => apply_alacritty(&palette),
        "foot" => apply_foot(&palette),
        "kitty" => apply_kitty(&palette),
        "xfce4-terminal" => apply_xfce4_terminal(&palette),
        "terminator" => apply_terminator(&palette),
        "konsole" => apply_konsole(&palette),
        _ => {}
    }

    nudge_live_reload(name);
}

/// Best-effort "apply without restarting" for already-open terminal windows.
/// Alacritty already watches its own config file and reloads automatically —
/// nothing to do there. kitty reloads its config on SIGUSR1. The rest
/// (foot, xfce4-terminal, terminator, konsole) don't have a reliable
/// signal-based live-reload, so those just pick up the new colors next time
/// a window is opened.
fn nudge_live_reload(terminal_name: &str) {
    if terminal_name == "kitty" {
        let _ = std::process::Command::new("pkill")
            .args(["-SIGUSR1", "-x", "kitty"])
            .status();
    }
}
