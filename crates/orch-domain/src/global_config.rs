//! Global configuration — the shape of `~/.orchestry/global.yml`
//! Persists across projects.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Activity feed filter preset name.
///
/// Deliberately a plain `String` rather than an enum: the reader does
/// **not** validate this field — any value passes straight through
/// — and an enum would reject a file that existing readers accept.
pub type ActivityFilterPreset = String;

/// The five known presets, in display order.
pub const ACTIVITY_FILTER_PRESETS: [&str; 5] = ["all", "text", "tools", "errors", "events"];

/// Built-in TUI colour palette name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuiPaletteName {
    Amber,
    Ocean,
    Forest,
    Violet,
    Light,
}

pub const TUI_PALETTE_NAMES: [TuiPaletteName; 5] = [
    TuiPaletteName::Amber,
    TuiPaletteName::Ocean,
    TuiPaletteName::Forest,
    TuiPaletteName::Violet,
    TuiPaletteName::Light,
];

impl TuiPaletteName {
    pub fn as_str(&self) -> &'static str {
        match self {
            TuiPaletteName::Amber => "amber",
            TuiPaletteName::Ocean => "ocean",
            TuiPaletteName::Forest => "forest",
            TuiPaletteName::Violet => "violet",
            TuiPaletteName::Light => "light",
        }
    }
}

impl std::fmt::Display for TuiPaletteName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TuiPaletteName {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TUI_PALETTE_NAMES
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// Type guard for a palette name, over an arbitrary parsed value.
pub fn is_tui_palette_name(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .map(|s| TUI_PALETTE_NAMES.iter().any(|p| p.as_str() == s))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub toast: bool,
    pub bell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuiPreferences {
    pub palette: TuiPaletteName,
    pub activity_filter: ActivityFilterPreset,
    pub notifications: NotificationPreferences,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub tui: TuiPreferences,
}

/// The default global configuration.
pub fn default_global_config() -> GlobalConfig {
    GlobalConfig {
        tui: TuiPreferences {
            palette: TuiPaletteName::Amber,
            activity_filter: "all".into(),
            notifications: NotificationPreferences {
                toast: true,
                bell: false,
            },
        },
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        default_global_config()
    }
}

impl GlobalConfig {
    /// Field-by-field tolerant read of a raw `global.yml` value, mirroring
    /// `GlobalConfigStore.read`:
    ///
    /// - an unknown `tui.palette` falls back to `amber`;
    /// - `tui.activity_filter` is **not** validated and passes through;
    /// - a non-boolean `toast`/`bell` falls back to its default.
    pub fn from_raw(raw: Option<&Value>) -> GlobalConfig {
        let defaults = default_global_config();
        let Some(Value::Object(raw)) = raw else {
            return defaults;
        };

        let tui = raw.get("tui").and_then(Value::as_object);
        let notifications = tui
            .and_then(|t| t.get("notifications"))
            .and_then(Value::as_object);
        let palette_raw = tui.and_then(|t| t.get("palette"));

        GlobalConfig {
            tui: TuiPreferences {
                palette: if is_tui_palette_name(palette_raw) {
                    palette_raw
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(defaults.tui.palette)
                } else {
                    defaults.tui.palette
                },
                activity_filter: tui
                    .and_then(|t| t.get("activity_filter"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or(defaults.tui.activity_filter),
                notifications: NotificationPreferences {
                    toast: notifications
                        .and_then(|n| n.get("toast"))
                        .and_then(Value::as_bool)
                        .unwrap_or(defaults.tui.notifications.toast),
                    bell: notifications
                        .and_then(|n| n.get("bell"))
                        .and_then(Value::as_bool)
                        .unwrap_or(defaults.tui.notifications.bell),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn has_notifications_toast_true_by_default() {
        assert!(default_global_config().tui.notifications.toast);
    }

    #[test]
    fn has_notifications_bell_false_by_default() {
        assert!(!default_global_config().tui.notifications.bell);
    }

    #[test]
    fn has_activity_filter_all_by_default() {
        assert_eq!(default_global_config().tui.activity_filter, "all");
    }

    #[test]
    fn has_palette_amber_by_default() {
        assert_eq!(default_global_config().tui.palette, TuiPaletteName::Amber);
    }

    #[test]
    fn validates_every_built_in_palette_name() {
        let names: Vec<&str> = TUI_PALETTE_NAMES.iter().map(|p| p.as_str()).collect();
        assert_eq!(names, ["amber", "ocean", "forest", "violet", "light"]);
        for palette in TUI_PALETTE_NAMES {
            assert!(is_tui_palette_name(Some(&json!(palette.as_str()))));
        }
        assert!(!is_tui_palette_name(Some(&json!("unknown"))));
        assert!(!is_tui_palette_name(Some(&Value::Null)));
        assert!(!is_tui_palette_name(None));
    }

    #[test]
    fn default_file_has_the_expected_content() {
        let value = serde_json::to_value(default_global_config()).unwrap();
        assert_eq!(
            value,
            json!({
                "tui": {
                    "palette": "amber",
                    "activity_filter": "all",
                    "notifications": { "toast": true, "bell": false },
                }
            })
        );
    }

    #[test]
    fn missing_file_yields_defaults() {
        assert_eq!(GlobalConfig::from_raw(None), default_global_config());
    }

    #[test]
    fn invalid_palette_falls_back_to_amber() {
        let raw = json!({ "tui": { "palette": "chartreuse" } });
        assert_eq!(
            GlobalConfig::from_raw(Some(&raw)).tui.palette,
            TuiPaletteName::Amber
        );
    }

    #[test]
    fn activity_filter_is_not_validated() {
        let raw = json!({ "tui": { "activity_filter": "something-new" } });
        assert_eq!(
            GlobalConfig::from_raw(Some(&raw)).tui.activity_filter,
            "something-new"
        );
    }

    #[test]
    fn non_boolean_notifications_fall_back() {
        let raw = json!({ "tui": { "notifications": { "toast": "yes", "bell": 1 } } });
        let config = GlobalConfig::from_raw(Some(&raw));
        assert!(config.tui.notifications.toast);
        assert!(!config.tui.notifications.bell);
    }

    #[test]
    fn a_valid_file_reads_through() {
        let raw = json!({
            "tui": {
                "palette": "ocean",
                "activity_filter": "errors",
                "notifications": { "toast": false, "bell": true },
            }
        });
        let config = GlobalConfig::from_raw(Some(&raw));
        assert_eq!(config.tui.palette, TuiPaletteName::Ocean);
        assert_eq!(config.tui.activity_filter, "errors");
        assert!(!config.tui.notifications.toast);
        assert!(config.tui.notifications.bell);
    }
}
