//! Builds `OG_Custom`, an Xcursor theme that inherits every shape from
//! `OG_Red` (the cursor we like — see PLAN.md/user request) except the ones
//! the user has explicitly overridden with their own SVG/PNG. A logical
//! "role" in [`CURSOR_ROLES`] maps to several real Xcursor shape names at
//! once (mirroring how OG_Red itself aliases e.g. `arrow`/`left_ptr` to the
//! same file), so picking one image covers every app's naming convention
//! for that role.

use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

pub const CUSTOM_THEME_NAME: &str = "OG_Custom";
const BASE_THEME_NAME: &str = "OG_Red";

/// Nominal pixel sizes baked into every generated cursor file — wide enough
/// to cover XCURSOR_SIZE settings from small laptop panels to large 4K/HiDPI
/// pointers without the compositor upscaling a single low-res frame.
const SIZES: &[u32] = &[16, 24, 32, 48, 64, 96, 128, 150, 192];

pub struct CursorRole {
    pub key: &'static str,
    pub label: &'static str,
    pub shapes: &'static [&'static str],
}

pub const CURSOR_ROLES: &[CursorRole] = &[
    CursorRole { key: "default", label: "Default / Arrow", shapes: &["default", "arrow", "left_ptr", "top_left_arrow"] },
    CursorRole { key: "pointer", label: "Link / Pointer", shapes: &["pointer", "hand", "hand1", "hand2", "pointing_hand"] },
    CursorRole { key: "text", label: "Text / I-beam", shapes: &["text", "xterm", "ibeam"] },
    CursorRole { key: "wait", label: "Wait / Busy", shapes: &["wait", "watch"] },
    CursorRole { key: "progress", label: "Progress", shapes: &["progress", "left_ptr_watch"] },
    CursorRole { key: "crosshair", label: "Crosshair", shapes: &["crosshair", "cross"] },
    CursorRole { key: "move", label: "Move", shapes: &["move", "fleur", "size_all"] },
    CursorRole { key: "grab", label: "Grab / Drag", shapes: &["grab", "openhand", "grabbing", "closedhand"] },
    CursorRole { key: "not-allowed", label: "Not Allowed", shapes: &["not-allowed", "no-drop", "forbidden"] },
    CursorRole { key: "help", label: "Help", shapes: &["help", "question_arrow", "whats_this"] },
    CursorRole { key: "ns-resize", label: "Resize ↕", shapes: &["ns-resize", "sb_v_double_arrow", "row-resize"] },
    CursorRole { key: "ew-resize", label: "Resize ↔", shapes: &["ew-resize", "sb_h_double_arrow", "col-resize"] },
    CursorRole { key: "nesw-resize", label: "Resize ↗↙", shapes: &["nesw-resize"] },
    CursorRole { key: "nwse-resize", label: "Resize ↖↘", shapes: &["nwse-resize"] },
];

pub fn custom_theme_dir() -> PathBuf {
    PathBuf::from(home()).join(".local/share/icons").join(CUSTOM_THEME_NAME)
}

fn cursors_dir() -> PathBuf {
    custom_theme_dir().join("cursors")
}

/// Preview PNGs live alongside the real Xcursor binaries (which iced's
/// `image` widget can't decode directly) so the settings UI has something
/// to render as a thumbnail.
fn preview_path(shape: &str) -> PathBuf {
    custom_theme_dir().join("previews").join(format!("{shape}.png"))
}

pub fn preview_path_for_role(role_key: &str) -> Option<PathBuf> {
    let role = CURSOR_ROLES.iter().find(|r| r.key == role_key)?;
    let p = preview_path(role.shapes[0]);
    p.exists().then_some(p)
}

pub fn role_is_customized(role_key: &str) -> bool {
    let Some(role) = CURSOR_ROLES.iter().find(|r| r.key == role_key) else { return false };
    cursors_dir().join(role.shapes[0]).exists()
}

fn ensure_custom_theme() -> Result<(), String> {
    let dir = custom_theme_dir();
    std::fs::create_dir_all(dir.join("cursors")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.join("previews")).map_err(|e| e.to_string())?;
    let index = dir.join("index.theme");
    if !index.exists() {
        std::fs::write(
            &index,
            format!(
                "[Icon Theme]\nName={CUSTOM_THEME_NAME}\nComment=Your custom cursor icons, everything else falls back to {BASE_THEME_NAME}\nInherits={BASE_THEME_NAME}\n"
            ),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn rasterize(src: &Path, out_png: &Path, size: u32) -> Result<(), String> {
    let is_svg = src.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("svg")).unwrap_or(false);
    let status = if is_svg {
        Command::new("rsvg-convert")
            .args(["-w", &size.to_string(), "-h", &size.to_string(), "-o"])
            .arg(out_png)
            .arg(src)
            .status()
    } else {
        Command::new("convert")
            .arg(src)
            .args(["-resize", &format!("{size}x{size}"), "-gravity", "center", "-background", "none", "-extent", &format!("{size}x{size}")])
            .arg(out_png)
            .status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(format!("failed to rasterize {} at {size}px", src.display())),
        Err(e) => Err(format!("{} not found ({e})", if is_svg { "rsvg-convert" } else { "convert" })),
    }
}

fn build_shape_file(shape: &str, src: &Path, tmp_dir: &Path) -> Result<(), String> {
    if Command::new("xcursorgen").arg("--version").output().is_err() {
        return Err("xcursorgen not found — install it with: sudo pacman -S xorg-xcursorgen".to_string());
    }

    let mut config_lines = String::new();
    for &size in SIZES {
        let png = tmp_dir.join(format!("{shape}_{size}.png"));
        rasterize(src, &png, size)?;
        let hot = (size / 8).max(1);
        config_lines.push_str(&format!("{size} {hot} {hot} {}\n", png.display()));
    }
    let config_path = tmp_dir.join(format!("{shape}.cfg"));
    std::fs::write(&config_path, config_lines).map_err(|e| e.to_string())?;

    let out_path = cursors_dir().join(shape);
    let status = Command::new("xcursorgen")
        .arg(&config_path)
        .arg(&out_path)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("xcursorgen failed for shape '{shape}'"));
    }

    // Largest rasterized frame doubles as the settings-UI thumbnail.
    let biggest = tmp_dir.join(format!("{shape}_{}.png", SIZES[SIZES.len() - 1]));
    let _ = std::fs::copy(&biggest, preview_path(shape));

    Ok(())
}

/// Renders `src` (SVG or PNG) into every shape name covered by `role_key`
/// and (re)writes OG_Custom's index.theme if this is the first customization.
pub fn set_cursor_role_image(role_key: &str, src: &Path) -> Result<(), String> {
    let role = CURSOR_ROLES.iter().find(|r| r.key == role_key)
        .ok_or_else(|| format!("unknown cursor role '{role_key}'"))?;

    ensure_custom_theme()?;

    let tmp_dir = std::env::temp_dir().join(format!("og-cursor-build-{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;

    let result = (|| {
        for &shape in role.shapes {
            build_shape_file(shape, src, &tmp_dir)?;
        }
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

/// Deletes this role's generated shape files so lookups fall back to
/// OG_Custom's `Inherits=OG_Red` again.
pub fn reset_cursor_role(role_key: &str) -> Result<(), String> {
    let role = CURSOR_ROLES.iter().find(|r| r.key == role_key)
        .ok_or_else(|| format!("unknown cursor role '{role_key}'"))?;
    for &shape in role.shapes {
        let _ = std::fs::remove_file(cursors_dir().join(shape));
    }
    let _ = std::fs::remove_file(preview_path(role.shapes[0]));
    Ok(())
}
