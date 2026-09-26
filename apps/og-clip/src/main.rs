mod app;
mod config;
mod fonts;
mod history;

use app::App;

fn main() -> iced::Result {
    // `wl-paste --watch <cmd>` runs `<cmd>` with the clipboard contents on
    // stdin every time the selection changes — these two subcommands are
    // that `<cmd>`, not a GUI launch. See sway config for how they're
    // wired (`wl-paste --type text --watch og-clip --store-text`, same
    // for `image/png` / `--store-image`).
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--store-text") {
        history::store_text_from_stdin();
        return Ok(());
    }
    if args.iter().any(|a| a == "--store-image") {
        history::store_image_from_stdin();
        return Ok(());
    }

    let app = iced::application("og-clip", App::update, App::view)
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
            size: iced::Size::new(560.0, 560.0),
            decorations: false,
            transparent: true,
            position: iced::window::Position::Centered,
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-clip".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run_with(App::new)
}
