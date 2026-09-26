//! Region-marking GUI: load a photo, filter it to line-art or 2-color,
//! mark animated regions on it (rectangle drag, hand-traced lasso
//! polygon, or magic-wand flood-fill), assign an effect + params to
//! each, save as a theme og-wallpaper can load.

use iced::widget::canvas::{self, Canvas, Frame, Path, Stroke};
use iced::widget::{button, column, container, pick_list, row, scrollable, slider, text, text_input};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Task, Theme as IcedTheme};
use std::sync::Arc;

use og_config::Config;
use og_theme::AppColors;
use og_wallpaper_core::filters;
use og_wallpaper_core::manifest::{Effect, Manifest, Region, RegionBounds, RegionShape};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterMode {
    LineArt,
    TwoColor,
    Raw,
}

impl FilterMode {
    const ALL: [FilterMode; 3] = [FilterMode::LineArt, FilterMode::TwoColor, FilterMode::Raw];
}

impl std::fmt::Display for FilterMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FilterMode::LineArt => "Line Art",
            FilterMode::TwoColor => "2-Color",
            FilterMode::Raw => "None (raw image)",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Rect,
    Lasso,
    Wand,
}

impl Tool {
    const ALL: [Tool; 3] = [Tool::Rect, Tool::Lasso, Tool::Wand];
}

impl std::fmt::Display for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tool::Rect => "Rectangle (drag)",
            Tool::Lasso => "Lasso (click points, close near start)",
            Tool::Wand => "Magic Wand (click, flood-fills similar color)",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectKind {
    Rain,
    Flash,
    Twinkle,
    Wave,
}

impl EffectKind {
    const ALL: [EffectKind; 4] = [EffectKind::Rain, EffectKind::Flash, EffectKind::Twinkle, EffectKind::Wave];

    /// (label for param A, label for param B) — the two generic sliders
    /// mean differently per effect (count vs rate vs amplitude), rather
    /// than a bespoke form per effect type.
    fn param_labels(&self) -> (&'static str, &'static str) {
        match self {
            EffectKind::Rain => ("Density", "Speed"),
            EffectKind::Flash => ("(unused)", "Rate"),
            EffectKind::Twinkle => ("Count", "Rate"),
            EffectKind::Wave => ("Amplitude", "Speed"),
        }
    }

    fn build(&self, a: f32, b: f32) -> Effect {
        match self {
            EffectKind::Rain => Effect::Rain { density: a.round().max(1.0) as u32, speed: b },
            EffectKind::Flash => Effect::Flash { rate: b },
            EffectKind::Twinkle => Effect::Twinkle { count: a.round().max(1.0) as u32, rate: b },
            EffectKind::Wave => Effect::Wave { amplitude: a, speed: b },
        }
    }
}

impl std::fmt::Display for EffectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EffectKind::Rain => "Rain",
            EffectKind::Flash => "Flash",
            EffectKind::Twinkle => "Twinkle",
            EffectKind::Wave => "Wave",
        })
    }
}

/// A Mask region's actual pixel data, held here (not written to disk)
/// until Save — the manifest only ever stores a *filename*, so the real
/// bytes need to live somewhere between "magic-wand click" and "Save
/// Theme".
struct RawMask {
    w: u32,
    h: u32,
    bytes: Arc<Vec<u8>>,
}

