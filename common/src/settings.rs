//! The single registry of user-facing settings.
//!
//! A setting used to be a `(section, key, value)` triple hard-coded at every
//! click site (`ui/settings.rs` listed the rows, `ui/mod.rs` wrote the strings,
//! `config.rs` parsed them). That made adding a setting a four-file edit and
//! let dead config fields sit unnoticed.
//!
//! `SettingId` is now the one identity of an exposed setting. It owns:
//! - where the value lives in TOML (`section`, `key`),
//! - what values are legal (`choices` / `bounds` / `validate`),
//! - how to render and cycle it (`label`, `next`),
//! - how to produce the TOML edits (`edits` — one write for multi-key presets).
//!
//! The UI only asks the registry for rows; the write path only accepts a
//! `SettingId`. Neither spells a TOML key by hand. Values are still parsed by
//! `serde` into `Config`, and hot reload still flows through the existing
//! watcher -> `ConfigHandle::reload()` path, so nothing here changes how the
//! rest of the app *reads* configuration.

use crate::config::{set_toml_values, Config};

/// Every setting exposed in the settings page. Order is display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    TakeoverMode,
    TabRadius,
    ThemePreset,
    Autostart,
    CloseAction,
}

impl SettingId {
    /// All exposed settings, in display order.
    pub const ALL: &'static [SettingId] = &[
        SettingId::TakeoverMode,
        SettingId::TabRadius,
        SettingId::ThemePreset,
        SettingId::Autostart,
        SettingId::CloseAction,
    ];

    /// Human label shown in the settings page.
    pub fn label(self) -> &'static str {
        match self {
            SettingId::TakeoverMode => "接管",
            SettingId::TabRadius => "标签圆角",
            SettingId::ThemePreset => "配色预设",
            SettingId::Autostart => "开机自启",
            SettingId::CloseAction => "关闭按钮",
        }
    }

    /// Whether this setting can be changed in the current config. Autostart
    /// is meaningless when takeover is off — the user explicitly decided that
    /// "off" means uninstall, so there must be no process left to start.
    pub fn is_available(self, cfg: &Config) -> bool {
        match self {
            SettingId::Autostart => cfg.takeover.mode == "redirect",
            _ => true,
        }
    }

    /// Hint shown while the setting is unavailable.
    pub fn unavailable_hint(self) -> &'static str {
        match self {
            SettingId::Autostart => "需先开启接管",
            _ => "",
        }
    }

    /// One-line hint shown under the row, if any.
    pub fn hint(self) -> &'static str {
        match self {
            SettingId::TakeoverMode => "开启需 UAC（首次）；关闭即卸载并清除配置",
            SettingId::Autostart => "开机自动进入接管；任务管理器「启动应用」可禁用",
            SettingId::CloseAction => "托盘隐藏时强制退出",
            _ => "",
        }
    }

    /// TOML section + key for single-key settings. Multi-key settings
    /// (`ThemePreset`) return `None` and build their own edits.
    pub fn toml_key(self) -> Option<(&'static str, &'static str)> {
        match self {
            SettingId::TakeoverMode => Some(("takeover", "mode")),
            SettingId::TabRadius => Some(("theme", "tab_radius")),
            SettingId::Autostart => Some(("tray", "autostart")),
            SettingId::CloseAction => Some(("tray", "close_action")),
            SettingId::ThemePreset => None,
        }
    }

    /// Current value as a TOML literal (quoted strings, bare bool/int).
    pub fn toml_value(self, cfg: &Config) -> String {
        match self {
            SettingId::TakeoverMode => format!("\"{}\"", cfg.takeover.mode),
            SettingId::TabRadius => cfg.theme.tab_radius.to_string(),
            SettingId::Autostart => cfg.tray.autostart.to_string(),
            SettingId::CloseAction => format!("\"{}\"", cfg.tray.close_action),
            SettingId::ThemePreset => String::new(), // rendered via preset_name
        }
    }

    /// Human-readable current value.
    pub fn display(self, cfg: &Config) -> String {
        match self {
            SettingId::TakeoverMode => {
                if cfg.takeover.mode == "redirect" { "是" } else { "否" }.into()
            }
            SettingId::TabRadius => cfg.theme.tab_radius.to_string(),
            SettingId::Autostart => if cfg.tray.autostart { "是" } else { "否" }.into(),
            SettingId::CloseAction => {
                if cfg.tray.close_action == "hide" { "隐藏到托盘" } else { "退出" }.into()
            }
            SettingId::ThemePreset => crate::theme_presets::current_preset_name(cfg).to_string(),
        }
    }

    /// Next value when clicked, as TOML edits. Single-key settings produce one
    /// edit; ThemePreset produces the whole colour scheme in one batch so the
    /// user never sees a half-applied theme (the old "3 shapes" bug).
    pub fn next_edits(self, cfg: &Config) -> Vec<(&'static str, &'static str, String)> {
        match self {
            SettingId::TakeoverMode => {
                let next = if cfg.takeover.mode == "redirect" { "off" } else { "redirect" };
                vec![("takeover", "mode", format!("\"{next}\""))]
            }
            SettingId::TabRadius => {
                let next = match cfg.theme.tab_radius {
                    0 => 4,
                    4 => 6,
                    6 => 10,
                    _ => 0,
                };
                vec![("theme", "tab_radius", next.to_string())]
            }
            SettingId::Autostart => {
                let next = !cfg.tray.autostart;
                vec![("tray", "autostart", next.to_string())]
            }
            SettingId::CloseAction => {
                // The tray is permanent now, so hide/exit is a plain toggle.
                let next = if cfg.tray.close_action == "hide" { "exit" } else { "hide" };
                vec![("tray", "close_action", format!("\"{next}\""))]
            }
            SettingId::ThemePreset => {
                let next = crate::theme_presets::next_preset(cfg);
                next.iter()
                    .map(|(s, k, v)| (*s, *k, (*v).to_string()))
                    .collect()
            }
        }
    }

    /// Reject impossible values before they reach disk. Returns the TOML key
    /// path in the error so the settings page can log a precise message.
    pub fn validate(self, value: &str) -> Result<(), &'static str> {
        let valid = match self {
            SettingId::TakeoverMode => matches!(value, "\"off\"" | "\"redirect\""),
            SettingId::TabRadius => value.parse::<i32>().map(|n| (0..=20).contains(&n)).unwrap_or(false),
            SettingId::Autostart => matches!(value, "true" | "false"),
            SettingId::CloseAction => matches!(value, "\"exit\"" | "\"hide\""),
            SettingId::ThemePreset => true, // a batch of colour keys, checked by serde on reload
        };
        if valid { Ok(()) } else { Err(self.label()) }
    }
}

