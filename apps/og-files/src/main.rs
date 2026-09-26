mod app;
mod desktop;
mod devices;
mod filesystem;
mod jobs;
mod panes;
mod tab;
mod theme;
mod thumbs;
mod watcher;

use app::App;

fn main() -> iced::Result {
    iced::application("OG Files", App::update, App::view)
        .window(iced::window::Settings {
            size: iced::Size::new(1200.0, 800.0),
            decorations: true,
            position: iced::window::Position::Centered,
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-files".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .subscription(App::subscription)
        .run_with(App::new)
}
