//! Model loaders (glTF/glb, OBJ, STL, PLY) normalised into one GPU-ready
//! mesh: interleaved (position, normal, color) vertices, triangle indices
//! and a line-list index buffer of unique edges for wireframe mode.
//! Pure CPU code — runs inside `spawn_blocking`, testable without a GPU.

use base64::Engine as _;
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Triangles kept for rendering; bigger models are decimated (every n-th
/// triangle) so the flat-shaded vertex buffer stays under wgpu's default
/// 256 MiB buffer limit (2M × 3 × 36 B ≈ 216 MB).
pub const MAX_TRIANGLES: usize = 2_000_000;
/// Models above this are refused outright.
pub const HARD_LIMIT: usize = 50_000_000;
/// Files above this are refused before reading.
const MAX_FILE_BYTES: u64 = 2 << 30;

const DEFAULT_COLOR: Vec3 = Vec3::new(0.72, 0.72, 0.74);

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Up {
    Y,
    Z,
}

impl Up {
    /// Model → world rotation (world is always Y-up).
    pub fn matrix(self) -> Mat4 {
        match self {
            Up::Y => Mat4::IDENTITY,
            // (x, y, z) → (x, z, -y): +Z becomes +Y
            Up::Z => Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        }
    }
}

pub struct Model {
    /// Unique per load; keys the GPU buffer cache.
    pub id: u64,
    pub format: &'static str,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Line list (pairs of vertex indices), one per unique edge.
    pub lines: Vec<u32>,
    pub min: Vec3,
    pub max: Vec3,
    /// Unique vertex positions in the source.
    pub source_vertices: usize,
    pub source_triangles: usize,
    /// Triangles actually rendered (< source_triangles if decimated).
    pub triangles: usize,
    pub materials: usize,
    /// Some faces had no normals in the file: flat normals were computed.
    pub computed_normals: bool,
    pub default_up: Up,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("id", &self.id)
            .field("format", &self.format)
            .field("vertices", &self.vertices.len())
            .field("triangles", &self.triangles)
            .field("edges", &(self.lines.len() / 2))
            .field("min", &self.min)
            .field("max", &self.max)
            .finish()
    }
}

impl Model {
    pub fn decimated(&self) -> bool {
        self.triangles < self.source_triangles
    }

    pub fn edges(&self) -> usize {
        self.lines.len() / 2
    }

    /// Bounding box after the up-axis rotation (world space).
    pub fn world_bounds(&self, up: Up) -> (Vec3, Vec3) {
        let m = up.matrix();
        let (a, b) = (self.min, self.max);
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { a.x } else { b.x },
                if i & 2 == 0 { a.y } else { b.y },
                if i & 4 == 0 { a.z } else { b.z },
            );
            let w = m.transform_point3(c);
            lo = lo.min(w);
            hi = hi.max(w);
        }
        (lo, hi)
    }
}

pub fn supported_ext(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "gltf" => "glTF",
        "glb" => "glTF (binary)",
        "obj" => "Wavefront OBJ",
        "stl" => "STL",
        "ply" => "PLY",
        _ => return None,
    })
}

/// Blocking: read + parse + normalise `path`.
pub fn load(path: &Path) -> Result<Model, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let format = supported_ext(&ext).ok_or("unsupported 3D format")?;
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE_BYTES {
        return Err(format!(
            "file too large for the 3D viewer ({:.1} GiB)",
            len as f64 / (1u64 << 30) as f64
        ));
    }
    let (b, materials, up) = match ext.as_str() {
        "gltf" | "glb" => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let (b, m) = parse_gltf(&bytes, path.parent())?;
            (b, m, Up::Y)
        }
        "obj" => {
            let (b, m) = parse_obj(path)?;
            (b, m, Up::Y)
        }
        "stl" => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            (parse_stl(&bytes)?, 0, Up::Z)
        }
        _ => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            (parse_ply(&bytes)?, 0, Up::Y)
        }
    };
    b.finish(format, materials, up)
}

// ---------------------------------------------------------------- builder

/// Raw geometry before normalisation. `nrm` is zero where unknown.
#[derive(Default)]
pub struct Builder {
    pos: Vec<Vec3>,
    nrm: Vec<Vec3>,
    col: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
}

