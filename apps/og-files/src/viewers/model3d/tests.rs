use super::camera::Camera;
use super::load::{self, Up};
use super::render::{clip_correction, grid_lines};
use super::*;
use base64::Engine as _;
use glam::{Vec3, Vec4};

/// Box 2 × 1 × 3 (x, y, z) so axis mix-ups show up.
const SIZE: [f32; 3] = [2.0, 1.0, 3.0];

fn corners() -> Vec<[f32; 3]> {
    (0..8)
        .map(|i| {
            [
                if i & 1 == 0 { 0.0 } else { SIZE[0] },
                if i & 2 == 0 { 0.0 } else { SIZE[1] },
                if i & 4 == 0 { 0.0 } else { SIZE[2] },
            ]
        })
        .collect()
}

/// Quads (outward, CCW) over `corners()` indices.
const QUADS: [[u32; 4]; 6] = [
    [0, 4, 6, 2], // -x
    [1, 3, 7, 5], // +x
    [0, 1, 5, 4], // -y
    [2, 6, 7, 3], // +y
    [0, 2, 3, 1], // -z
    [4, 5, 7, 6], // +z
];

fn tris() -> Vec<[u32; 3]> {
    QUADS
        .iter()
        .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
        .collect()
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let home = std::env::var_os("HOME").expect("HOME");
        let d = PathBuf::from(home)
            .join(".cache/ogx-p2-3d-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn write(&self, name: &str, data: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_box(m: &Model) {
    assert_eq!(m.source_vertices, 8);
    assert_eq!(m.source_triangles, 12);
    assert_eq!(m.triangles, 12);
    assert_eq!(m.indices.len(), 36);
    // 12 box edges + 6 face diagonals
    assert_eq!(m.edges(), 18);
    assert!((m.min - Vec3::ZERO).length() < 1e-6, "min {:?}", m.min);
    assert!(
        (m.max - Vec3::from_array(SIZE)).length() < 1e-6,
        "max {:?}",
        m.max
    );
    assert!(m.lines.iter().all(|&i| (i as usize) < m.vertices.len()));
    assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
}

/// Flat normals: unit length, axis-aligned, pointing away from the centre.
fn assert_flat_normals(m: &Model) {
    assert!(m.computed_normals);
    assert_eq!(m.vertices.len(), 36);
    let center = Vec3::from_array(SIZE) * 0.5;
    for t in m.indices.chunks(3) {
        let vs: Vec<_> = t.iter().map(|&i| m.vertices[i as usize]).collect();
        let n = Vec3::from_array(vs[0].normal);
        assert!((n.length() - 1.0).abs() < 1e-5);
        assert!(vs.iter().all(|v| v.normal == vs[0].normal));
        assert!(
            (n.abs().max_element() - 1.0).abs() < 1e-5,
            "not axis aligned {n:?}"
        );
        let face_c = vs.iter().map(|v| Vec3::from_array(v.pos)).sum::<Vec3>() / 3.0;
        assert!(n.dot(face_c - center) > 0.0, "inward normal {n:?}");
    }
}

fn stl_ascii() -> String {
    let c = corners();
    let mut s = String::from("solid box\n");
    for t in tris() {
        s += "facet normal 0 0 0\nouter loop\n";
        for i in t {
            let v = c[i as usize];
            s += &format!("vertex {} {} {}\n", v[0], v[1], v[2]);
        }
        s += "endloop\nendfacet\n";
    }
    s + "endsolid box\n"
}

fn stl_binary() -> Vec<u8> {
    let c = corners();
    let t = tris();
    let mut b = vec![0u8; 80];
    b.extend((t.len() as u32).to_le_bytes());
    for tri in t {
        b.extend([0u8; 12]);
        for i in tri {
            for x in c[i as usize] {
                b.extend(x.to_le_bytes());
            }
        }
        b.extend([0u8; 2]);
    }
    b
}

#[test]
fn supported_extensions() {
    for f in ["a.glb", "a.GLTF", "a.obj", "a.Stl", "a.ply"] {
        assert!(supported(Path::new(f)), "{f}");
    }
    for f in ["a.fbx", "a.txt", "noext"] {
        assert!(!supported(Path::new(f)), "{f}");
    }
}

#[test]
fn stl_ascii_box() {
    let d = Dir::new("stl-ascii");
    let m = load::load(&d.write("box.stl", stl_ascii())).unwrap();
    assert_eq!(m.format, "STL");
    assert_eq!(m.default_up, Up::Z);
    assert_eq!(m.materials, 0);
    assert_box(&m);
    assert_flat_normals(&m);
}

#[test]
fn stl_binary_box() {
    let d = Dir::new("stl-bin");
    let m = load::load(&d.write("box.stl", stl_binary())).unwrap();
    assert_box(&m);
    assert_flat_normals(&m);
}

#[test]
fn obj_box_without_normals_uses_material_colour() {
    let d = Dir::new("obj");
    let mut obj = String::from("mtllib box.mtl\no box\n");
    for v in corners() {
        obj += &format!("v {} {} {}\n", v[0], v[1], v[2]);
    }
    obj += "usemtl red\n";
    for q in QUADS {
        obj += &format!("f {} {} {} {}\n", q[0] + 1, q[1] + 1, q[2] + 1, q[3] + 1);
    }
    d.write("box.mtl", "newmtl red\nKd 1.0 0.0 0.0\n");
    let m = load::load(&d.write("box.obj", obj)).unwrap();
    assert_eq!(m.format, "Wavefront OBJ");
    assert_eq!(m.materials, 1);
    assert_box(&m);
    assert_flat_normals(&m);
    assert!(m.vertices.iter().all(|v| v.color == [1.0, 0.0, 0.0]));
}

#[test]
fn obj_with_normals_keeps_them() {
    let d = Dir::new("obj-n");
    let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvn 0 0 1\nf 1//1 2//1 3//1\n";
    let m = load::load(&d.write("tri.obj", obj)).unwrap();
    assert!(!m.computed_normals);
    assert_eq!(m.vertices.len(), 3);
    assert_eq!(m.edges(), 3);
    assert!(m.vertices.iter().all(|v| v.normal == [0.0, 0.0, 1.0]));
    // no mtl: default grey
    assert_eq!(m.materials, 0);
}

fn ply_header(format: &str) -> String {
    format!(
        "ply\nformat {format} 1.0\ncomment test\nelement vertex 8\nproperty float x\n\
         property float y\nproperty float z\nproperty uchar red\nproperty uchar green\n\
         property uchar blue\nelement face 6\nproperty list uchar int vertex_indices\nend_header\n"
    )
}

#[test]
fn ply_ascii_box() {
    let d = Dir::new("ply-ascii");
    let mut s = ply_header("ascii");
    for v in corners() {
        s += &format!("{} {} {} 255 255 255\n", v[0], v[1], v[2]);
    }
    for q in QUADS {
        s += &format!("4 {} {} {} {}\n", q[0], q[1], q[2], q[3]);
    }
    let m = load::load(&d.write("box.ply", s)).unwrap();
    assert_eq!(m.format, "PLY");
    assert_box(&m);
    assert_flat_normals(&m);
    assert!(m.vertices.iter().all(|v| v.color == [1.0, 1.0, 1.0]));
}

#[test]
fn ply_binary_box() {
    let d = Dir::new("ply-bin");
    let mut b = ply_header("binary_little_endian").into_bytes();
    for v in corners() {
        for x in v {
            b.extend(x.to_le_bytes());
        }
        b.extend([0u8, 0, 255]);
    }
    for q in QUADS {
        b.push(4);
        for i in q {
            b.extend((i as i32).to_le_bytes());
        }
    }
    let m = load::load(&d.write("box.ply", b)).unwrap();
    assert_box(&m);
    assert!(m.vertices.iter().all(|v| v.color == [0.0, 0.0, 1.0]));
}

#[test]
fn ply_truncated_is_an_error() {
    let mut s = ply_header("ascii");
    s += "0 0 0 1 1 1\n";
    assert!(load::parse_ply(s.as_bytes()).is_err());
}

fn gltf_json() -> String {
    let mut buf = Vec::new();
    for v in corners() {
        for x in v {
            buf.extend(x.to_le_bytes());
        }
    }
    for t in tris() {
        for i in t {
            buf.extend((i as u16).to_le_bytes());
        }
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"children": [1], "translation": [10, 0, 0]}}, {{"mesh": 0, "scale": [1, 2, 1]}}],
  "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}, "indices": 1, "material": 0}}]}}],
  "materials": [{{"pbrMetallicRoughness": {{"baseColorFactor": [0.5, 0.25, 1.0, 1.0]}}}}],
  "buffers": [{{"byteLength": {len}, "uri": "data:application/octet-stream;base64,{b64}"}}],
  "bufferViews": [
    {{"buffer": 0, "byteOffset": 0, "byteLength": 96}},
    {{"buffer": 0, "byteOffset": 96, "byteLength": 72}}
  ],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 8, "type": "VEC3",
      "min": [0, 0, 0], "max": [{x}, {y}, {z}]}},
    {{"bufferView": 1, "componentType": 5123, "count": 36, "type": "SCALAR"}}
  ]
}}"#,
        len = buf.len(),
        x = SIZE[0],
        y = SIZE[1],
        z = SIZE[2],
    )
}

