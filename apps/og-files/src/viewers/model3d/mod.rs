//! 3D models: glTF/glb, OBJ, STL, PLY rendered with an iced `shader`
//! widget (wgpu): orbit/pan/zoom camera, shaded / wireframe / normals,
//! bounding-box dimensions and triangle count. Read-only.
//!
//! `load` parses and normalises on a blocking thread, `render` owns the
//! GPU pipeline + buffers, `camera` the orbit math. Without a wgpu backend
//! (tiny-skia fallback) the shader primitive is dropped by iced; the text
//! layered underneath the viewport then explains why only info is shown.

mod camera;
mod load;
mod render;

use super::Event;
use crate::theme;
use camera::Camera;
use iced::widget::shader;
use iced::widget::{button, column, container, row, stack, text, Space};
use iced::{mouse, Element, Length, Point, Rectangle, Task};
use load::{Model, Up};
use render::{Mode, Primitive};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(load::supported_ext)
        .is_some()
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    model: Arc<Model>,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    let p = path.clone();
    let model = tokio::task::spawn_blocking(move || load::load(&p))
        .await
        .map_err(|e| e.to_string())??;
    Ok(Opened {
        path,
        model: Arc::new(model),
    })
}

pub struct State {
    path: PathBuf,
    model: Arc<Model>,
    camera: Camera,
    up: Up,
    mode: Mode,
    grid: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Orbit(f32, f32),
    Pan(f32, f32, f32),
    Zoom(f32),
    Reset,
    SetMode(Mode),
    SetUp(Up),
    ToggleGrid,
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let up = o.model.default_up;
        let (lo, hi) = o.model.world_bounds(up);
        (
            Self {
                path: o.path,
                camera: Camera::fit(lo, hi),
                model: o.model,
                up,
                mode: Mode::Shaded,
                grid: true,
            },
            Task::none(),
        )
    }

    fn reset_camera(&mut self) {
        let (lo, hi) = self.model.world_bounds(self.up);
        self.camera = Camera::fit(lo, hi);
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let mut event = None;
        match msg {
            Msg::Orbit(dx, dy) => self.camera.orbit(dx, dy),
            Msg::Pan(x, y, z) => self.camera.target += glam::Vec3::new(x, y, z),
            Msg::Zoom(f) => self.camera.zoom_by(f),
            Msg::Reset => self.reset_camera(),
            Msg::SetMode(Mode::Wireframe) if self.model.lines.is_empty() => {
                event = Some(Event::Status("No wireframe for this model".into()));
            }
            Msg::SetMode(m) => self.mode = m,
            Msg::SetUp(up) => {
                if up != self.up {
                    self.up = up;
                    self.reset_camera();
                }
            }
            Msg::ToggleGrid => self.grid = !self.grid,
        }
        (Task::none(), event)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let fallback = container(
            text(
                "3D preview needs the GPU (wgpu) renderer, which is not available \
                 here. Model information is shown on the right.",
            )
            .color(theme::MUTED)
            .center(),
        )
        .padding(24)
        .center(Length::Fill);
        // later stack layers draw over earlier ones: with wgpu the shader's
        // opaque background covers the fallback text
        let viewport = stack![
            fallback,
            iced::widget::shader(Scene { state: self })
                .width(Length::Fill)
                .height(Length::Fill),
        ];
        row![
            container(viewport)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::panel)
                .padding(1),
            self.side_panel(),
        ]
        .spacing(8)
        .padding(8)
        .into()
    }

    fn side_panel(&self) -> Element<'_, Msg> {
        let m = &self.model;
        let size = m.max - m.min;
        let info = |k: &'static str, v: String| {
            row![
                text(k)
                    .size(13)
                    .color(theme::MUTED)
                    .width(Length::Fixed(86.0)),
                text(v).size(13),
            ]
            .spacing(6)
        };
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut tris = group(m.source_triangles);
        if m.decimated() {
            tris = format!("{tris}\n(showing {})", group(m.triangles));
        }
        let choice = |label: &'static str, active: bool, msg: Option<Msg>| {
            button(text(label).size(13))
                .on_press_maybe(msg)
                .style(if active {
                    button::primary
                } else {
                    button::secondary
                })
                .padding([4, 8])
        };
        let mode = |label, md: Mode| {
            let ok = md != Mode::Wireframe || !m.lines.is_empty();
            choice(label, self.mode == md, ok.then_some(Msg::SetMode(md)))
        };
        let mut notes = column![].spacing(4);
        if m.decimated() {
            notes = notes.push(
                text(format!(
                    "Large model: every {}th triangle is drawn.",
                    m.source_triangles.div_ceil(m.triangles.max(1))
                ))
                .size(12)
                .color(theme::MUTED),
            );
        }
        if m.computed_normals {
            notes = notes.push(text("Flat normals computed.").size(12).color(theme::MUTED));
        }
        let panel = column![
            text(name).size(15),
            info("Format", m.format.to_string()),
            info("Vertices", group(m.source_vertices)),
            info("Triangles", tris),
            info("Edges", group(m.edges())),
            info("Materials", m.materials.to_string()),
            info("Size X", num(size.x)),
            info("Size Y", num(size.y)),
            info("Size Z", num(size.z)),
            info("Min", vec3(m.min)),
            info("Max", vec3(m.max)),
            notes,
            Space::with_height(6),
            text("Display").size(13).color(theme::MUTED),
            row![
                mode("Shaded", Mode::Shaded),
                mode("Wire", Mode::Wireframe),
                mode("Normals", Mode::Normals),
            ]
            .spacing(4),
            text("Up axis").size(13).color(theme::MUTED),
            row![
                choice("Y up", self.up == Up::Y, Some(Msg::SetUp(Up::Y))),
                choice("Z up", self.up == Up::Z, Some(Msg::SetUp(Up::Z))),
                choice("Grid", self.grid, Some(Msg::ToggleGrid)),
            ]
            .spacing(4),
            button(text("Reset view").size(13))
                .on_press(Msg::Reset)
                .style(button::secondary)
                .padding([4, 8]),
            Space::with_height(Length::Fill),
            text(
                "Left drag: orbit\nRight/middle or Shift+drag: pan\n\
                 Wheel: zoom\nDouble-click: fit"
            )
            .size(12)
            .color(theme::MUTED),
        ]
        .spacing(6);
        container(panel)
            .padding(12)
            .width(Length::Fixed(250.0))
            .height(Length::Fill)
            .style(theme::panel)
            .into()
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        false
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        None
    }

    /// Ctrl+E: read-only viewer, nothing to toggle.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        Task::none()
    }

    /// Ctrl+S: read-only viewer, never dirty.
    pub fn save(&mut self) -> Task<Msg> {
        Task::none()
    }
}

