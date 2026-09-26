//! Plain-readable connected-hardware overview: USB, input peripherals,
//! Bluetooth, monitors, PCI. See `crate::devices` for the "status" caveats
//! (dmesg restricted on this system — driver-bound in sysfs is the signal).

use iced::widget::{column, container, row, text};
use iced::{Background, Border, Length};

use crate::app::{AppColors, Message};
use crate::devices::{InputDevice, PciDevice, UsbDevice};
use crate::sway::{BluetoothDevice, MonitorInfo};

type Element<'a> = iced::Element<'a, Message>;

fn status_pill(colors: AppColors, ok: bool, ok_label: &'static str, bad_label: &'static str) -> Element<'static> {
    let color = if ok { colors.accent } else { colors.dim_text };
    container(text(if ok { ok_label } else { bad_label }).size(11).style(move |_| text::Style { color: Some(color) }))
        .padding([2, 8])
        .style(move |_| container::Style {
            background: Some(Background::Color(color.scale_alpha(0.15))),
            border: Border { color, width: 1.0, radius: 999.0.into() },
            ..Default::default()
        })
        .into()
}

fn row_card<'a>(colors: AppColors, title: String, subtitle: String, pill: Element<'static>) -> Element<'a> {
    container(
        row![
            column![
                text(title).size(14).style(move |_| text::Style { color: Some(colors.text) }),
                text(subtitle).size(11).style(move |_| text::Style { color: Some(colors.dim_text) }),
            ]
            .spacing(2),
            iced::widget::horizontal_space(),
            pill,
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8)
        .padding(12),
    )
    .style(move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    })
    .width(Length::Fill)
    .into()
}

fn section<'a>(colors: AppColors, title: &'a str, rows: Vec<Element<'a>>) -> Element<'a> {
    if rows.is_empty() {
        return column![
            text(title).size(15).style(move |_| text::Style { color: Some(colors.text) }),
            text("None found.").size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
        ]
        .spacing(6)
        .into();
    }
    column![
        text(title).size(15).style(move |_| text::Style { color: Some(colors.text) }),
        column(rows).spacing(8),
    ]
    .spacing(10)
    .into()
}

pub fn view<'a>(
    colors: AppColors,
    usb: &'a [UsbDevice],
    input: &'a [InputDevice],
    bluetooth: &'a [BluetoothDevice],
    monitors: &'a [MonitorInfo],
    pci: &'a [PciDevice],
) -> Element<'a> {
    let usb_rows: Vec<Element> = usb
        .iter()
        .map(|d| {
            row_card(
                colors,
                d.description.clone(),
                format!("USB · bus {} device {} · {}:{}", d.bus, d.device_num, d.vendor_id, d.product_id),
                status_pill(colors, d.driver_bound, "OK", "No driver"),
            )
        })
        .collect();

    let input_rows: Vec<Element> = input
        .iter()
        .map(|d| {
            row_card(
                colors,
                d.name.clone(),
                "Input peripheral".to_string(),
                status_pill(colors, d.has_event_handler, "OK", "No event node"),
            )
        })
        .collect();

    let bt_rows: Vec<Element> = bluetooth
        .iter()
        .map(|d| {
            let status = if d.connected { "Connected" } else if d.paired { "Paired" } else { "Not paired" };
            row_card(
                colors,
                d.name.clone(),
                format!("Bluetooth · {}", d.mac),
                status_pill(colors, d.connected, status, status),
            )
        })
        .collect();

    let monitor_rows: Vec<Element> = monitors
        .iter()
        .map(|m| {
            row_card(
                colors,
                format!("{} {}", m.make, m.model).trim().to_string(),
                format!("{} · {}x{} @ ({},{})", m.name, m.width, m.height, m.x, m.y),
                status_pill(colors, m.active, "Active", "Inactive"),
            )
        })
        .collect();

    let pci_rows: Vec<Element> = pci
        .iter()
        .map(|d| {
            row_card(
                colors,
                d.description.clone(),
                format!("PCI · slot {}", d.slot),
                status_pill(colors, d.driver_bound, "OK", "No driver"),
            )
        })
        .collect();

    iced::widget::scrollable(
        column![
            section(colors, "Monitors", monitor_rows),
            section(colors, "Bluetooth", bt_rows),
            section(colors, "USB", usb_rows),
            section(colors, "Input Peripherals", input_rows),
            section(colors, "PCI Hardware", pci_rows),
        ]
        .spacing(20)
        .padding(20),
    )
    .height(Length::Fill)
    .into()
}