struct App {
    colors: AppColors,
    source: Option<og_wallpaper_core::image::DynamicImage>,
    filter_mode: FilterMode,
    threshold: u8,
    filtered: Option<og_wallpaper_core::image::RgbaImage>,
    filtered_handle: Option<iced::widget::image::Handle>,
    tool: Tool,
    wand_threshold: u8,
    regions: Vec<Region>,
    /// Parallel to `regions` — `Some` only for entries whose shape is a
    /// Mask, `None` otherwise. Kept separate from `Region` itself since
    /// `Region`/`Manifest` are the shared on-disk format (og-wallpaper
    /// reads them too) and have no business holding undsaved raw bytes.
    raw_masks: Vec<Option<RawMask>>,
    /// Full-image-sized (matches `filtered`'s dimensions), nonzero where
    /// any existing region already covers that pixel — rebuilt after
    /// every region add/delete. Magic Wand treats this as an extra flood
    /// boundary so a new selection can't creep into ground an earlier
    /// region already claimed.
    claimed: Vec<u8>,
    /// (line/foreground hex, background hex) actually baked into
    /// `filtered` by the last `apply_filter()` call — `None` for Raw
    /// mode. Saved into the manifest so og-wallpaper can remap these
    /// exact colors to whatever the *current* theme is at load time,
    /// instead of the artwork staying frozen at whatever was live here.
    baked_colors: Option<(String, String)>,
    selected_effect: EffectKind,
    param_a: f32,
    param_b: f32,
    color_override: String,
    theme_name: String,
    status: String,
}

#[derive(Debug, Clone)]
enum Message {
    OpenImage,
    FilterModeChanged(FilterMode),
    ThresholdChanged(u8),
    ApplyFilter,
    ToolChanged(Tool),
    WandThresholdChanged(u8),
    EffectKindChanged(EffectKind),
    ParamAChanged(f32),
    ParamBChanged(f32),
    ColorOverrideChanged(String),
    RegionRectDrawn(RegionBounds),
    RegionPolygonDrawn(Vec<(f32, f32)>),
    RegionMaskDrawn { bounds: RegionBounds, w: u32, h: u32, bytes: Arc<Vec<u8>> },
    DeleteRegion(usize),
    ThemeNameChanged(String),
    Save,
    /// No-op — iced's canvas only redraws when Program::update() emits a
    /// Message (see Editor::update below), so live previews (drag box,
    /// in-progress lasso line) need *something* published on every
    /// CursorMoved while active or the canvas just sits frozen.
    Redraw,
}

fn new() -> (App, Task<Message>) {
    let colors = AppColors::from_config(&Config::load(), "og-wallpaper-studio");
    (
        App {
            colors,
            source: None,
            filter_mode: FilterMode::LineArt,
            threshold: 60,
            filtered: None,
            filtered_handle: None,
            tool: Tool::Rect,
            wand_threshold: 24,
            regions: Vec::new(),
            raw_masks: Vec::new(),
            claimed: Vec::new(),
            baked_colors: None,
            selected_effect: EffectKind::Rain,
            param_a: 20.0,
            param_b: 0.5,
            color_override: String::new(),
            theme_name: String::new(),
            status: "Open an image to begin.".to_string(),
        },
        Task::none(),
    )
}

fn apply_filter(app: &mut App) {
    let Some(src) = &app.source else { return };
    let config = og_config::Config::load();
    let line = filters::hex_to_rgba(&config.accent, 255);
    let bg = filters::hex_to_rgba(&config.bar_bg, 255);
    let out = match app.filter_mode {
        FilterMode::LineArt => filters::line_art(src, line, bg, app.threshold as u32 * 4),
        FilterMode::TwoColor => filters::two_color(src, bg, line, app.threshold),
        FilterMode::Raw => src.to_rgba8(),
    };
    app.baked_colors = match app.filter_mode {
        FilterMode::LineArt | FilterMode::TwoColor => Some((config.accent.clone(), config.bar_bg.clone())),
        FilterMode::Raw => None,
    };
    let (w, h) = out.dimensions();
    app.filtered_handle = Some(iced::widget::image::Handle::from_rgba(w, h, out.clone().into_raw()));
    app.filtered = Some(out);
}