impl Builder {
    fn vertex(&mut self, p: Vec3, n: Option<Vec3>, c: Vec3) -> u32 {
        self.pos.push(p);
        self.nrm.push(n.unwrap_or(Vec3::ZERO));
        self.col.push(c);
        (self.pos.len() - 1) as u32
    }

    fn base(&self) -> u32 {
        self.pos.len() as u32
    }

    fn check_size(&self) -> Result<(), String> {
        if self.tris.len() > HARD_LIMIT || self.pos.len() > u32::MAX as usize / 2 {
            Err(format!(
                "model too large ({} triangles, limit {})",
                self.tris.len(),
                HARD_LIMIT
            ))
        } else {
            Ok(())
        }
    }

    pub fn finish(
        mut self,
        format: &'static str,
        materials: usize,
        up: Up,
    ) -> Result<Model, String> {
        let n = self.pos.len();
        self.tris.retain(|t| {
            t.iter()
                .all(|&i| (i as usize) < n && self.pos[i as usize].is_finite())
        });
        if self.tris.is_empty() {
            return Err("no triangles in model".into());
        }
        self.check_size()?;
        let source_triangles = self.tris.len();
        let stride = source_triangles.div_ceil(MAX_TRIANGLES);

        // weld identical positions: unique-vertex count + shared wireframe edges
        let mut weld: HashMap<[u32; 3], u32> = HashMap::new();
        let canon: Vec<u32> = self
            .pos
            .iter()
            .map(|p| {
                let key = (*p + Vec3::ZERO).to_array().map(f32::to_bits); // -0 → +0
                let next = weld.len() as u32;
                *weld.entry(key).or_insert(next)
            })
            .collect();
        let source_vertices = weld.len();
        drop(weld);

        let (mut min, mut max) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for t in &self.tris {
            for &i in t {
                min = min.min(self.pos[i as usize]);
                max = max.max(self.pos[i as usize]);
            }
        }

        let kept = source_triangles.div_ceil(stride);
        let mut vertices: Vec<Vertex> = Vec::with_capacity(kept * 3);
        let mut indices: Vec<u32> = Vec::with_capacity(kept * 3);
        let mut lines: Vec<u32> = Vec::new();
        let mut shared = vec![u32::MAX; n]; // source vertex → gpu vertex (smooth)
        let mut any_gpu = vec![u32::MAX; source_vertices]; // welded → some gpu vertex
        let mut edges: HashSet<u64> = HashSet::new();
        let mut computed_normals = false;

        for t in self.tris.iter().step_by(stride) {
            let [a, b, c] = t.map(|i| self.pos[i as usize]);
            let face = (b - a).cross(c - a).try_normalize().unwrap_or(Vec3::Y);
            let smooth = t
                .iter()
                .all(|&i| self.nrm[i as usize].length_squared() > 0.25);
            let mut gpu = [0u32; 3];
            for (k, &i) in t.iter().enumerate() {
                let i = i as usize;
                gpu[k] = if smooth {
                    if shared[i] == u32::MAX {
                        shared[i] = vertices.len() as u32;
                        vertices.push(Vertex {
                            pos: self.pos[i].to_array(),
                            normal: self.nrm[i].normalize().to_array(),
                            color: self.col[i].to_array(),
                        });
                    }
                    shared[i]
                } else {
                    vertices.push(Vertex {
                        pos: self.pos[i].to_array(),
                        normal: face.to_array(),
                        color: self.col[i].to_array(),
                    });
                    vertices.len() as u32 - 1
                };
                let ci = canon[i] as usize;
                if any_gpu[ci] == u32::MAX {
                    any_gpu[ci] = gpu[k];
                }
            }
            computed_normals |= !smooth;
            indices.extend_from_slice(&gpu);
            for (x, y) in [(0, 1), (1, 2), (2, 0)] {
                let (cx, cy) = (canon[t[x] as usize], canon[t[y] as usize]);
                if cx == cy {
                    continue;
                }
                let key = (u64::from(cx.min(cy)) << 32) | u64::from(cx.max(cy));
                if edges.insert(key) {
                    lines.push(any_gpu[cx as usize]);
                    lines.push(any_gpu[cy as usize]);
                }
            }
        }

        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Ok(Model {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            format,
            triangles: indices.len() / 3,
            vertices,
            indices,
            lines,
            min,
            max,
            source_vertices,
            source_triangles,
            materials,
            computed_normals,
            default_up: up,
        })
    }
}

