mod app;
mod config;
mod pkg;

use app::App;

fn main() -> iced::Result {
    iced::application("OG Apps", App::update, App::view)
        // Our own container draws the rounded card + border; everything
        // outside it (the window's corners) needs the renderer's clear
        // color to be fully transparent too, not just the surface flagged
        // as transparent, or it paints opaque white there instead.
        .style(|_state, _theme| iced::application::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: iced::Color::WHITE,
        })
        .window(iced::window::Settings {
            size: iced::Size::new(1000.0, 700.0),
            decorations: false,
            transparent: true,
            position: iced::window::Position::Centered,
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-apps".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run_with(App::new)
}
