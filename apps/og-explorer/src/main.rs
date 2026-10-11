//! OG Explorer — file tree on the left, tabbed viewers/editors on the right,
//! inspector beside them and a details/history strip underneath.
//! See SPEC.md for the full design and roadmap.

mod app;
mod browse;
mod config;
mod details;
mod filetype;
mod fsops;
mod inspector;
mod search;
mod sidebar;
mod tabs;
mod theme;
mod thumbs;
mod tree;
mod viewers;

fn main() -> iced::Result {
    iced::application("OG Explorer", app::App::update, app::App::view)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window_size((1400.0, 860.0))
        .run_with(app::App::new)
}