// ---------------------------------------------------------------- STL

pub fn parse_stl(bytes: &[u8]) -> Result<Builder, String> {
    let mesh =
        stl_io::read_stl(&mut std::io::Cursor::new(bytes)).map_err(|e| format!("STL: {e}"))?;
    let mut b = Builder::default();
    for v in &mesh.vertices {
        b.vertex(Vec3::from_array(v.0), None, DEFAULT_COLOR);
    }
    // STL facet normals are per face (often junk): always compute flat normals
    b.tris = mesh
        .faces
        .iter()
        .map(|f| f.vertices.map(|i| i as u32))
        .collect();
    Ok(b)
}

// ---------------------------------------------------------------- OBJ

pub fn parse_obj(path: &Path) -> Result<(Builder, usize), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(file);
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let (models, mats) = tobj::load_obj_buf(&mut reader, &tobj::GPU_LOAD_OPTIONS, |p| {
        tobj::load_mtl(dir.join(p))
    })
    .map_err(|e| format!("OBJ: {e}"))?;
    let mats = mats.unwrap_or_default(); // missing .mtl is fine
    let mut b = Builder::default();
    for m in &models {
        let mesh = &m.mesh;
        let base = b.base();
        let mat_color = mesh
            .material_id
            .and_then(|i| mats.get(i))
            .and_then(|m| m.diffuse)
            .map(Vec3::from_array);
        let nv = mesh.positions.len() / 3;
        let has_n = mesh.normals.len() == nv * 3;
        let has_c = mesh.vertex_color.len() == nv * 3;
        for i in 0..nv {
            let p = Vec3::from_slice(&mesh.positions[i * 3..i * 3 + 3]);
            let n = has_n.then(|| Vec3::from_slice(&mesh.normals[i * 3..i * 3 + 3]));
            let c = if has_c {
                Vec3::from_slice(&mesh.vertex_color[i * 3..i * 3 + 3])
            } else {
                mat_color.unwrap_or(DEFAULT_COLOR)
            };
            b.vertex(p, n, c);
        }
        b.tris.extend(
            mesh.indices
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
        );
        b.check_size()?;
    }
    Ok((b, mats.len()))
}

// ---------------------------------------------------------------- glTF

fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let (_, data) = rest.split_once(";base64,")?;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

fn read_uri(uri: &str, dir: Option<&Path>) -> Result<Vec<u8>, String> {
    if uri.starts_with("data:") {
        return decode_data_uri(uri).ok_or_else(|| "glTF: bad data URI".to_string());
    }
    let rel = uri.replace("%20", " ");
    let p = dir
        .map(|d| d.join(&rel))
        .unwrap_or_else(|| rel.clone().into());
    std::fs::read(&p).map_err(|e| format!("glTF: {rel}: {e}"))
}

struct Texture {
    w: u32,
    h: u32,
    rgb: Vec<[u8; 3]>,
}

