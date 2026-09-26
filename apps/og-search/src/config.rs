/// This app owns none of its own settings — it just reads the shared
/// config og-settings writes (`~/.config/sway-power/config.json`),
/// via the shared `og-config` crate. `AppColors`/`apply_color_variance`
/// come from `og-theme` the same way.
pub use og_config::Config;

/// Same per-app tint hash convention every OG-toolkit app uses — seeded by
/// this app's own name so it always shifts the same way, not random.
pub const APP_TINT_SEED: &str = "og-search";
