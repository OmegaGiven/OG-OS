//! The video surface: an iced `shader` widget owning ONE wgpu texture per
//! player (keyed by player id in the shared `Storage`). `prepare` uploads the
//! shared frame with `queue.write_texture` only when its serial moved
//! (re-creating the texture on size changes) and reports the widget's
//! physical size back to the render thread; `render` draws a quad
//! letterboxed to the frame's aspect. While playing, the program asks for a
//! redraw every vsync, so frames reach the screen without any UI message.

use super::mpv::Shared;
use super::Msg;
use iced::advanced::Shell;
use iced::mouse;
use iced::widget::shader::{self, wgpu, Storage, Viewport};
use iced::{Point, Rectangle};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Textures kept for players not drawn this frame (other tabs).
const CACHE: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(350);
/// Minimum gap between pointer-motion messages (controls auto-hide).
const MOTION_EVERY: Duration = Duration::from_millis(250);

pub const WGSL: &str = r#"
// rect: quad in NDC (x0, y0 top, x1, y1 bottom), grown by 1 px per side when
// the edge outline is on; params: (quarter turns, -, inset x, inset y) where
// inset is that 1 px in quad units; edge: outline colour (premultiplied no)
struct U { rect: vec4<f32>, params: vec4<f32>, edge: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) t: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    var c = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0));
    let t = c[i];
    var o: VOut;
    o.pos = vec4<f32>(mix(u.rect.x, u.rect.z, t.x), mix(u.rect.y, u.rect.w, t.y), 0.0, 1.0);
    o.t = t;
    return o;
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    let inset = u.params.zw;
    let p = (v.t - inset) / (vec2<f32>(1.0, 1.0) - 2.0 * inset);
    // clockwise quarter turns of the picture
    let k = u32(u.params.x + 0.5);
    var uv = p;
    if k == 1u {
        uv = vec2<f32>(p.y, 1.0 - p.x);
    } else if k == 2u {
        uv = vec2<f32>(1.0 - p.x, 1.0 - p.y);
    } else if k == 3u {
        uv = vec2<f32>(1.0 - p.y, p.x);
    }
    let rgb = textureSampleLevel(tex, samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
    let outside = any(p < vec2<f32>(0.0)) || any(p > vec2<f32>(1.0));
    return select(vec4<f32>(rgb, 1.0), u.edge, outside);
}
"#;

/// `src` (w×h) fitted inside `dst` (x, y, w, h), centred.
pub fn letterbox(src: (u32, u32), dst: [f32; 4]) -> [f32; 4] {
    let [x, y, w, h] = dst;
    if src.0 == 0 || src.1 == 0 || w <= 0.0 || h <= 0.0 {
        return [x, y, 0.0, 0.0];
    }
    let s = (w / src.0 as f32).min(h / src.1 as f32);
    let (fw, fh) = (src.0 as f32 * s, src.1 as f32 * s);
    [x + (w - fw) / 2.0, y + (h - fh) / 2.0, fw, fh]
}

/// Pixel rect → NDC [x0, y0(top), x1, y1(bottom)] for a `target` surface.
pub fn to_ndc(r: [f32; 4], target: (u32, u32)) -> [f32; 4] {
    let (tw, th) = (target.0.max(1) as f32, target.1.max(1) as f32);
    [
        r[0] / tw * 2.0 - 1.0,
        1.0 - r[1] / th * 2.0,
        (r[0] + r[2]) / tw * 2.0 - 1.0,
        1.0 - (r[1] + r[3]) / th * 2.0,
    ]
}

/// The widget program (borrowing the viewer state for one view).
pub struct Screen {
    pub id: u64,
    pub shared: Arc<Shared>,
    pub playing: bool,
    /// clockwise display rotation: 0, 90, 180, 270
    pub rotate: u32,
    /// hairline outline colour at the picture edge (None = off)
    pub edge: Option<iced::Color>,
    /// emit `Msg::Motion` for pointer movement (fullscreen auto-hide)
    pub track_motion: bool,
}

#[derive(Default)]
pub struct Interaction {
    last_click: Option<(Instant, Point)>,
    drag: Option<Point>,
    last_motion: Option<Instant>,
    ctrl: bool,
}

impl shader::Program<Msg> for Screen {
    type State = Interaction;
    type Primitive = Primitive;

