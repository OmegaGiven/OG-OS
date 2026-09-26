mod app;
mod apps;
mod config;
mod file_search;
mod launch;
mod settings_search;

use app::App;

fn main() -> iced::Result {
    iced::application("og-search", App::update, App::view)
        .subscription(App::subscription)
        // The window itself is a plain rectangle — our own container draws
        // the rounded card, so everything outside it (the corners, and any
        // gap if the card doesn't fill the window) needs the renderer's own
        // clear color to be fully transparent too, not just the window
        // surface flagged as transparent, or it paints opaque white there.
        .style(|_state, _theme| iced::application::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: iced::Color::WHITE,
        })
        .window(iced::window::Settings {
            size: iced::Size::new(900.0, 550.0),
            decorations: false,
            transparent: true,
            position: iced::window::Position::Centered,
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-search".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run_with(App::new)
}