/// 1234567 → "1 234 567".
fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('\u{2009}');
        }
        out.push(c);
    }
    out
}

/// Up to 4 significant digits, no exponent for everyday sizes.
fn num(v: f32) -> String {
    let a = v.abs();
    if a != 0.0 && !(1e-3..1e6).contains(&a) {
        return format!("{v:.3e}");
    }
    let decimals = if a >= 1000.0 {
        0
    } else {
        (3 - a.max(1e-3).log10().floor() as i32).clamp(0, 6) as usize
    };
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

fn vec3(v: glam::Vec3) -> String {
    format!("{}, {}, {}", num(v.x), num(v.y), num(v.z))
}

// ---------------------------------------------------------------- widget

/// The `shader::Program` for the viewport: turns mouse input into camera
/// messages and draws a `Primitive` snapshot of the current state.
struct Scene<'a> {
    state: &'a State,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    Orbit,
    Pan,
}

#[derive(Default)]
struct Interaction {
    drag: Option<(Drag, Point)>,
    last_click: Option<(Instant, Point)>,
    shift: bool,
}

const DOUBLE_CLICK: Duration = Duration::from_millis(350);

impl shader::Program<Msg> for Scene<'_> {
    type State = Interaction;
    type Primitive = Primitive;

    fn update(
        &self,
        st: &mut Interaction,
        event: shader::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        _shell: &mut iced::advanced::Shell<'_, Msg>,
    ) -> (iced::event::Status, Option<Msg>) {
        use iced::event::Status::{Captured, Ignored};
        use iced::keyboard;
        match event {
            shader::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                st.shift = m.shift();
                (Ignored, None)
            }
            shader::Event::Mouse(ev) => match ev {
                mouse::Event::ButtonPressed(b) => {
                    let Some(p) = cursor.position_over(bounds) else {
                        return (Ignored, None);
                    };
                    let kind = match b {
                        mouse::Button::Left if st.shift => Drag::Pan,
                        mouse::Button::Left => Drag::Orbit,
                        mouse::Button::Right | mouse::Button::Middle => Drag::Pan,
                        _ => return (Ignored, None),
                    };
                    if b == mouse::Button::Left {
                        let now = Instant::now();
                        if let Some((t, q)) = st.last_click {
                            if now.duration_since(t) < DOUBLE_CLICK && p.distance(q) < 5.0 {
                                st.last_click = None;
                                st.drag = None;
                                return (Captured, Some(Msg::Reset));
                            }
                        }
                        st.last_click = Some((now, p));
                    }
                    st.drag = Some((kind, p));
                    (Captured, None)
                }
                mouse::Event::ButtonReleased(_) if st.drag.is_some() => {
                    st.drag = None;
                    (Captured, None)
                }
                mouse::Event::CursorMoved { position } => {
                    let Some((kind, last)) = st.drag else {
                        return (Ignored, None);
                    };
                    st.drag = Some((kind, position));
                    let (dx, dy) = (position.x - last.x, position.y - last.y);
                    if dx == 0.0 && dy == 0.0 {
                        return (Captured, None);
                    }
                    let msg = match kind {
                        Drag::Orbit => Msg::Orbit(dx, dy),
                        Drag::Pan => {
                            let aspect = bounds.width / bounds.height.max(1.0);
                            let d = self.state.camera.pan_delta(dx, dy, bounds.height, aspect);
                            Msg::Pan(d.x, d.y, d.z)
                        }
                    };
                    (Captured, Some(msg))
                }
                mouse::Event::WheelScrolled { delta } => {
                    if cursor.position_over(bounds).is_none() {
                        return (Ignored, None);
                    }
                    let y = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => y,
                        mouse::ScrollDelta::Pixels { y, .. } => y / 40.0,
                    };
                    (Captured, Some(Msg::Zoom(0.88f32.powf(y))))
                }
                _ => (Ignored, None),
            },
            _ => (Ignored, None),
        }
    }

    fn draw(&self, _st: &Interaction, _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        let s = self.state;
        let t = theme::theme();
        let p = t.extended_palette();
        Primitive {
            model: s.model.clone(),
            camera: s.camera,
            up: s.up,
            mode: s.mode,
            grid: s.grid,
            bg_top: p.background.weak.color,
            bg_bottom: p.background.base.color,
            wire: p.primary.base.color,
            grid_color: theme::MUTED.scale_alpha(0.35),
        }
    }

    fn mouse_interaction(
        &self,
        st: &Interaction,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if st.drag.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(bounds) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests;
