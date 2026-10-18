mod circuit;
mod direct;
mod effects;
mod scene;
mod sway;
mod tokyo;

use std::time::Instant;

use iced::widget::canvas::{self, Canvas};
use iced::{mouse, Element, Length, Rectangle, Renderer, Subscription, Task, Theme};
use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};

use effects::{MaskBuf, RuntimeRegion};
use og_config::Config;
use og_theme::AppColors;
use og_wallpaper_core::manifest::{Manifest, RegionShape};

/// Largest size a theme image is kept at (covers 5120-wide ultrawides and
/// 5K panels).
const MAX_W: u32 = 5120;
const MAX_H: u32 = 2880;

/// A loaded theme: the base artwork (already filtered to line-art/2-color
/// by og-wallpaper-studio, not touched here) plus which regions of it
/// are animated and how.
struct LoadedTheme {
    image: iced::widget::image::Handle,
    size: (f32, f32),
    regions: Vec<RuntimeRegion>,
}

impl LoadedTheme {
    fn load(name: &str) -> Option<Self> {
        let dir = og_wallpaper_core::manifest::theme_dir(name);
        let img = match og_wallpaper_core::image::open(dir.join("base.png")) {
            Ok(img) => img,
            Err(e) => {
                eprintln!("og-wallpaper: theme \"{name}\" failed to load ({}): {e}", dir.display());
                return None;
            }
        };
        // Themes are often huge (6000px+ source art); nothing needs more
        // than the widest output, so shrink once here instead of having
        // the renderer resample the full-size image on every frame.
        let img = if img.width() > MAX_W || img.height() > MAX_H {
            img.resize(MAX_W, MAX_H, og_wallpaper_core::image::imageops::FilterType::Triangle)
        } else {
            img
        };
        let mut rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let manifest = Manifest::load(name).unwrap_or_default();

        // The artwork's colors are baked into the PNG at filter time in
        // og-wallpaper-studio (both line_art and two_color produce flat,
        // unblended colors) — without this, a saved theme stays frozen
        // at whatever the system theme was the moment it was filtered,
        // silently drifting out of sync on every later theme change.
        // Exact-match remap from those baked colors to the *current*
        // theme's, once here at load (this process gets killed and
        // relaunched on every theme apply — see sway.rs's
        // apply_wallpaper() — so "current" really does mean current).
        if let Some((old_line, old_bg)) = &manifest.base_colors {
            let config = Config::load();
            og_wallpaper_core::filters::remap_colors(
                &mut rgba,
                og_wallpaper_core::filters::hex_to_rgba(old_line, 255),
                og_wallpaper_core::filters::hex_to_rgba(old_bg, 255),
                og_wallpaper_core::filters::hex_to_rgba(&config.accent, 255),
                og_wallpaper_core::filters::hex_to_rgba(&config.bar_bg, 255),
            );
        }
        let regions = manifest
            .regions
            .into_iter()
            .map(|r| {
                let mask = match &r.shape {
                    RegionShape::Mask { mask_file, .. } => match og_wallpaper_core::image::open(dir.join(mask_file)) {
                        Ok(m) => {
                            let gray = m.to_luma8();
                            let (mw, mh) = gray.dimensions();
                            Some(MaskBuf { w: mw, h: mh, data: gray.into_raw() })
                        }
                        Err(e) => {
                            eprintln!("og-wallpaper: theme \"{name}\" mask \"{mask_file}\" failed to load: {e}");
                            None
                        }
                    },
                    _ => None,
                };
                RuntimeRegion { shape: r.shape, mask, effect: r.effect, color_override: r.color_override }
            })
            .collect();
        Some(Self {
            image: iced::widget::image::Handle::from_rgba(w, h, rgba.into_raw()),
            size: (w as f32, h as f32),
            regions,
        })
    }
}

/// - `Theme`: a theme is configured and loaded fine.
/// - `Demo`: no theme configured at all — show the built-in scene.
/// - `Failed`: a theme *was* configured but couldn't load (renamed/
///   deleted/corrupt) — plain background, not a silent fallback to the
///   demo scene, which reads as "my theme selection did nothing" rather
///   than "something's actually wrong". Check /tmp/og-wallpaper.log.
enum Background {
    Theme(LoadedTheme),
    Demo,
    Failed,
}

/// Built-in procedural scenes. These don't use iced at all: they run on the
/// lean Wayland + GLES renderer in direct/.
enum Procedural {
    /// Futuristic motherboard (circuit/).
    Circuit,
    /// Neo-Tokyo night with rain and sakura (tokyo/).
    Tokyo,
}

/// The configured theme's built-in procedural scene, if it is one.
fn configured_procedural(config: &Config) -> Option<Procedural> {
    if config.wallpaper_animated_theme.is_empty() {
        return None;
    }
    match Manifest::load(&config.wallpaper_animated_theme)?.procedural?.as_str() {
        "circuit" => Some(Procedural::Circuit),
        "tokyo" => Some(Procedural::Tokyo),
        other => {
            eprintln!("og-wallpaper: unknown procedural theme \"{other}\"");
            None
        }
    }
}

/// Set when a procedural scene couldn't start (e.g. no GLES 3): the iced
/// path then shows the plain fill instead of a silent swap to the demo.
static PROCEDURAL_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct App {
    colors: AppColors,
    background: Background,
    demo: scene::Scene,
    start: Instant,
    t: f32,
}

#[derive(Debug, Clone, Copy)]
enum Message {
    Tick,
}

// iced_layershell requires Message convert into its own action enum for
// apps that use runtime layer-shell actions (LayerChange, SizeChange,
// etc, via #[to_layer_message]) — this app never sends any, so a trivial
// infallible conversion (this variant is never actually produced) is all
// that's needed to satisfy the trait bound.
impl TryFrom<Message> for iced_layershell::actions::LayershellCustomActionsWithId {
    type Error = Message;
    fn try_from(value: Message) -> Result<Self, Self::Error> {
        Err(value)
    }
}

