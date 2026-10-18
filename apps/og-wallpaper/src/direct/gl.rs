//! EGL + OpenGL ES 3.0 plumbing for procedural scenes (circuit, tokyo, ...).
//!
//! A scene owns a few GL programs and some per-output-size textures
//! generated once on the CPU. Each frame it sets a small `uniform vec4
//! u[U_LEN]` array and issues a handful of draws (a fullscreen triangle,
//! instanced sprites). GLES 3.0 is the floor (integer textures, instancing):
//! Raspberry Pi 4/5, Mali/Panfrost, every desktop GPU.

use std::ffi::c_void;

use glow::HasContext;
use khronos_egl as egl;

/// Size of every program's `uniform vec4 u[...]` array.
pub const U_LEN: usize = 8;

pub type Egl = egl::Instance<egl::Static>;
pub type Uniforms = [[f32; 4]; U_LEN];

/// Fullscreen triangle.
pub const FULLSCREEN_VERT: &str = "
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

/// Shared by vertex and fragment shaders: precision, the uniform array and
/// integer hashes (exact and identical on every GPU, unlike fract(sin(x)*k),
/// whose low bits differ between drivers and fall apart at mediump).
const COMMON: &str = "#version 300 es
precision highp float;
precision highp int;
uniform vec4 u[8];

uint hu(uint x) {
    x ^= x >> 16; x *= 0x7feb352du;
    x ^= x >> 15; x *= 0x846ca68bu;
    x ^= x >> 16;
    return x;
}
float unorm(uint x) { return float(x >> 8) * (1.0 / 16777216.0); }
uint hcell(vec2 p) { ivec2 i = ivec2(floor(p)); return hu(uint(i.x) ^ hu(uint(i.y) + 0x9e3779b9u)); }
float h1(float x, uint salt) { return unorm(hu(uint(int(floor(x))) ^ hu(salt))); }
float h2(vec2 p, uint salt) { return unorm(hu(hcell(p) ^ salt)); }
vec2 h22(vec2 p, uint salt) { uint a = hu(hcell(p) ^ salt); return vec2(unorm(a), unorm(hu(a))); }

// Screen pixel (top-left origin) -> clip space.
vec4 clip(vec2 px) { return vec4(px.x / u[0].x * 2.0 - 1.0, 1.0 - px.y / u[0].y * 2.0, 0.0, 1.0); }
";

/// Fragment-only prelude: `frag` helpers and `finish()`, which applies the
/// sRGB encode when the surface can't do it in hardware (u[0].w = 1).
const FRAG_PRELUDE: &str = "
precision highp usampler2D;
precision mediump sampler2D;
out vec4 o_color;

vec3 to_srgb(vec3 c) {
    c = clamp(c, 0.0, 1.0);
    return mix(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, vec3(lessThanEqual(c, vec3(0.0031308))));
}
void finish(vec3 col) {
    o_color = vec4(u[0].w > 0.5 ? to_srgb(col) : col, 1.0);
}
";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TexFormat {
    /// One unsigned-normalized byte.
    R8,
    /// Packed integer data (`usampler2D`).
    R32Ui,
    /// sRGB-encoded RGBA8, decoded to linear when sampled.
    Srgba8,
}

impl TexFormat {
    fn gl(self) -> (u32, u32, u32) {
        match self {
            TexFormat::R8 => (glow::R8, glow::RED, glow::UNSIGNED_BYTE),
            TexFormat::R32Ui => (glow::R32UI, glow::RED_INTEGER, glow::UNSIGNED_INT),
            TexFormat::Srgba8 => (glow::SRGB8_ALPHA8, glow::RGBA, glow::UNSIGNED_BYTE),
        }
    }
}

/// One texture a scene generated, uploaded once and never touched again.
pub struct Tex {
    pub w: u32,
    pub h: u32,
    pub format: TexFormat,
    pub bytes: Vec<u8>,
}

/// What a scene generated for one output size.
pub struct Generated<I> {
    pub textures: Vec<Tex>,
    /// Layout facts the scene needs again when drawing.
    pub info: I,
}

/// A procedural scene. `Clone` hands a copy of its settings to the worker
/// thread that generates boards.
pub trait Scene: Clone + Send + 'static {
    /// Per-output animation state (e.g. circuit's eye).
    type Out: Default;
    type Info: Send + 'static;
    /// The scene's programs, built once with the context current.
    type Gl;

    fn init_gl(r: &Renderer) -> Result<Self::Gl, String>;
    fn generate(&self, w: u32, h: u32) -> Generated<Self::Info>;
    /// Once per batch of frames, before any `draw`.
    fn tick(&mut self) {}
    /// Draw one frame into `f.target` (viewport already set).
    fn draw(&self, r: &Renderer, g: &Self::Gl, board: &mut Board<Self::Info>, out: &mut Self::Out, f: &Frame);
    /// Shown until the first board is ready (linear RGB).
    fn clear_color(&self) -> [f32; 3];
}