impl Texture {
    /// Nearest sample with repeat wrapping, as linear RGB.
    fn sample(&self, uv: [f32; 2]) -> Vec3 {
        let fx = uv[0] - uv[0].floor();
        let fy = uv[1] - uv[1].floor();
        let x = ((fx * self.w as f32) as u32).min(self.w - 1);
        let y = ((fy * self.h as f32) as u32).min(self.h - 1);
        let p = self.rgb[(y * self.w + x) as usize];
        let c = |v: u8| srgb_to_linear(f32::from(v) / 255.0);
        Vec3::new(c(p[0]), c(p[1]), c(p[2]))
    }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn decode_texture(img: &gltf::Image, buffers: &[Vec<u8>], dir: Option<&Path>) -> Option<Texture> {
    let data = match img.source() {
        gltf::image::Source::View { view, .. } => {
            let b = buffers.get(view.buffer().index())?;
            b.get(view.offset()..view.offset() + view.length())?
                .to_vec()
        }
        gltf::image::Source::Uri { uri, .. } => read_uri(uri, dir).ok()?,
    };
    let rgb = image::load_from_memory(&data).ok()?.to_rgb8();
    let (w, h) = rgb.dimensions();
    (w > 0 && h > 0).then(|| Texture {
        w,
        h,
        rgb: rgb.pixels().map(|p| p.0).collect(),
    })
}

pub fn parse_gltf(bytes: &[u8], dir: Option<&Path>) -> Result<(Builder, usize), String> {
    let gltf = gltf::Gltf::from_slice(bytes).map_err(|e| format!("glTF: {e}"))?;
    let doc = &gltf.document;
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    for buf in doc.buffers() {
        buffers.push(match buf.source() {
            gltf::buffer::Source::Bin => gltf.blob.clone().ok_or("glTF: missing BIN chunk")?,
            gltf::buffer::Source::Uri(uri) => read_uri(uri, dir)?,
        });
    }

    // base-colour textures (decode failures just mean "no texture")
    let mut textures: HashMap<usize, Texture> = HashMap::new();
    for mat in doc.materials() {
        if let Some(info) = mat.pbr_metallic_roughness().base_color_texture() {
            let img = info.texture().source();
            if let std::collections::hash_map::Entry::Vacant(e) = textures.entry(img.index()) {
                if let Some(t) = decode_texture(&img, &buffers, dir) {
                    e.insert(t);
                }
            }
        }
    }

    // collect (mesh, world matrix) from the scene graph
    let mut instances: Vec<(gltf::Mesh, Mat4)> = Vec::new();
    fn walk<'a>(node: gltf::Node<'a>, parent: Mat4, out: &mut Vec<(gltf::Mesh<'a>, Mat4)>) {
        let m = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            out.push((mesh, m));
        }
        for c in node.children() {
            walk(c, m, out);
        }
    }
    match doc.default_scene().or_else(|| doc.scenes().next()) {
        Some(scene) => scene
            .nodes()
            .for_each(|n| walk(n, Mat4::IDENTITY, &mut instances)),
        None => instances.extend(doc.meshes().map(|m| (m, Mat4::IDENTITY))),
    }

    let mut b = Builder::default();
    for (mesh, world) in &instances {
        let normal_m = world.inverse().transpose();
        for prim in mesh.primitives() {
            use gltf::mesh::Mode;
            let mode = prim.mode();
            if !matches!(
                mode,
                Mode::Triangles | Mode::TriangleStrip | Mode::TriangleFan
            ) {
                continue;
            }
            let reader = prim.reader(|buf| buffers.get(buf.index()).map(Vec::as_slice));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<Vec3> = positions.map(Vec3::from_array).collect();
            let normals: Option<Vec<Vec3>> = reader
                .read_normals()
                .map(|n| n.map(Vec3::from_array).collect())
                .filter(|n: &Vec<Vec3>| n.len() == positions.len());
            let colors: Option<Vec<Vec3>> = reader
                .read_colors(0)
                .map(|c| c.into_rgb_f32().map(Vec3::from_array).collect())
                .filter(|c: &Vec<Vec3>| c.len() == positions.len());
            let pbr = prim.material().pbr_metallic_roughness();
            let f = pbr.base_color_factor();
            let factor = Vec3::new(f[0], f[1], f[2]);
            let tex = pbr.base_color_texture().and_then(|info| {
                let uvs: Vec<[f32; 2]> = reader
                    .read_tex_coords(info.tex_coord())?
                    .into_f32()
                    .collect();
                let t = textures.get(&info.texture().source().index())?;
                (uvs.len() == positions.len()).then_some((t, uvs))
            });

            let base = b.base();
            for (i, p) in positions.iter().enumerate() {
                let mut c = colors.as_ref().map_or(Vec3::ONE, |c| c[i]) * factor;
                if let Some((t, uvs)) = &tex {
                    c *= t.sample(uvs[i]);
                }
                let n = normals
                    .as_ref()
                    .map(|n| normal_m.transform_vector3(n[i]).normalize_or_zero());
                b.vertex(world.transform_point3(*p), n, c);
            }
            let idx: Vec<u32> = match reader.read_indices() {
                Some(ix) => ix.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            match mode {
                Mode::Triangles => b.tris.extend(
                    idx.as_chunks::<3>()
                        .0
                        .iter()
                        .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
                ),
                Mode::TriangleStrip => b.tris.extend(idx.windows(3).enumerate().map(|(k, w)| {
                    if k % 2 == 0 {
                        [w[0] + base, w[1] + base, w[2] + base]
                    } else {
                        [w[1] + base, w[0] + base, w[2] + base]
                    }
                })),
                _ => {
                    if let Some((&first, rest)) = idx.split_first() {
                        b.tris.extend(
                            rest.windows(2)
                                .map(|w| [first + base, w[0] + base, w[1] + base]),
                        );
                    }
                }
            }
            b.check_size()?;
        }
    }
    Ok((b, doc.materials().len()))
}

// ---------------------------------------------------------------- PLY

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Result<Ty, String> {
        Ok(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            _ => return Err(format!("PLY: unknown type {s}")),
        })
    }
    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }
    fn is_float(self) -> bool {
        matches!(self, Ty::F32 | Ty::F64)
    }
}