/// Rebuilds `app.claimed` from scratch as the union of every current
/// region's coverage — simplest correct approach given how rarely this
/// needs to happen (region add/delete, image (re)load) versus how often
/// it'd need bookkeeping if we tried to patch it incrementally instead.
fn rebuild_claimed(app: &mut App) {
    let Some(img) = &app.filtered else {
        app.claimed.clear();
        return;
    };
    let (w, h) = img.dimensions();
    let mut claimed = vec![0u8; (w * h) as usize];

    for (i, region) in app.regions.iter().enumerate() {
        match &region.shape {
            shape @ (RegionShape::Rect(_) | RegionShape::Polygon { .. }) => {
                let b = shape.bounds();
                let x0 = ((b.x * w as f32) as u32).min(w);
                let y0 = ((b.y * h as f32) as u32).min(h);
                let x1 = (((b.x + b.w) * w as f32).ceil() as u32).min(w);
                let y1 = (((b.y + b.h) * h as f32).ceil() as u32).min(h);
                for y in y0..y1 {
                    for x in x0..x1 {
                        if shape_contains_pixel(shape, x, y, w, h) {
                            claimed[(y * w + x) as usize] = 255;
                        }
                    }
                }
            }
            RegionShape::Mask { bounds, .. } => {
                let Some(Some(raw)) = app.raw_masks.get(i) else { continue };
                let x0 = (bounds.x * w as f32) as u32;
                let y0 = (bounds.y * h as f32) as u32;
                for my in 0..raw.h {
                    for mx in 0..raw.w {
                        if raw.bytes[(my * raw.w + mx) as usize] == 0 {
                            continue;
                        }
                        let (gx, gy) = (x0 + mx, y0 + my);
                        if gx < w && gy < h {
                            claimed[(gy * w + gx) as usize] = 255;
                        }
                    }
                }
            }
        }
    }
    app.claimed = claimed;
}

fn shape_contains_pixel(shape: &RegionShape, x: u32, y: u32, w: u32, h: u32) -> bool {
    match shape {
        RegionShape::Rect(b) => {
            let x0 = (b.x * w as f32) as u32;
            let y0 = (b.y * h as f32) as u32;
            let x1 = ((b.x + b.w) * w as f32) as u32;
            let y1 = ((b.y + b.h) * h as f32) as u32;
            x >= x0 && x < x1 && y >= y0 && y < y1
        }
        RegionShape::Polygon { points } => filters::point_in_polygon(x as f32 / w as f32, y as f32 / h as f32, points),
        // Masks (Wand) are already clipped at flood-fill time (see
        // magic_wand's `exclude` param) — this path isn't used for them.
        RegionShape::Mask { .. } => false,
    }
}

