//! User-configurable settings, hot-reloaded from TOML.
//! Spec §4.0: single config file, runtime-writable, changes apply immediately.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: Theme,
    pub behavior: Behavior,
    pub takeover: Takeover,
    pub tray: Tray,
    pub bookmarks: Bookmarks,
}

/// Bookmark bar (phase 2 of feat/tab-context-menu). UI shell only for now —
/// items arrive in phase 3 together with "add to bookmarks" and persistence.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Bookmarks {
    pub enabled: bool,
}

impl Default for Bookmarks {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Tray icon and session-level launcher.
///
/// The tray is permanent now: `enabled` is kept only so old config files still
/// deserialize, and is never read. `autostart` is a HKCU Run entry, so Task
/// Manager's Startup apps page shows it and the user can remove it there.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Tray {
    pub enabled: bool,
    /// Register `revolc-tray.exe` under HKCU\...\Run.
    pub autostart: bool,
    /// What the window X does while the tray is visible: "exit" | "hide".
    /// "hide" is ignored when the tray is off (there would be no way back).
    pub close_action: String,
}

impl Default for Tray {
    fn default() -> Self {
        Self {
            enabled: true,
            autostart: false,
            close_action: "exit".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Takeover {
    /// "off" | "redirect" (docs/Takeover_Research.md §4; spec §7.1)
    pub mode: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub tabbar_background: String,
    pub tab_active: String,
    /// Under the pointer, not active. Sits between `tab_active` and
    /// `tab_inactive` on the same ramp, so hovering reads as "almost the tab
    /// you are about to pick" rather than as a separate state.
    pub tab_hover: String,
    pub tab_inactive: String,
    pub tab_text: String,
    pub tab_text_active: String,
    pub tabbar_text: String,
    /// Hover fill behind a caption button (min/max), and the red used by the
    /// close button — Chrome-style integrated title bar uses these.
    pub caption_hover: String,
    pub caption_close_hover: String,
    /// Frame around the whole window. A frameless window has no frame and
    /// (on Win10) no DWM shadow, so without this the folder content looks
    /// pasted straight onto the desktop. Its width is a compile-time constant
    /// (`ui::WINDOW_BORDER`); this sets only the colour.
    pub window_border: String,
    /// Background of the bookmark bar under the tab strip.
    pub bookmark_bar_background: String,
    /// Corner radius in px; 0 = square (Win7 fallback also forces 0)
    pub tab_radius: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Behavior {
    /// "last" | "new" | "list"  (spec §3.2)
    pub startup_mode: String,
    /// Used when startup_mode == "list"
    pub startup_paths: Vec<String>,
    /// Hover-to-switch tabs (spec §3.1)
    pub hover_switch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            behavior: Behavior::default(),
            takeover: Takeover::default(),
            tray: Tray::default(),
            bookmarks: Bookmarks::default(),
        }
    }
}

impl Default for Takeover {
    fn default() -> Self {
        Self { mode: "off".into() }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            tabbar_background: "#2b2b2b".into(),
            tab_active: "#4a6984".into(),
            tab_hover: "#41474d".into(),
            tab_inactive: "#3c3c3c".into(),
            tab_text: "#cccccc".into(),
            tab_text_active: "#ffffff".into(),
            tabbar_text: "#cccccc".into(),
            caption_hover: "#3d3d3d".into(),
            caption_close_hover: "#c42b1c".into(),
            window_border: "#151515".into(),
            bookmark_bar_background: "#202020".into(),
            tab_radius: 6,
        }
    }
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            startup_mode: "last".into(),
            startup_paths: Vec::new(),
            hover_switch: true,
        }
    }
}

impl Config {
    /// Parse "#RRGGBB" into (r, g, b).
    pub fn color(&self, key: impl FnOnce(&Theme) -> &str) -> (u8, u8, u8) {
        parse_color(key(&self.theme))
    }
}

/// Replace one key's line in TOML text, preserving every other byte
/// (comments, blank lines, formatting). The settings panel uses this instead
/// of serializing `Config` so the user's annotated config survives edits.
/// Returns the new text; the caller decides when/how to write it to disk.
pub fn set_toml_value(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let section_header = format!("[{section}]");
    let mut in_section = false;
    let mut found = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == section_header;
            continue;
        }
        if !in_section || trimmed.starts_with('#') {
            continue;
        }
        let some_key = trimmed.split('=').next().unwrap_or("").trim();
        if some_key == key {
            // Keep the user's indentation, replace only the value part.
            let indent_len = line.len() - line.trim_start().len();
            *line = format!("{}{key} = {value}", &line[..indent_len]);
            found = true;
            break;
        }
    }
    if !found {
        if let Some(pos) = lines.iter().position(|l| l.trim() == section_header) {
            lines.insert(pos + 1, format!("{key} = {value}"));
        } else {
            if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                lines.push(String::new());
            }
            lines.push(section_header);
            lines.push(format!("{key} = {value}"));
        }
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Apply many key/value edits to TOML text in one pass, preserving comments
/// and layout like `set_toml_value`. Used by the theme preset switch so the
/// whole scheme lands atomically (one file write -> one watcher reload).
pub fn set_toml_values(text: &str, edits: &[(&str, &str, &str)]) -> String {
    let mut out = text.to_string();
    for (section, key, value) in edits {
        out = set_toml_value(&out, section, key, value);
    }
    out
}

