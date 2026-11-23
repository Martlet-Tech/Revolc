//! Settings tab: a painted (no HWND) page inside the main window's view rect.
//!
//! The rows come from `revolc_common::settings::SettingId::ALL` — this file
//! knows how to *draw* a switch row and how to hit-test it, but not what the
//! settings are, where they live in TOML, or what values are legal. Adding a
//! setting is one entry in the registry; nothing here changes.
//!
//! Everything is drawn from the same `Config` the main window reads, so a
//! config change repaints both at once via the existing watcher ->
//! WM_APP_CONFIG_CHANGED path.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject, SelectObject, SetBkMode,
    SetTextColor, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HDC, HGDIOBJ, TRANSPARENT,
};

use revolc_common::config::Config;
use revolc_common::settings::SettingId;

use super::gdi::{contrast_text, rgb};

const RAIL_W: i32 = 140;
const ROW_H: i32 = 52;
const ROW_GAP: i32 = 8;

const CATEGORIES: [&str; 3] = ["常规", "主题", "关于"];
pub const CATEGORY_COUNT: usize = CATEGORIES.len();

/// Which category owns each exposed setting. Kept here (UI concern) so the
/// registry does not need to know about page layout.
fn settings_in_category(category: usize) -> Vec<SettingId> {
    match category {
        0 => vec![
            SettingId::TakeoverMode,
            SettingId::TrayEnabled,
            SettingId::Autostart,
            SettingId::CloseAction,
        ],
        1 => vec![SettingId::ThemePreset, SettingId::TabRadius],
        _ => Vec::new(),
    }
}

/// What a mouse click on the settings page selected: a category on the rail,
/// or one of the registry settings (identity carried by `SettingId`, not a
/// hard-coded variant per row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Category(usize),
    Setting(SettingId),
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

    fn color(cfg: &Config, key: impl FnOnce(&revolc_common::config::Theme) -> &str) -> COLORREF {
        let (r, g, b) = cfg.color(key);
        rgb(r, g, b)
    }

    /// Paint the settings page into `rc` (already the view rect).
    pub unsafe fn paint(&self, hdc: HDC, rc: RECT, cfg: &Config) {
        let bg = Self::color(cfg, |t| &t.tabbar_background);
        let fill = Self::color(cfg, |t| &t.tab_active);
        let rail_bg = Self::color(cfg, |t| &t.bookmark_bar_background);
        let page_ink = contrast_text(bg);
        let rail_ink = contrast_text(rail_bg);

        // Whole page background + left rail.
        let brush = CreateSolidBrush(bg);
        let _ = FillRect(hdc, &rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        let rail = RECT { left: rc.left, top: rc.top, right: rc.left + RAIL_W, bottom: rc.bottom };
        let brush = CreateSolidBrush(rail_bg);
        let _ = FillRect(hdc, &rail, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));

        let _ = SetBkMode(hdc, TRANSPARENT);
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
                let _ = SetTextColor(hdc, contrast_text(fill));
            } else {
                let _ = SetTextColor(hdc, rail_ink);
            }
            let mut text = row;
            text.left += 12;
            let mut wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
            let _ = DrawTextW(hdc, &mut wide, &mut text, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }

        // Right pane: heading + one row per registry setting in this category.
        let pane_left = rc.left + RAIL_W + 24;
        let pane_right = rc.right - 24;
        let mut y = rc.top + 40;
        match self.category {
            0 => self.text(hdc, pane_left, &mut y, pane_right, "常规", page_ink, true),
            1 => self.text(hdc, pane_left, &mut y, pane_right, "主题", page_ink, true),
            _ => {
                self.text(hdc, pane_left, &mut y, pane_right, "关于 Revolc", page_ink, true);
                self.note(hdc, pane_left, &mut y, pane_right, "把 Windows 资源管理器改造成多标签浏览器", page_ink);
                self.note(hdc, pane_left, &mut y, pane_right, "Clover 的原生重制版 · MPL-2.0", page_ink);
            }
        }
        for id in settings_in_category(self.category) {
            self.row(hdc, pane_left, &mut y, pane_right, id.label(), &id.display(cfg), cfg);
            let hint = id.hint();
            if !hint.is_empty() {
                y -= 12; // tuck the hint under the row without a full gap
                self.note(hdc, pane_left + 12, &mut y, pane_right, hint, page_ink);
            }
        }
        if self.category != 2 {
            self.note(hdc, pane_left, &mut y, pane_right, "点击行右侧的值可切换", page_ink);
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
        let _ = SetTextColor(hdc, contrast_text(row_bg));
        let mut lrc = RECT { left: x + 12, top: *y, right: x + 180, bottom: *y + ROW_H };
        let mut lwide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut lwide, &mut lrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        let mut vrc = RECT { left: right - 180, top: *y, right: right - 12, bottom: *y + ROW_H };
        let mut vwide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut vwide, &mut vrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        *y += ROW_H + ROW_GAP;
    }

    /// Hit-test a point in view-rect client coords.
    pub fn hit(&self, x: i32, y: i32, rc: RECT) -> Option<SettingsHit> {
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
        // Rows follow the heading; hints shift later rows, so walk them in
        // the same order paint() laid them out.
        let mut y_cursor = rc.top + 40 + 36;
        match self.category {
            0 | 1 => {
                for id in settings_in_category(self.category) {
                    if in_row(y, y_cursor) {
                        return Some(SettingsHit::Setting(id));
                    }
                    y_cursor += ROW_H + ROW_GAP;
                    if !id.hint().is_empty() {
                        y_cursor -= 12;
                        y_cursor += 28; // note() height
                    }
                }
            }
            _ => {}
        }
        None
    }
}

fn in_row(y: i32, top: i32) -> bool {
    y >= top && y < top + ROW_H
}