/// Clips a freshly-drawn Rect/Polygon against `app.claimed` — same intent
/// as Wand's `exclude` param, but Rect/Lasso aren't flood-fills, so this
/// clips after the fact instead of during. Three outcomes:
/// - No overlap at all: the shape comes back unchanged (stays a cheap
///   Rect/Polygon, no mask needed).
/// - Partial overlap: downgrades to a Mask of just the unclaimed part.
/// - Fully covered by existing regions: `None` (nothing left to add).
fn clip_new_shape(app: &App, shape: RegionShape) -> Option<(RegionShape, Option<RawMask>)> {
    let img = app.filtered.as_ref()?;
    let (w, h) = img.dimensions();
    if app.claimed.is_empty() {
        return Some((shape, None));
    }

    let b = shape.bounds();
    let x0 = ((b.x * w as f32) as u32).min(w);
    let y0 = ((b.y * h as f32) as u32).min(h);
    let x1 = (((b.x + b.w) * w as f32).ceil() as u32).min(w);
    let y1 = (((b.y + b.h) * h as f32).ceil() as u32).min(h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let (mw, mh) = (x1 - x0, y1 - y0);
    let mut mask = vec![0u8; (mw * mh) as usize];
    let (mut any_in, mut any_clipped, mut any_selected) = (false, false, false);
    for y in y0..y1 {
        for x in x0..x1 {
            if !shape_contains_pixel(&shape, x, y, w, h) {
                continue;
            }
            any_in = true;
            if app.claimed[(y * w + x) as usize] != 0 {
                any_clipped = true;
                continue;
            }
            mask[((y - y0) * mw + (x - x0)) as usize] = 255;
            any_selected = true;
        }
    }

    if !any_in || !any_selected {
        return None;
    }
    if !any_clipped {
        return Some((shape, None));
    }
    let bounds = RegionBounds { x: x0 as f32 / w as f32, y: y0 as f32 / h as f32, w: mw as f32 / w as f32, h: mh as f32 / h as f32 };
    Some((RegionShape::Mask { bounds, mask_file: String::new() }, Some(RawMask { w: mw, h: mh, bytes: Arc::new(mask) })))
}

fn current_color_override(app: &App) -> Option<String> {
    if app.color_override.trim().is_empty() {
        None
    } else {
        Some(app.color_override.trim().to_string())
    }
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::OpenImage => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", &["jpg", "jpeg", "png", "webp", "bmp"])
                .pick_file()
            {
                match og_wallpaper_core::image::open(&path) {
                    Ok(img) => {
                        app.source = Some(img);
                        app.regions.clear();
                        app.raw_masks.clear();
                        apply_filter(app);
                        rebuild_claimed(app);
                        app.status = format!("Loaded {}", path.display());
                    }
                    Err(e) => app.status = format!("Failed to open image: {e}"),
                }
            }
        }
        Message::FilterModeChanged(mode) => {
            app.filter_mode = mode;
            apply_filter(app);
        }
        Message::ThresholdChanged(v) => {
            app.threshold = v;
            apply_filter(app);
        }
        Message::ApplyFilter => apply_filter(app),
        Message::ToolChanged(tool) => app.tool = tool,
        Message::WandThresholdChanged(v) => app.wand_threshold = v,
        Message::EffectKindChanged(kind) => app.selected_effect = kind,
        Message::ParamAChanged(v) => app.param_a = v,
        Message::ParamBChanged(v) => app.param_b = v,
        Message::ColorOverrideChanged(v) => app.color_override = v,
        Message::RegionRectDrawn(bounds) => match clip_new_shape(app, RegionShape::Rect(bounds)) {
            Some((shape, raw_mask)) => {
                let color_override = current_color_override(app);
                app.regions.push(Region { shape, effect: app.selected_effect.build(app.param_a, app.param_b), color_override });
                app.raw_masks.push(raw_mask);
                rebuild_claimed(app);
                app.status = format!("{} region(s) marked.", app.regions.len());
            }
            None => app.status = "That area is already fully claimed by another region.".to_string(),
        },
        Message::RegionPolygonDrawn(points) => match clip_new_shape(app, RegionShape::Polygon { points }) {
            Some((shape, raw_mask)) => {
                let color_override = current_color_override(app);
                app.regions.push(Region { shape, effect: app.selected_effect.build(app.param_a, app.param_b), color_override });
                app.raw_masks.push(raw_mask);
                rebuild_claimed(app);
                app.status = format!("{} region(s) marked.", app.regions.len());
            }
            None => app.status = "That area is already fully claimed by another region.".to_string(),
        },
        Message::RegionMaskDrawn { bounds, w, h, bytes } => {
            let color_override = current_color_override(app);
            app.regions.push(Region {
                shape: RegionShape::Mask { bounds, mask_file: String::new() },
                effect: app.selected_effect.build(app.param_a, app.param_b),
                color_override,
            });
            app.raw_masks.push(Some(RawMask { w, h, bytes }));
            rebuild_claimed(app);
            app.status = format!("{} region(s) marked.", app.regions.len());
        }
        Message::DeleteRegion(i) => {
            if i < app.regions.len() {
                app.regions.remove(i);
                app.raw_masks.remove(i);
                rebuild_claimed(app);
            }
        }
        Message::ThemeNameChanged(v) => app.theme_name = v,
        Message::Save => {
            let name = app.theme_name.trim();
            if name.is_empty() {
                app.status = "Give the theme a name first.".to_string();
            } else if let Some(filtered) = &app.filtered {
                let dir = og_wallpaper_core::manifest::theme_dir(name);
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    app.status = format!("Failed to create theme dir: {e}");
                } else if let Err(e) = filtered.save(dir.join("base.png")) {
                    app.status = format!("Failed to save image: {e}");
                } else {
                    // Masks get their real filenames only now (save time)
                    // — assign & write them, then swap the placeholder
                    // empty mask_file in each Region.
                    let mut regions = app.regions.clone();
                    let mut save_err = None;
                    for (i, region) in regions.iter_mut().enumerate() {
                        if let RegionShape::Mask { mask_file, .. } = &mut region.shape {
                            if let Some(raw) = &app.raw_masks[i] {
                                let file_name = format!("region_{i}_mask.png");
                                let img = og_wallpaper_core::image::GrayImage::from_raw(raw.w, raw.h, (*raw.bytes).clone())
                                    .expect("mask buffer size matches its own w*h by construction");
                                if let Err(e) = img.save(dir.join(&file_name)) {
                                    save_err = Some(format!("Failed to save mask {file_name}: {e}"));
                                    break;
                                }
                                *mask_file = file_name;
                            }
                        }
                    }
                    if let Some(e) = save_err {
                        app.status = e;
                    } else {
                        let manifest = Manifest { regions: regions.clone(), base_colors: app.baked_colors.clone() };
                        match manifest.save(name) {
                            Ok(()) => {
                                app.regions = regions;
                                app.status = format!("Saved theme \"{name}\" ({} regions).", app.regions.len());
                            }
                            Err(e) => app.status = format!("Failed to save manifest: {e}"),
                        }
                    }
                }
            } else {
                app.status = "Nothing to save — open and filter an image first.".to_string();
            }
        }
        Message::Redraw => {}
    }
    Task::none()
}

