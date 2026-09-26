//! A "theme" is a directory: `base.png` (the filtered artwork) plus
//! `manifest.json` (this file's `Manifest` — which regions of that image
//! are animated, and how). Both og-wallpaper (the renderer) and
//! og-wallpaper-studio (the region-marking GUI that produces these) share
//! this so the manifest format only lives in one place.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Fractions of the base image's width/height (0.0..1.0), not raw
/// pixels — so a theme authored against one image resolution still maps
/// sanely onto whatever size the base image actually ends up, and onto
/// whatever the actual output resolution is at render time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RegionBounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Effect {
    /// Falling line/particle overlay within the region.
    Rain { density: u32, speed: f32 },
    /// The whole region's overlay opacity pulses — for neon signs,
    /// screens, anything that reads as "lit up".
    Flash { rate: f32 },
    /// Several small points within the region twinkle on independent
    /// phases — string lights, stars, distant windows.
    Twinkle { count: u32, rate: f32 },
    /// Horizontal sine-wave strip displacement — cloth, flags, reeds.
    Wave { amplitude: f32, speed: f32 },
}

/// Not everything worth animating is a box — a banner in a photo has an
/// actual irregular outline, and a scattered thing like "the starry sky"
/// isn't a single contiguous shape you'd want to trace by hand at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "shape")]
pub enum RegionShape {
    Rect(RegionBounds),
    /// Points in image-fraction space, in order, implicitly closed
    /// (last point connects back to the first) — traced by hand in
    /// og-wallpaper-studio's lasso tool. Exact, unlike Rect, for
    /// something like a flag's actual silhouette.
    ///
    /// A struct variant, not `Polygon(Vec<(f32,f32)>)` — serde's
    /// internally-tagged representation (`#[serde(tag = "shape")]`)
    /// can't serialize a newtype variant wrapping a sequence (there's
    /// nowhere to put the tag alongside a bare JSON array), only ones
    /// wrapping map-shaped data. Found this the hard way: `Manifest::
    /// save()` panicked-via-Err on any theme with a traced polygon.
    Polygon { points: Vec<(f32, f32)> },
    /// A flood-fill selection (og-wallpaper-studio's magic-wand tool) —
    /// `bounds` is the mask's tight bounding box, `mask_file` a 1-bit-
    /// per-pixel PNG (relative to the theme dir) covering just that box,
    /// not the whole image. Good for a scattered/irregular area like a
    /// starry sky that Polygon would be tedious to trace and Rect can't
    /// represent at all. Only Rain/Twinkle (particle effects, naturally
    /// bounded by rejection-sampling against the mask) support Mask
    /// shapes right now — Flash/Wave fall back to the bounding box,
    /// since neither has a cheap way to fill/follow an arbitrary mask
    /// outline the way Polygon's Path-based rendering does.
    Mask { bounds: RegionBounds, mask_file: String },
}

impl RegionShape {
    /// The shape's own bounding box, in image-fraction space — every
    /// shape has one, used for Flash/Wave's rectangle fallback and for
    /// mapping a Polygon/Mask onto the base image's on-canvas rect the
    /// same way Rect already does.
    pub fn bounds(&self) -> RegionBounds {
        match self {
            RegionShape::Rect(b) => *b,
            RegionShape::Mask { bounds, .. } => *bounds,
            RegionShape::Polygon { points } => {
                let (mut x0, mut y0, mut x1, mut y1) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
                for &(x, y) in points {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
                RegionBounds { x: x0, y: y0, w: (x1 - x0).max(0.0), h: (y1 - y0).max(0.0) }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub shape: RegionShape,
    pub effect: Effect,
    /// Hex color override (e.g. "#ff8800"); `None` means "use the
    /// system's current accent color", same as everything else in
    /// OG-toolkit — a theme doesn't have to hardcode a palette that'll
    /// clash if the user's system theme changes later.
    pub color_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Manifest {
    pub regions: Vec<Region>,
    /// The two hex colors `base.png` was actually rendered in at filter
    /// time (line/foreground, background) — `None` for the "Raw" filter
    /// mode, which keeps the source photo's own colors and has nothing
    /// to remap. og-wallpaper's `line_art`/`two_color` filters both
    /// produce exactly two flat colors with no blending, so at theme-load
    /// time it can exact-match these against the *current* theme's
    /// accent/background and remap the whole image pixel-for-pixel —
    /// otherwise a saved theme's artwork stays frozen at whatever colors
    /// were live in og-settings the moment it was filtered, silently
    /// drifting out of sync with the system theme from then on.
    // `#[serde(default)]` matters here, not just style — without it,
    // themes saved before this field existed (no `base_colors` key in
    // their manifest.json at all) fail to deserialize entirely instead
    // of just skipping the remap. serde does NOT treat a missing
    // `Option<T>` field as `None` automatically.
    #[serde(default)]
    pub base_colors: Option<(String, String)>,
}

pub fn themes_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/share/og-wallpaper/themes")
}

pub fn theme_dir(name: &str) -> PathBuf {
    themes_dir().join(name)
}

pub fn list_themes() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(themes_dir()) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir() && e.path().join("manifest.json").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

impl Manifest {
    pub fn load(theme_name: &str) -> Option<Self> {
        let path = theme_dir(theme_name).join("manifest.json");
        let content = std::fs::read_to_string(&path)
            .inspect_err(|e| eprintln!("og-wallpaper-core: failed to read {}: {e}", path.display()))
            .ok()?;
        // Callers (both og-wallpaper and og-wallpaper-studio) previously
        // did `.unwrap_or_default()` on this — a parse failure here used
        // to silently come back as "0 regions" with zero indication
        // anything was wrong (this is exactly how a schema change once
        // silently dropped an existing theme's regions). Logging here
        // means that can't happen invisibly again, regardless of what
        // any given caller does with the `None`.
        serde_json::from_str(&content)
            .inspect_err(|e| eprintln!("og-wallpaper-core: failed to parse {}: {e}", path.display()))
            .ok()
    }

    pub fn save(&self, theme_name: &str) -> Result<(), String> {
        let dir = theme_dir(theme_name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let content = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("manifest.json"), content).map_err(|e| e.to_string())
    }
}
