//! Settings tab: a painted (no HWND) page inside the main window's view rect.
//!
//! Mirrors the desktop-app settings layout: a left category rail and a right
//! pane of rows. Everything is GDI-drawn from the same `Config` the main
//! window reads, so a config change repaints both at once via the existing
//! watcher -> WM_APP_CONFIG_CHANGED path.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject, SelectObject, SetBkMode,
    SetTextColor, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HDC, HGDIOBJ, TRANSPARENT,
};

use crate::config::Config;

use super::gdi::rgb;

const RAIL_W: i32 = 140;
const ROW_H: i32 = 52;
const ROW_GAP: i32 = 8;

const CATEGORIES: [&str; 3] = ["常规", "主题", "关于"];
pub const CATEGORY_COUNT: usize = CATEGORIES.len();

/// Two complete colour schemes. Every theme key that affects what the user
/// sees must appear in BOTH lists — a partial switch produced a green/grey
/// hybrid (the "3 shapes" bug). Values are TOML literals (quoted strings).
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

/// Which preset (if any) the current config matches, by checking the whole
/// scheme — not just tabbar_background — so a half-switched config reports
/// "自定义" instead of claiming to be a preset.
pub fn current_preset(cfg: &Config) -> Option<&'static str> {
    let matches = |preset: &[(&str, &str, &str)]| {
        preset.iter().all(|(_, key, expected)| {
            let actual = theme_value(cfg, key);
            // TOML literal -> plain value for comparison.
            let want = expected.trim_matches('"');
            actual.eq_ignore_ascii_case(want)
        })
    };
    if matches(GREEN_THEME) {
        Some("绿色")
    } else if matches(DARK_THEME) {
        Some("深灰蓝")
    } else {
        None
    }
}

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

/// What a mouse click on the settings page selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Category(usize),
    /// Toggle takeover mode between off and redirect.
    TakeoverMode,
    /// Cycle tab corner radius through a small preset list.
    TabRadius,
    /// Cycle the whole theme through bundled presets.
    ThemePreset,
}

pub struct SettingsPanel {
    category: usize,
}

impl SettingsPanel {
    pub fn new() -> Self {
        Self { category: 0 }
    }

    pub fn select(&mut self, category: usize) {
        if category < CATEGORY_COUNT {
            self.category = category;
        }
    }

    fn color(cfg: &Config, key: impl FnOnce(&crate::config::Theme) -> &str) -> COLORREF {
        let (r, g, b) = cfg.color(key);
        rgb(r, g, b)
    }

    /// Pick legible text on an arbitrary background: light bg -> dark ink and
    /// vice versa. The theme's tab_text is tuned for tabs, not for this page,
    /// so we derive contrast instead of reusing it blindly.
    fn ink_on(bg: COLORREF) -> COLORREF {
        let r = (bg.0 & 0xff) as u32;
        let g = ((bg.0 >> 8) & 0xff) as u32;
        let b = ((bg.0 >> 16) & 0xff) as u32;
        let luma = r * 299 / 1000 + g * 587 / 1000 + b * 114 / 1000;
        if luma > 128 {
            COLORREF(0x0010_1010) // near-black
        } else {
            COLORREF(0x00F5_F5F5) // near-white
        }
    }