/// Per-frame facts handed to `Scene::draw`.
pub struct Frame {
    /// Buffer size in pixels.
    pub w: f32,
    pub h: f32,
    /// Seconds since start, wrapped hourly (f32 precision).
    pub t: f32,
    /// Pointer in buffer pixels, if it's over this output's desktop.
    pub mouse: Option<(f32, f32)>,
    /// The shader must sRGB-encode (no sRGB surface).
    pub encode: bool,
    /// Framebuffer to draw the frame into (None = the window surface).
    pub target: Option<glow::Framebuffer>,
}

impl Frame {
    /// u[0] as every shader expects it: w, h, t, encode.
    pub fn u0(&self) -> [f32; 4] {
        [self.w, self.h, self.t, if self.encode { 1.0 } else { 0.0 }]
    }
}

/// An offscreen render target a scene keeps per board (e.g. a cache).
pub struct Target {
    pub tex: glow::Texture,
    pub fb: glow::Framebuffer,
    pub w: u32,
    pub h: u32,
}

/// A generated board living on the GPU.
pub struct Board<I> {
    pub textures: Vec<glow::Texture>,
    pub info: I,
    /// Scene-owned offscreen targets, created on first draw.
    pub targets: Vec<Target>,
    /// Scene-owned timestamp (e.g. when a cache was last refreshed).
    pub stamp: Option<f32>,
}

pub struct Program {
    prog: glow::Program,
    u_loc: Option<glow::UniformLocation>,
}

pub struct Renderer {
    pub gl: glow::Context,
    vao: glow::VertexArray,
}

