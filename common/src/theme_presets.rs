//! Complete colour schemes for the settings page.
//!
//! Lives in `common` so both the settings registry and the UI can name a
//! preset without the registry depending on the executable. Every preset must
//! list every theme key that affects what the user sees: a partial switch
//! once produced a green/grey hybrid (the "3 shapes" bug, 2026-11-23).

use crate::config::Config;

/// Two complete colour schemes. Values are TOML literals (quoted strings).
pub const GREEN_THEME: &[(&str, &str, &str)] = &[
    ("theme", "tabbar_background", "\"#1b3d28\""),
    ("theme", "tab_inactive", "\"#a5d6a7\""),
    ("theme", "tab_hover", "\"#c8e6c9\""),
    ("theme", "tab_active", "\"#e8f5e9\""),
    ("theme", "tab_text", "\"#1b3d28\""),
    ("theme", "tab_text_active", "\"#0f2a1a\""),
    ("theme", "tabbar_text", "\"#d8ecd9\""),
    ("theme", "caption_hover", "\"#2b5c3f\""),
    ("theme", "caption_close_hover", "\"#c42b1c\""),
    ("theme", "window_border", "\"#0f2317\""),
    ("theme", "bookmark_bar_background", "\"#202020\""),
];

pub const DARK_THEME: &[(&str, &str, &str)] = &[
    ("theme", "tabbar_background", "\"#2b2b2b\""),
    ("theme", "tab_inactive", "\"#3c3c3c\""),
    ("theme", "tab_hover", "\"#41474d\""),
    ("theme", "tab_active", "\"#4a6984\""),
    ("theme", "tab_text", "\"#cccccc\""),
    ("theme", "tab_text_active", "\"#ffffff\""),
    ("theme", "tabbar_text", "\"#cccccc\""),
    ("theme", "caption_hover", "\"#3d3d3d\""),
    ("theme", "caption_close_hover", "\"#c42b1c\""),
    ("theme", "window_border", "\"#151515\""),
    ("theme", "bookmark_bar_background", "\"#202020\""),
];

fn theme_value(cfg: &Config, key: &str) -> String {
    let t = &cfg.theme;
    match key {
        "tabbar_background" => t.tabbar_background.clone(),
        "tab_inactive" => t.tab_inactive.clone(),
        "tab_hover" => t.tab_hover.clone(),
        "tab_active" => t.tab_active.clone(),
        "tab_text" => t.tab_text.clone(),
        "tab_text_active" => t.tab_text_active.clone(),
        "tabbar_text" => t.tabbar_text.clone(),
        "caption_hover" => t.caption_hover.clone(),
        "caption_close_hover" => t.caption_close_hover.clone(),
        "window_border" => t.window_border.clone(),
        "bookmark_bar_background" => t.bookmark_bar_background.clone(),
        _ => String::new(),
    }
}

fn matches(cfg: &Config, preset: &[(&str, &str, &str)]) -> bool {
    preset.iter().all(|(_, key, expected)| {
        theme_value(cfg, key).eq_ignore_ascii_case(expected.trim_matches('"'))
    })
}

/// Which bundled preset the current config matches, if any. Checks the whole
/// scheme so a half-switched config reports "自定义" instead of claiming to
/// be a preset.
pub fn current_preset_name(cfg: &Config) -> &'static str {
    if matches(cfg, GREEN_THEME) {
        "绿色"
    } else if matches(cfg, DARK_THEME) {
        "深灰蓝"
    } else {
        "自定义"
    }
}

/// The scheme a click should switch to: green <-> dark.
pub fn next_preset(cfg: &Config) -> &'static [(&'static str, &'static str, &'static str)] {
    if current_preset_name(cfg) == "绿色" { DARK_THEME } else { GREEN_THEME }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_presets_cover_the_same_keys() {
        let green: Vec<_> = GREEN_THEME.iter().map(|(_, k, _)| *k).collect();
        let dark: Vec<_> = DARK_THEME.iter().map(|(_, k, _)| *k).collect();
        assert_eq!(green, dark);
        assert_eq!(green.len(), 11);
    }

    #[test]
    fn preset_detection_is_exact() {
        let mut cfg = Config::default();
        assert_eq!(current_preset_name(&cfg), "深灰蓝");
        for (_, key, value) in GREEN_THEME {
            match *key {
                "tabbar_background" => cfg.theme.tabbar_background = value.trim_matches('"').into(),
                "tab_inactive" => cfg.theme.tab_inactive = value.trim_matches('"').into(),
                "tab_hover" => cfg.theme.tab_hover = value.trim_matches('"').into(),
                "tab_active" => cfg.theme.tab_active = value.trim_matches('"').into(),
                "tab_text" => cfg.theme.tab_text = value.trim_matches('"').into(),
                "tab_text_active" => cfg.theme.tab_text_active = value.trim_matches('"').into(),
                "tabbar_text" => cfg.theme.tabbar_text = value.trim_matches('"').into(),
                "caption_hover" => cfg.theme.caption_hover = value.trim_matches('"').into(),
                "caption_close_hover" => cfg.theme.caption_close_hover = value.trim_matches('"').into(),
                "window_border" => cfg.theme.window_border = value.trim_matches('"').into(),
                "bookmark_bar_background" => cfg.theme.bookmark_bar_background = value.trim_matches('"').into(),
                _ => {}
            }
        }
        assert_eq!(current_preset_name(&cfg), "绿色");
        cfg.theme.tab_hover = "#123456".into();
        assert_eq!(current_preset_name(&cfg), "自定义");
    }
}
