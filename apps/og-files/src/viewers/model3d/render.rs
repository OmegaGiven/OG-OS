//! GPU side: an iced `shader::Primitive` that uploads a `Model` once
//! (cached by model id in the shared `Storage`) and draws it with its own
//! depth buffer into the widget's rectangle of the frame.

use super::camera::Camera;
use super::load::{srgb_to_linear, Model, Up, Vertex};
use glam::{Mat4, Vec3, Vec4};
use iced::widget::shader::{self, wgpu, Storage, Viewport};
use iced::{Color, Rectangle};
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// GPU meshes kept around for models not drawn this frame (other tabs).
const CACHE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shaded,
    Wireframe,
    Normals,
}

#[derive(Debug)]
pub struct Primitive {
    pub model: Arc<Model>,
    pub camera: Camera,
    pub up: Up,
    pub mode: Mode,
    pub grid: bool,
    pub bg_top: Color,
    pub bg_bottom: Color,
    pub wire: Color,
    pub grid_color: Color,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    eye: [f32; 4],
    line_color: [f32; 4],
    bg_top: [f32; 4],
    bg_bottom: [f32; 4],
    params: [f32; 4],
}

fn linear(c: Color) -> [f32; 4] {
    [
        srgb_to_linear(c.r),
        srgb_to_linear(c.g),
        srgb_to_linear(c.b),
        c.a,
    ]
}

/// Maps NDC of the full widget rect `full` onto NDC of the (clipped)
/// viewport rect `clip` (both `[x, y, w, h]` in physical pixels), so a
/// partly off-screen widget still renders undistorted.
pub fn clip_correction(full: [f32; 4], clip: [f32; 4]) -> Mat4 {
    let [fx, fy, fw, fh] = full;
    let [cx, cy, cw, ch] = clip;
    let sx = fw / cw;
    let sy = fh / ch;
    let tx = (2.0 * (fx - cx) + fw) / cw - 1.0;
    let ty = 1.0 - (2.0 * (fy - cy) + fh) / ch;
    Mat4::from_cols(
        Vec4::new(sx, 0.0, 0.0, 0.0),
        Vec4::new(0.0, sy, 0.0, 0.0),
        Vec4::Z,
        Vec4::new(tx, ty, 0.0, 1.0),
    )
}

/// Ground grid (line list) under the world-space box `min..max`.
pub fn grid_lines(min: Vec3, max: Vec3) -> Vec<Vertex> {
    let size = max - min;
    let extent = size.x.max(size.z).max(size.y * 0.5).max(1e-6) * 1.6;
    let step = 10f32.powf((extent / 10.0).log10().floor());
    let half = ((extent * 0.5 / step).ceil() as i32).clamp(1, 60);
    let cx = ((min.x + max.x) * 0.5 / step).round() * step;
    let cz = ((min.z + max.z) * 0.5 / step).round() * step;
    let y = min.y;
    let r = half as f32 * step;
    let v = |x: f32, z: f32| Vertex {
        pos: [x, y, z],
        normal: [0.0, 1.0, 0.0],
        color: [1.0; 3],
    };
    let mut out = Vec::with_capacity((half as usize * 2 + 1) * 4);
    for i in -half..=half {
        let o = i as f32 * step;
        out.extend([
            v(cx + o, cz - r),
            v(cx + o, cz + r),
            v(cx - r, cz + o),
            v(cx + r, cz + o),
        ]);
    }
    out
}

struct Gpu {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    lines: Option<(wgpu::Buffer, u32)>,
    grids: [(wgpu::Buffer, u32); 2],
    main_ub: wgpu::Buffer,
    main_bind: wgpu::BindGroup,
    grid_ub: wgpu::Buffer,
    grid_bind: wgpu::BindGroup,
    /// Viewport rect for this frame; None = nothing visible / too big.
    viewport: Option<[f32; 4]>,
    drawable: bool,
    last_used: u64,
}

struct Pipeline {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    mesh: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    bg: wgpu::RenderPipeline,
    depth: Option<(wgpu::TextureView, u32, u32)>,
    meshes: HashMap<u64, Gpu>,
    frame: u64,
}

