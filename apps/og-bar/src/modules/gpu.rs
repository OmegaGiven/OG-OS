use iced::widget::{button, container};
use iced::{Background, Border, Color, Element, Length, Subscription};

use crate::message::Message;
use crate::module::{label_value, poll_changes, Module, Orientation};
use og_theme::AppColors;

const WARNING_PCT: f32 = 80.0;
const CRITICAL_PCT: f32 = 95.0;
const WARNING_COLOR: Color = Color::from_rgb(0.85, 0.62, 0.2);
const CRITICAL_COLOR: Color = Color::from_rgb(0.85, 0.27, 0.27);

#[derive(Clone, Copy, Default)]
struct Reading {
    usage_pct: f32,
    vram_used_gb: f32,
    vram_total_gb: f32,
    temp_c: f32,
}

/// Which backend to read from, picked once at startup — the same binary
/// has to work on both AMD and NVIDIA machines, so this can't be a
/// build-time choice.
enum Backend {
    /// sysfs under a specific `/sys/class/drm/cardN/device` — resolved once
    /// so a later re-enumeration (replug, driver reload) can't silently
    /// switch which GPU this reads.
    Amdgpu { dir: std::path::PathBuf, hwmon_temp: Option<std::path::PathBuf> },
    Nvidia,
    /// No usable GPU sensor found — module still renders (as "GPU --") so
    /// it doesn't look broken, rather than disappearing from the bar.
    None,
}

fn find_amdgpu() -> Option<Backend> {
    let root = std::path::Path::new("/sys/class/drm");
    let rd = std::fs::read_dir(root).ok()?;
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Only bare "cardN" — skip "cardN-DP-1" connector nodes and renderD*.
        if !name.starts_with("card") || name[4..].contains('-') {
            continue;
        }
        let dir = entry.path().join("device");
        let driver = std::fs::read_to_string(dir.join("uevent")).unwrap_or_default();
        if !driver.contains("DRIVER=amdgpu") {
            continue;
        }
        if !dir.join("gpu_busy_percent").exists() {
            continue;
        }
        let hwmon_temp = std::fs::read_dir(dir.join("hwmon"))
            .ok()
            .and_then(|rd| rd.flatten().next())
            .map(|e| e.path().join("temp1_input"))
            .filter(|p| p.exists());
        return Some(Backend::Amdgpu { dir, hwmon_temp });
    }
    None
}

fn detect_backend() -> Backend {
    if let Some(b) = find_amdgpu() {
        return b;
    }
    if std::process::Command::new("nvidia-smi").arg("-L").output().map(|o| o.status.success()).unwrap_or(false) {
        return Backend::Nvidia;
    }
    Backend::None
}

fn read_u64(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_amdgpu(dir: &std::path::Path, hwmon_temp: &Option<std::path::PathBuf>) -> Option<Reading> {
    let usage_pct = read_u64(&dir.join("gpu_busy_percent"))? as f32;
    let used = read_u64(&dir.join("mem_info_vram_used")).unwrap_or(0) as f32;
    let total = read_u64(&dir.join("mem_info_vram_total")).unwrap_or(0) as f32;
    let temp_c = hwmon_temp.as_ref().and_then(|p| read_u64(p)).map(|milli| milli as f32 / 1000.0).unwrap_or(0.0);
    const GIB: f32 = 1024.0 * 1024.0 * 1024.0;
    Some(Reading { usage_pct, vram_used_gb: used / GIB, vram_total_gb: total / GIB, temp_c })
}

fn read_nvidia() -> Option<Reading> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let first = line.lines().next()?;
    let mut fields = first.split(',').map(|s| s.trim());
    let usage_pct: f32 = fields.next()?.parse().ok()?;
    let used_mib: f32 = fields.next()?.parse().ok()?;
    let total_mib: f32 = fields.next()?.parse().ok()?;
    let temp_c: f32 = fields.next()?.parse().unwrap_or(0.0);
    Some(Reading { usage_pct, vram_used_gb: used_mib / 1024.0, vram_total_gb: total_mib / 1024.0, temp_c })
}

fn sample(backend: &Backend) -> Option<Reading> {
    match backend {
        Backend::Amdgpu { dir, hwmon_temp } => read_amdgpu(dir, hwmon_temp),
        Backend::Nvidia => read_nvidia(),
        Backend::None => None,
    }
}

/// A reading rounded to exactly what the bar shows, so sub-display jitter
/// (temperature, VRAM bytes) doesn't count as a change worth a redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuSample {
    usage_pct: u32,
    /// VRAM used in tenths of a GiB; `None` when total VRAM is unknown.
    vram_tenths_gb: Option<u32>,
}

impl From<Reading> for GpuSample {
    fn from(r: Reading) -> Self {
        Self {
            usage_pct: r.usage_pct.round() as u32,
            vram_tenths_gb: (r.vram_total_gb > 0.0).then(|| (r.vram_used_gb * 10.0).round() as u32),
        }
    }
}

pub struct Gpu {
    /// `None` until the first sample arrives, or when no GPU sensor exists.
    sample: Option<GpuSample>,
}

impl Gpu {
    pub fn new() -> Self {
        Self { sample: None }
    }
}

impl Module for Gpu {
    fn cell_length(&self, _size: u32, _orientation: Orientation) -> Length {
        Length::Shrink
    }

    fn view(&self, colors: AppColors, size: u32, orientation: Orientation) -> Element<'_, Message> {
        let (fg, value) = match self.sample {
            None => (colors.dim_text, "--".to_string()),
            Some(s) => {
                let pct = s.usage_pct as f32;
                let fg = if pct >= CRITICAL_PCT {
                    CRITICAL_COLOR
                } else if pct >= WARNING_PCT {
                    WARNING_COLOR
                } else {
                    colors.text
                };
                let value = match s.vram_tenths_gb {
                    Some(t) => format!("{}%  {}.{}G", s.usage_pct, t / 10, t % 10),
                    None => format!("{}%", s.usage_pct),
                };
                (fg, value)
            }
        };
        button(container(label_value("GPU", value, fg, size, orientation)))
            .padding(4)
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                    colors.header_btn_bg
                } else {
                    Color::TRANSPARENT
                })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                text_color: fg,
                ..Default::default()
            })
            .on_press(Message::Launch("alacritty -e btop".to_string()))
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        // Backend detection (may shell out to nvidia-smi) happens once, on
        // the sampling thread, not while building the bar.
        let mut backend = None;
        poll_changes(
            "gpu",
            std::time::Duration::from_secs(2),
            move || sample(backend.get_or_insert_with(detect_backend)).map(GpuSample::from),
            Message::GpuUsage,
        )
    }

    fn update(&mut self, message: &Message) {
        if let Message::GpuUsage(sample) = message {
            self.sample = *sample;
        }
    }
}