#[test]
fn gltf_embedded_buffer_with_node_transforms() {
    let d = Dir::new("gltf");
    let m = load::load(&d.write("box.gltf", gltf_json())).unwrap();
    assert_eq!(m.format, "glTF");
    assert_eq!(m.materials, 1);
    assert_eq!(m.default_up, Up::Y);
    assert_eq!(m.source_triangles, 12);
    assert_eq!(m.source_vertices, 8);
    assert_eq!(m.edges(), 18);
    // parent translation (10,0,0) ∘ child scale (1,2,1)
    assert!(
        (m.min - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-5,
        "{:?}",
        m.min
    );
    assert!(
        (m.max - Vec3::new(12.0, 2.0, 3.0)).length() < 1e-5,
        "{:?}",
        m.max
    );
    assert!(m.computed_normals);
    assert!(m.vertices.iter().all(|v| v.color == [0.5, 0.25, 1.0]));
}

#[test]
fn bad_files_are_errors_not_panics() {
    let d = Dir::new("bad");
    for (name, data) in [
        ("a.stl", &b"solid x\nfacet nonsense"[..]),
        (
            "a.ply",
            b"ply\nformat ascii 1.0\nelement vertex 99999999999\nend_header\n",
        ),
        ("a.gltf", b"{not json"),
        ("a.glb", b"glTF\x02\0\0\0"),
        ("empty.obj", b""),
    ] {
        assert!(load::load(&d.write(name, data)).is_err(), "{name}");
    }
}

#[test]
fn z_up_conversion() {
    let m = Up::Z.matrix();
    assert!((m.transform_point3(Vec3::Z) - Vec3::Y).length() < 1e-6);
    assert!((m.transform_point3(Vec3::Y) + Vec3::Z).length() < 1e-6);
    assert!((m.transform_point3(Vec3::X) - Vec3::X).length() < 1e-6);
    assert_eq!(Up::Y.matrix(), glam::Mat4::IDENTITY);

    let d = Dir::new("zup");
    let model = load::load(&d.write("box.stl", stl_binary())).unwrap();
    let (lo, hi) = model.world_bounds(Up::Z);
    // the 3-unit Z extent becomes world height
    assert!(
        ((hi - lo) - Vec3::new(2.0, 3.0, 1.0)).length() < 1e-5,
        "{:?}",
        hi - lo
    );
    assert!((lo.y - 0.0).abs() < 1e-6);
    let (lo, hi) = model.world_bounds(Up::Y);
    assert!(((hi - lo) - Vec3::from_array(SIZE)).length() < 1e-5);
}

fn ndc(vp: glam::Mat4, p: Vec3) -> Vec3 {
    let c = vp * p.extend(1.0);
    assert!(c.w > 0.0, "behind camera");
    c.truncate() / c.w
}

#[test]
fn camera_fit_keeps_model_in_frustum() {
    let boxes = [
        (Vec3::ZERO, Vec3::from_array(SIZE)),
        (Vec3::new(-500.0, 10.0, -3.0), Vec3::new(900.0, 12.0, 4.0)),
        (Vec3::splat(1e-4), Vec3::splat(2e-4)),
    ];
    for (lo, hi) in boxes {
        for aspect in [0.3, 1.0, 1.78, 4.0] {
            for (yaw, pitch) in [(0.6, 0.42), (2.0, -1.2), (-3.0, 1.5)] {
                let mut cam = Camera::fit(lo, hi);
                cam.yaw = yaw;
                cam.pitch = pitch;
                let vp = cam.view_proj(aspect);
                let mut max_xy: f32 = 0.0;
                for i in 0..8 {
                    let p = Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    );
                    let n = ndc(vp, p);
                    assert!(
                        n.x.abs() <= 1.0 && n.y.abs() <= 1.0,
                        "{n:?} aspect {aspect}"
                    );
                    assert!((0.0..=1.0).contains(&n.z), "depth {n:?}");
                    max_xy = max_xy.max(n.x.abs()).max(n.y.abs());
                }
                // and it is actually framed, not a speck
                assert!(max_xy > 0.15, "too small: {max_xy}");
            }
        }
    }
}

