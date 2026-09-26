mod app;
mod config;
mod fonts;
mod notif;
mod placement;

use app::{App, SIZE};

fn main() -> iced::Result {
    let app = iced::application("og-notif-center", App::update, App::view)
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
            size: iced::Size::new(SIZE.0, SIZE.1),
            decorations: false,
            transparent: true,
            // Starts hidden — App::new's init Task positions it (via
            // swaymsg, since Wayland doesn't let a client set its own
            // toplevel position — see placement.rs) and only then reveals
            // it, so there's no visible pop-in-the-center-then-jump.
            visible: false,
            position: placement::near_bar(SIZE),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-notif-center".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run_with(App::new)
}