fn view(app: &App) -> Element<'_, Message> {
    let colors = app.colors;
    let text_style = move |c: Color| move |_: &IcedTheme| iced::widget::text::Style { color: Some(c) };

    let mut top_bar = row![
        button(text("Open Image…")).on_press(Message::OpenImage),
        pick_list(FilterMode::ALL, Some(app.filter_mode), Message::FilterModeChanged),
        text(format!("Threshold: {}", app.threshold)).style(text_style(colors.text)),
        slider(1..=100, app.threshold, Message::ThresholdChanged).width(140),
        button(text("Re-apply")).on_press(Message::ApplyFilter),
        pick_list(Tool::ALL, Some(app.tool), Message::ToolChanged),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);
    if app.tool == Tool::Wand {
        top_bar = top_bar.push(text("Tolerance").style(text_style(colors.text)));
        top_bar = top_bar.push(slider(1..=100, app.wand_threshold, Message::WandThresholdChanged).width(120));
    }

    let canvas_area: Element<'_, Message> = Canvas::new(Editor {
        handle: app.filtered_handle.as_ref(),
        filtered: app.filtered.as_ref(),
        image_size: app.filtered.as_ref().map(|f| (f.width() as f32, f.height() as f32)),
        regions: &app.regions,
        colors,
        tool: app.tool,
        wand_threshold: app.wand_threshold,
        claimed: &app.claimed,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into();

    let effect_panel = column![
        text("New region effect").style(text_style(colors.text)),
        text("Set these first, then use the selected tool on the image — each region uses whatever's set here at the moment it's finished.")
            .size(11)
            .style(text_style(colors.dim_text)),
        pick_list(EffectKind::ALL, Some(app.selected_effect), Message::EffectKindChanged),
        text(app.selected_effect.param_labels().0).style(text_style(colors.dim_text)),
        slider(0.0..=100.0, app.param_a, Message::ParamAChanged),
        text(app.selected_effect.param_labels().1).style(text_style(colors.dim_text)),
        slider(0.0..=3.0, app.param_b, Message::ParamBChanged).step(0.05),
        text("Color override (hex, optional)").style(text_style(colors.dim_text)),
        text_input("system accent", &app.color_override).on_input(Message::ColorOverrideChanged),
        text(match app.tool {
            Tool::Rect => "Drag a box on the image.",
            Tool::Lasso => "Click to place points; click near the first point again (or the first point shown) to close the shape.",
            Tool::Wand => "Click a spot on the image — everything connected to it within the tolerance above gets selected.",
        })
        .size(11)
        .style(text_style(colors.dim_text)),
    ]
    .spacing(8)
    .padding(12)
    .width(260);

    let region_rows: Vec<Element<'_, Message>> = app
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let shape_label = match &r.shape {
                RegionShape::Rect(_) => "Rect",
                RegionShape::Polygon { .. } => "Polygon",
                RegionShape::Mask { .. } => "Mask",
            };
            row![
                text(format!("{i}: {shape_label} / {:?}", r.effect)).size(12).style(text_style(colors.dim_text)),
                button(text("×")).on_press(Message::DeleteRegion(i)),
            ]
            .spacing(8)
            .into()
        })
        .collect();
    let region_list = scrollable(column(region_rows).spacing(4)).height(Length::Fill);

    let side_panel = column![effect_panel, text("Regions").style(text_style(colors.text)), region_list]
        .spacing(12)
        .width(260)
        .padding(12);

    let save_bar = row![
        text_input("theme name", &app.theme_name).on_input(Message::ThemeNameChanged).width(200),
        button(text("Save Theme")).on_press(Message::Save),
        text(&app.status).size(12).style(text_style(colors.dim_text)),
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);

    container(
        column![
            top_bar,
            row![canvas_area, side_panel].height(Length::Fill),
            save_bar,
        ]
        .spacing(10)
        .padding(10),
    )
    .style(move |_| container::Style { background: Some(colors.bar_bg.into()), ..Default::default() })
    .into()
}