#[test]
fn camera_zoom_pan_orbit() {
    let mut cam = Camera::fit(Vec3::ZERO, Vec3::ONE);
    let d0 = cam.distance(1.0);
    cam.zoom_by(0.5);
    assert!((cam.distance(1.0) - d0 * 0.5).abs() < 1e-5);
    cam.zoom_by(1e-9);
    assert!(cam.zoom >= 0.01);
    cam.orbit(0.0, 1e6);
    assert!(cam.pitch < std::f32::consts::FRAC_PI_2);
    // panning right moves the target along the camera's right vector
    let (right, up) = cam.basis();
    let d = cam.pan_delta(-10.0, 0.0, 100.0, 1.0);
    assert!(d.normalize().dot(right) > 0.999);
    assert!(d.dot(up).abs() < 1e-5);
}

#[test]
fn clip_correction_maps_full_rect_onto_clip() {
    let full = [100.0, 50.0, 400.0, 300.0];
    assert!(clip_correction(full, full).abs_diff_eq(glam::Mat4::IDENTITY, 1e-6));
    // widget hanging off the left/top edge: clip = visible part
    let full = [-100.0, -50.0, 400.0, 300.0];
    let clip = [0.0, 0.0, 300.0, 250.0];
    let m = clip_correction(full, clip);
    // full-NDC point at pixel (0, 0) → clip-NDC top-left (-1, 1)
    let px_to_full = |x: f32, y: f32| {
        Vec4::new(
            (x - full[0]) / full[2] * 2.0 - 1.0,
            1.0 - (y - full[1]) / full[3] * 2.0,
            0.5,
            1.0,
        )
    };
    let a = m * px_to_full(0.0, 0.0);
    assert!(
        (a.x + 1.0).abs() < 1e-5 && (a.y - 1.0).abs() < 1e-5,
        "{a:?}"
    );
    let b = m * px_to_full(300.0, 250.0);
    assert!(
        (b.x - 1.0).abs() < 1e-5 && (b.y + 1.0).abs() < 1e-5,
        "{b:?}"
    );
    assert!((b.z - 0.5).abs() < 1e-6);
}

