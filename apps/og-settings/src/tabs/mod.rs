pub mod ai_context;
pub mod audio;
pub mod bar;
pub mod devices;
pub mod display;
pub mod history;
pub mod hotkeys;
pub mod mouse_keyboard;
pub mod network;
pub mod notifications;
pub mod power;
pub mod printing;
pub mod search;
pub mod sysmon;
pub mod taskbar_arrange;
pub mod theme;
pub mod updater;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tab {
    Power,
    Display,
    Network,
    AiContext,
    Updates,
    Hotkeys,
    Theme,
    MouseKeyboard,
    Bar,
    History,
    SysMonitor,
    Notifications,
    Search,
    Audio,
    Devices,
    Printing,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Power,
            Tab::Display,
            Tab::Network,
            Tab::AiContext,
            Tab::Audio,
            Tab::Devices,
            Tab::Printing,
            Tab::Updates,
            Tab::Hotkeys,
            Tab::Theme,
            Tab::MouseKeyboard,
            Tab::Bar,
            Tab::Search,
            Tab::Notifications,
            Tab::History,
            Tab::SysMonitor,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Power => "Power",
            Tab::Display => "Display",
            Tab::Network => "Network",
            Tab::AiContext => "AI Context",
            Tab::Updates => "Updates",
            Tab::Hotkeys => "Hotkeys",
            Tab::Theme => "Theme",
            Tab::MouseKeyboard => "Mouse & Keyboard",
            Tab::Bar => "Bar",
            Tab::History => "History",
            Tab::SysMonitor => "System Monitor",
            Tab::Notifications => "Notifications",
            Tab::Search => "Search",
            Tab::Audio => "Audio",
            Tab::Devices => "Devices",
            Tab::Printing => "Printing",
        }
    }
}
