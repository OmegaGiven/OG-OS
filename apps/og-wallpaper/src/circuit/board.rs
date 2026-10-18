//! Procedural motherboard layout for one output size.
//!
//! Produces two full-resolution images the shader composites every frame:
//! - `static_k`: everything that never changes (substrate, grid, chips, dim
//!   copper traces), drawn once with tiny-skia. Every color on the board is
//!   a blend of the theme background and accent, so it's stored as one byte:
//!   the blend factor (0 = background, 1 = accent; sRGB-space, exactly how
//!   tiny-skia blends). The shader applies the live theme colors, so a theme
//!   change never needs a new board.
//! - `data`: per-pixel info for the nearest trace within `HALO` px, packed
//!   into one u32 (see `pack`): distance along the trace (from the CPU end),
//!   the trace's and its bus's random seeds, whether it's a filler trace, and
//!   the perpendicular distance to its centerline (for the core/glow falloff).
//!   The shader turns that into moving pulses with no per-frame CPU work.
//!
//! Layout: buses leave all four sides of the central CPU, run straight out
//! briefly, then fan out with 45° diagonals to spread across the board.
//! Within one side every bus starts its diagonal at the same distance and the
//! destination order matches the pin order, so buses never cross.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

/// How far from a trace's centerline the pulse glow reaches (px).
pub const HALO: f32 = 11.0;
/// Packed-dist value meaning "no trace nearby".
const DIST_NONE: u32 = 63;

/// One data texel: bits 0-12 along-distance (px, wraps at 8192), 13-18
/// perpendicular distance (0..62 maps 0..HALO; 63 = no trace), 19-24 trace
/// seed, 25-30 bus seed, 31 filler flag. Must match `circuit.wgsl`.
fn pack(along: f32, dist: f32, seed: f32, bus: f32, filler: bool) -> u32 {
    let a = (along.max(0.0).round() as u32) & 0x1fff;
    let d = ((dist / HALO) * 62.0).round().clamp(0.0, 62.0) as u32;
    let s6 = |x: f32| ((x * 64.0) as u32).min(63);
    a | (d << 13) | (s6(seed) << 19) | (s6(bus) << 25) | ((filler as u32) << 31)
}

const PITCH: f32 = 9.0; // trace spacing inside a bus
const BUS_W: f32 = 2.6; // bus trace width
const FILL_W: f32 = 1.6; // filler trace width
const STUB: f32 = 34.0; // straight run out of the CPU before buses fan out
const CELL: f32 = 6.0; // occupancy grid for filler routing

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct V2 {
    pub x: f32,
    pub y: f32,
}

fn v(x: f32, y: f32) -> V2 {
    V2 { x, y }
}
impl std::ops::Add for V2 {
    type Output = V2;
    fn add(self, o: V2) -> V2 {
        v(self.x + o.x, self.y + o.y)
    }
}
impl std::ops::Sub for V2 {
    type Output = V2;
    fn sub(self, o: V2) -> V2 {
        v(self.x - o.x, self.y - o.y)
    }
}
impl std::ops::Mul<f32> for V2 {
    type Output = V2;
    fn mul(self, k: f32) -> V2 {
        v(self.x * k, self.y * k)
    }
}
impl V2 {
    fn dot(self, o: V2) -> f32 {
        self.x * o.x + self.y * o.y
    }
    fn len(self) -> f32 {
        self.dot(self).sqrt()
    }
    fn norm(self) -> V2 {
        let l = self.len();
        if l > 0.0 { self * (1.0 / l) } else { self }
    }
    /// Rotate 90° (clockwise in screen space, y down).
    fn perp(self) -> V2 {
        v(-self.y, self.x)
    }
}

/// One copper trace: a polyline starting at its CPU-facing end.
struct Trace {
    pts: Vec<V2>,
    width: f32,
    seed: f32,
    bus_seed: f32,
    filler: bool,
}

struct Chip {
    rect: (f32, f32, f32, f32), // x, y, w, h
    /// Which edges carry pins: (left/right, top/bottom).
    pins_lr: bool,
}

pub struct Board {
    /// R8: background→accent blend factor per pixel.
    pub static_k: Vec<u8>,
    /// R32Uint: packed trace info per pixel (see `pack`).
    pub data: Vec<u32>,
    pub cpu_center: V2,
    pub cpu_size: f32,
}

