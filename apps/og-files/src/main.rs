//! OG Files — file tree on the left, tabbed viewers/editors on the right,
//! inspector beside them and a details/history strip underneath.
//! See SPEC.md for the full design and roadmap.

// Windows release builds: no console window behind the GUI (the CLI
// subcommands re-attach to the parent console, see `platform`).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod archops;
mod browse;
mod cli;
mod config;
mod details;
mod devices;
mod extdnd;
mod filetype;
mod fsops;
mod inspector;
#[cfg(any(target_os = "macos", test))]
mod macapps;
#[cfg(target_os = "macos")]
mod macos;
mod net;
mod netui;
mod openwith;
mod ops;
mod platform;
mod search;
mod sidebar;
mod tabs;
mod terminal;
mod theme;
mod thumbs;
mod tree;
mod viewers;
mod widgets;

fn main() -> iced::Result {
    // subcommands (extract, compress, list, open, open-with, apps, --help)
    // run without a window
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match cli::parse(&args) {
        Ok(Some(cmd)) => {
            platform::attach_parent_console();
            std::process::exit(cli::run(cmd))
        }
        Ok(None) => {}
        Err(e) => {
            platform::attach_parent_console();
            eprintln!("og-files: {e}");
            std::process::exit(2);
        }
    }
    config::migrate_old_dirs();
    // App Store build: re-open the folders the user granted earlier
    #[cfg(target_os = "macos")]
    macos::restore_access();
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut window = iced::window::Settings {
        size: iced::Size::new(1400.0, 860.0),
        // taskbar / alt-tab icon on Linux and Windows (macOS uses the
        // bundle's icon)
        icon: iced::window::icon::from_file_data(include_bytes!("../packaging/icons/og-files.png"), None).ok(),
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    {
        // sway rules and other apps address the window by this id
        window.platform_specific.application_id = "og-files".to_string();
    }
    iced::application("OG Files", app::App::update, app::App::view)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(window)
        .font(iced_fonts::BOOTSTRAP_FONT_BYTES)
        .run_with(app::App::new)
}
