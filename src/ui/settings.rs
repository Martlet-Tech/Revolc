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

const RAIL_W: i32 = 140;
const ROW_H: i32 = 52;
const ROW_GAP: i32 = 8;

const CATEGORIES: [&str; 3] = ["常规", "主题", "关于"];
pub const CATEGORY_COUNT: usize = CATEGORIES.len();

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
        COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
    }

    /// Paint the settings page into `rc` (already the view rect).
    pub unsafe fn paint(&self, hdc: HDC, rc: RECT, cfg: &Config) {
        let bg = Self::color(cfg, |t| &t.tabbar_background);
        let fill = Self::color(cfg, |t| &t.tab_active);
        let ink = Self::color(cfg, |t| &t.tab_text);
        let rail_bg = Self::color(cfg, |t| &t.bookmark_bar_background);

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
                let _ = SetTextColor(hdc, Self::color(cfg, |t| &t.tab_text_active));
            } else {
                let _ = SetTextColor(hdc, ink);
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
                self.text(hdc, pane_left, &mut y, rc.right - 24, "常规", cfg, true);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "接管模式", &self.takeover_value(cfg), cfg);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "标签圆角", &cfg.theme.tab_radius.to_string(), cfg);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "点击行右侧的值可切换", cfg);
            }
            1 => {
                self.text(hdc, pane_left, &mut y, rc.right - 24, "主题", cfg, true);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "配色预设", &self.theme_preset_name(cfg), cfg);
                self.row(hdc, pane_left, &mut y, rc.right - 24, "标签圆角", &cfg.theme.tab_radius.to_string(), cfg);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "配色与圆角改动会立即生效", cfg);
            }
            _ => {
                self.text(hdc, pane_left, &mut y, rc.right - 24, "关于 Revolc", cfg, true);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "把 Windows 资源管理器改造成多标签浏览器", cfg);
                self.note(hdc, pane_left, &mut y, rc.right - 24, "Clover 的原生重制版 · MPL-2.0", cfg);
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
        cfg: &Config,
        heading: bool,
    ) {
        let _ = SetTextColor(hdc, Self::color(cfg, |t| &t.tab_text_active));
        let mut rc = RECT { left: x, top: *y, right, bottom: *y + (if heading { 30 } else { 24 }) };
        let mut wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        *y += if heading { 36 } else { 28 };
    }

    unsafe fn note(&self, hdc: HDC, x: i32, y: &mut i32, right: i32, s: &str, cfg: &Config) {
        let _ = SetTextColor(hdc, Self::color(cfg, |t| &t.tab_text));
        let mut rc = RECT { left: x, top: *y, right, bottom: *y + 24 };
        let mut wide: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut wide, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        *y += 28;
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn row(&self, hdc: HDC, x: i32, y: &mut i32, right: i32, label: &str, value: &str, cfg: &Config) {
        let rc = RECT { left: x, top: *y, right, bottom: *y + ROW_H };
        let brush = CreateSolidBrush(Self::color(cfg, |t| &t.tab_inactive));
        let _ = FillRect(hdc, &rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let _ = SetTextColor(hdc, Self::color(cfg, |t| &t.tab_text_active));
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
        let bg = cfg.theme.tabbar_background.to_lowercase();
        if bg == "#1b3d28" { "绿色（当前）" } else { "自定义" }.to_string()
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