struct Editor<'a> {
    handle: Option<&'a iced::widget::image::Handle>,
    filtered: Option<&'a og_wallpaper_core::image::RgbaImage>,
    image_size: Option<(f32, f32)>,
    regions: &'a [Region],
    colors: AppColors,
    tool: Tool,
    wand_threshold: u8,
    claimed: &'a [u8],
}

impl Editor<'_> {
    /// Contain-fit (letterboxed), not cover-fit — an editor should show
    /// the whole image being marked up, unlike og-wallpaper's own
    /// cover-fit renderer which is fine cropping for a full-bleed
    /// wallpaper.
    fn image_rect(&self, bounds: Rectangle) -> Option<Rectangle> {
        let (iw, ih) = self.image_size?;
        let scale = (bounds.width / iw).min(bounds.height / ih);
        let w = iw * scale;
        let h = ih * scale;
        Some(Rectangle { x: (bounds.width - w) * 0.5, y: (bounds.height - h) * 0.5, width: w, height: h })
    }
}

/// Canvas-space point close enough to `start` to count as "closing the
/// lasso loop" when clicked.
const LASSO_CLOSE_RADIUS: f32 = 10.0;

#[derive(Default)]
struct EditState {
    /// Rect tool's in-progress drag start.
    drag_start: Option<Point>,
    /// Lasso tool's points so far, canvas-space, in progress.
    lasso_points: Vec<Point>,
}