#[test]
fn grid_sits_under_model() {
    let g = grid_lines(Vec3::new(-1.0, 2.0, -1.0), Vec3::new(1.0, 5.0, 1.0));
    assert!(!g.is_empty() && g.len().is_multiple_of(2));
    assert!(g.iter().all(|v| v.pos[1] == 2.0));
    assert!(g.len() <= 121 * 4);
}

#[test]
fn number_formatting() {
    assert_eq!(group(1234567), "1\u{2009}234\u{2009}567");
    assert_eq!(group(12), "12");
    assert_eq!(num(2.0), "2");
    assert_eq!(num(0.125), "0.125");
    assert_eq!(num(1234.4), "1234");
}

#[tokio::test]
async fn open_and_state() {
    let d = Dir::new("open");
    let p = d.write("box.stl", stl_ascii());
    let o = open(p).await.unwrap();
    let (mut s, _) = State::new(o.clone());
    assert_eq!(s.up, Up::Z);
    assert!(!s.is_dirty());
    assert_eq!(s.is_editing(), None);
    let cam = s.camera;
    let _ = s.update(Msg::Orbit(10.0, 5.0));
    assert_ne!(s.camera, cam);
    let _ = s.update(Msg::Reset);
    assert_eq!(s.camera, cam);
    let _ = s.update(Msg::SetUp(Up::Y));
    assert_eq!(s.up, Up::Y);
    let _ = s.update(Msg::SetMode(Mode::Wireframe));
    assert_eq!(s.mode, Mode::Wireframe);
    assert!(open(d.0.join("missing.obj")).await.is_err());
}

