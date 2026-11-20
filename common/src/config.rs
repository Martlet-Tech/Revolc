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
}

/// Tray icon (Phase 1 of docs/双进程架构设计.md). Read at startup only:
/// changing `enabled` requires a restart until the tray becomes its own exe.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Tray {
    pub enabled: bool,
}

impl Default for Tray {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Takeover {
    /// "off" | "redirect" (doc/Takeover_Research.md §4; spec §7.1)
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
mode = "off"             # "off" | "redirect"
                         # redirect: 新 Explorer 文件夹窗口自动改由 Revolc 打开

[tray]
enabled = true           # revolc-tray.exe 是否注册/运行（改后需重启 tray）
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