impl Renderer {
    /// The EGL context must be current.
    pub fn new(gl: glow::Context) -> Result<Self, String> {
        unsafe {
            let vao = gl.create_vertex_array()?;
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::DITHER);
            Ok(Self { gl, vao })
        }
    }

    /// Compile a program. `vert`/`frag` are bodies appended to the preludes;
    /// samplers are bound to texture units in order.
    pub fn program(&self, vert: &str, frag: &str, samplers: &[&str]) -> Result<Program, String> {
        let gl = &self.gl;
        unsafe {
            let prog = gl.create_program()?;
            let vs = format!("{COMMON}{vert}");
            let fs = format!("{COMMON}{FRAG_PRELUDE}{frag}");
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, vs.as_str()), (glow::FRAGMENT_SHADER, fs.as_str())] {
                let sh = gl.create_shader(kind)?;
                gl.shader_source(sh, src);
                gl.compile_shader(sh);
                if !gl.get_shader_compile_status(sh) {
                    return Err(format!("shader compile: {}", gl.get_shader_info_log(sh)));
                }
                gl.attach_shader(prog, sh);
                shaders.push(sh);
            }
            gl.link_program(prog);
            if !gl.get_program_link_status(prog) {
                return Err(format!("shader link: {}", gl.get_program_info_log(prog)));
            }
            for sh in shaders {
                gl.detach_shader(prog, sh);
                gl.delete_shader(sh);
            }
            gl.use_program(Some(prog));
            for (i, name) in samplers.iter().enumerate() {
                if let Some(loc) = gl.get_uniform_location(prog, name) {
                    gl.uniform_1_i32(Some(&loc), i as i32);
                }
            }
            let u_loc = gl.get_uniform_location(prog, "u");
            Ok(Program { prog, u_loc })
        }
    }

    /// Bind a program, its uniforms and textures (unit i = texture i).
    pub fn use_program(&self, p: &Program, u: &Uniforms, textures: &[glow::Texture]) {
        let gl = &self.gl;
        unsafe {
            gl.use_program(Some(p.prog));
            gl.bind_vertex_array(Some(self.vao));
            for (i, t) in textures.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, Some(*t));
            }
            if let Some(loc) = &p.u_loc {
                gl.uniform_4_f32_slice(Some(loc), bytemuck::cast_slice(u));
            }
        }
    }

    pub fn fullscreen(&self) {
        unsafe { self.gl.draw_arrays(glow::TRIANGLES, 0, 3) };
    }

    /// `count` instanced quads (triangle strip, gl_VertexID 0..3).
    pub fn quads(&self, count: i32) {
        if count > 0 {
            unsafe { self.gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, count) };
        }
    }

    /// No blending / additive / straight alpha.
    pub fn blend(&self, mode: Blend) {
        let gl = &self.gl;
        unsafe {
            match mode {
                Blend::Off => gl.disable(glow::BLEND),
                Blend::Add => {
                    gl.enable(glow::BLEND);
                    gl.blend_func(glow::ONE, glow::ONE);
                }
                Blend::Alpha => {
                    gl.enable(glow::BLEND);
                    gl.blend_func_separate(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA, glow::ZERO, glow::ONE);
                }
            }
        }
    }

    pub fn bind_target(&self, fb: Option<glow::Framebuffer>, w: u32, h: u32) {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, fb);
            self.gl.viewport(0, 0, w as i32, h as i32);
        }
    }

    /// An sRGB RGBA8 render target.
    pub fn target(&self, w: u32, h: u32) -> Target {
        let gl = &self.gl;
        unsafe {
            let tex = gl.create_texture().expect("create texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_storage_2d(glow::TEXTURE_2D, 1, glow::SRGB8_ALPHA8, w as i32, h as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
            let fb = gl.create_framebuffer().expect("create framebuffer");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            Target { tex, fb, w, h }
        }
    }

    /// Upload a generated board (consuming the CPU copies).
    pub fn upload<I>(&self, generated: Generated<I>) -> Board<I> {
        let gl = &self.gl;
        let mut textures = Vec::new();
        unsafe {
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for t in generated.textures {
                let tex = gl.create_texture().expect("create texture");
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                let (internal, format, ty) = t.format.gl();
                // Immutable storage, one level: drivers can skip mip
                // bookkeeping and integer textures must not filter anyway.
                gl.tex_storage_2d(glow::TEXTURE_2D, 1, internal, t.w as i32, t.h as i32);
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    t.w as i32,
                    t.h as i32,
                    format,
                    ty,
                    glow::PixelUnpackData::Slice(&t.bytes),
                );
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
                textures.push(tex);
            }
        }
        trim_heap();
        Board { textures, info: generated.info, targets: Vec::new(), stamp: None }
    }

    pub fn free<I>(&self, board: Board<I>) {
        unsafe {
            for t in board.textures {
                self.gl.delete_texture(t);
            }
            for t in board.targets {
                self.gl.delete_framebuffer(t.fb);
                self.gl.delete_texture(t.tex);
            }
        }
    }

    pub fn clear(&self, rgb: [f32; 3], encode: bool) {
        let c = if encode { rgb.map(srgb_encode) } else { rgb };
        unsafe {
            self.gl.clear_color(c[0], c[1], c[2], 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }
}

#[derive(Clone, Copy)]
pub enum Blend {
    Off,
    Add,
    Alpha,
}

pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

pub fn srgb_encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Give freed generation buffers back to the OS (glibc keeps them otherwise).
pub fn trim_heap() {
    extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    // SAFETY: glibc's malloc_trim only releases free heap memory.
    unsafe {
        malloc_trim(0);
    }
}

/// EGL display + one GLES 3 context shared by every output's surface.
pub struct Context {
    pub egl: Egl,
    pub display: egl::Display,
    pub config: egl::Config,
    pub context: egl::Context,
    /// EGL_KHR_gl_colorspace: surfaces encode sRGB in hardware.
    pub srgb_surfaces: bool,
}

impl Context {
    /// `native` is a `wl_display*`, or None for a surfaceless (headless) display.
    pub fn new(native: Option<*mut c_void>) -> Result<Self, String> {
        let egl = egl::Instance::new(egl::Static);
        const PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;
        const PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;
        let display = unsafe {
            match native {
                Some(d) => egl.get_platform_display(PLATFORM_WAYLAND_KHR, d, &[egl::ATTRIB_NONE]),
                None => egl.get_platform_display(PLATFORM_SURFACELESS_MESA, egl::DEFAULT_DISPLAY, &[egl::ATTRIB_NONE]),
            }
        }
        .map_err(|e| format!("eglGetPlatformDisplay: {e}"))?;
        egl.initialize(display).map_err(|e| format!("eglInitialize: {e}"))?;
        egl.bind_api(egl::OPENGL_ES_API).map_err(|e| format!("eglBindAPI: {e}"))?;
        let exts = egl.query_string(Some(display), egl::EXTENSIONS).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let surface_type = if native.is_some() { egl::WINDOW_BIT } else { egl::PBUFFER_BIT };
        // No alpha: the buffer is XRGB, so sway knows it's opaque and never
        // blends anything under it.
        let attrs = [
            egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8, egl::ALPHA_SIZE, 0,
            egl::DEPTH_SIZE, 0, egl::STENCIL_SIZE, 0,
            egl::SURFACE_TYPE, surface_type,
            egl::RENDERABLE_TYPE, egl::OPENGL_ES3_BIT,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &attrs)
            .map_err(|e| format!("eglChooseConfig: {e}"))?
            .ok_or("no RGB888 GLES3 EGL config")?;
        let context = egl
            .create_context(display, config, None, &[egl::CONTEXT_MAJOR_VERSION, 3, egl::CONTEXT_MINOR_VERSION, 0, egl::NONE])
            .map_err(|e| format!("eglCreateContext (GLES 3.0): {e}"))?;
        let srgb_surfaces = exts.split(' ').any(|e| e == "EGL_KHR_gl_colorspace");
        Ok(Self { egl, display, config, context, srgb_surfaces })
    }

    /// GL function loader for the current context.
    pub fn glow(&self) -> glow::Context {
        unsafe {
            glow::Context::from_loader_function(|s| {
                self.egl.get_proc_address(s).map_or(std::ptr::null(), |p| p as *const c_void)
            })
        }
    }

    /// Window surface on a `wl_egl_window*`. Returns the surface and whether
    /// the shader must sRGB-encode itself.
    pub fn window_surface(&self, window: *mut c_void) -> Result<(egl::Surface, bool), String> {
        const GL_COLORSPACE_KHR: egl::Int = 0x309D;
        const GL_COLORSPACE_SRGB_KHR: egl::Int = 0x3089;
        if self.srgb_surfaces {
            let attrs = [GL_COLORSPACE_KHR, GL_COLORSPACE_SRGB_KHR, egl::NONE];
            if let Ok(s) = unsafe { self.egl.create_window_surface(self.display, self.config, window, Some(&attrs)) } {
                return Ok((s, false));
            }
        }
        let s = unsafe { self.egl.create_window_surface(self.display, self.config, window, None) }
            .map_err(|e| format!("eglCreateWindowSurface: {e}"))?;
        Ok((s, true))
    }
}

#[cfg(test)]
pub mod preview {
    //! Headless rendering for scene previews (Mesa surfaceless EGL).
    use super::*;
    use og_wallpaper_core::image::RgbaImage;

    pub struct Gpu<S: Scene> {
        ctx: Context,
        r: Renderer,
        g: S::Gl,
        scene: S,
    }

    impl<S: Scene> Gpu<S> {
        pub fn new(scene: S) -> Self {
            let ctx = Context::new(None).expect("surfaceless EGL");
            ctx.egl.make_current(ctx.display, None, None, Some(ctx.context)).expect("make current");
            let r = Renderer::new(ctx.glow()).expect("renderer");
            let g = S::init_gl(&r).expect("programs");
            Self { ctx, r, g, scene }
        }

        pub fn scene(&mut self) -> &mut S {
            &mut self.scene
        }

        pub fn board(&mut self, w: u32, h: u32) -> Board<S::Info> {
            self.r.upload(self.scene.generate(w, h))
        }

        /// Render `frames` frames (the last one at `t`) and read it back;
        /// returns the image and the GPU time per frame in ms.
        pub fn render(&mut self, board: &mut Board<S::Info>, w: u32, h: u32, t: f32, mouse: Option<(f32, f32)>, out: &mut S::Out, frames: u32) -> (RgbaImage, f64) {
            let _ = &self.ctx;
            let target = self.r.target(w, h);
            let gl = &self.r.gl;
            let f = Frame { w: w as f32, h: h as f32, t, mouse, encode: false, target: Some(target.fb) };
            // Warm up (shader compile, cache fill), then time.
            self.r.bind_target(f.target, w, h);
            self.scene.draw(&self.r, &self.g, board, out, &f);
            unsafe { gl.finish() };
            let start = std::time::Instant::now();
            for i in 0..frames {
                let f = Frame { t: t - (frames - 1 - i) as f32 / 60.0, ..f };
                self.r.bind_target(f.target, w, h);
                self.scene.draw(&self.r, &self.g, board, out, &f);
            }
            unsafe { gl.finish() };
            let ms = start.elapsed().as_secs_f64() * 1000.0 / frames.max(1) as f64;
            let mut px = vec![0u8; (w * h * 4) as usize];
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.fb));
                gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(&mut px));
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                gl.delete_framebuffer(target.fb);
                gl.delete_texture(target.tex);
            }
            // GL rows are bottom-up.
            let row = (w * 4) as usize;
            let flipped: Vec<u8> = px.chunks(row).rev().flatten().copied().collect();
            (RgbaImage::from_raw(w, h, flipped).unwrap(), ms)
        }
    }
}