#[tokio::test]
async fn viewport_input_and_view() {
    use iced::advanced::Shell;
    use shader::Program as _;
    let d = Dir::new("input");
    let o = open(d.write("box.stl", stl_binary())).await.unwrap();
    let (mut s, _) = State::new(o);
    let bounds = Rectangle::new(Point::new(10.0, 10.0), iced::Size::new(400.0, 300.0));
    let at = |x, y| mouse::Cursor::Available(Point::new(x, y));
    let mut ist = Interaction::default();
    let mut out = Vec::new();
    let mut send = |s: &State, ist: &mut Interaction, ev, c| {
        let mut shell = Shell::new(&mut out);
        Scene { state: s }.update(ist, ev, bounds, c, &mut shell).1
    };
    use mouse::Event as M;
    use shader::Event::Mouse;

    // left drag → orbit
    assert!(send(
        &s,
        &mut ist,
        Mouse(M::ButtonPressed(mouse::Button::Left)),
        at(100.0, 100.0)
    )
    .is_none());
    let m = send(
        &s,
        &mut ist,
        Mouse(M::CursorMoved {
            position: Point::new(110.0, 95.0),
        }),
        at(110.0, 95.0),
    );
    assert!(
        matches!(m, Some(Msg::Orbit(dx, dy)) if dx == 10.0 && dy == -5.0),
        "{m:?}"
    );
    send(
        &s,
        &mut ist,
        Mouse(M::ButtonReleased(mouse::Button::Left)),
        at(110.0, 95.0),
    );
    // quick second press near the first → fit
    let m = send(
        &s,
        &mut ist,
        Mouse(M::ButtonPressed(mouse::Button::Left)),
        at(101.0, 100.0),
    );
    assert!(matches!(m, Some(Msg::Reset)), "{m:?}");
    send(
        &s,
        &mut ist,
        Mouse(M::ButtonReleased(mouse::Button::Left)),
        at(111.0, 95.0),
    );

    // right drag → pan
    send(
        &s,
        &mut ist,
        Mouse(M::ButtonPressed(mouse::Button::Right)),
        at(200.0, 200.0),
    );
    let m = send(
        &s,
        &mut ist,
        Mouse(M::CursorMoved {
            position: Point::new(220.0, 200.0),
        }),
        at(220.0, 200.0),
    );
    let Some(Msg::Pan(x, y, z)) = m else {
        panic!("{m:?}")
    };
    let target = s.camera.target;
    let _ = s.update(Msg::Pan(x, y, z));
    assert_ne!(s.camera.target, target);
    send(
        &s,
        &mut ist,
        Mouse(M::ButtonReleased(mouse::Button::Right)),
        at(220.0, 200.0),
    );

    // wheel over the view → zoom in; outside → ignored
    let wheel = || {
        Mouse(M::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
        })
    };
    let m = send(&s, &mut ist, wheel(), at(50.0, 50.0));
    let Some(Msg::Zoom(f)) = m else {
        panic!("{m:?}")
    };
    assert!(f < 1.0);
    let _ = s.update(Msg::Zoom(f));
    assert!(s.camera.zoom < 1.0);
    assert!(send(&s, &mut ist, wheel(), at(900.0, 900.0)).is_none());

    let _ = s.update(Msg::ToggleGrid);
    assert!(!s.grid);
    let _ = s.update(Msg::SetMode(Mode::Normals));
    assert_eq!(s.mode, Mode::Normals);
    assert!(!s.model.decimated());
    let p = Scene { state: &s }.draw(&ist, at(0.0, 0.0), bounds);
    assert_eq!(p.mode, Mode::Normals);
    assert_eq!(
        Scene { state: &s }.mouse_interaction(&ist, bounds, at(50.0, 50.0)),
        mouse::Interaction::Grab
    );
    // building the widget tree needs no GPU
    let _ = s.view();
    let _ = s.toggle_edit();
    let _ = s.save();
    assert_eq!(vec3(Vec3::new(1.0, 2.5, 0.0)), "1, 2.5, 0");
}

#[test]
fn shader_wgsl_validates() {
    let module = naga::front::wgsl::parse_str(include_str!("shader.wgsl")).expect("WGSL parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("WGSL validation");
    for entry in ["vs_main", "fs_main", "vs_bg", "fs_bg"] {
        assert!(
            module.entry_points.iter().any(|e| e.name == entry),
            "{entry}"
        );
    }
}
