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
    TrayEnabled,
    Autostart,
    CloseAction,
}

impl SettingId {
    /// All exposed settings, in display order.
    pub const ALL: &'static [SettingId] = &[
        SettingId::TakeoverMode,
        SettingId::TabRadius,
        SettingId::ThemePreset,
        SettingId::TrayEnabled,
        SettingId::Autostart,
        SettingId::CloseAction,
    ];

    /// Human label shown in the settings page.
    pub fn label(self) -> &'static str {
        match self {
            SettingId::TakeoverMode => "接管模式",
            SettingId::TabRadius => "标签圆角",
            SettingId::ThemePreset => "配色预设",
            SettingId::TrayEnabled => "显示托盘图标",
            SettingId::Autostart => "开机自启",
            SettingId::CloseAction => "关闭按钮",
        }
    }

    /// One-line hint shown under the row, if any.
    pub fn hint(self) -> &'static str {
        match self {
            SettingId::TrayEnabled => "关闭后托盘退出，窗口 X 强制退出",
            SettingId::Autostart => "启动 revolc.exe；任务管理器「启动应用」可禁用",
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
            SettingId::TrayEnabled => Some(("tray", "enabled")),
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
            SettingId::TrayEnabled => cfg.tray.enabled.to_string(),
            SettingId::Autostart => cfg.tray.autostart.to_string(),
            SettingId::CloseAction => format!("\"{}\"", cfg.tray.close_action),
            SettingId::ThemePreset => String::new(), // rendered via preset_name
        }
    }

    /// Human-readable current value.
    pub fn display(self, cfg: &Config) -> String {
        match self {
            SettingId::TakeoverMode => {
                if cfg.takeover.mode == "redirect" { "接管新窗口" } else { "关闭" }.into()
            }
            SettingId::TabRadius => cfg.theme.tab_radius.to_string(),
            SettingId::TrayEnabled => if cfg.tray.enabled { "开" } else { "关" }.into(),
            SettingId::Autostart => if cfg.tray.autostart { "开" } else { "关" }.into(),
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
            SettingId::TrayEnabled => {
                let next = !cfg.tray.enabled;
                vec![("tray", "enabled", next.to_string())]
            }
            SettingId::Autostart => {
                let next = !cfg.tray.autostart;
                vec![("tray", "autostart", next.to_string())]
            }
            SettingId::CloseAction => {
                let next = if cfg.tray.close_action == "hide" { "exit" } else { "hide" };
                let eff = if cfg.tray.enabled { next } else { "exit" };
                vec![("tray", "close_action", format!("\"{eff}\""))]
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
            SettingId::TrayEnabled | SettingId::Autostart => matches!(value, "true" | "false"),
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
        let cfg = Config::default();
        let text = "# keep me\n[tray]\nenabled = true # mine\n";
        let out = apply(&cfg, text, SettingId::TrayEnabled).unwrap();
        assert!(out.contains("# keep me"), "{out}");
        assert!(out.contains("enabled = false"), "{out}");
    }

    #[test]
    fn hide_is_forced_to_exit_without_tray() {
        let mut cfg = Config::default();
        cfg.tray.enabled = false;
        let edits = SettingId::CloseAction.next_edits(&cfg);
        assert_eq!(edits[0].2, "\"exit\"");
    }
}
