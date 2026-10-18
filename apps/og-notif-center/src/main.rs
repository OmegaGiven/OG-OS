mod app;
mod config;
mod fonts;
mod notif;

use app::App;
use iced_layershell::build_pattern::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};

/// macOS-style Notification Center: a layer-shell surface covering the
/// focused output (below og-bar's exclusive zone), with the panel slid in
/// against the right edge. The rest of the surface is transparent but
/// catches clicks, so clicking anywhere outside the panel closes it.
fn main() -> iced_layershell::Result {
    // The bell toggles: a second launch while open closes the open one.
    if toggle_existing() {
        return Ok(());
    }

    let app = application(App::namespace, App::update, App::view)
        .subscription(App::subscription)
        .style(|_state, _theme| iced_layershell::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: iced::Color::WHITE,
        });

    let app = match fonts::load_default_font() {
        Some((family, bytes)) => app.font(bytes).default_font(iced::Font::with_name(family)),
        None => app,
    };

    app.layer_settings(LayerShellSettings {
        anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
        // 0 = sit inside other surfaces' exclusive zones (i.e. below the bar)
        // without reserving any space ourselves.
        exclusive_zone: 0,
        layer: Layer::Overlay,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        start_mode: StartMode::Active,
        ..Default::default()
    })
    .run_with(App::new)
}

fn pid_path() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(dir).join("og-notif-center.pid")
}

/// If another instance is running, ask it to close (SIGTERM) and report
/// true; otherwise record our own pid and report false.
fn toggle_existing() -> bool {
    let path = pid_path();
    if let Some(pid) = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if pid != std::process::id() as i32 && comm.trim() == "og-notif-center" {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            return true;
        }
    }
    let _ = std::fs::write(&path, std::process::id().to_string());
    false
}

pub fn exit() -> ! {
    let path = pid_path();
    if std::fs::read_to_string(&path).ok().as_deref().map(str::trim) == Some(&std::process::id().to_string()) {
        let _ = std::fs::remove_file(path);
    }
    std::process::exit(0)
}
