//! Semantic colour themes for Revolc.
//!
//! A theme file only declares meaning (`chrome.background`, `accent.default`…),
//! never a component-specific colour. The UI reads a resolved `Theme` and maps
//! roles to controls in `docs/UI_SPEC.md`. This crate has no Win32 dependency,
//! so theme creation/import can be unit-tested on its own.

pub mod builtin;
pub mod color;
pub mod derive;
pub mod import;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use color::Color;

/// The only keys a theme file may define. UI code reads these through the
/// accessors on `Theme`; it must never reach into raw TOML/JSON colour keys.
pub const ROLE_KEYS: &[&str] = &[
    "app.background",
    "chrome.background",
    "surface.background",
    "surface.raised",
    "surface.hover",
    "border.subtle",
    "foreground.primary",
    "foreground.secondary",
    "foreground.inverse",
    "accent.default",
    "accent.soft",
    "danger.default",
    "danger.hover",
    "focus.ring",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeFile {
    /// Stable identifier used by `[theme] active` in TOML.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ThemeKind,
    #[serde(default)]
    pub inherit: Option<String>,
    #[serde(default)]
    pub colors: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub kind: ThemeKind,
    pub colors: HashMap<String, Color>,
    /// "builtin" or "user" — useful for settings UI and logs.
    pub source: &'static str,
}

impl Theme {
    pub fn color(&self, role: &str) -> Color {
        self.colors.get(role).copied().unwrap_or(Color(0, 0, 0))
    }

    pub fn hex(&self, role: &str) -> String {
        self.color(role).to_hex()
    }

    pub fn app_background(&self) -> Color {
        self.color("app.background")
    }
    pub fn chrome_background(&self) -> Color {
        self.color("chrome.background")
    }
    pub fn surface_background(&self) -> Color {
        self.color("surface.background")
    }
    pub fn surface_raised(&self) -> Color {
        self.color("surface.raised")
    }
    pub fn surface_hover(&self) -> Color {
        self.color("surface.hover")
    }
    pub fn border_subtle(&self) -> Color {
        self.color("border.subtle")
    }
    pub fn foreground_primary(&self) -> Color {
        self.color("foreground.primary")
    }
    pub fn foreground_secondary(&self) -> Color {
        self.color("foreground.secondary")
    }
    pub fn foreground_inverse(&self) -> Color {
        self.color("foreground.inverse")
    }
    pub fn accent_default(&self) -> Color {
        self.color("accent.default")
    }
    pub fn accent_soft(&self) -> Color {
        self.color("accent.soft")
    }
    pub fn danger_default(&self) -> Color {
        self.color("danger.default")
    }
    pub fn danger_hover(&self) -> Color {
        self.color("danger.hover")
    }
    pub fn focus_ring(&self) -> Color {
        self.color("focus.ring")
    }
}

/// Fallback used while a config is being parsed (serde skips the resolved
/// palette). Guaranteed complete because `resolve` fills missing keys.
pub fn fallback() -> Theme {
    ThemeSet::discover()
        .resolve("clover-dark")
        .unwrap_or_else(|_| Theme {
            id: "fallback".into(),
            name: "回退主题".into(),
            kind: ThemeKind::Dark,
            colors: HashMap::new(),
            source: "builtin",
        })
}

/// `%APPDATA%\revolc\themes` — user-created theme JSONs live here.
pub fn themes_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("revolc").join("themes")
}

#[derive(Debug, Clone)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    pub kind: ThemeKind,
    pub source: &'static str,
}