impl Pipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("og-files model3d"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model3d uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("model3d"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        const ATTRS: [wgpu::VertexAttribute; 3] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 24,
                shader_location: 2,
            },
        ];
        let vlayout = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }];
        let make = |label: &str,
                    topology: wgpu::PrimitiveTopology,
                    write: bool,
                    compare: wgpu::CompareFunction,
                    blend: wgpu::BlendState,
                    entry: (&str, &str),
                    buffers: &[wgpu::VertexBufferLayout]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: entry.0,
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: write,
                    depth_compare: compare,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: entry.1,
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
            })
        };
        use wgpu::{BlendState as B, CompareFunction as C, PrimitiveTopology as T};
        let main = ("vs_main", "fs_main");
        Self {
            format,
            mesh: make(
                "model3d mesh",
                T::TriangleList,
                true,
                C::Less,
                B::REPLACE,
                main,
                &vlayout,
            ),
            lines: make(
                "model3d wire",
                T::LineList,
                true,
                C::Less,
                B::REPLACE,
                main,
                &vlayout,
            ),
            grid: make(
                "model3d grid",
                T::LineList,
                false,
                C::Less,
                B::ALPHA_BLENDING,
                main,
                &vlayout,
            ),
            bg: make(
                "model3d bg",
                T::TriangleList,
                false,
                C::Always,
                B::REPLACE,
                ("vs_bg", "fs_bg"),
                &[],
            ),
            layout,
            depth: None,
            meshes: HashMap::new(),
            frame: 0,
        }
    }

    fn ensure_depth(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let (w, h) = (w.max(1), h.max(1));
        if matches!(self.depth, Some((_, dw, dh)) if dw == w && dh == h) {
            return;
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model3d depth"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.depth = Some((tex.create_view(&Default::default()), w, h));
    }

    fn upload(&self, device: &wgpu::Device, m: &Model) -> Gpu {
        let max = device.limits().max_buffer_size;
        let vbytes: &[u8] = bytemuck::cast_slice(&m.vertices);
        let drawable = (vbytes.len() as u64) <= max && (m.indices.len() as u64 * 4) <= max;
        let buf = |label: &str, data: &[u8], usage: wgpu::BufferUsages| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: if drawable { data } else { &[0u8; 64] },
                usage,
            })
        };
        let vertices = buf("model3d vertices", vbytes, wgpu::BufferUsages::VERTEX);
        let indices = buf(
            "model3d indices",
            bytemuck::cast_slice(&m.indices),
            wgpu::BufferUsages::INDEX,
        );
        let lines = (drawable && !m.lines.is_empty()).then(|| {
            let b = buf(
                "model3d edges",
                bytemuck::cast_slice(&m.lines),
                wgpu::BufferUsages::INDEX,
            );
            (b, m.lines.len() as u32)
        });
        let grids = [Up::Y, Up::Z].map(|up| {
            let (lo, hi) = m.world_bounds(up);
            let g = grid_lines(lo, hi);
            let b = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("model3d grid"),
                contents: bytemuck::cast_slice(&g),
                usage: wgpu::BufferUsages::VERTEX,
            });
            (b, g.len() as u32)
        });
        let ub = || {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("model3d uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let bind = |b: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("model3d uniforms"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: b.as_entire_binding(),
                }],
            })
        };
        let (main_ub, grid_ub) = (ub(), ub());
        Gpu {
            main_bind: bind(&main_ub),
            grid_bind: bind(&grid_ub),
            main_ub,
            grid_ub,
            vertices,
            indices,
            index_count: m.indices.len() as u32,
            lines,
            grids,
            viewport: None,
            drawable,
            last_used: 0,
        }
    }

    fn evict(&mut self) {
        while self.meshes.len() > CACHE {
            let Some((&id, _)) = self.meshes.iter().min_by_key(|(_, g)| g.last_used) else {
                break;
            };
            self.meshes.remove(&id);
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
        let frame = p.frame;
        let target = viewport.physical_size();
        p.ensure_depth(device, target.width, target.height);
        if !p.meshes.contains_key(&self.model.id) {
            let mut g = p.upload(device, &self.model);
            g.last_used = frame;
            p.meshes.insert(self.model.id, g);
            p.evict();
        }
        let Some(g) = p.meshes.get_mut(&self.model.id) else {
            return;
        };
        g.last_used = frame;

        let s = viewport.scale_factor() as f32;
        let full = [
            bounds.x * s,
            bounds.y * s,
            bounds.width * s,
            bounds.height * s,
        ];
        let x0 = full[0].max(0.0);
        let y0 = full[1].max(0.0);
        let x1 = (full[0] + full[2]).min(target.width as f32);
        let y1 = (full[1] + full[3]).min(target.height as f32);
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            g.viewport = None;
            return;
        }
        let clip = [x0, y0, x1 - x0, y1 - y0];
        g.viewport = Some(clip);

        let aspect = full[2] / full[3].max(1.0);
        let view_proj = clip_correction(full, clip) * self.camera.view_proj(aspect);
        let mode = match self.mode {
            Mode::Shaded => 0.0,
            Mode::Normals => 1.0,
            Mode::Wireframe => 3.0,
        };
        let encode = if format.is_srgb() { 0.0 } else { 1.0 };
        let mut u = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
            model: self.up.matrix().to_cols_array_2d(),
            eye: self.camera.eye(aspect).extend(1.0).to_array(),
            line_color: linear(self.wire),
            bg_top: linear(self.bg_top),
            bg_bottom: linear(self.bg_bottom),
            params: [mode, encode, 0.0, 0.0],
        };
        queue.write_buffer(&g.main_ub, 0, bytemuck::bytes_of(&u));
        u.model = Mat4::IDENTITY.to_cols_array_2d();
        u.line_color = linear(self.grid_color);
        u.params[0] = 3.0;
        queue.write_buffer(&g.grid_ub, 0, bytemuck::bytes_of(&u));
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
        let (Some(g), Some((depth, ..))) = (p.meshes.get(&self.model.id), &p.depth) else {
            return;
        };
        let Some([x, y, w, h]) = g.viewport else {
            return;
        };
        if clip_bounds.width == 0 || clip_bounds.height == 0 {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("model3d"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.set_scissor_rect(
            clip_bounds.x,
            clip_bounds.y,
            clip_bounds.width,
            clip_bounds.height,
        );

        pass.set_bind_group(0, &g.main_bind, &[]);
        pass.set_pipeline(&p.bg);
        pass.draw(0..3, 0..1);

        if g.drawable {
            pass.set_vertex_buffer(0, g.vertices.slice(..));
            match (self.mode, &g.lines) {
                (Mode::Wireframe, Some((lines, n))) => {
                    pass.set_pipeline(&p.lines);
                    pass.set_index_buffer(lines.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..*n, 0, 0..1);
                }
                _ => {
                    pass.set_pipeline(&p.mesh);
                    pass.set_index_buffer(g.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..g.index_count, 0, 0..1);
                }
            }
        }

        if self.grid {
            let (buf, n) = &g.grids[usize::from(self.up == Up::Z)];
            pass.set_pipeline(&p.grid);
            pass.set_bind_group(0, &g.grid_bind, &[]);
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..*n, 0..1);
        }
    }
}