    /// Paint the settings page into `rc` (already the view rect).
    pub unsafe fn paint(&self, hdc: HDC, rc: RECT, cfg: &Config) {
        let bg = Self::color(cfg, |t| &t.tabbar_background);
        let fill = Self::color(cfg, |t| &t.tab_active);
        let rail_bg = Self::color(cfg, |t| &t.bookmark_bar_background);
        let page_ink = Self::ink_on(bg);
        let rail_ink = Self::ink_on(rail_bg);

        // Whole page background + left rail.
        let brush = CreateSolidBrush(bg);
        let _ = FillRect(hdc, &rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let rail = RECT { left: rc.left, top: rc.top, right: rc.left + RAIL_W, bottom: rc.bottom };
        let brush = CreateSolidBrush(rail_bg);
        let _ = FillRect(hdc, &rail, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));

        let _ = SetBkMode(hdc, TRANSPARENT);
        // Font: use stock GUI font like the tab strip; DPI scaling is handled
        // by Windows for the stock font.
        let font = GetStockObject(DEFAULT_GUI_FONT);
        let old_font = SelectObject(hdc, HGDIOBJ(font.0));

        // Category rail.
        let cat_y0 = rc.top + 40;
        for (i, label) in CATEGORIES.iter().enumerate() {
            let row = RECT {
                left: rc.left + 4,
                top: cat_y0 + (i as i32) * (ROW_H + ROW_GAP),
                right: rc.left + RAIL_W - 4,
                bottom: cat_y0 + (i as i32) * (ROW_H + ROW_GAP) + ROW_H,
            };
            if i == self.category {
                let brush = CreateSolidBrush(fill);
                let _ = FillRect(hdc, &row, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0));
                let _ = SetTextColor(hdc, Self::ink_on(fill));
            } else {
                let _ = SetTextColor(hdc, rail_ink);
            }
            let mut text = row;
            text.left += 12;
            let mut wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let _ = DrawTextW(
                hdc,
                &mut wide,
                &mut text,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
        }

        // Right pane.
        let pane_left = rc.left + RAIL_W + 24;
        let mut y = rc.top + 40;
        match self.category {
            0 => {
                self.text(hdc, pane_left, &mut y, rc.right - 24, "常规", page_ink, true);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "接管模式", &self.takeover_value(cfg), cfg);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "标签圆角", &cfg.theme.tab_radius.to_string(), cfg);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "点击行右侧的值可切换", page_ink);
            }
            1 => {
                self.text(hdc, pane_left, &mut y, rc.right - 24, "主题", page_ink, true);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "配色预设", &self.theme_preset_name(cfg), cfg);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "标签圆角", &cfg.theme.tab_radius.to_string(), cfg);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "配色与圆角改动会立即生效", page_ink);
            }
            _ => {
                self.text(hdc, pane_left, &mut y, rc.right - 24, "关于 Revolc", page_ink, true);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "把 Windows 资源管理器改造成多标签浏览器", page_ink);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "Clover 的原生重制版 · MPL-2.0", page_ink);
            }
        }

        let _ = SelectObject(hdc, old_font);
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn text(
        &self,
        hdc: HDC,
        x: i32,
        y: &mut i32,
        right: i32,
        s: &str,
        ink: COLORREF,
        heading: bool,
    ) {
        let _ = SetTextColor(hdc, ink);
        let mut rc = RECT { left: x, top: *y, right, bottom: *y + (if heading { 30 } else { 24 }) };
        let mut wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        *y += if heading { 36 } else { 28 };
    }

    unsafe fn note(&self, hdc: HDC, x: i32, y: &mut i32, right: i32, s: &str, ink: COLORREF) {
        let _ = SetTextColor(hdc, ink);
        let mut rc = RECT { left: x, top: *y, right, bottom: *y + 24 };
        let mut wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        *y += 28;
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn row(&self, hdc: HDC, x: i32, y: &mut i32, right: i32, label: &str, value: &str, cfg: &Config) {
        let rc = RECT { left: x, top: *y, right, bottom: *y + ROW_H };
        let row_bg = Self::color(cfg, |t| &t.tab_inactive);
        let brush = CreateSolidBrush(row_bg);
        let _ = FillRect(hdc, &rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = SetTextColor(hdc, Self::ink_on(row_bg));
        let mut lrc = RECT { left: x + 12, top: *y, right: x + 180, bottom: *y + ROW_H };
        let mut lwide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut lwide, &mut lrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        let mut vrc = RECT { left: right - 180, top: *y, right: right - 12, bottom: *y + ROW_H };
        let mut vwide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut vwide, &mut vrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        *y += ROW_H + ROW_GAP;
    }

    fn takeover_value(&self, cfg: &Config) -> String {
        if cfg.takeover.mode == "redirect" { "接管新窗口" } else { "关闭" }.to_string()
    }

    fn theme_preset_name(&self, cfg: &Config) -> String {
        match current_preset(cfg) {
            Some(name) => format!("{name}（当前）"),
            None => "自定义".to_string(),
        }
    }

    /// Hit-test a point in view-rect client coords.
    pub fn hit(&self, x: i32, y: i32, rc: RECT) -> Option<SettingsHit> {
        // Rail categories.
        let cat_y0 = rc.top + 40;
        for i in 0..CATEGORIES.len() {
            let top = cat_y0 + (i as i32) * (ROW_H + ROW_GAP);
            if x >= rc.left && x < rc.left + RAIL_W && y >= top && y < top + ROW_H {
                return Some(SettingsHit::Category(i));
            }
        }
        if x < rc.left + RAIL_W + 24 {
            return None;
        }
        // Rows start after the heading.
        let mut y_cursor = rc.top + 40 + 36;
        match self.category {
            0 => {
                if in_row(y, y_cursor) { return Some(SettingsHit::TakeoverMode); }
                y_cursor += ROW_H + ROW_GAP;
                if in_row(y, y_cursor) { return Some(SettingsHit::TabRadius); }
            }
            1 => {
                if in_row(y, y_cursor) { return Some(SettingsHit::ThemePreset); }
                y_cursor += ROW_H + ROW_GAP;
                if in_row(y, y_cursor) { return Some(SettingsHit::TabRadius); }
            }
            _ => {}
        }
        None
    }
}

fn in_row(y: i32, top: i32) -> bool {
    y >= top && y < top + ROW_H
}
