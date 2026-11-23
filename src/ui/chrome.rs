//! The toolbar row of the unified chrome popup.
//!
//! Sits directly below the tab strip (y = TABBAR_HEIGHT..CHROME_HEIGHT).
//! Owned by the same popup as the tabs, so it inherits the Z-order guarantee
//! (no Explorer pane can cover it) and the single paint buffer. Everything
//! here is paint-only; hit testing lives in the main window's WndProc.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject, LineTo,
    MoveToEx, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, HDC, HGDIOBJ, PS_SOLID, TRANSPARENT,
};

use crate::config::Config;

use super::caption;

/// X position of the settings button's left edge.
pub fn settings_button_x(width: i32) -> i32 {
    width - super::SETTINGS_BTN_W
}

/// Is (x, y) — in chrome-popup client coords — over the settings button?
pub fn hit_settings(x: i32, y: i32, width: i32) -> bool {
    let top = caption::HEIGHT;
    let bottom = top + super::BOOKMARK_HEIGHT;
    if !(top..bottom).contains(&y) {
        return false;
    }
    x >= settings_button_x(width)
}

/// Paint the toolbar row. `top` is where the row starts (= TABBAR_HEIGHT).
pub unsafe fn paint(hdc: HDC, width: i32, top: i32, cfg: &Config) {
    let height = super::BOOKMARK_HEIGHT;
    let (r, g, b) = cfg.color(|t| &t.bookmark_bar_background);
    let bg = COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32);
    let brush = CreateSolidBrush(bg);
    let row = RECT { left: 0, top, right: width, bottom: top + height };
    let _ = FillRect(hdc, &row, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));

    // Text colour: lift the background toward light so it reads on both the
    // default dark grey and a user-supplied green.
    let tr = (r as i32 / 2 + 0x50).min(0xff) as u8;
    let tg = (g as i32 / 2 + 0x50).min(0xff) as u8;
    let tb = (b as i32 / 2 + 0x50).min(0xff) as u8;
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let old_font = SelectObject(hdc, HGDIOBJ(font.0));
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF((tb as u32) << 16 | (tg as u32) << 8 | tr as u32));
    let mut text = RECT { left: 10, top, right: width - 10, bottom: top + height };
    let mut label: Vec<u16> = "收藏夹".encode_utf16().chain(Some(0)).collect();
    let _ = DrawTextW(
        hdc,
        &mut label,
        &mut text,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE,
    );
    let _ = SelectObject(hdc, old_font);

    // Settings gear at the right edge, drawn with the same 1px-line glyph
    // approach as the caption buttons (no font dependency, DPI-stable).
    let (sr, sg, sb) = cfg.color(|t| &t.tabbar_text);
    paint_gear(
        hdc,
        settings_button_x(width),
        top,
        super::SETTINGS_BTN_W,
        height,
        COLORREF((sb as u32) << 16 | (sg as u32) << 8 | sr as u32),
    );
}

/// A simple 8-tooth gear: circle + centre dot + short radial strokes.
unsafe fn paint_gear(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    let cx = x + w / 2;
    let cy = y + h / 2;
    let r = 5;
    let pen = CreatePen(PS_SOLID, 1, color);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    // Approximate a circle with a diamond + cross; at this size it reads as
    // "settings" without needing Ellipse() maths or a second pen.
    line(hdc, cx - r, cy, cx + r, cy);
    line(hdc, cx, cy - r, cx, cy + r);
    line(hdc, cx - r + 2, cy - r + 2, cx + r - 2, cy + r - 2);
    line(hdc, cx - r + 2, cy + r - 2, cx + r - 2, cy - r + 2);
    // Centre dot.
    line(hdc, cx - 1, cy, cx + 1, cy);
    SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn line(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32) {
    let _ = MoveToEx(hdc, x1, y1, None);
    let _ = LineTo(hdc, x2, y2);
}
