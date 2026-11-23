//! The toolbar row of the unified chrome popup.
//!
//! Sits directly below the tab strip (y = TABBAR_HEIGHT..CHROME_HEIGHT).
//! Owned by the same popup as the tabs, so it inherits the Z-order guarantee
//! (no Explorer pane can cover it) and the single paint buffer. Everything
//! here is paint-only; hit testing lives in the main window's WndProc.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetStockObject, LineTo,
    MoveToEx, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, HDC, HGDIOBJ, PS_SOLID, TRANSPARENT,
};

use crate::bookmarks::Bookmark;
use crate::config::Config;

use super::caption;

/// Geometry of one bookmark chip. Fixed width keeps paint and hit-test
/// perfectly in sync (no GDI text measurement needed), at the cost of a
/// simple ellipsis for long labels.
const BM_X0: i32 = 8;
const BM_W: i32 = 92;
const BM_GAP: i32 = 4;
const BM_H: i32 = 20;

/// X position of the settings button's left edge.
pub fn settings_button_x(width: i32) -> i32 {
    width - super::SETTINGS_BTN_W
}

/// How many bookmark chips fit before the settings button. Extra bookmarks
/// are not painted or clickable until the window is widened.
pub fn visible_bookmarks(width: i32, total: usize) -> usize {
    let room = settings_button_x(width) - 8 - BM_X0;
    if room <= 0 {
        return 0;
    }
    let per = BM_W + BM_GAP;
    (((room + BM_GAP) / per).max(0) as usize).min(total)
}

/// Rectangle of bookmark `i` in chrome-popup client coords, if visible.
fn bookmark_rect(i: usize, width: i32) -> Option<RECT> {
    if i >= visible_bookmarks(width, usize::MAX) {
        return None;
    }
    let top = caption::HEIGHT + (super::BOOKMARK_HEIGHT - BM_H) / 2;
    Some(RECT {
        left: BM_X0 + (i as i32) * (BM_W + BM_GAP),
        top,
        right: BM_X0 + (i as i32) * (BM_W + BM_GAP) + BM_W,
        bottom: top + BM_H,
    })
}

/// Which bookmark chip is under (x, y) in chrome-popup client coords?
pub fn hit_bookmark(x: i32, y: i32, width: i32, total: usize) -> Option<usize> {
    let top = caption::HEIGHT + (super::BOOKMARK_HEIGHT - BM_H) / 2;
    if !(top..top + BM_H).contains(&y) {
        return None;
    }
    (0..visible_bookmarks(width, total)).find(|&i| {
        let left = BM_X0 + (i as i32) * (BM_W + BM_GAP);
        x >= left && x < left + BM_W
    })
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

/// Which layer handles a left click in the main window's client coordinates.
///
/// The split is purely vertical: everything above CHROME_HEIGHT is window
/// chrome (tabs, caption buttons) and always goes to the tab bar hit test —
/// even when a Painted (settings) tab is active. Only the area below the
/// chrome belongs to the painted page. Getting this wrong swallows the X /
/// min / max buttons and makes an active settings tab impossible to escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickRoute {
    Chrome,
    PaintedPage,
    FolderView,
}

pub fn route_click(y: i32, painted_active: bool) -> ClickRoute {
    if y < super::CHROME_HEIGHT {
        ClickRoute::Chrome
    } else if painted_active {
        ClickRoute::PaintedPage
    } else {
        ClickRoute::FolderView
    }
}

/// Only a double-click in the chrome band may toggle maximize. The tab bar's
/// hit test reports `Band` for the whole content area (y >= TABBAR_HEIGHT),
/// so double-clicking a painted settings page would otherwise maximize the
/// window. Gate on the chrome band first.
pub fn is_chrome_double_click(y: i32) -> bool {
    y < super::CHROME_HEIGHT
}

