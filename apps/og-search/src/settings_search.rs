/// Mirrors og-settings' own tabs — one entry per individual setting/control
/// (not just the tab itself), so e.g. "12h" or "corner rounding" surfaces
/// the specific thing instead of only the tab it lives on. Kept as a small
/// static list here rather than a dependency on sway-control's own crate
/// (og-settings isn't structured as a library og-search could import) — if
/// a tab's --tab argument or which settings live on it changes there, it
/// needs to change here too. There's no deep-link into a tab yet (og-search
/// can only launch `og-settings --tab <arg>`), so multiple settings on the
/// same tab all resolve to that tab opening, just under their own specific
/// label so the right one is easy to spot in the results.
struct SettingEntry {
    tab_arg: &'static str,
    tab_label: &'static str,
    setting: &'static str,
    keywords: &'static [&'static str],
}

const SETTINGS: &[SettingEntry] = &[
    // Power
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "Monitor sleep timeout", keywords: &["screen off", "display sleep"] },
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "System suspend timeout", keywords: &["suspend", "sleep"] },
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "Screen lock timeout", keywords: &["lock on idle", "swaylock"] },
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "Keep games running when tabbed away", keywords: &["gamescope", "steam", "big picture"] },
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "Startup commands", keywords: &["exec", "exec_always", "sway startup"] },
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "Startup services", keywords: &["systemd", "user service"] },
    SettingEntry { tab_arg: "power", tab_label: "Power", setting: "System services", keywords: &["systemd", "root service"] },

    // Display
    SettingEntry { tab_arg: "display", tab_label: "Display", setting: "Monitor arrangement", keywords: &["arrange", "drag", "position", "multi-monitor"] },
    SettingEntry { tab_arg: "display", tab_label: "Display", setting: "Monitor resolution", keywords: &["mode", "refresh rate"] },
    SettingEntry { tab_arg: "display", tab_label: "Display", setting: "Screen brightness", keywords: &["backlight", "brightness"] },

    // Network
    SettingEntry { tab_arg: "network", tab_label: "Network", setting: "Wi-Fi", keywords: &["wifi", "wireless", "scan"] },
    SettingEntry { tab_arg: "network", tab_label: "Network", setting: "Ethernet", keywords: &["wired", "interface"] },
    SettingEntry { tab_arg: "network", tab_label: "Network", setting: "Bluetooth", keywords: &["pair", "discoverable"] },
    SettingEntry { tab_arg: "network", tab_label: "Network", setting: "Tailscale exit node", keywords: &["vpn", "tailscale"] },
    SettingEntry { tab_arg: "network", tab_label: "Network", setting: "VPN / WireGuard", keywords: &["wireguard", "per-app vpn", "vpn"] },
    SettingEntry { tab_arg: "network", tab_label: "Network", setting: "Web shortcuts (go/alias)", keywords: &["galias", "go alias", "shortcut"] },

    // Updates
    SettingEntry { tab_arg: "updates", tab_label: "Updates", setting: "Pacman updates", keywords: &["pacman", "system update"] },
    SettingEntry { tab_arg: "updates", tab_label: "Updates", setting: "AUR updates", keywords: &["aur", "yay", "paru"] },
    SettingEntry { tab_arg: "updates", tab_label: "Updates", setting: "Flatpak updates", keywords: &["flatpak"] },

    // Hotkeys
    SettingEntry { tab_arg: "hotkeys", tab_label: "Hotkeys", setting: "Sway variables", keywords: &["set $", "variable"] },
    SettingEntry { tab_arg: "hotkeys", tab_label: "Hotkeys", setting: "Key bindings", keywords: &["keybind", "shortcut", "bindsym"] },

    // Theme
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Theme presets", keywords: &["dark mode", "light mode", "import", "export"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "System colors", keywords: &["background color", "accent color", "text color"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Per-app color variance", keywords: &["variance", "gradient"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Default terminal", keywords: &["terminal emulator"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Default browser", keywords: &["browser"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "AI CLI", keywords: &["og-search ai", "ask ai"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Inner / outer gaps", keywords: &["gaps"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Border width", keywords: &["border"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Corner rounding", keywords: &["radius", "rounded corners"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Unfocused window opacity", keywords: &["opacity", "transparency"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Taskbar edge & thickness", keywords: &["waybar", "taskbar position", "thickness"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Clock 12h/24h format", keywords: &["12h", "24h", "hour format"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Clock timezone", keywords: &["timezone", "extra clock"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Taskbar module arrangement", keywords: &["module positions", "drag module"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Wallpaper", keywords: &["background image", "wallpaper color", "fit"] },
    SettingEntry { tab_arg: "theme", tab_label: "Theme", setting: "Sync login background (LightDM)", keywords: &["lightdm", "login screen"] },

    // Mouse & Keyboard
    SettingEntry { tab_arg: "input", tab_label: "Mouse & Keyboard", setting: "Window focus (click vs hover)", keywords: &["focus follows mouse", "autofocus", "hover to focus"] },
    SettingEntry { tab_arg: "input", tab_label: "Mouse & Keyboard", setting: "Cursor warping", keywords: &["mouse warping", "jump cursor"] },
    SettingEntry { tab_arg: "input", tab_label: "Mouse & Keyboard", setting: "Pointer sensitivity", keywords: &["mouse speed"] },
    SettingEntry { tab_arg: "input", tab_label: "Mouse & Keyboard", setting: "Cursor theme & size", keywords: &["cursor icon"] },
    SettingEntry { tab_arg: "input", tab_label: "Mouse & Keyboard", setting: "Key repeat rate & delay", keywords: &["keyboard repeat", "key repeat"] },

    // Bar (og-bar full editor)
    SettingEntry { tab_arg: "bar", tab_label: "Bar", setting: "Bar edge", keywords: &["og-bar position", "top bottom left right"] },
    SettingEntry { tab_arg: "bar", tab_label: "Bar", setting: "Bar thickness / item size / spacing / padding", keywords: &["og-bar size"] },
    SettingEntry { tab_arg: "bar", tab_label: "Bar", setting: "Bar sections", keywords: &["section share", "section percent", "divider"] },
    SettingEntry { tab_arg: "bar", tab_label: "Bar", setting: "Bar modules", keywords: &["add module", "workspaces", "tray", "notifications module"] },

    // Search
    SettingEntry { tab_arg: "search", tab_label: "Search", setting: "Also search files on disk", keywords: &["file search"] },
    SettingEntry { tab_arg: "search", tab_label: "Search", setting: "Also search settings", keywords: &["settings search"] },

    // Notifications
    SettingEntry { tab_arg: "notifications", tab_label: "Notifications", setting: "Enable notifications", keywords: &["mako", "master switch"] },
    SettingEntry { tab_arg: "notifications", tab_label: "Notifications", setting: "Notification effects", keywords: &["rain", "glow", "wind", "sparkle"] },
    SettingEntry { tab_arg: "notifications", tab_label: "Notifications", setting: "Notification color & duration", keywords: &["dnd", "do not disturb"] },

    // History
    SettingEntry { tab_arg: "history", tab_label: "History", setting: "Restore previous config", keywords: &["undo", "snapshot"] },

    // System Monitor
    SettingEntry { tab_arg: "sysmonitor", tab_label: "System Monitor", setting: "System monitor", keywords: &["cpu", "ram", "memory", "btop", "htop"] },

    // Audio
    SettingEntry { tab_arg: "audio", tab_label: "Audio", setting: "Output devices & volume", keywords: &["speaker", "volume", "default output"] },
    SettingEntry { tab_arg: "audio", tab_label: "Audio", setting: "Input devices", keywords: &["microphone", "recording"] },
    SettingEntry { tab_arg: "audio", tab_label: "Audio", setting: "Audio profile", keywords: &["sound card", "profile"] },

    // Devices
    SettingEntry { tab_arg: "devices", tab_label: "Devices", setting: "Hardware overview", keywords: &["usb", "pci", "peripherals", "monitors"] },

    // Printing
    SettingEntry { tab_arg: "printing", tab_label: "Printing", setting: "Printers", keywords: &["cups", "print", "test page", "default printer"] },
];

#[derive(Debug, Clone, Copy)]
pub struct SettingsMatch {
    pub tab_arg: &'static str,
    pub label: &'static str,
}

/// Ranked so a setting whose own name matches lands above one that only
/// matches because its tab label or a loosely-related keyword does — e.g.
/// searching "gaps" should put "Inner / outer gaps" above "Theme" itself.
pub fn search(query: &str) -> Vec<SettingsMatch> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let q = query.to_lowercase();

    let mut ranked: Vec<(u8, &'static str, &'static str)> = SETTINGS
        .iter()
        .filter_map(|s| {
            let setting_lc = s.setting.to_lowercase();
            let tab_lc = s.tab_label.to_lowercase();
            if setting_lc.starts_with(&q) {
                Some((0, s.tab_arg, s.setting))
            } else if setting_lc.contains(&q) {
                Some((1, s.tab_arg, s.setting))
            } else if tab_lc.contains(&q) {
                Some((2, s.tab_arg, s.setting))
            } else if s.keywords.iter().any(|k| k.contains(&q as &str)) {
                Some((3, s.tab_arg, s.setting))
            } else {
                None
            }
        })
        .collect();

    ranked.sort_by_key(|(rank, _, _)| *rank);
    ranked.into_iter().take(10).map(|(_, tab_arg, label)| SettingsMatch { tab_arg, label }).collect()
}