/// Offset a polyline sideways by `d` with mitered corners (exact for the
/// 45°/90° bends used here, so parallel bus traces keep constant spacing).
fn offset(pts: &[V2], d: f32) -> Vec<V2> {
    let n = pts.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let dir_in = if i > 0 { (pts[i] - pts[i - 1]).norm() } else { (pts[1] - pts[0]).norm() };
        let dir_out = if i + 1 < n { (pts[i + 1] - pts[i]).norm() } else { dir_in };
        let n_in = dir_in.perp();
        let n_out = dir_out.perp();
        let m = (n_in + n_out).norm();
        let cos = m.dot(n_in).max(0.3);
        out.push(pts[i] + m * (d / cos));
    }
    out
}

/// Route one bus's centerline in side-local coordinates, then map to screen.
/// `o` = outward direction, `t` = tangent along the CPU edge.
fn route(edge_pt: V2, o: V2, t: V2, delta: f32, reach: f32) -> Vec<V2> {
    let a = edge_pt + o * STUB;
    let mut pts = vec![edge_pt, a];
    if delta.abs() > 0.5 {
        // 45° diagonal: equal outward and sideways travel. Clip it to the
        // board edge if the lane is further away than the room left.
        let diag = delta.abs().min(reach - STUB);
        let b = a + o * diag + t * (diag * delta.signum());
        pts.push(b);
        if diag < delta.abs() {
            return pts;
        }
    }
    let last = *pts.last().unwrap();
    let remaining = reach - (last - edge_pt).dot(o);
    pts.push(last + o * remaining.max(0.0));
    pts
}

struct Raster {
    w: usize,
    h: usize,
    dist: Vec<f32>,
    packed: Vec<u32>,
}

impl Raster {
    fn new(w: usize, h: usize) -> Self {
        let n = w * h;
        Raster { w, h, dist: vec![f32::MAX; n], packed: vec![DIST_NONE << 13; n] }
    }

    /// Record, for every pixel within HALO of the trace, the nearest trace's
    /// along-distance, seeds and perpendicular distance.
    fn trace(&mut self, tr: &Trace) {
        let mut acc = 0.0;
        for seg in tr.pts.windows(2) {
            let (p, q) = (seg[0], seg[1]);
            let d = q - p;
            let len = d.len();
            if len < 1e-3 {
                continue;
            }
            let dir = d * (1.0 / len);
            let x0 = (p.x.min(q.x) - HALO).floor().max(0.0) as usize;
            let x1 = (p.x.max(q.x) + HALO).ceil().min(self.w as f32 - 1.0) as usize;
            let y0 = (p.y.min(q.y) - HALO).floor().max(0.0) as usize;
            let y1 = (p.y.max(q.y) + HALO).ceil().min(self.h as f32 - 1.0) as usize;
            if x0 > x1 || y0 > y1 {
                acc += len;
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let c = v(x as f32 + 0.5, y as f32 + 0.5);
                    let s = (c - p).dot(dir).clamp(0.0, len);
                    let dd = (c - (p + dir * s)).len();
                    let i = y * self.w + x;
                    if dd < HALO && dd < self.dist[i] {
                        self.dist[i] = dd;
                        self.packed[i] = pack(acc + s, dd, tr.seed, tr.bus_seed, tr.filler);
                    }
                }
            }
            acc += len;
        }
    }
}

/// Coarse occupancy grid so filler traces and parts stay out of the buses.
struct Occ {
    cw: usize,
    ch: usize,
    cells: Vec<bool>,
}

impl Occ {
    fn new(w: f32, h: f32) -> Self {
        let cw = (w / CELL).ceil() as usize;
        let ch = (h / CELL).ceil() as usize;
        Occ { cw, ch, cells: vec![false; cw * ch] }
    }
    fn idx(&self, p: V2) -> Option<usize> {
        if p.x < 0.0 || p.y < 0.0 {
            return None;
        }
        let (cx, cy) = ((p.x / CELL) as usize, (p.y / CELL) as usize);
        (cx < self.cw && cy < self.ch).then(|| cy * self.cw + cx)
    }
    fn mark_rect(&mut self, x: f32, y: f32, w: f32, h: f32, margin: f32) {
        let x0 = ((x - margin) / CELL).floor().max(0.0) as usize;
        let y0 = ((y - margin) / CELL).floor().max(0.0) as usize;
        let x1 = (((x + w + margin) / CELL).ceil() as usize).min(self.cw);
        let y1 = (((y + h + margin) / CELL).ceil() as usize).min(self.ch);
        for cy in y0..y1 {
            for cx in x0..x1 {
                self.cells[cy * self.cw + cx] = true;
            }
        }
    }
    fn mark_poly(&mut self, pts: &[V2], margin: f32) {
        for seg in pts.windows(2) {
            let d = seg[1] - seg[0];
            let steps = (d.len() / (CELL * 0.5)).ceil().max(1.0) as usize;
            for k in 0..=steps {
                let p = seg[0] + d * (k as f32 / steps as f32);
                self.mark_rect(p.x, p.y, 0.0, 0.0, margin);
            }
        }
    }
    fn free(&self, p: V2) -> bool {
        self.idx(p).is_some_and(|i| !self.cells[i])
    }
}