    fn update(
        &self,
        st: &mut Interaction,
        event: shader::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Msg>,
    ) -> (iced::event::Status, Option<Msg>) {
        use iced::keyboard;
        use iced::event::Status::{Captured, Ignored};
        match event {
            shader::Event::RedrawRequested(_) => {
                if self.playing {
                    shell.request_redraw(iced::window::RedrawRequest::NextFrame);
                }
                (Ignored, None)
            }
            shader::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                st.ctrl = m.control();
                (Ignored, None)
            }
            shader::Event::Mouse(ev) => match ev {
                mouse::Event::ButtonPressed(mouse::Button::Left) => {
                    let Some(p) = cursor.position_over(bounds) else {
                        return (Ignored, None);
                    };
                    let now = Instant::now();
                    if let Some((t, q)) = st.last_click {
                        if now.duration_since(t) < DOUBLE_CLICK && p.distance(q) < 6.0 {
                            st.last_click = None;
                            st.drag = None;
                            return (Captured, Some(Msg::ToggleFullscreen));
                        }
                    }
                    st.last_click = Some((now, p));
                    st.drag = Some(p);
                    (Captured, None)
                }
                mouse::Event::ButtonReleased(mouse::Button::Left) if st.drag.is_some() => {
                    st.drag = None;
                    (Captured, None)
                }
                mouse::Event::CursorMoved { position } => {
                    if let Some(last) = st.drag {
                        st.drag = Some(position);
                        let (dx, dy) = (
                            (position.x - last.x) / bounds.width.max(1.0),
                            (position.y - last.y) / bounds.height.max(1.0),
                        );
                        if dx != 0.0 || dy != 0.0 {
                            return (Captured, Some(Msg::Pan(dx as f64, dy as f64)));
                        }
                    }
                    if self.track_motion
                        && cursor.position_over(bounds).is_some()
                        && st.last_motion.is_none_or(|t| t.elapsed() >= MOTION_EVERY)
                    {
                        st.last_motion = Some(Instant::now());
                        return (Ignored, Some(Msg::Motion));
                    }
                    (Ignored, None)
                }
                mouse::Event::WheelScrolled { delta } => {
                    if cursor.position_over(bounds).is_none() {
                        return (Ignored, None);
                    }
                    let y = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => y,
                        mouse::ScrollDelta::Pixels { y, .. } => y / 40.0,
                    };
                    if y == 0.0 {
                        return (Ignored, None);
                    }
                    (Captured, Some(Msg::Wheel(y, st.ctrl)))
                }
                _ => (Ignored, None),
            },
            _ => (Ignored, None),
        }
    }

    fn draw(&self, _st: &Interaction, _cursor: mouse::Cursor, _bounds: Rectangle) -> Primitive {
        Primitive {
            id: self.id,
            shared: self.shared.clone(),
            rotate: self.rotate,
            edge: self.edge,
        }
    }
}

pub struct Primitive {
    id: u64,
    shared: Arc<Shared>,
    rotate: u32,
    edge: Option<iced::Color>,
}

impl std::fmt::Debug for Primitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoPrimitive").field("id", &self.id).finish()
    }
}

struct Tex {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    size: (u32, u32),
}

struct Entry {
    ub: wgpu::Buffer,
    tex: Option<Tex>,
    serial: u64,
    visible: bool,
    last_used: u64,
}

struct Pipeline {
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    entries: HashMap<u64, Entry>,
    frame: u64,
}

impl Pipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("video shader"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs_main",
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // only the outline is translucent; the picture is opaque
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("video sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Pipeline {
            format,
            pipeline,
            layout,
            sampler,
            entries: HashMap::new(),
            frame: 0,
        }
    }

    fn evict(&mut self) {
        while self.entries.len() > CACHE {
            let Some((&id, _)) = self.entries.iter().min_by_key(|(_, e)| e.last_used) else {
                break;
            };
            self.entries.remove(&id);
        }
    }
}

impl shader::Primitive for Primitive {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        storage: &mut Storage,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        if storage.get::<Pipeline>().map(|p| p.format) != Some(format) {
            storage.store(Pipeline::new(device, format));
        }
        let Some(p) = storage.get_mut::<Pipeline>() else {
            return;
        };
        p.frame += 1;
        let frame_no = p.frame;
        let s = viewport.scale_factor() as f32;
        let phys = [bounds.x * s, bounds.y * s, bounds.width * s, bounds.height * s];
        // mpv renders unrotated: on a quarter turn it gets the swapped box
        let quarter = self.rotate % 180 == 90;
        let (pw, ph) = (phys[2].round() as u32, phys[3].round() as u32);
        self.shared
            .set_widget_size(if quarter { (ph, pw) } else { (pw, ph) });

        if !p.entries.contains_key(&self.id) {
            let ub = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("video rect"),
                size: 48,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            p.entries.insert(
                self.id,
                Entry {
                    ub,
                    tex: None,
                    serial: 0,
                    visible: false,
                    last_used: frame_no,
                },
            );
            p.evict();
        }
        let tex_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let (layout, sampler) = (&p.layout, &p.sampler);
        let Some(e) = p.entries.get_mut(&self.id) else {
            return;
        };
        e.last_used = frame_no;