fn new() -> (App, Task<Message>) {
    let config = Config::load();
    let colors = AppColors::from_config(&config, "og-wallpaper");
    let background = if config.wallpaper_animated_theme.is_empty() {
        Background::Demo
    } else if PROCEDURAL_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
        Background::Failed
    } else {
        match LoadedTheme::load(&config.wallpaper_animated_theme) {
            Some(theme) => Background::Theme(theme),
            None => Background::Failed,
        }
    };
    (App { colors, background, demo: scene::Scene::generate(), start: Instant::now(), t: 0.0 }, Task::none())
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Tick => {
            // Wrapped hourly: f32 seconds lose sub-frame precision after a
            // few days, which made the animation stutter.
            app.t = (app.start.elapsed().as_secs_f64() % 3600.0) as f32;
        }
    }
    Task::none()
}

fn view(app: &App, _id: iced::window::Id) -> Element<'_, Message> {
    Canvas::new(app).width(Length::Fill).height(Length::Fill).into()
}

fn remove_id(_app: &mut App, _id: iced::window::Id) {}

fn subscription(_app: &App) -> Subscription<Message> {
    // ~30fps — ambient wallpaper running full-time on every output.
    iced::time::every(std::time::Duration::from_millis(33)).map(|_| Message::Tick)
}

/// Where the theme image lands: cover-fit (scale to fill the output,
/// center-cropping overflow) — same intent as sway's own "fill" wallpaper
/// mode, so an animated theme behaves like any other wallpaper choice.
fn cover_rect(theme: &LoadedTheme, bounds: Rectangle) -> Rectangle {
    let scale = (bounds.width / theme.size.0).max(bounds.height / theme.size.1);
    let width = theme.size.0 * scale;
    let height = theme.size.1 * scale;
    Rectangle { x: (bounds.width - width) * 0.5, y: (bounds.height - height) * 0.5, width, height }
}

impl canvas::Program<Message> for App {
    /// Per-output cache of everything that doesn't move (fill, artwork,
    /// static line-art). Only the animated layer is rebuilt each tick —
    /// redrawing the full-screen artwork 30x/s per output is what used to
    /// pin the CPU. The cache redraws itself when the output size changes.
    type State = canvas::Cache;

    fn draw(
        &self,
        cache: &canvas::Cache,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let still = cache.draw(renderer, bounds.size(), |frame| {
            frame.fill_rectangle(iced::Point::ORIGIN, bounds.size(), self.colors.bar_bg);
            match &self.background {
                Background::Theme(theme) => frame.draw_image(cover_rect(theme, bounds), &theme.image),
                Background::Demo => self.demo.draw_static(frame, &self.colors),
                Background::Failed => {}
            }
        });

        let mut frame = canvas::Frame::new(renderer, bounds.size());
        match &self.background {
            Background::Theme(theme) => {
                let rect = cover_rect(theme, bounds);
                for region in &theme.regions {
                    // Regions are authored in the base image's own
                    // fraction space — map into the same scaled/offset
                    // rect the image itself was just drawn into, not raw
                    // canvas bounds, so they stay pinned to the artwork
                    // under any output resolution/aspect ratio.
                    effects::draw_region_in(
                        &mut frame,
                        region,
                        rect,
                        self.t,
                        self.colors.accent,
                    );
                }
            }
            Background::Demo => self.demo.draw_animated(&mut frame, self.t, &self.colors),
            // Plain fill (already drawn above) — a configured-but-broken
            // theme should look "off", not silently swap in the demo.
            Background::Failed => {}
        }

        vec![still, frame.into_geometry()]
    }
}

fn main() -> iced_layershell::Result {
    // Launched unconditionally via sway's exec_always (same convention as
    // og-bar/og-notify/etc) — self-gates here instead of needing sway
    // config to know about the setting, so switching wallpaper_mode in
    // og-settings is the only place that decides whether this runs.
    let config = Config::load();
    if config.wallpaper_mode != "animated" {
        return Ok(());
    }
    if let Some(p) = configured_procedural(&config) {
        let fps = config.wallpaper_fps;
        let r = match p {
            Procedural::Circuit => {
                let colors = AppColors::from_config(&config, "og-wallpaper");
                let c = |c: iced::Color| [c.r, c.g, c.b];
                direct::run(circuit::Circuit { accent: c(colors.accent), bg: c(colors.bar_bg) }, fps)
            }
            Procedural::Tokyo => direct::run(tokyo::Tokyo::default(), fps),
        };
        // Only returns on failure.
        if let Err(e) = r {
            eprintln!("og-wallpaper: procedural scene failed: {e}");
        }
        PROCEDURAL_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // Vsync (FIFO) blocks each present on a Wayland frame callback, and
    // sway sends none to a background surface that windows cover — the
    // render loop froze after one frame. The 33 ms tick already paces
    // frames, so present without vsync (Immediate → Mailbox → Fifo).
    if std::env::var_os("ICED_PRESENT_MODE").is_none() {
        std::env::set_var("ICED_PRESENT_MODE", "no_vsync");
    }

    daemon("og-wallpaper", update, view, remove_id)
        .subscription(subscription)
        .layer_settings(LayerShellSettings {
            // Background, not Top/Overlay — this sits under every real
            // window. Purely decorative: input-transparent.
            layer: Layer::Background,
            anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
            exclusive_zone: 0,
            keyboard_interactivity: KeyboardInteractivity::None,
            events_transparent: true,
            start_mode: StartMode::AllScreens,
            ..Default::default()
        })
        .run_with(new)
}
