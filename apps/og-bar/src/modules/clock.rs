use iced::widget::{column, container, row, text};
use iced::{Element, Length, Subscription};

use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

/// `/etc/localtime` is a symlink into the zoneinfo database on every distro
/// that matters here — reading it back out gets us the IANA name for
/// "local time" so it can go through chrono-tz too and get a real `%Z`
/// abbreviation (EST/CST/etc), instead of chrono::Local's offset-only one.
fn detect_system_tz() -> Option<chrono_tz::Tz> {
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let path = link.to_string_lossy();
    let name = path.split("zoneinfo/").nth(1)?;
    name.parse().ok()
}

pub struct Clock {
    /// Empty means local time (auto-detected via /etc/localtime for a
    /// proper %Z abbreviation). Anything else is parsed as an IANA tz name
    /// (e.g. "America/Chicago") on every tick — invalid names fall back to
    /// local time rather than showing garbage.
    timezone: String,
    hour12: bool,
    show_timezone: bool,
    show_date: bool,
}

impl Clock {
    pub fn new(timezone: String, hour12: bool, show_timezone: bool, show_date: bool) -> Self {
        Self { timezone, hour12, show_timezone, show_date }
    }

    /// date / time / AM-PM / tz abbreviation, as separate segments — AM/PM
    /// gets its own segment rather than sharing a line with the time
    /// (`"%I:%M %p"` as one string) specifically so a narrow vertical bar
    /// never wraps mid-string and splits "05:32 PM" across two lines at
    /// an arbitrary point; "05:32" now always stays intact on its own
    /// line, with "PM" cleanly on the next.
    fn segments(&self) -> ClockSegments {
        let fmt = if self.hour12 { "%m/%d\n%I:%M\n%p\n%Z" } else { "%m/%d\n%H:%M\n%Z" };

        let explicit_tz = if self.timezone.is_empty() { None } else { self.timezone.parse::<chrono_tz::Tz>().ok() };

        let formatted = match explicit_tz.or_else(detect_system_tz) {
            Some(tz) => chrono::Utc::now().with_timezone(&tz).format(fmt).to_string(),
            None => chrono::Local::now().format(fmt).to_string(),
        };
        let mut parts = formatted.split('\n');
        let date = parts.next().unwrap_or_default().to_string();
        let time = parts.next().unwrap_or_default().to_string();
        if self.hour12 {
            let ampm = parts.next().unwrap_or_default().to_string();
            let tz = parts.next().unwrap_or_default().to_string();
            ClockSegments { date, time, ampm: Some(ampm), tz }
        } else {
            let tz = parts.next().unwrap_or_default().to_string();
            ClockSegments { date, time, ampm: None, tz }
        }
    }
}

struct ClockSegments {
    date: String,
    time: String,
    ampm: Option<String>,
    tz: String,
}

impl Module for Clock {
    fn cell_length(&self, _size: u32, _orientation: Orientation) -> Length {
        // A fixed size×size square (the trait default) fits the old
        // 3-line stacked layout but clips a horizontal row of 3 segments
        // down to almost nothing — this needs to shrink to its own
        // content instead, same as cpu/memory's cell.
        Length::Shrink
    }

    fn view(&self, colors: AppColors, _size: u32, orientation: Orientation) -> Element<'_, Message> {
        let text_style = move |_: &_| text::Style { color: Some(colors.text) };
        let seg = self.segments();
        let widget = |s: String| -> Element<'_, Message> { text(s).size(11).style(text_style).into() };

        let content: Element<'_, Message> = match orientation {
            // Top/bottom bar: room for one line, natural reading order.
            Orientation::Horizontal => {
                let mut parts = Vec::new();
                if self.show_date { parts.push(widget(seg.date)); }
                parts.push(widget(seg.time));
                if let Some(ampm) = seg.ampm { parts.push(widget(ampm)); }
                if self.show_timezone { parts.push(widget(seg.tz)); }
                row(parts).spacing(6).align_y(iced::Alignment::Center).into()
            }
            // Left/right bar: date on top, time (always intact, never
            // sharing a line with AM/PM), AM/PM, timezone at the bottom.
            Orientation::Vertical => {
                let mut parts = Vec::new();
                if self.show_date { parts.push(widget(seg.date)); }
                parts.push(widget(seg.time));
                if let Some(ampm) = seg.ampm { parts.push(widget(ampm)); }
                if self.show_timezone { parts.push(widget(seg.tz)); }
                column(parts).align_x(iced::Alignment::Center).into()
            }
        };

        container(content).padding(4).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        // Same id for every Clock instance -> one shared timer.
        Subscription::run_with_id("clock-minute", minute_stream())
    }
}

/// Fires once per wall-clock minute, right after it turns over — the clock
/// has no seconds field, so a 1 s tick was 59 wasted redraws a minute.
/// Sleeps are capped at 5 s because tokio's timer is monotonic and doesn't
/// advance across suspend; the cap bounds how stale the clock can be after
/// resume without adding redraws (nothing is sent until the minute changes).
fn minute_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(1, |mut sender| async move {
        use iced::futures::SinkExt;

        let minute = || chrono::Utc::now().timestamp().div_euclid(60);
        let mut last = minute();
        loop {
            let now = chrono::Utc::now();
            let into_minute_ms = (now.timestamp().rem_euclid(60) * 1000) as u64 + now.timestamp_subsec_millis() as u64;
            let until_next_ms = 60_000 - into_minute_ms + 20;
            tokio::time::sleep(std::time::Duration::from_millis(until_next_ms.min(5_000))).await;
            let current = minute();
            if current != last {
                last = current;
                if sender.send(Message::ClockMinute).await.is_err() {
                    return;
                }
            }
        }
    })
}