pub fn parse_color(s: &str) -> (u8, u8, u8) {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 {
        if let Ok(v) = u32::from_str_radix(s, 16) {
            return (((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8);
        }
    }
    tracing::warn!(value = s, "invalid color, using fallback");
    (0x3c, 0x3c, 0x3c)
}

pub fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("revolc").join("revolc.toml")
}

/// Ensure a config file exists; if missing, write a commented default.
pub fn ensure_config_file(path: &PathBuf) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // r## delimiter: the TOML contains "#..." color strings which would
    // terminate an r#"..."# raw string early.
    let default = r##"# Revolc configuration. Changes apply immediately (hot reload).

[theme]
tabbar_background = "#2b2b2b"
tab_active        = "#4a6984"
tab_hover         = "#41474d"  # under the pointer, not active
tab_inactive      = "#3c3c3c"
tab_text          = "#cccccc"
tab_text_active   = "#ffffff"
tabbar_text       = "#cccccc"
caption_hover     = "#3d3d3d"  # min/max button hover fill
caption_close_hover = "#c42b1c" # close button hover (red)
window_border     = "#151515" # colour of the frame around the whole window
tab_radius        = 6      # 0 = square corners

[behavior]
startup_mode = "last"    # "last" | "new" | "list"
startup_paths = []       # used when startup_mode == "list"
hover_switch = true

[takeover]
mode = "off"             # "off" = 否（卸载清理）| "redirect" = 是（接管工作）
                         # 两者都通过设置页「接管」切换，首次需要 UAC

[tray]
autostart = false        # 开机自启（写入 HKCU Run，任务管理器「启动应用」可见）
                         # 启动的是 revolc.exe；托盘永久显示，由主程序拉起
close_action = "exit"    # 窗口 X 的行为："exit" 退出 | "hide" 隐藏到托盘
                         # 托盘永久显示，因此 "hide" 总能通过托盘恢复

[bookmarks]
enabled = true           # 标签栏下方的收藏栏（热重载即时生效）

# theme.bookmark_bar_background 可覆盖收藏栏底色（默认 #202020）
"##;
    let _ = std::fs::write(path, default);
    tracing::info!(?path, "wrote default config");
}

/// Load config, falling back to defaults with a warning on parse errors.
pub fn load(path: &PathBuf) -> Config {
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::error!(error = %e, "config parse error, using defaults");
                Config::default()
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "config read error, using defaults");
            Config::default()
        }
    }
}

/// Shared, hot-swappable config handle.
pub struct ConfigHandle {
    swap: ArcSwap<Config>,
    path: PathBuf,
}

impl ConfigHandle {
    pub fn new(path: PathBuf) -> Self {
        ensure_config_file(&path);
        let cfg = load(&path);
        Self {
            swap: ArcSwap::from_pointee(cfg),
            path,
        }
    }

    pub fn get(&self) -> Arc<Config> {
        self.swap.load_full()
    }

    pub fn reload(&self) {
        let cfg = load(&self.path);
        tracing::info!("config reloaded");
        self.swap.store(Arc::new(cfg));
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_toml_value_preserves_comments() {
        let original = "[theme]\ntab_radius = 6 # 0 = square\nwindow_border = \"#151515\"\n\n[takeover]\nmode = \"off\"\n";
        let updated = set_toml_value(original, "theme", "tab_radius", "12");
        assert!(updated.contains("tab_radius = 12"), "{updated}");
        assert!(updated.contains("window_border = \"#151515\""), "{updated}");
        assert!(updated.contains("mode = \"off\""), "{updated}");
        // The inline comment on the replaced line is dropped (we replaced the
        // whole line); all *other* comments and layout survive.
        assert!(updated.ends_with('\n'));
    }

    #[test]
    fn set_toml_value_appends_missing_key() {
        let original = "[theme]\ntab_radius = 6\n\n[takeover]\nmode = \"off\"\n";
        let updated = set_toml_value(original, "theme", "tab_hover", "\"#c8e6c9\"");
        let theme_section = updated.split("[takeover]").next().unwrap();
        assert!(theme_section.contains("tab_hover = \"#c8e6c9\""), "{updated}");
        assert!(updated.contains("mode = \"off\""), "{updated}");
    }

    #[test]
    fn set_toml_value_creates_missing_section() {
        let original = "# just a comment\n";
        let updated = set_toml_value(original, "search", "backend", "\"system\"");
        assert!(updated.contains("[search]"), "{updated}");
        assert!(updated.contains("backend = \"system\""), "{updated}");
        assert!(updated.contains("# just a comment"), "{updated}");
    }

    #[test]
    fn set_toml_values_applies_all_edits() {
        let original = "[theme]\nbg = \"#green\"\nfg = \"#dark\"\n# keep me\n";
        let updated = set_toml_values(
            original,
            &[("theme", "bg", "\"#2b2b2b\""), ("theme", "fg", "\"#cccccc\"")],
        );
        assert!(updated.contains("bg = \"#2b2b2b\""), "{updated}");
        assert!(updated.contains("fg = \"#cccccc\""), "{updated}");
        assert!(updated.contains("# keep me"), "{updated}");
    }
}