/// Apply one setting to a TOML document, preserving untouched bytes.
///
/// Computes the next value from `cfg`, validates it, then writes every edit in
/// one pass so the watcher sees a single change (one reload, no half state).
pub fn apply(cfg: &Config, text: &str, id: SettingId) -> Result<String, &'static str> {
    if !id.is_available(cfg) {
        return Err("setting unavailable");
    }
    let edits = id.next_edits(cfg);
    if let Some((_, _, value)) = edits.first() {
        id.validate(value)?;
    }
    let borrowed: Vec<(&str, &str, &str)> = edits
        .iter()
        .map(|(s, k, v)| (*s, *k, v.as_str()))
        .collect();
    Ok(set_toml_values(text, &borrowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_has_a_label_and_cycles() {
        let cfg = Config::default();
        for &id in SettingId::ALL {
            assert!(!id.label().is_empty(), "{id:?}");
            assert!(!id.next_edits(&cfg).is_empty(), "{id:?}");
        }
    }

    #[test]
    fn validate_accepts_own_next_values() {
        let cfg = Config::default();
        for &id in SettingId::ALL {
            for (_, _, v) in id.next_edits(&cfg) {
                assert!(id.validate(&v).is_ok(), "{id:?} rejected {v}");
            }
        }
    }

    #[test]
    fn validate_rejects_garbage() {
        assert!(SettingId::Autostart.validate("yes").is_err());
        assert!(SettingId::TabRadius.validate("999").is_err());
        assert!(SettingId::CloseAction.validate("\"vanish\"").is_err());
    }

    #[test]
    fn apply_preserves_comments() {
        let mut cfg = Config::default();
        cfg.takeover.mode = "redirect".into(); // autostart is gated on takeover
        let text = "# keep me\n[tray]\nautostart = false # mine\n";
        let out = apply(&cfg, text, SettingId::Autostart).unwrap();
        assert!(out.contains("# keep me"), "{out}");
        assert!(out.contains("autostart = true"), "{out}");
    }

    #[test]
    fn close_action_is_always_available_with_permanent_tray() {
        let mut cfg = Config::default();
        cfg.tray.enabled = false; // stale config from an older build is ignored
        assert!(SettingId::CloseAction.is_available(&cfg));
        assert!(apply(&cfg, "[tray]\nclose_action = \"exit\"\n", SettingId::CloseAction).is_ok());
    }

    #[test]
    fn autostart_requires_takeover() {
        let mut cfg = Config::default();
        assert!(!SettingId::Autostart.is_available(&cfg));
        assert_eq!(SettingId::Autostart.unavailable_hint(), "需先开启接管");
        cfg.takeover.mode = "redirect".into();
        assert!(SettingId::Autostart.is_available(&cfg));
    }
}
