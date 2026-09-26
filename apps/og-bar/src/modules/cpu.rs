use iced::widget::{button, container};
use iced::{Background, Border, Color, Element, Length, Subscription};

use crate::message::Message;
use crate::module::{label_value, poll_changes, Module, Orientation};
use og_theme::AppColors;

const WARNING_PCT: f32 = 70.0;
const CRITICAL_PCT: f32 = 90.0;
const WARNING_COLOR: Color = Color::from_rgb(0.85, 0.62, 0.2);
const CRITICAL_COLOR: Color = Color::from_rgb(0.85, 0.27, 0.27);

fn read_cpu_totals() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let fields: Vec<u64> = line.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().sum();
    Some((idle, total))
}

/// Usage since the previous call, rounded to what the bar displays.
fn sample_usage(prev: &mut Option<(u64, u64)>) -> Option<u32> {
    let (idle, total) = read_cpu_totals()?;
    let mut pct = None;
    if let Some((prev_idle, prev_total)) = *prev {
        let idle_delta = idle.saturating_sub(prev_idle) as f32;
        let total_delta = total.saturating_sub(prev_total) as f32;
        if total_delta > 0.0 {
            pct = Some(((1.0 - idle_delta / total_delta) * 100.0).round() as u32);
        }
    }
    *prev = Some((idle, total));
    pct
}

pub struct Cpu {
    usage_pct: f32,
}

impl Cpu {
    pub fn new() -> Self {
        Self { usage_pct: 0.0 }
    }
}

impl Module for Cpu {
    fn cell_length(&self, _size: u32, _orientation: Orientation) -> Length {
        Length::Shrink
    }

    fn view(&self, colors: AppColors, size: u32, orientation: Orientation) -> Element<'_, Message> {
        let fg = if self.usage_pct >= CRITICAL_PCT {
            CRITICAL_COLOR
        } else if self.usage_pct >= WARNING_PCT {
            WARNING_COLOR
        } else {
            colors.text
        };
        button(container(label_value("CPU", format!("{:.0}%", self.usage_pct), fg, size, orientation)))
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
        .on_press(Message::Launch("alacritty -e htop".to_string()))
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut prev = read_cpu_totals();
        poll_changes(
            "cpu",
            std::time::Duration::from_secs(3),
            move || sample_usage(&mut prev),
            |pct| Message::CpuUsage(pct.unwrap_or(0)),
        )
    }

    fn update(&mut self, message: &Message) {
        if let Message::CpuUsage(pct) = message {
            self.usage_pct = *pct as f32;
        }
    }
}
