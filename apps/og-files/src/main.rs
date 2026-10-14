//! OG Files — file tree on the left, tabbed viewers/editors on the right,
//! inspector beside them and a details/history strip underneath.
//! See SPEC.md for the full design and roadmap.

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
mod net;
mod netui;
mod openwith;
mod ops;
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
        Ok(Some(cmd)) => std::process::exit(cli::run(cmd)),
        Ok(None) => {}
        Err(e) => {
            eprintln!("og-files: {e}");
            std::process::exit(2);
        }
    }
    config::migrate_old_dirs();
    iced::application("OG Files", app::App::update, app::App::view)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1400.0, 860.0),
            platform_specific: iced::window::settings::PlatformSpecific {
                // sway rules and other apps address the window by this id
                application_id: "og-files".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .font(iced_fonts::BOOTSTRAP_FONT_BYTES)
        .run_with(app::App::new)
}