#[derive(Debug, Clone)]
pub struct ThemeSet {
    files: HashMap<String, (ThemeFile, &'static str)>,
}

impl ThemeSet {
    pub fn discover() -> Self {
        let mut files = HashMap::new();
        for (id, json) in builtin::ALL {
            if let Ok(file) = serde_json::from_str::<ThemeFile>(json) {
                files.insert((*id).to_string(), (file, "builtin"));
            }
        }
        let dir = themes_dir();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let text = match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                let mut file: ThemeFile = match serde_json::from_str(&text) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing_unavailable(&format!("theme {:?}: {e}", path.file_name()));
                        continue;
                    }
                };
                let id = file.id.clone().unwrap_or_else(|| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unnamed")
                        .to_string()
                });
                file.id = Some(id.clone());
                files.insert(id, (file, "user")); // user themes shadow builtins
            }
        }
        Self { files }
    }

    /// Ordered list for settings UI. Builtins first, then user themes.
    pub fn list(&self) -> Vec<ThemeInfo> {
        let mut result: Vec<ThemeInfo> = builtin::ALL
            .iter()
            .filter_map(|(id, _)| self.files.get(*id))
            .chain(self.files.values())
            .map(|(file, source)| ThemeInfo {
                id: file.id.clone().unwrap_or_default(),
                name: if file.name.is_empty() {
                    "未命名主题".into()
                } else {
                    file.name.clone()
                },
                kind: file.kind,
                source: *source,
            })
            .collect();
        result.sort_by_key(|t| (t.source != "builtin", t.name.clone()));
        result.dedup_by_key(|t| t.id.clone());
        result
    }

    pub fn contains(&self, id: &str) -> bool {
        self.files.contains_key(id)
    }

    /// Resolve inheritance, apply overrides, then fill missing roles.
    pub fn resolve(&self, id: &str) -> Result<Theme, String> {
        let mut stack = Vec::new();
        self.resolve_inner(id, &mut stack)
    }

    fn resolve_inner(&self, id: &str, stack: &mut Vec<String>) -> Result<Theme, String> {
        if stack.iter().any(|s| s == id) {
            return Err(format!("theme inheritance cycle at {id}"));
        }
        let (file, source) = self
            .files
            .get(id)
            .ok_or_else(|| format!("unknown theme {id}"))?;
        stack.push(id.to_string());
        let mut colors = match &file.inherit {
            Some(parent) => self.resolve_inner(parent, stack)?.colors,
            None => HashMap::new(),
        };
        for (key, value) in &file.colors {
            match Color::parse(value) {
                Some(color) => {
                    colors.insert(key.clone(), color);
                }
                None => tracing_unavailable(&format!("theme {id}: invalid colour {value}")),
            }
        }
        stack.pop();
        let mut theme = Theme {
            id: id.to_string(),
            name: if file.name.is_empty() {
                id.to_string()
            } else {
                file.name.clone()
            },
            kind: file.kind,
            colors,
            source,
        };
        derive::fill_missing(&mut theme);
        Ok(theme)
    }
}

/// Write a user theme JSON. Atomic replace so a half-written file is never
/// picked up by the watcher.
pub fn save_user_theme(id: &str, file: &ThemeFile) -> std::io::Result<()> {
    let dir = themes_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.json"));
    let tmp = dir.join(format!("{id}.json.tmp"));
    let text = serde_json::to_string_pretty(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn tracing_unavailable(message: &str) {
    // `tracing` is intentionally not a dependency of this crate: theme
    // diagnostics go to stderr in tools and are ignored in the UI process.
    eprintln!("[theme] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_builtins_are_complete_and_contrasty() {
        let set = ThemeSet::discover();
        assert!(set.list().len() >= 4);
        for info in set.list() {
            let theme = set.resolve(&info.id).unwrap();
            for role in ROLE_KEYS {
                assert!(
                    theme.colors.contains_key(*role),
                    "{} missing {role}",
                    info.id
                );
            }
            assert!(
                theme.foreground_primary().contrast(theme.app_background()) >= 4.5,
                "{} primary contrast",
                info.id
            );
            assert!(
                theme
                    .foreground_secondary()
                    .contrast(theme.surface_background())
                    >= 4.5,
                "{} secondary contrast",
                info.id
            );
        }
    }

    #[test]
    fn user_theme_can_inherit_and_override() {
        let mut parent: ThemeFile = serde_json::from_str(builtin::CLOVER_DARK.1).unwrap();
        parent.id = None;
        let mut child: ThemeFile = serde_json::from_str(builtin::CLOVER_DARK.1).unwrap();
        child.name = "我的主题".into();
        child.inherit = Some("clover-dark".into());
        child
            .colors
            .insert("accent.default".into(), "#FF0000".into());
        let mut set = ThemeSet::discover();
        set.files.insert("mine".into(), (child, "user"));
        let theme = set.resolve("mine").unwrap();
        assert_eq!(theme.name, "我的主题");
        assert_eq!(theme.accent_default(), Color(0xff, 0, 0));
        assert_eq!(
            theme.app_background(),
            parent
                .colors
                .get("app.background")
                .and_then(|s| Color::parse(s))
                .unwrap()
        );
    }

    #[test]
    fn cycle_is_rejected() {
        let mut file: ThemeFile = serde_json::from_str(builtin::DARK.1).unwrap();
        file.inherit = Some("loop".into());
        let mut set = ThemeSet::discover();
        set.files.insert("loop".into(), (file, "user"));
        assert!(set.resolve("loop").is_err());
    }

    #[test]
    fn unknown_theme_is_an_error() {
        assert!(ThemeSet::discover().resolve("no-such-theme").is_err());
    }
}