enum Prop {
    Scalar(String, Ty),
    List(String, Ty, Ty),
}

struct Element {
    name: String,
    count: usize,
    props: Vec<Prop>,
}

enum Data<'a> {
    Ascii(std::str::SplitAsciiWhitespace<'a>),
    Bin { d: &'a [u8], at: usize, le: bool },
}

impl Data<'_> {
    fn read(&mut self, ty: Ty) -> Result<f64, String> {
        match self {
            Data::Ascii(it) => it
                .next()
                .ok_or("PLY: unexpected end of data")?
                .parse::<f64>()
                .map_err(|e| format!("PLY: {e}")),
            Data::Bin { d, at, le } => {
                let n = ty.size();
                let s = d.get(*at..*at + n).ok_or("PLY: unexpected end of data")?;
                *at += n;
                let mut a = [0u8; 8];
                a[..n].copy_from_slice(s);
                if !*le {
                    a[..n].reverse();
                }
                Ok(match ty {
                    Ty::I8 => a[0] as i8 as f64,
                    Ty::U8 => a[0] as f64,
                    Ty::I16 => i16::from_le_bytes([a[0], a[1]]) as f64,
                    Ty::U16 => u16::from_le_bytes([a[0], a[1]]) as f64,
                    Ty::I32 => i32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
                    Ty::U32 => u32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
                    Ty::F32 => f32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
                    Ty::F64 => f64::from_le_bytes(a),
                })
            }
        }
    }
}

