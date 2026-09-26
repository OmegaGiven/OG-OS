//! Shared config: the single `Config` struct definition and its plain
//! load/save, used by every OG-toolkit app. Only `og-settings` (the theme
//! editor) writes this file — everyone else just reads it.
//!
//! Deliberately does *not* include any sway/waybar-config-scraping "seed
//! from live system state" logic (cursor theme, wallpaper, module order,
//! etc.) — that's only ever needed by the one app that owns first-run
//! setup, and lives there instead of here.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn config_path() -> PathBuf {
    let mut p = dirs_home();
    p.push(".config/sway-power/config.json");
    p
}

pub fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SleepSetting {
    pub enabled: bool,
    pub minutes: u64,
}

impl Default for SleepSetting {
    fn default() -> Self {
        Self { enabled: false, minutes: 15 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MonitorConfig {
    pub name: String,
    pub resolution: String,
    pub x: i32,
    pub y: i32,
    pub enabled: bool,
}

/// An additional clock widget with its own timezone. `id` is a stable,
/// never-reused counter (not an array index) so removing one clock can't
/// shift another's `clock#N` module id out from under it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClockConfig {
    pub id: u32,
    pub timezone: String,
}

impl ClockConfig {
    pub fn module_id(&self) -> String {
        format!("clock#{}", self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub monitor_sleep: SleepSetting,
    #[serde(default)]
    pub system_sleep: SleepSetting,
    /// Locks via swaylock after this many idle minutes, and always before
    /// system sleep once enabled. Read by `og-power-apply`.
    #[serde(default)]
    pub screen_lock: SleepSetting,
    #[serde(default)]
    pub gaps_inner: i32,
    #[serde(default)]
    pub gaps_outer: i32,
    #[serde(default = "default_border")]
    pub border_width: i32,
    #[serde(default = "default_bar_bg")]
    pub bar_bg: String,
    #[serde(default = "default_bar_text")]
    pub bar_text: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    /// Unfocused workspace numbers/icons in waybar, and og-settings' own
    /// box borders. Distinct from `inactive_color` (unfocused window
    /// borders) and `accent` (the primary/focused accent).
    #[serde(default = "default_accent2")]
    pub accent2: String,
    #[serde(default = "default_sec_bg")]
    pub sec_bg: String,
    #[serde(default = "default_inactive")]
    pub inactive_color: String,
    #[serde(default = "default_urgent")]
    pub urgent_color: String,
    #[serde(default = "default_terminal")]
    pub terminal: String,
    /// Used by og-search for web search / `go/<alias>` lookups.
    #[serde(default)]
    pub default_browser: String,
    /// Used by og-search's "Ask AI" suggestion.
    #[serde(default)]
    pub default_ai_cli: String,
    #[serde(default)]
    pub monitor_configs: Vec<MonitorConfig>,
    #[serde(default = "default_waybar_position")]
    pub waybar_position: String,
    #[serde(default = "default_waybar_thickness")]
    pub waybar_thickness: i32,
    #[serde(default)]
    pub clock_timezone: String,
    #[serde(default = "default_clock_12h")]
    pub clock_12h: bool,
    /// Shared corner-rounding radius (pixels) for every OG-toolkit app's UI
    /// chrome.
    #[serde(default = "default_corner_radius")]
    pub corner_radius: f32,
    /// When enabled, each app that shares this theme tints its background
    /// slightly differently (a small deterministic hash-based shift, not
    /// random) so overlapping windows are easier to tell apart at a glance
    /// while still visibly sharing the same base theme.
    #[serde(default)]
    pub color_variance_enabled: bool,
    /// Max shift fraction applied per-app when variance is enabled — 0.0
    /// disables it in practice, larger values make the tint more obvious.
    #[serde(default = "default_color_variance_amount")]
    pub color_variance_amount: f32,
    /// Renders background (bg → sec_bg) and accent (accent → accent2) as
    /// gradients instead of flat colors, wherever that's wired up (waybar,
    /// wofi, and og-settings' own window bg / Apply & Save button).
    #[serde(default)]
    pub gradient_enabled: bool,
    #[serde(default)]
    pub extra_clocks: Vec<ClockConfig>,
    #[serde(default)]
    pub next_clock_id: u32,
    /// Module ids per bar section (waybar's `modules-left/center/right`).
    /// Empty until first seeded from the live waybar config.
    #[serde(default)]
    pub modules_left: Vec<String>,
    #[serde(default)]
    pub modules_center: Vec<String>,
    #[serde(default)]
    pub modules_right: Vec<String>,
    /// -1.0 (slowest) .. 1.0 (fastest), sway's own `pointer_accel` range.
    #[serde(default)]
    pub mouse_sensitivity: f32,
    /// Empty until first seeded from the live sway config's `seat seat0
    /// xcursor_theme` line, alongside `cursor_size`.
    #[serde(default)]
    pub cursor_theme: String,
    #[serde(default = "default_cursor_size")]
    pub cursor_size: i32,
    /// sway's own `focus_follows_mouse` setting — whether hovering a
    /// window (no click) gives it keyboard focus.
    #[serde(default)]
    pub focus_follows_mouse: FocusFollowsMouse,
    /// sway's own `mouse_warping` setting — whether the cursor jumps when
    /// focus changes via a non-mouse action (keybind, workspace switch).
    #[serde(default)]
    pub mouse_warping: MouseWarping,
    /// sway's `input type:keyboard repeat_rate` (characters/sec once a key
    /// is held past `keyboard_repeat_delay`).
    #[serde(default = "default_keyboard_repeat_rate")]
    pub keyboard_repeat_rate: i32,
    /// sway's `input type:keyboard repeat_delay` (ms held before repeat
    /// kicks in).
    #[serde(default = "default_keyboard_repeat_delay")]
    pub keyboard_repeat_delay: i32,
    /// "image" or "color".
    #[serde(default = "default_wallpaper_mode")]
    pub wallpaper_mode: String,
    /// Empty until first seeded from the live sway config's `output * bg`
    /// line, alongside `wallpaper_mode`/`cursor_theme`.
    #[serde(default)]
    pub wallpaper_path: String,
    #[serde(default = "default_wallpaper_color")]
    pub wallpaper_color: String,
    /// sway's `output bg` scaling mode: "fill", "stretch", or "center"
    /// (center = native resolution, unscaled).
    #[serde(default = "default_wallpaper_fit")]
    pub wallpaper_fit: String,
    /// Folder name under `~/.local/share/og-wallpaper/themes/` — only
    /// meaningful when `wallpaper_mode == "animated"`; empty means
    /// og-wallpaper falls back to its own built-in demo scene.
    #[serde(default)]
    pub wallpaper_animated_theme: String,
    /// Read directly by the standalone `og-notify` daemon, not og-settings
    /// — flashes a rain/glow/wind/sparkle overlay on every `Notify` dbus
    /// call.
    #[serde(default)]
    pub notif_fx_enabled: bool,
    /// "rain", "glow", "wind", or "sparkle" — mutually exclusive, not
    /// independent toggles.
    #[serde(default = "default_notif_fx_effect")]
    pub notif_fx_effect: String,
    #[serde(default = "default_notif_fx_color")]
    pub notif_fx_color: String,
    #[serde(default = "default_notif_fx_duration")]
    pub notif_fx_duration_ms: u32,
    /// 1.0 = fully opaque (effectively off). Applied to every window that
    /// isn't currently focused, by the standalone `og-opacity-apply`
    /// daemon watching sway's window-focus events.
    #[serde(default = "default_unfocused_opacity")]
    pub unfocused_opacity: f32,
    /// When on, Steam launches inside gamescope (a nested wlroots
    /// compositor). Alt-tabbing/losing focus on the *outer* sway desktop
    /// then never touches the game's own window-activation state, since
    /// from gamescope's perspective its client never lost focus — fixes
    /// games (particularly Proton titles) throttling/pausing their
    /// simulation when the host tabs away, which freezes them for anyone
    /// else connected. Applied by `og-settings` writing/removing a
    /// `~/.local/share/applications/steam.desktop` override (XDG user
    /// overrides take precedence over the system one) rather than editing
    /// per-game Steam launch options.
    #[serde(default)]
    pub gamescope_steam: bool,
    /// og-search: also show matching filenames from the filesystem, always
    /// ranked after app/web/AI results — a toggle since a filesystem walk
    /// is real I/O, not free, unlike everything else og-search already
    /// searches.
    #[serde(default = "default_true_bool")]
    pub search_files_enabled: bool,
    /// og-search: also show matching og-settings tabs (e.g. typing
    /// "bluetooth" surfaces the Network tab), ranked after everything
    /// else including file results.
    #[serde(default = "default_true_bool")]
    pub search_settings_enabled: bool,
}

fn default_true_bool() -> bool { true }

fn default_border() -> i32 { 1 }
fn default_bar_bg() -> String { "#1a1a2e".into() }
fn default_bar_text() -> String { "#e0e0e0".into() }
fn default_accent() -> String { "#ff7800".into() }
fn default_accent2() -> String { "#8855ff".into() }
fn default_sec_bg() -> String { "#2a2535".into() }
fn default_inactive() -> String { "#3a3a4a".into() }
fn default_urgent() -> String { "#ff4444".into() }
fn default_terminal() -> String { "alacritty".into() }
fn default_waybar_position() -> String { "left".into() }
fn default_waybar_thickness() -> i32 { 32 }
fn default_clock_12h() -> bool { false }
fn default_corner_radius() -> f32 { 0.0 }
fn default_color_variance_amount() -> f32 { 0.06 }
fn default_cursor_size() -> i32 { 24 }
fn default_keyboard_repeat_rate() -> i32 { 25 }
fn default_keyboard_repeat_delay() -> i32 { 600 }
fn default_wallpaper_mode() -> String { "image".into() }
fn default_wallpaper_color() -> String { "#1a1a2e".into() }
fn default_wallpaper_fit() -> String { "fill".into() }
fn default_notif_fx_effect() -> String { "rain".into() }
fn default_notif_fx_color() -> String { "#ff7800".into() }
fn default_notif_fx_duration() -> u32 { 1600 }
fn default_unfocused_opacity() -> f32 { 1.0 }

impl Default for Config {
    fn default() -> Self {
        Self {
            monitor_sleep: SleepSetting { enabled: true, minutes: 15 },
            system_sleep: SleepSetting { enabled: false, minutes: 30 },
            screen_lock: SleepSetting { enabled: false, minutes: 10 },
            gaps_inner: 0,
            gaps_outer: 0,
            border_width: 1,
            bar_bg: default_bar_bg(),
            bar_text: default_bar_text(),
            accent: default_accent(),
            accent2: default_accent2(),
            sec_bg: default_sec_bg(),
            inactive_color: default_inactive(),
            urgent_color: default_urgent(),
            terminal: default_terminal(),
            default_browser: String::new(),
            default_ai_cli: String::new(),
            monitor_configs: Vec::new(),
            waybar_position: default_waybar_position(),
            waybar_thickness: default_waybar_thickness(),
            clock_timezone: String::new(),
            clock_12h: default_clock_12h(),
            corner_radius: default_corner_radius(),
            color_variance_enabled: false,
            gradient_enabled: false,
            color_variance_amount: default_color_variance_amount(),
            extra_clocks: Vec::new(),
            next_clock_id: 0,
            modules_left: Vec::new(),
            modules_center: Vec::new(),
            modules_right: Vec::new(),
            mouse_sensitivity: 0.0,
            cursor_theme: String::new(),
            cursor_size: default_cursor_size(),
            focus_follows_mouse: FocusFollowsMouse::default(),
            mouse_warping: MouseWarping::default(),
            keyboard_repeat_rate: default_keyboard_repeat_rate(),
            keyboard_repeat_delay: default_keyboard_repeat_delay(),
            wallpaper_mode: default_wallpaper_mode(),
            wallpaper_path: String::new(),
            wallpaper_color: default_wallpaper_color(),
            wallpaper_fit: default_wallpaper_fit(),
            wallpaper_animated_theme: String::new(),
            notif_fx_enabled: false,
            notif_fx_effect: default_notif_fx_effect(),
            notif_fx_color: default_notif_fx_color(),
            notif_fx_duration_ms: default_notif_fx_duration(),
            unfocused_opacity: default_unfocused_opacity(),
            gamescope_steam: false,
            search_files_enabled: true,
            search_settings_enabled: true,
        }
    }
}

impl Config {
    /// Plain load: read the shared JSON file, defaulting any missing
    /// field. Does *not* seed anything from live sway/waybar config files
    /// — see `og-settings`'s own `config.rs` for that (first-run seeding
    /// is that app's job alone).
    pub fn load() -> Self {
        let path = config_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}

fn bar_config_path() -> PathBuf {
    let mut p = dirs_home();
    p.push(".config/sway-power/bar-config.json");
    p
}

/// sway's own three focus_follows_mouse values (`man 5 sway`) — "no"
/// requires a click to focus a window, "yes" focuses on hover, "always"
/// re-focuses on every pointer motion over an already-focused window too
/// (rare, but sway exposes it, so this does too).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum FocusFollowsMouse {
    #[default]
    No,
    Yes,
    Always,
}

impl FocusFollowsMouse {
    pub fn sway_value(&self) -> &'static str {
        match self {
            FocusFollowsMouse::No => "no",
            FocusFollowsMouse::Yes => "yes",
            FocusFollowsMouse::Always => "always",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FocusFollowsMouse::No => "Click to focus",
            FocusFollowsMouse::Yes => "Hover to focus",
            FocusFollowsMouse::Always => "Always (re-focus on hover)",
        }
    }

    pub fn from_sway_value(v: &str) -> Option<Self> {
        match v {
            "no" => Some(FocusFollowsMouse::No),
            "yes" => Some(FocusFollowsMouse::Yes),
            "always" => Some(FocusFollowsMouse::Always),
            _ => None,
        }
    }
}

/// sway's own `mouse_warping` setting — whether/where the cursor jumps
/// when focus changes without the mouse moving (a keybind, a workspace
/// switch). Companion to `focus_follows_mouse`: with hover-to-focus on,
/// warping the cursor to match a keybind-driven focus change avoids the
/// two fighting each other.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum MouseWarping {
    #[default]
    Output,
    Container,
    None,
}

impl MouseWarping {
    pub fn sway_value(&self) -> &'static str {
        match self {
            MouseWarping::Output => "output",
            MouseWarping::Container => "container",
            MouseWarping::None => "none",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            MouseWarping::Output => "On output change",
            MouseWarping::Container => "On container focus",
            MouseWarping::None => "Never",
        }
    }

    pub fn from_sway_value(v: &str) -> Option<Self> {
        match v {
            "output" => Some(MouseWarping::Output),
            "container" => Some(MouseWarping::Container),
            "none" => Some(MouseWarping::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

/// Which output(s) og-bar creates a surface on. `AllScreens` (the
/// original/default behavior) puts one bar on every connected output;
/// `SingleOutput` pins it to one output by compositor name (e.g. "DP-3"),
/// so there's exactly one surface and no cross-output focus interaction
/// at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "mode", content = "output")]
pub enum BarOutputMode {
    #[default]
    AllScreens,
    SingleOutput(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum ModuleKind {
    Workspaces,
    Clock {
        timezone: String,
        // Per-instance, independent of the waybar-only global
        // `Config::clock_12h` — a bar can carry multiple clock modules
        // (e.g. local + UTC) that each want their own format.
        #[serde(default = "default_true")]
        hour12: bool,
        #[serde(default = "default_true")]
        show_timezone: bool,
        #[serde(default = "default_true")]
        show_date: bool,
    },
    Cpu,
    Memory,
    Gpu,
    Tray,
    Bluetooth,
    Network,
    Pulseaudio,
    Notifications,
    Clipboard,
    Launcher { icon: String, tooltip: String, command: String },
    Settings,
    Power,
}

impl ModuleKind {
    /// Kinds the "Add module" picker can add with a single click, using a
    /// sensible default for any per-instance fields (Clock: system-default
    /// timezone, 12h, timezone shown — all editable after adding). Launcher
    /// is the one exception: an icon/command placeholder isn't a
    /// meaningful default, so it still only comes from hand-edited config.
    pub fn addable() -> Vec<ModuleKind> {
        vec![
            ModuleKind::Workspaces,
            ModuleKind::Clock { timezone: String::new(), hour12: true, show_timezone: true, show_date: true },
            ModuleKind::Cpu,
            ModuleKind::Memory,
            ModuleKind::Gpu,
            ModuleKind::Tray,
            ModuleKind::Bluetooth,
            ModuleKind::Network,
            ModuleKind::Pulseaudio,
            ModuleKind::Notifications,
            ModuleKind::Clipboard,
            ModuleKind::Settings,
            ModuleKind::Power,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModuleConfig {
    pub kind: ModuleKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Escape hatch from the bar's global `item_size` for one module.
    #[serde(default)]
    pub size_override: Option<u32>,
}

fn default_true() -> bool {
    true
}

/// Left/Middle/Right when the bar is on Top/Bottom; Top/Middle/Bottom
/// when it's on Left/Right — same enum either way, meaning just follows
/// `BarConfig::position`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SectionAlign {
    #[default]
    Start,
    Middle,
    End,
}

impl SectionAlign {
    pub fn label(&self, position: Edge) -> &'static str {
        let vertical_bar = matches!(position, Edge::Left | Edge::Right);
        match (self, vertical_bar) {
            (SectionAlign::Start, false) => "Left",
            (SectionAlign::Middle, false) => "Middle",
            (SectionAlign::End, false) => "Right",
            (SectionAlign::Start, true) => "Top",
            (SectionAlign::Middle, true) => "Middle",
            (SectionAlign::End, true) => "Bottom",
        }
    }
}

/// A user-defined slice of the bar. `percent` is a relative share of the
/// bar's total length (via `Length::FillPortion`, not a strict 0-100 that
/// must sum to 100) — sections don't need to add up exactly, each just
/// gets `percent / total_of_all_sections` of the space. `id` is a stable,
/// never-reused counter (see `BarConfig::next_section_id`) so removing
/// one section can't shift another's identity out from under it, same
/// convention as `ClockConfig::id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BarSection {
    pub id: u32,
    #[serde(default = "default_section_percent")]
    pub percent: u32,
    #[serde(default)]
    pub align: SectionAlign,
    #[serde(default)]
    pub modules: Vec<ModuleConfig>,
}

fn default_section_percent() -> u32 {
    33
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BarConfig {
    #[serde(default)]
    pub position: Edge,
    #[serde(default = "default_bar_thickness")]
    pub thickness: u32,
    #[serde(default = "default_item_size")]
    pub item_size: u32,
    #[serde(default = "default_bar_spacing")]
    pub spacing: u32,
    #[serde(default = "default_bar_padding")]
    pub padding: u32,
    #[serde(default = "default_sections")]
    pub sections: Vec<BarSection>,
    #[serde(default = "default_next_section_id")]
    pub next_section_id: u32,
    /// Per-app-id/class icon for the workspaces module — matched
    /// case-insensitively as a substring against a window's app_id or
    /// class, first match wins. User-editable list, not hardcoded
    /// (PLAN.md's own parity checklist calls this out specifically) —
    /// seeded with a sane default here, same as every other field.
    #[serde(default = "default_icon_rewrite")]
    pub icon_rewrite: Vec<IconRewriteRule>,
    #[serde(default)]
    pub output_mode: BarOutputMode,
    /// Auto-hide to a thin edge strip, reappearing on hover — a bar
    /// pinned to one output (SingleOutput) has no cross-output focus
    /// concerns, so this is safe to combine with either output_mode.
    #[serde(default)]
    pub auto_hide: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IconRewriteRule {
    pub match_value: String,
    pub icon: String,
}

fn rule(match_value: &str, icon: &str) -> IconRewriteRule {
    IconRewriteRule { match_value: match_value.to_string(), icon: icon.to_string() }
}

/// Every glyph here is a `\u{...}` escape, not a literal character — a
/// literal nerd-font glyph typed directly into a string silently produced
/// an *empty* string more than once while authoring this table (not a font
/// coverage gap, an actually-empty `&str`), and only got caught because it
/// showed up as blank instead of tofu. Escapes are the only way to be sure
/// what's actually in the string without a rendered preview.
fn default_icon_rewrite() -> Vec<IconRewriteRule> {
    vec![
        rule("og-settings", "\u{eaf8}"),     // cod-gear
        rule("alacritty", "./"),
        rule("brave-browser", "\u{f059f}"),  // md-brave (via nf-md alias used elsewhere)
        rule("firefox", "\u{f269}"),         // fa-firefox
        rule("chromium", "\u{f02af}"),       // md-google_chrome
        rule("code-oss", "\u{f0a1e}"),
        rule("vesktop", "\u{f066f}"),        // md-discord
        rule("discord", "\u{f066f}"),
        rule("godot", "\u{e7ee}"),           // dev-godot
        rule("factorio", "\u{f02b4}"),
        rule("steam_app", "\u{f02b4}"),
        rule("steamapp", "\u{f02b4}"),
        rule("steam", "\u{f1b6}"),           // fa-steam
        rule("spotify", "\u{f1bc}"),         // fa-spotify
        rule("thunar", "\u{e5ff}"),          // custom-folder
        rule("nautilus", "\u{e5ff}"),        // custom-folder
        rule("vlc", "\u{f057c}"),
        rule("gimp", "\u{e7e7}"),            // dev-gimp
        rule("obs", "\u{f1720}"),            // md-broadcast (no literal OBS glyph in this icon set)
        rule("minecraft", "\u{f034e}"),
    ]
}

fn default_bar_thickness() -> u32 {
    32
}

fn default_item_size() -> u32 {
    32
}

fn default_bar_spacing() -> u32 {
    4
}

fn default_bar_padding() -> u32 {
    4
}

fn default_next_section_id() -> u32 {
    4
}

fn default_sections() -> Vec<BarSection> {
    vec![
        BarSection {
            id: 1,
            percent: 33,
            align: SectionAlign::Start,
            modules: vec![ModuleConfig { kind: ModuleKind::Workspaces, enabled: true, size_override: None }],
        },
        BarSection {
            id: 2,
            percent: 34,
            align: SectionAlign::Middle,
            modules: vec![
                ModuleConfig { kind: ModuleKind::Cpu, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Memory, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Clock { timezone: String::new(), hour12: true, show_timezone: true, show_date: true }, enabled: true, size_override: None },
            ],
        },
        BarSection {
            id: 3,
            percent: 33,
            align: SectionAlign::End,
            modules: vec![
                ModuleConfig {
                    kind: ModuleKind::Launcher {
                        icon: "./".to_string(),
                        tooltip: "Open Terminal".to_string(),
                        command: "alacritty".to_string(),
                    },
                    enabled: true,
                    size_override: None,
                },
                ModuleConfig {
                    kind: ModuleKind::Launcher {
                        icon: "\u{ee0d}".to_string(), // fa-robot — no literal Claude glyph in this icon set
                        tooltip: "Open Claude CLI".to_string(),
                        command: "alacritty -e claude --dangerously-skip-permissions".to_string(),
                    },
                    enabled: true,
                    size_override: None,
                },
                ModuleConfig {
                    kind: ModuleKind::Launcher {
                        icon: "\u{f002}".to_string(), // fa-search
                        tooltip: "Search".to_string(),
                        // ~/.local/bin, not bare "og-search" — sway's own
                        // process environment doesn't have ~/.local/bin on
                        // PATH (same class of bug fixed earlier for
                        // og-power-apply), and every other launcher/click
                        // command that targets a user-installed binary in
                        // this file already uses the full path for exactly
                        // that reason.
                        command: "~/.local/bin/og-search".to_string(),
                    },
                    enabled: true,
                    size_override: None,
                },
                ModuleConfig { kind: ModuleKind::Settings, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Tray, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Bluetooth, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Network, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Pulseaudio, enabled: true, size_override: None },
                ModuleConfig { kind: ModuleKind::Power, enabled: true, size_override: None },
            ],
        },
    ]
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            position: Edge::Top,
            thickness: default_bar_thickness(),
            item_size: default_item_size(),
            spacing: default_bar_spacing(),
            padding: default_bar_padding(),
            sections: default_sections(),
            next_section_id: default_next_section_id(),
            icon_rewrite: default_icon_rewrite(),
            output_mode: BarOutputMode::default(),
            auto_hide: false,
        }
    }
}

impl BarConfig {
    pub fn load() -> Self {
        let path = bar_config_path();
        let Some(text) = std::fs::read_to_string(&path).ok() else { return Self::default() };

        // Pre-section-system config files have `modules_start`/`_center`/
        // `_end` instead of `sections` — without this, loading one of
        // those would silently fall back to serde's `#[serde(default =
        // "default_sections")]` and discard whatever modules were
        // actually in those three lists (real user customization, not
        // just the stock defaults, on at least the machine this was
        // written on). Migrate them into three sections (Start/Middle/End,
        // ~33% each) instead of losing that arrangement.
        let mut value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        if value.get("sections").is_none() {
            if let Some(obj) = value.as_object_mut() {
                let take_list = |obj: &mut serde_json::Map<String, serde_json::Value>, key: &str| {
                    obj.remove(key).and_then(|v| v.as_array().cloned()).unwrap_or_default()
                };
                let start = take_list(obj, "modules_start");
                let center = take_list(obj, "modules_center");
                let end = take_list(obj, "modules_end");
                let make_section = |id: u32, percent: u32, align: &str, modules: Vec<serde_json::Value>| {
                    serde_json::json!({ "id": id, "percent": percent, "align": align, "modules": modules })
                };
                obj.insert(
                    "sections".to_string(),
                    serde_json::Value::Array(vec![
                        make_section(1, 33, "Start", start),
                        make_section(2, 34, "Middle", center),
                        make_section(3, 33, "End", end),
                    ]),
                );
                obj.insert("next_section_id".to_string(), serde_json::json!(4));
            }
        }

        serde_json::from_value(value).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = bar_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}
