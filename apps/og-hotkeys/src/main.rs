mod app;
mod bindings;
mod config;
mod fonts;

use app::App;

fn main() -> iced::Result {
    let app = iced::application("og-hotkeys", App::update, App::view)
        .subscription(App::subscription)
        .style(|_state, _theme| iced::application::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: iced::Color::WHITE,
        });

    let app = match fonts::load_default_font() {
        Some((family, bytes)) => app.font(bytes).default_font(iced::Font::with_name(family)),
        None => app,
    };

    app
        .window(iced::window::Settings {
            size: iced::Size::new(640.0, 700.0),
            decorations: false,
            transparent: true,
            position: iced::window::Position::Centered,
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-hotkeys".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run_with(App::new)
}
