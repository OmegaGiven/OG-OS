//! Orbit camera around a target. Distance is stored relative to the
//! "fit" distance (the one that frames the scene's bounding sphere for the
//! current aspect ratio), so resizing the view keeps the model framed.

use glam::{Mat4, Vec3};

pub const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
const DEFAULT_YAW: f32 = 0.6;
const DEFAULT_PITCH: f32 = 0.42;
const PITCH_LIMIT: f32 = 1.55;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    /// Multiplier on the fit distance (1 = framed).
    pub zoom: f32,
    /// Scene bounding sphere (world space).
    pub center: Vec3,
    pub radius: f32,
}

impl Camera {
    /// Camera framing the world-space box `min..max`.
    pub fn fit(min: Vec3, max: Vec3) -> Self {
        let center = (min + max) * 0.5;
        let r = ((max - min).length() * 0.5).max(1e-6);
        Self {
            target: center,
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            zoom: 1.0,
            center,
            radius: if r.is_finite() { r } else { 1.0 },
        }
    }

    /// Distance at which the bounding sphere fills the narrower FOV.
    pub fn fit_distance(&self, aspect: f32) -> f32 {
        let half_y = FOV_Y * 0.5;
        let half_x = ((half_y).tan() * aspect.max(1e-3)).atan();
        self.radius / half_y.min(half_x).sin() * 1.05
    }

    pub fn distance(&self, aspect: f32) -> f32 {
        self.fit_distance(aspect) * self.zoom
    }

    /// Unit vector from target to eye.
    fn dir(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * sy, sp, cp * cy)
    }

    pub fn eye(&self, aspect: f32) -> Vec3 {
        self.target + self.dir() * self.distance(aspect)
    }

    /// (right, up) camera basis vectors.
    pub fn basis(&self) -> (Vec3, Vec3) {
        let fwd = -self.dir();
        let right = fwd.cross(Vec3::Y).try_normalize().unwrap_or(Vec3::X);
        (right, right.cross(fwd))
    }

    pub fn near_far(&self, aspect: f32) -> (f32, f32) {
        let d = self.distance(aspect);
        let reach = (self.target - self.center).length() + self.radius * 3.0;
        ((d - reach).max(d * 0.02).max(1e-6), d + reach)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let (near, far) = self.near_far(aspect);
        let proj = Mat4::perspective_rh(FOV_Y, aspect.max(1e-3), near, far);
        proj * Mat4::look_at_rh(self.eye(aspect), self.target, Vec3::Y)
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.01;
        self.pitch = (self.pitch + dy * 0.01).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// World-space translation for a drag of (dx, dy) pixels in a view
    /// `height` pixels tall, so the point under the cursor follows it.
    pub fn pan_delta(&self, dx: f32, dy: f32, height: f32, aspect: f32) -> Vec3 {
        let per_px = 2.0 * self.distance(aspect) * (FOV_Y * 0.5).tan() / height.max(1.0);
        let (right, up) = self.basis();
        (-dx * right + dy * up) * per_px
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.zoom = (self.zoom * factor).clamp(0.01, 50.0);
    }
}