/// Paint the toolbar row. `top` is where the row starts (= TABBAR_HEIGHT).
/// Bookmarks are drawn left-to-right as chips; the settings glyph owns the
/// right edge. With no bookmarks a hint reminds the user where to add them.
pub unsafe fn paint(hdc: HDC, width: i32, top: i32, cfg: &Config, bookmarks: &[Bookmark]) {
    let height = super::BOOKMARK_HEIGHT;
    let (r, g, b) = cfg.color(|t| &t.bookmark_bar_background);
    let bg = COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32);
    let brush = CreateSolidBrush(bg);
    let row = RECT { left: 0, top, right: width, bottom: top + height };
    let _ = FillRect(hdc, &row, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));

    let font = GetStockObject(DEFAULT_GUI_FONT);
    let old_font = SelectObject(hdc, HGDIOBJ(font.0));
    let _ = SetBkMode(hdc, TRANSPARENT);

    let n = visible_bookmarks(width, bookmarks.len());
    if n == 0 {
        // Text colour: lift the background toward light so it reads on both
        // the default dark grey and a user-supplied green.
        let tr = (r as i32 / 2 + 0x50).min(0xff) as u8;
        let tg = (g as i32 / 2 + 0x50).min(0xff) as u8;
        let tb = (b as i32 / 2 + 0x50).min(0xff) as u8;
        let _ = SetTextColor(hdc, COLORREF((tb as u32) << 16 | (tg as u32) << 8 | tr as u32));
        let mut text = RECT { left: 10, top, right: settings_button_x(width) - 4, bottom: top + height };
        let mut label: Vec<u16> = "收藏夹：在标签上点右键即可收藏".encode_utf16().chain(Some(0)).collect();
        let _ = DrawTextW(hdc, &mut label, &mut text, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    } else {
        let (cr, cg, cb) = cfg.color(|t| &t.tab_active);
        let chip = COLORREF((cb as u32) << 16 | (cg as u32) << 8 | cr as u32);
        let (ir, ig, ib) = cfg.color(|t| &t.tab_text_active);
        let ink = COLORREF((ib as u32) << 16 | (ig as u32) << 8 | ir as u32);
        let chip_brush = CreateSolidBrush(chip);
        for (i, bm) in bookmarks.iter().take(n).enumerate() {
            let Some(rect) = bookmark_rect(i, width) else { continue };
            let _ = FillRect(hdc, &rect, chip_brush);
            let mut text = RECT {
                left: rect.left + 6,
                top: rect.top,
                right: rect.right - 4,
                bottom: rect.bottom,
            };
            let mut label: Vec<u16> = bm.label.encode_utf16().chain(Some(0)).collect();
            let _ = SetTextColor(hdc, ink);
            let _ = DrawTextW(
                hdc,
                &mut label,
                &mut text,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
        let _ = DeleteObject(HGDIOBJ(chip_brush.0));
    }
    let _ = SelectObject(hdc, old_font);

    // Settings glyph at the right edge, drawn with the same 1px-line approach
    // as the caption buttons (no font dependency, DPI-stable).
    let (sr, sg, sb) = cfg.color(|t| &t.tabbar_text);
    paint_hexagon(
        hdc,
        settings_button_x(width),
        top,
        super::SETTINGS_BTN_W,
        height,
        COLORREF((sb as u32) << 16 | (sg as u32) << 8 | sr as u32),
    );
}

/// A regular hexagon outline with a small filled centre dot — the familiar
/// "settings" glyph, drawn with 1px GDI lines like the caption buttons.
/// At r=6 the six vertices are (±6,0) and (±3,±5); cos/sin(60°) rounded to
/// integers, so no floating point is needed and it is pixel-stable.
unsafe fn paint_hexagon(hdc: HDC, x: i32, y: i32, w: i32, h: i32, color: COLORREF) {
    let cx = x + w / 2;
    let cy = y + h / 2;
    let r = 6;
    let dx = 3; // r * cos(60°) = 3
    let dy = 5; // r * sin(60°) ≈ 5.2 -> 5
    let vertices = [
        (cx - r, cy),
        (cx - dx, cy - dy),
        (cx + dx, cy - dy),
        (cx + r, cy),
        (cx + dx, cy + dy),
        (cx - dx, cy + dy),
    ];
    let pen = CreatePen(PS_SOLID, 1, color);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    for i in 0..vertices.len() {
        let (x1, y1) = vertices[i];
        let (x2, y2) = vertices[(i + 1) % vertices.len()];
        line(hdc, x1, y1, x2, y2);
    }
    // Centre dot for the "settings" affordance.
    line(hdc, cx - 1, cy, cx + 1, cy);
    SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn line(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32) {
    let _ = MoveToEx(hdc, x1, y1, None);
    let _ = LineTo(hdc, x2, y2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::caption;
    use super::super::{BOOKMARK_HEIGHT, CHROME_HEIGHT};

    #[test]
    fn caption_band_always_routes_to_chrome_even_when_settings_active() {
        // Regression: an active settings tab used to swallow every click,
        // including the window X / min / max buttons (y < TABBAR_HEIGHT) and
        // the tab strip. Those must always reach the tab bar hit test.
        for y in [0, caption::HEIGHT / 2, caption::HEIGHT - 1] {
            assert_eq!(route_click(y, true), ClickRoute::Chrome, "y={y}");
        }
    }

    #[test]
    fn toolbar_band_routes_to_chrome() {
        // The bookmark row is chrome, not the painted page.
        assert_eq!(route_click(caption::HEIGHT, true), ClickRoute::Chrome);
        assert_eq!(
            route_click(caption::HEIGHT + BOOKMARK_HEIGHT - 1, true),
            ClickRoute::Chrome
        );
    }

    #[test]
    fn view_rect_routes_by_active_tab_kind() {
        let y = CHROME_HEIGHT + 10;
        assert_eq!(route_click(y, true), ClickRoute::PaintedPage);
        assert_eq!(route_click(y, false), ClickRoute::FolderView);
    }

    #[test]
    fn double_click_in_view_rect_never_maximizes() {
        // Regression: tabbar.hit() reports Band for y >= TABBAR_HEIGHT, so a
        // double-click on the settings page used to toggle maximize.
        assert!(is_chrome_double_click(caption::HEIGHT / 2));
        assert!(!is_chrome_double_click(CHROME_HEIGHT));
        assert!(!is_chrome_double_click(CHROME_HEIGHT + 50));
    }

    #[test]
    fn bookmark_chips_hit_test_inside_toolbar_row() {
        let width = 800;
        let y = caption::HEIGHT + BOOKMARK_HEIGHT / 2;
        assert_eq!(hit_bookmark(10, y, width, 3), Some(0));
        assert_eq!(hit_bookmark(8 + 92 + 4 + 10, y, width, 3), Some(1));
        // The gaps between chips are dead space.
        assert_eq!(hit_bookmark(8 + 92 + 2, y, width, 3), None);
        // Not in the toolbar row -> no hit.
        assert_eq!(hit_bookmark(10, caption::HEIGHT / 2, width, 3), None);
    }

    #[test]
    fn bookmark_count_is_clamped_by_width_and_total() {
        assert_eq!(visible_bookmarks(800, 3), 3);
        assert_eq!(visible_bookmarks(800, 0), 0);
        // A narrow window leaves room for fewer chips.
        assert!(visible_bookmarks(140, 10) < 10);
        assert_eq!(hit_bookmark(600, caption::HEIGHT + BOOKMARK_HEIGHT / 2, 140, 10), None);
    }
}
