//! Map other apps' colour themes onto Revolc's semantic roles.
//! The mapping lives here so the UI never has to know a foreign key name.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Color, ThemeKind};

/// VS Code workbench keys -> Revolc semantic roles.
pub const VSCODE_MAP: &[(&str, &str)] = &[
    ("editor.background", "app.background"),
    ("editorGroupHeader.tabsBackground", "chrome.background"),
    ("titleBar.activeBackground", "chrome.background"),
    ("sideBar.background", "surface.background"),
    ("panel.background", "surface.raised"),
    ("editorWidget.background", "surface.raised"),
    ("list.hoverBackground", "surface.hover"),
    ("editor.foreground", "foreground.primary"),
    ("descriptionForeground", "foreground.secondary"),
    ("tab.activeBackground", "accent.soft"),
    ("tab.activeForeground", "foreground.inverse"),
    ("focusBorder", "accent.default"),
    ("activityBar.activeBorder", "accent.default"),
    ("editorGroup.border", "border.subtle"),
    ("panel.border", "border.subtle"),
    ("errorForeground", "danger.default"),
];

/// Infer light/dark from a theme's main background when `type` is absent.
pub fn infer_kind(background: &str) -> ThemeKind {
    match Color::parse(background) {
        Some(c) if c.luminance() > 0.35 => ThemeKind::Light,
        _ => ThemeKind::Dark,
    }
}

/// Accept either a whole VS Code theme JSON or just its `"colors"` object.
/// Returns semantic role -> hex string; unknown keys are ignored.
pub fn from_vscode_json(text: &str) -> Result<(ThemeKind, HashMap<String, String>), String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let object = value
        .get("colors")
        .and_then(|v| v.as_object())
        .or_else(|| value.as_object())
        .ok_or_else(|| "theme must be a JSON object".to_string())?;

    let mut mapped = HashMap::new();
    for (foreign, role) in VSCODE_MAP {
        if let Some(value) = object.get(*foreign).and_then(|v| v.as_str()) {
            if Color::parse(value).is_some() {
                mapped.insert((*role).to_string(), value.to_string());
            }
        }
    }

    let kind = match value.get("type").and_then(|v| v.as_str()) {
        Some("light") => ThemeKind::Light,
        Some("dark") => ThemeKind::Dark,
        _ => infer_kind(
            object
                .get("editor.background")
                .and_then(|v| v.as_str())
                .unwrap_or("#202020"),
        ),
    };
    Ok((kind, mapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_vscode_keys() {
        let json = r##"{
            "name": "Tomorrow Night Blue",
            "type": "dark",
            "colors": {
                "editor.background": "#1E1E2E",
                "sideBar.background": "#191926",
                "editor.foreground": "#D8D8D8",
                "focusBorder": "#81A2BE",
                "editor.background.unused": "#000000",
                "not.a.colour": "not-a-colour"
            }
        }"##;
        let (kind, map) = from_vscode_json(json).unwrap();
        assert_eq!(kind, ThemeKind::Dark);
        assert_eq!(
            map.get("app.background").map(String::as_str),
            Some("#1E1E2E")
        );
        assert_eq!(
            map.get("accent.default").map(String::as_str),
            Some("#81A2BE")
        );
        assert_eq!(map.len(), 4);
    }

    #[test]
    fn infers_kind_without_type() {
        let json = r##"{"colors":{"editor.background":"#FFFFFF"}}"##;
        assert_eq!(from_vscode_json(json).unwrap().0, ThemeKind::Light);
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(from_vscode_json("not json").is_err());
    }
}
