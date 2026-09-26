use iced::widget::container;
use iced::{Color, Element, Length, Subscription};

use crate::message::Message;
use crate::module::{label_value, poll_changes, Module, Orientation};
use og_theme::AppColors;

const WARNING_PCT: f32 = 75.0;
const CRITICAL_PCT: f32 = 90.0;
const WARNING_COLOR: Color = Color::from_rgb(0.85, 0.62, 0.2);
const CRITICAL_COLOR: Color = Color::from_rgb(0.85, 0.27, 0.27);

fn read_mem_pct() -> Option<f32> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.trim().split_whitespace().next()?.parse::<f64>().ok();
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.trim().split_whitespace().next()?.parse::<f64>().ok();
        }
    }
    let (total, available) = (total?, available?);
    if total <= 0.0 {
        return None;
    }
    Some(((total - available) / total * 100.0) as f32)
}

pub struct Memory {
    usage_pct: f32,
}

impl Memory {
    pub fn new() -> Self {
        Self { usage_pct: read_mem_pct().unwrap_or(0.0) }
    }
}

impl Module for Memory {
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
        container(label_value("RAM", format!("{:.0}%", self.usage_pct), fg, size, orientation))
            .padding(4)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        poll_changes(
            "memory",
            std::time::Duration::from_secs(5),
            || read_mem_pct().map(|pct| pct.round() as u32),
            |pct| Message::MemoryUsage(pct.unwrap_or(0)),
        )
    }

    fn update(&mut self, message: &Message) {
        if let Message::MemoryUsage(pct) = message {
            self.usage_pct = *pct as f32;
        }
    }
}