impl canvas::Program<Message> for Editor<'_> {
    type State = EditState;

    fn update(
        &self,
        state: &mut EditState,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        let Some(pos) = cursor.position_in(bounds) else {
            return (canvas::event::Status::Ignored, None);
        };
        let Some(img_rect) = self.image_rect(bounds) else {
            return (canvas::event::Status::Ignored, None);
        };
        let to_fraction = |p: Point| -> (f32, f32) {
            (((p.x - img_rect.x) / img_rect.width).clamp(0.0, 1.0), ((p.y - img_rect.y) / img_rect.height).clamp(0.0, 1.0))
        };

        match (self.tool, event) {
            (Tool::Rect, canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))) => {
                state.drag_start = Some(pos);
                (canvas::event::Status::Captured, None)
            }
            (Tool::Rect, canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))) => {
                let Some(start) = state.drag_start.take() else {
                    return (canvas::event::Status::Ignored, None);
                };
                let (fx0, fy0) = to_fraction(Point::new(start.x.min(pos.x), start.y.min(pos.y)));
                let (fx1, fy1) = to_fraction(Point::new(start.x.max(pos.x), start.y.max(pos.y)));
                let bounds_frac = RegionBounds { x: fx0, y: fy0, w: fx1 - fx0, h: fy1 - fy0 };
                if bounds_frac.w > 0.005 && bounds_frac.h > 0.005 {
                    (canvas::event::Status::Captured, Some(Message::RegionRectDrawn(bounds_frac)))
                } else {
                    (canvas::event::Status::Captured, None)
                }
            }
            (Tool::Rect, canvas::Event::Mouse(mouse::Event::CursorMoved { .. })) if state.drag_start.is_some() => {
                (canvas::event::Status::Captured, Some(Message::Redraw))
            }

            (Tool::Lasso, canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))) => {
                if let Some(&first) = state.lasso_points.first() {
                    let d = ((pos.x - first.x).powi(2) + (pos.y - first.y).powi(2)).sqrt();
                    if state.lasso_points.len() >= 3 && d < LASSO_CLOSE_RADIUS {
                        let points: Vec<(f32, f32)> = state.lasso_points.drain(..).map(to_fraction).collect();
                        return (canvas::event::Status::Captured, Some(Message::RegionPolygonDrawn(points)));
                    }
                }
                state.lasso_points.push(pos);
                (canvas::event::Status::Captured, Some(Message::Redraw))
            }
            (Tool::Lasso, canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))) => {
                // Right-click cancels an in-progress lasso.
                state.lasso_points.clear();
                (canvas::event::Status::Captured, Some(Message::Redraw))
            }
            (Tool::Lasso, canvas::Event::Mouse(mouse::Event::CursorMoved { .. })) if !state.lasso_points.is_empty() => {
                (canvas::event::Status::Captured, Some(Message::Redraw))
            }

            (Tool::Wand, canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))) => {
                let Some(img) = self.filtered else {
                    return (canvas::event::Status::Captured, None);
                };
                let (fx, fy) = to_fraction(pos);
                let px = (fx * img.width() as f32) as u32;
                let py = (fy * img.height() as f32) as u32;
                let exclude = (!self.claimed.is_empty()).then_some(self.claimed);
                let (mask, mw, mh, bx, by) =
                    filters::magic_wand(img, px.min(img.width() - 1), py.min(img.height() - 1), self.wand_threshold, exclude);
                if mw == 0 || mh == 0 {
                    return (canvas::event::Status::Captured, None);
                }
                let bounds = RegionBounds {
                    x: bx as f32 / img.width() as f32,
                    y: by as f32 / img.height() as f32,
                    w: mw as f32 / img.width() as f32,
                    h: mh as f32 / img.height() as f32,
                };
                (
                    canvas::event::Status::Captured,
                    Some(Message::RegionMaskDrawn { bounds, w: mw, h: mh, bytes: Arc::new(mask) }),
                )
            }

            _ => (canvas::event::Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        state: &EditState,
        renderer: &Renderer,
        _theme: &IcedTheme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), self.colors.surface);

        if let (Some(handle), Some(img_rect)) = (self.handle, self.image_rect(bounds)) {
            frame.draw_image(img_rect, handle);

            for region in self.regions {
                let label = match &region.effect {
                    Effect::Rain { .. } => "Rain",
                    Effect::Flash { .. } => "Flash",
                    Effect::Twinkle { .. } => "Twinkle",
                    Effect::Wave { .. } => "Wave",
                };
                match &region.shape {
                    RegionShape::Rect(b) | RegionShape::Mask { bounds: b, .. } => {
                        let r = Rectangle {
                            x: img_rect.x + b.x * img_rect.width,
                            y: img_rect.y + b.y * img_rect.height,
                            width: b.w * img_rect.width,
                            height: b.h * img_rect.height,
                        };
                        draw_marked_rect(&mut frame, r, self.colors.accent, label);
                    }
                    RegionShape::Polygon { points } if points.len() >= 3 => {
                        let canvas_points: Vec<Point> = points
                            .iter()
                            .map(|&(fx, fy)| Point::new(img_rect.x + fx * img_rect.width, img_rect.y + fy * img_rect.height))
                            .collect();
                        draw_marked_polygon(&mut frame, &canvas_points, self.colors.accent, label);
                    }
                    RegionShape::Polygon { .. } => {}
                }
            }
        }

        if let Some(start) = state.drag_start {
            if let Some(pos) = cursor.position_in(bounds) {
                let r = Rectangle::new(
                    Point::new(start.x.min(pos.x), start.y.min(pos.y)),
                    Size::new((pos.x - start.x).abs(), (pos.y - start.y).abs()),
                );
                draw_marked_rect(&mut frame, r, Color::WHITE, "");
            }
        }

        if !state.lasso_points.is_empty() {
            let mut preview = state.lasso_points.clone();
            if let Some(pos) = cursor.position_in(bounds) {
                preview.push(pos);
            }
            let path = Path::new(|p| {
                p.move_to(preview[0]);
                for &pt in &preview[1..] {
                    p.line_to(pt);
                }
            });
            frame.stroke(&path, Stroke { style: Color::WHITE.into(), width: 2.0, ..Stroke::default() });
            for &pt in &state.lasso_points {
                frame.fill(&Path::circle(pt, 3.5), Color::WHITE);
            }
            // First point drawn larger — that's the click target that
            // closes the loop.
            if let Some(&first) = state.lasso_points.first() {
                frame.stroke(
                    &Path::circle(first, LASSO_CLOSE_RADIUS),
                    Stroke { style: self.colors.accent.into(), width: 1.5, ..Stroke::default() },
                );
            }
        }

        vec![frame.into_geometry()]
    }
}