/// Colors are straight sRGB 0..1 (tiny-skia's own convention).
#[derive(Clone, Copy)]
struct Palette {
    pub accent: [f32; 3],
    pub bg: [f32; 3],
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn paint(c: [f32; 3], alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(tiny_skia::Color::from_rgba(c[0], c[1], c[2], alpha).unwrap());
    p.anti_alias = true;
    p
}

fn stroke(w: f32) -> Stroke {
    Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Miter, miter_limit: 4.0, ..Default::default() }
}

fn polyline(pts: &[V2]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].x, pts[0].y);
    for p in &pts[1..] {
        pb.line_to(p.x, p.y);
    }
    pb.finish()
}

fn rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<tiny_skia::Path> {
    Rect::from_xywh(x, y, w, h).map(PathBuilder::from_rect)
}

fn circle(c: V2, r: f32) -> Option<tiny_skia::Path> {
    PathBuilder::from_circle(c.x, c.y, r)
}

pub fn generate(w: u32, h: u32) -> Board {
    // Draw in grayscale: background = black, accent = white, so each pixel's
    // value is the blend factor the shader maps onto the live theme colors.
    let pal = Palette { accent: [1.0, 1.0, 1.0], bg: [0.0, 0.0, 0.0] };
    let (wf, hf) = (w as f32, h as f32);
    // Same board every time for a given output size — like real hardware.
    let mut rng = StdRng::seed_from_u64(0x0_6a3e_9a ^ ((w as u64) << 20) ^ h as u64);
    let center = v((wf * 0.5).round(), (hf * 0.5).round());
    let cpu = (hf.min(wf) * 0.30).clamp(220.0, 560.0).round();
    let half = cpu * 0.5;

    let mut traces: Vec<Trace> = Vec::new();
    let mut chips: Vec<Chip> = Vec::new();
    let mut vias: Vec<V2> = Vec::new();
    let mut occ = Occ::new(wf, hf);
    occ.mark_rect(center.x - half, center.y - half, cpu, cpu, 14.0);

    // --- buses ---------------------------------------------------------------
    // (outward dir, tangent dir, distance from CPU edge to board edge, lane half-span)
    let sides = [
        (v(1.0, 0.0), v(0.0, 1.0), wf - center.x - half, hf * 0.5 - 30.0),
        (v(-1.0, 0.0), v(0.0, 1.0), center.x - half, hf * 0.5 - 30.0),
        (v(0.0, -1.0), v(1.0, 0.0), center.y - half, wf * 0.5 - 30.0),
        (v(0.0, 1.0), v(1.0, 0.0), hf - center.y - half, wf * 0.5 - 30.0),
    ];
    for (o, t, reach, span) in sides {
        let margin = 26.0;
        let usable = cpu - margin * 2.0;
        let n_pins = (usable / PITCH).floor() as i32;
        // Partition pins into buses of 4..10 traces with 2-pin gaps.
        let mut groups: Vec<(i32, i32)> = Vec::new(); // (first pin, count)
        let mut p = 0;
        while p < n_pins {
            let count = rng.gen_range(4..=10).min(n_pins - p);
            if count >= 2 {
                groups.push((p, count));
            }
            p += count + 2;
        }
        let pin_v = |pin: f32| -usable * 0.5 + (pin + 0.5) * (usable / n_pins as f32);
        // Long sides spread wide; short (top/bottom on ultrawide) spread up to
        // what the vertical room allows, so diagonals mostly finish on-board.
        let spread = (span / (usable * 0.5)).min(1.0 + (reach - STUB) / (usable * 0.5)).max(1.0);
        for &(first, count) in &groups {
            let c = pin_v(first as f32 + (count - 1) as f32 * 0.5);
            let lane = (c * spread).clamp(-span, span);
            let edge_pt = center + o * half + t * c;
            let center_line = route(edge_pt, o, t, lane - c, reach);
            let bus_seed: f32 = rng.gen();
            // Some buses end at a peripheral chip part-way out.
            let total_out = (center_line.last().unwrap().clone() - edge_pt).dot(o);
            let ends_at_chip = total_out > 600.0 && rng.gen_bool(0.45);
            let mut line = center_line.clone();
            if ends_at_chip {
                let diag_end = (line[line.len() - 2] - edge_pt).dot(o);
                let stop = rng.gen_range((diag_end + 120.0).min(total_out - 80.0)..(total_out - 80.0));
                let last = line.len() - 1;
                line[last] = edge_pt + o * stop + t * (line[last] - edge_pt).dot(t);
                let bw = count as f32 * PITCH + 16.0;
                let depth = rng.gen_range(40.0..90.0);
                let tip = line[last];
                let (cx, cy, cw, ch) = if o.x != 0.0 {
                    let x = if o.x > 0.0 { tip.x } else { tip.x - depth };
                    (x, tip.y - bw * 0.5, depth, bw)
                } else {
                    let y = if o.y > 0.0 { tip.y } else { tip.y - depth };
                    (tip.x - bw * 0.5, y, bw, depth)
                };
                chips.push(Chip { rect: (cx, cy, cw, ch), pins_lr: o.x != 0.0 });
                occ.mark_rect(cx, cy, cw, ch, 8.0);
                // A thinner bus carries on from the far side to the board edge.
                let far = tip + o * depth;
                let rest = (reach - stop - depth).max(0.0);
                if rest > 60.0 {
                    let k = (count / 2).max(2);
                    let cont = vec![far, far + o * rest];
                    let cont_seed: f32 = rng.gen();
                    for j in 0..k {
                        let off = (j as f32 - (k - 1) as f32 * 0.5) * PITCH;
                        let pts = offset(&cont, off);
                        occ.mark_poly(&pts, 6.0);
                        traces.push(Trace { pts, width: BUS_W, seed: rng.gen(), bus_seed: cont_seed, filler: false });
                    }
                }
            }
            for j in 0..count {
                // `perp` of the outward direction is the tangent's sign-flipped
                // twin on some sides; offset in tangent order either way.
                let sign = o.perp().dot(t);
                let off = (j as f32 - (count - 1) as f32 * 0.5) * PITCH * sign;
                let pts = offset(&line, off);
                occ.mark_poly(&pts, 6.0);
                if !ends_at_chip && rng.gen_bool(0.12) {
                    // Occasional via mid-run for texture.
                    let s = pts.len() - 1;
                    let at = pts[s - 1] + (pts[s] - pts[s - 1]) * rng.gen_range(0.3..0.8);
                    vias.push(at);
                }
                traces.push(Trace { pts, width: BUS_W, seed: rng.gen(), bus_seed, filler: false });
            }
        }
    }

    // --- filler: small parts and short octilinear traces between vias --------
    let dirs8 = [
        v(1.0, 0.0), v(1.0, 1.0), v(0.0, 1.0), v(-1.0, 1.0),
        v(-1.0, 0.0), v(-1.0, -1.0), v(0.0, -1.0), v(1.0, -1.0),
    ];
    let mut smd: Vec<(f32, f32, f32, f32)> = Vec::new();
    let target_area = (wf * hf) / 9000.0;
    let mut placed = 0;
    let mut tries = 0;
    while placed < target_area as usize && tries < 40_000 {
        tries += 1;
        let start = v(rng.gen_range(8.0..wf - 8.0), rng.gen_range(8.0..hf - 8.0));
        if !occ.free(start) {
            continue;
        }
        if rng.gen_bool(0.25) {
            // SMD part (resistor/cap footprint).
            let (pw, ph) = if rng.gen_bool(0.5) { (12.0, 6.0) } else { (6.0, 12.0) };
            if occ.free(start + v(pw, ph)) && occ.free(start + v(pw, 0.0)) && occ.free(start + v(0.0, ph)) {
                smd.push((start.x, start.y, pw, ph));
                occ.mark_rect(start.x, start.y, pw, ph, 4.0);
                placed += 1;
            }
            continue;
        }
        let mut pts = vec![start];
        let mut dir = rng.gen_range(0..8usize);
        let mut pos = start;
        for _ in 0..rng.gen_range(2..7) {
            let step = dirs8[dir].norm() * (CELL * rng.gen_range(3..14) as f32);
            let n = 8;
            let mut ok = true;
            for k in 1..=n {
                if !occ.free(pos + step * (k as f32 / n as f32)) {
                    ok = false;
                    break;
                }
            }
            if !ok {
                break;
            }
            pos = pos + step;
            pts.push(pos);
            // Turn by ±45° (PCB-style), never reverse.
            dir = (dir + [1, 7][rng.gen_range(0..2)]) % 8;
        }
        if pts.len() >= 2 {
            occ.mark_poly(&pts, 4.0);
            vias.push(start);
            vias.push(*pts.last().unwrap());
            traces.push(Trace { pts, width: FILL_W, seed: rng.gen(), bus_seed: rng.gen(), filler: true });
            placed += 1;
        }
    }

    // --- static layer --------------------------------------------------------
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    let base = mix(pal.bg, pal.accent, 0.035);
    pm.fill(tiny_skia::Color::from_rgba(base[0], base[1], base[2], 1.0).unwrap());
    let id = Transform::identity();

    // Faint fabrication grid.
    let grid = paint(mix(base, pal.accent, 0.10), 1.0);
    let mut pb = PathBuilder::new();
    let gs = 48.0;
    let mut x = (center.x % gs) - gs;
    while x < wf {
        pb.move_to(x, 0.0);
        pb.line_to(x, hf);
        x += gs;
    }
    let mut y = (center.y % gs) - gs;
    while y < hf {
        pb.move_to(0.0, y);
        pb.line_to(wf, y);
        y += gs;
    }
    if let Some(p) = pb.finish() {
        pm.stroke_path(&p, &grid, &stroke(1.0), id, None);
    }

    let copper = mix(base, pal.accent, 0.22);
    let copper_f = mix(base, pal.accent, 0.14);
    for tr in &traces {
        if let Some(p) = polyline(&tr.pts) {
            let c = if tr.filler { copper_f } else { copper };
            pm.stroke_path(&p, &paint(c, 1.0), &stroke(tr.width), id, None);
        }
    }

    let pad = paint(mix(base, pal.accent, 0.40), 1.0);
    let hole = paint(base, 1.0);
    for via in &vias {
        if let Some(p) = circle(*via, 3.4) {
            pm.fill_path(&p, &pad, FillRule::Winding, id, None);
        }
        if let Some(p) = circle(*via, 1.4) {
            pm.fill_path(&p, &hole, FillRule::Winding, id, None);
        }
    }

    let part_fill = paint(mix(base, pal.accent, 0.07), 1.0);
    let part_edge = paint(mix(base, pal.accent, 0.32), 1.0);
    for &(x, y, pw, ph) in &smd {
        if let Some(p) = rect_path(x, y, pw, ph) {
            pm.fill_path(&p, &part_fill, FillRule::Winding, id, None);
        }
        // End caps.
        let (a, b) = if pw > ph { ((x, y, 3.0, ph), (x + pw - 3.0, y, 3.0, ph)) } else { ((x, y, pw, 3.0), (x, y + ph - 3.0, pw, 3.0)) };
        for (cx, cy, cw, ch) in [a, b] {
            if let Some(p) = rect_path(cx, cy, cw, ch) {
                pm.fill_path(&p, &part_edge, FillRule::Winding, id, None);
            }
        }
    }

    for chip in &chips {
        let (x, y, cw, ch) = chip.rect;
        if let Some(p) = rect_path(x, y, cw, ch) {
            pm.fill_path(&p, &paint(mix(base, pal.bg, 0.6), 1.0), FillRule::Winding, id, None);
            pm.stroke_path(&p, &part_edge, &stroke(1.4), id, None);
        }
        // Pin stubs along the two long edges facing the bus.
        let pins = paint(mix(base, pal.accent, 0.30), 1.0);
        if chip.pins_lr {
            let n = (ch / PITCH) as i32;
            for i in 0..n {
                let py = y + (i as f32 + 0.5) * (ch / n as f32) - 1.5;
                for px in [x - 4.0, x + cw] {
                    if let Some(p) = rect_path(px, py, 4.0, 3.0) {
                        pm.fill_path(&p, &pins, FillRule::Winding, id, None);
                    }
                }
            }
        } else {
            let n = (cw / PITCH) as i32;
            for i in 0..n {
                let px = x + (i as f32 + 0.5) * (cw / n as f32) - 1.5;
                for py in [y - 4.0, y + ch] {
                    if let Some(p) = rect_path(px, py, 3.0, 4.0) {
                        pm.fill_path(&p, &pins, FillRule::Winding, id, None);
                    }
                }
            }
        }
        // Silkscreen dot (pin 1).
        if let Some(p) = circle(v(x + 6.0, y + 6.0), 1.6) {
            pm.fill_path(&p, &part_edge, FillRule::Winding, id, None);
        }
    }

    // CPU package: substrate, bevel rings, pin pads, die window.
    let (px, py) = (center.x - half, center.y - half);
    if let Some(p) = rect_path(px, py, cpu, cpu) {
        pm.fill_path(&p, &paint(mix(base, pal.accent, 0.06), 1.0), FillRule::Winding, id, None);
        pm.stroke_path(&p, &paint(mix(base, pal.accent, 0.55), 1.0), &stroke(2.0), id, None);
    }
    for (k, a) in [(10.0, 0.20), (18.0, 0.12)] {
        if let Some(p) = rect_path(px + k, py + k, cpu - 2.0 * k, cpu - 2.0 * k) {
            pm.stroke_path(&p, &paint(mix(base, pal.accent, a), 1.0), &stroke(1.0), id, None);
        }
    }
    let die = cpu * 0.62;
    let (dx, dy) = (center.x - die * 0.5, center.y - die * 0.5);
    if let Some(p) = rect_path(dx, dy, die, die) {
        pm.fill_path(&p, &paint(mix(pal.bg, pal.accent, 0.015), 1.0), FillRule::Winding, id, None);
        pm.stroke_path(&p, &paint(mix(base, pal.accent, 0.35), 1.0), &stroke(1.5), id, None);
    }
    // Pin-1 notch.
    let mut pb = PathBuilder::new();
    pb.move_to(px + 4.0, py + 4.0);
    pb.line_to(px + 22.0, py + 4.0);
    pb.line_to(px + 4.0, py + 22.0);
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(mix(base, pal.accent, 0.5), 1.0), FillRule::Winding, id, None);
    }

    // --- data layer ----------------------------------------------------------
    let mut raster = Raster::new(w as usize, h as usize);
    for tr in &traces {
        raster.trace(tr);
    }

    Board {
        // Gray → one channel (opaque, so premultiplied R is the value).
        static_k: pm.data().chunks_exact(4).map(|px| px[0]).collect(),
        data: raster.packed,
        cpu_center: center,
        cpu_size: cpu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_layout() {
        let p = pack(1234.4, HALO * 0.5, 0.5, 0.99, true);
        assert_eq!(p & 0x1fff, 1234);
        assert_eq!((p >> 13) & 63, 31);
        assert_eq!((p >> 19) & 63, 32);
        assert_eq!((p >> 25) & 63, 63);
        assert_eq!(p >> 31, 1);
        assert_eq!((pack(0.0, HALO * 2.0, 0.0, 0.0, false) >> 13) & 63, 62);
    }

    #[test]
    fn offset_keeps_spacing_on_45_degree_bend() {
        let line = vec![v(0.0, 0.0), v(100.0, 0.0), v(200.0, 100.0), v(300.0, 100.0)];
        let a = offset(&line, 9.0);
        // Straight sections end up exactly 9px away.
        assert!((a[0].y - 9.0).abs() < 1e-3);
        assert!((a[3].y - 109.0).abs() < 1e-3);
    }

    #[test]
    fn generates_both_output_sizes() {
        for (w, h) in [(2560, 1440), (5120, 1440)] {
            let b = generate(w, h);
            assert_eq!(b.static_k.len(), (w * h) as usize);
            assert_eq!(b.data.len(), (w * h) as usize);
            let traced = b.data.iter().filter(|t| (*t >> 13) & 63 != DIST_NONE).count();
            assert!(traced > (w * h) as usize / 50, "too few trace pixels: {traced}");
            // Longest trace must fit the 13-bit along field without wrapping.
            let max_along = b.data.iter().filter(|t| (*t >> 13) & 63 != DIST_NONE).map(|t| t & 0x1fff).max().unwrap();
            assert!(max_along < 8000, "along {max_along}");
        }
    }
}