        let serial = self.shared.serial.load(Ordering::Acquire);
        if serial != e.serial {
            if let Ok(f) = self.shared.frame.lock() {
                if f.w > 0 && f.h > 0 {
                    if e.tex.as_ref().map(|t| t.size) != Some((f.w, f.h)) {
                        let texture = device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("video frame"),
                            size: wgpu::Extent3d {
                                width: f.w,
                                height: f.h,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: tex_format,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        });
                        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("video bind"),
                            layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: e.ub.as_entire_binding(),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::TextureView(&view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 2,
                                    resource: wgpu::BindingResource::Sampler(sampler),
                                },
                            ],
                        });
                        e.tex = Some(Tex {
                            texture,
                            bind,
                            size: (f.w, f.h),
                        });
                    }
                    if let Some(t) = &e.tex {
                        queue.write_texture(
                            wgpu::ImageCopyTexture {
                                texture: &t.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            &f.buf.bytes()[..f.stride * f.h as usize],
                            wgpu::ImageDataLayout {
                                offset: 0,
                                bytes_per_row: Some(f.stride as u32),
                                rows_per_image: Some(f.h),
                            },
                            wgpu::Extent3d {
                                width: f.w,
                                height: f.h,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }
                e.serial = serial;
            }
        }
        let Some(t) = &e.tex else {
            e.visible = false;
            return;
        };
        let shown = if quarter {
            (t.size.1, t.size.0)
        } else {
            t.size
        };
        let r = letterbox(shown, phys);
        e.visible = r[2] >= 1.0 && r[3] >= 1.0;
        let target = viewport.physical_size();
        // only the picture (plus the optional 1 px outline) is drawn: the
        // app background shows around it, never black bars
        let (quad, inset, edge) = match self.edge {
            Some(c) => {
                let g = [r[0] - 1.0, r[1] - 1.0, r[2] + 2.0, r[3] + 2.0];
                let lin = |x: f32| {
                    if format.is_srgb() {
                        x.powf(2.2)
                    } else {
                        x
                    }
                };
                (
                    g,
                    [1.0 / g[2].max(1.0), 1.0 / g[3].max(1.0)],
                    [lin(c.r), lin(c.g), lin(c.b), c.a],
                )
            }
            None => (r, [0.0, 0.0], [0.0; 4]),
        };
        let ndc = to_ndc(quad, (target.width, target.height));
        let u = [
            ndc[0],
            ndc[1],
            ndc[2],
            ndc[3],
            (self.rotate / 90 % 4) as f32,
            0.0,
            inset[0],
            inset[1],
            edge[0],
            edge[1],
            edge[2],
            edge[3],
        ];
        queue.write_buffer(&e.ub, 0, bytemuck::cast_slice(&u));
    }

    fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        storage: &Storage,
        target: &wgpu::TextureView,
        clip_bounds: &Rectangle<u32>,
    ) {
        let Some(p) = storage.get::<Pipeline>() else {
            return;
        };
        let Some(e) = p.entries.get(&self.id) else {
            return;
        };
        let (Some(t), true) = (&e.tex, e.visible) else {
            return;
        };
        if clip_bounds.width == 0 || clip_bounds.height == 0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("video"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_scissor_rect(
            clip_bounds.x,
            clip_bounds.y,
            clip_bounds.width,
            clip_bounds.height,
        );
        pass.set_pipeline(&p.pipeline);
        pass.set_bind_group(0, &t.bind, &[]);
        pass.draw(0..4, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_letterbox_math() {
        // 16:9 frame in a 4:3 box → bars top and bottom
        assert_eq!(
            letterbox((1600, 900), [0.0, 0.0, 800.0, 600.0]),
            [0.0, 75.0, 800.0, 450.0]
        );
        // 4:3 frame in a wide box, offset → bars left and right
        assert_eq!(
            letterbox((400, 300), [10.0, 20.0, 1000.0, 300.0]),
            [310.0, 20.0, 400.0, 300.0]
        );
        assert_eq!(letterbox((0, 10), [1.0, 2.0, 3.0, 4.0])[2], 0.0);
        assert_eq!(
            to_ndc([0.0, 0.0, 100.0, 50.0], (100, 50)),
            [-1.0, 1.0, 1.0, -1.0]
        );
        assert_eq!(
            to_ndc([25.0, 0.0, 50.0, 25.0], (100, 50)),
            [-0.5, 1.0, 0.5, 0.0]
        );
    }

    #[test]
    fn video_shader_wgsl_validates() {
        let module = naga::front::wgsl::parse_str(WGSL).expect("WGSL parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("WGSL validation");
        for entry in ["vs_main", "fs_main"] {
            assert!(module.entry_points.iter().any(|e| e.name == entry));
        }
    }
}