/// A translucent fill (not just a thin outline — easy to lose against a
/// busy line-art image) plus a solid border and, for finished regions, a
/// small label naming the effect so the marked-up image is legible at a
/// glance instead of a wall of unlabeled boxes.
fn draw_marked_rect(frame: &mut Frame, r: Rectangle, color: Color, label: &str) {
    frame.fill_rectangle(Point::new(r.x, r.y), Size::new(r.width, r.height), Color { a: 0.18, ..color });
    frame.stroke(
        &Path::rectangle(Point::new(r.x, r.y), Size::new(r.width, r.height)),
        Stroke { style: color.into(), width: 2.0, ..Stroke::default() },
    );
    draw_region_label(frame, Point::new(r.x + 4.0, r.y + 2.0), color, label);
}

fn draw_marked_polygon(frame: &mut Frame, points: &[Point], color: Color, label: &str) {
    let path = Path::new(|p| {
        p.move_to(points[0]);
        for &pt in &points[1..] {
            p.line_to(pt);
        }
        p.close();
    });
    frame.fill(&path, Color { a: 0.18, ..color });
    frame.stroke(&path, Stroke { style: color.into(), width: 2.0, ..Stroke::default() });
    let top = points.iter().cloned().reduce(|a, b| if b.y < a.y { b } else { a }).unwrap_or(points[0]);
    draw_region_label(frame, Point::new(top.x + 4.0, top.y + 2.0), color, label);
}

fn draw_region_label(frame: &mut Frame, position: Point, color: Color, label: &str) {
    if label.is_empty() {
        return;
    }
    frame.fill_text(canvas::Text { content: label.to_string(), position, color, size: iced::Pixels(13.0), ..canvas::Text::default() });
}

fn main() -> iced::Result {
    iced::application("og-wallpaper-studio", update, view).run_with(new)
}