pub fn parse_ply(bytes: &[u8]) -> Result<Builder, String> {
    const END: &[u8] = b"end_header";
    let hpos = bytes
        .windows(END.len())
        .position(|w| w == END)
        .ok_or("PLY: no end_header")?;
    let mut body = hpos + END.len();
    while body < bytes.len() && bytes[body] != b'\n' {
        body += 1;
    }
    body += 1;
    let header = std::str::from_utf8(&bytes[..hpos]).map_err(|_| "PLY: bad header")?;
    let mut lines = header.lines().map(str::trim);
    if lines.next() != Some("ply") {
        return Err("not a PLY file".into());
    }
    let mut format = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in lines {
        let w: Vec<&str> = line.split_ascii_whitespace().collect();
        match w.as_slice() {
            ["format", f, ..] => format = Some(f.to_string()),
            ["element", name, count] => elements.push(Element {
                name: name.to_string(),
                count: count.parse().map_err(|_| "PLY: bad element count")?,
                props: Vec::new(),
            }),
            ["property", "list", ct, it, name] => elements
                .last_mut()
                .ok_or("PLY: property before element")?
                .props
                .push(Prop::List(name.to_string(), Ty::parse(ct)?, Ty::parse(it)?)),
            ["property", ty, name] => elements
                .last_mut()
                .ok_or("PLY: property before element")?
                .props
                .push(Prop::Scalar(name.to_string(), Ty::parse(ty)?)),
            _ => {} // comment, obj_info, blank
        }
    }
    let rest = bytes.get(body..).unwrap_or_default();
    let mut data = match format.as_deref() {
        Some("ascii") => Data::Ascii(
            std::str::from_utf8(rest)
                .map_err(|_| "PLY: ascii body is not UTF-8")?
                .split_ascii_whitespace(),
        ),
        Some("binary_little_endian") => Data::Bin {
            d: rest,
            at: 0,
            le: true,
        },
        Some("binary_big_endian") => Data::Bin {
            d: rest,
            at: 0,
            le: false,
        },
        _ => return Err("PLY: unknown format".into()),
    };

    let mut b = Builder::default();
    for el in &elements {
        // every element/property costs at least one byte: reject absurd counts early
        if el.count > bytes.len() {
            return Err("PLY: element count exceeds file size".into());
        }
        let find = |names: &[&str]| {
            el.props
                .iter()
                .position(|p| matches!(p, Prop::Scalar(n, _) if names.contains(&n.as_str())))
        };
        match el.name.as_str() {
            "vertex" => {
                let xyz = [find(&["x"]), find(&["y"]), find(&["z"])];
                let nrm = [find(&["nx"]), find(&["ny"]), find(&["nz"])];
                let rgb = [
                    find(&["red", "r", "diffuse_red"]),
                    find(&["green", "g", "diffuse_green"]),
                    find(&["blue", "b", "diffuse_blue"]),
                ];
                if xyz.iter().any(Option::is_none) {
                    return Err("PLY: vertex without x/y/z".into());
                }
                let mut vals = vec![0f64; el.props.len()];
                for _ in 0..el.count {
                    for (k, p) in el.props.iter().enumerate() {
                        match p {
                            Prop::Scalar(_, ty) => vals[k] = data.read(*ty)?,
                            Prop::List(_, ct, it) => {
                                for _ in 0..data.read(*ct)? as usize {
                                    data.read(*it)?;
                                }
                            }
                        }
                    }
                    let get = |ix: [Option<usize>; 3]| ix.map(|i| i.map(|i| vals[i] as f32));
                    let [x, y, z] = get(xyz).map(Option::unwrap_or_default);
                    let n = match get(nrm) {
                        [Some(a), Some(b), Some(c)] => Some(Vec3::new(a, b, c)),
                        _ => None,
                    };
                    let c = match rgb {
                        [Some(r), Some(g), Some(bl)] => {
                            let ch = |i: usize| {
                                let v = vals[i] as f32;
                                match &el.props[i] {
                                    Prop::Scalar(_, t) if t.is_float() => v,
                                    _ => srgb_to_linear(v / 255.0),
                                }
                            };
                            Vec3::new(ch(r), ch(g), ch(bl))
                        }
                        _ => DEFAULT_COLOR,
                    };
                    b.vertex(Vec3::new(x, y, z), n, c);
                }
            }
            "face" => {
                let list = el
                    .props
                    .iter()
                    .position(|p| matches!(p, Prop::List(n, ..) if n == "vertex_indices" || n == "vertex_index"))
                    .or_else(|| el.props.iter().position(|p| matches!(p, Prop::List(..))));
                let mut poly: Vec<u32> = Vec::new();
                for _ in 0..el.count {
                    for (k, p) in el.props.iter().enumerate() {
                        match p {
                            Prop::Scalar(_, ty) => {
                                data.read(*ty)?;
                            }
                            Prop::List(_, ct, it) => {
                                let n = data.read(*ct)? as usize;
                                poly.clear();
                                for _ in 0..n {
                                    poly.push(data.read(*it)? as u32);
                                }
                                if Some(k) == list && n >= 3 {
                                    for w in poly[1..].windows(2) {
                                        b.tris.push([poly[0], w[0], w[1]]);
                                    }
                                }
                            }
                        }
                    }
                }
                b.check_size()?;
            }
            _ => {
                for _ in 0..el.count {
                    for p in &el.props {
                        match p {
                            Prop::Scalar(_, ty) => {
                                data.read(*ty)?;
                            }
                            Prop::List(_, ct, it) => {
                                for _ in 0..data.read(*ct)? as usize {
                                    data.read(*it)?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(b)
}
