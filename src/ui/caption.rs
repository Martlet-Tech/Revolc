//! Caption buttons (minimize / maximize / close), drawn at the right end of
//! the tab bar — Chrome-style integrated title bar (spec §4.2.1).
//!
//! The main window no longer carries `WS_CAPTION`; `WM_NCCALCSIZE` hands the
//! whole frame to us and the tab bar paints the caption band. That splits the
//! work in a non-obvious way:
//!
//! * **pixels** belong to the tab bar (it also owns the tabs next to them, so
//!   the two must share one painter and one geometry),
//! * **hit testing** belongs to the main window, because a child returning
//!   `HTTRANSPARENT` only forwards the point *up*; the codes `HTMINBUTTON` /
//!   `HTMAXBUTTON` / `HTCLOSE` must come from the toplevel, and only then does
//!   `DefWindowProc` run the real `WM_SYSCOMMAND` — which is also what makes
//!   Win11 Snap Layouts appear when the maximize button is hovered.
//!
//! So: one geometry source (this module), two consumers (tabbar paint, main
//! window hit test).

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, FillRect, LineTo, MoveToEx, SelectObject, HDC,
    HGDIOBJ, PS_SOLID,
};
use windows::Win32::UI::WindowsAndMessaging::{HTCLOSE, HTMAXBUTTON, HTMINBUTTON};

/// Height of the caption band, i.e. the whole tab bar. Windows' own caption is
/// 31–32px at 100% DPI; 34 leaves room for a full-height tab plus its rounding.
pub const HEIGHT: i32 = 34;
/// Width of one caption button. Windows 11 uses 46; keep it, muscle memory.
pub const BUTTON_W: i32 = 46;
/// Width of the whole button block (3 buttons).
pub const WIDTH: i32 = BUTTON_W * 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

impl Button {
    /// Non-client hit code for this button. Returned by the main window's
    /// `WM_NCHITTEST`; `DefWindowProc` turns a click on it into the matching
    /// `WM_SYSCOMMAND`.
    pub fn hit_code(self) -> u32 {
        match self {
            Button::Minimize => HTMINBUTTON,
            Button::Maximize => HTMAXBUTTON,
            Button::Close => HTCLOSE,
        }
    }

    fn slot(self) -> i32 {
        match self {
            Button::Minimize => 0,
            Button::Maximize => 1,
            Button::Close => 2,
        }
    }
}

/// Left edge of the button block in bar-client coordinates.
pub fn left_edge(bar_width: i32) -> i32 {
    bar_width - WIDTH
}

/// Which button (if any) is under a point given in bar-client coordinates.
pub fn hit(x: i32, y: i32, bar_width: i32) -> Option<Button> {
    if !(0..HEIGHT).contains(&y) {
        return None;
    }
    let left = left_edge(bar_width);
    if x < left {
        return None;
    }
    match (x - left) / BUTTON_W {
        0 => Some(Button::Minimize),
        1 => Some(Button::Maximize),
        2 => Some(Button::Close),
        _ => None,
    }
}

/// Theme colours for the caption buttons.
#[derive(Clone, Copy)]
pub struct Colors {
    pub hover: COLORREF,
    /// Close button hover is red everywhere; a "danger" affordance, not a theme.
    pub close_hover: COLORREF,
    pub symbol: COLORREF,
    /// Window is not the active window: Windows dims its glyphs the same way.
    pub symbol_idle: COLORREF,
}

impl Colors {
    pub fn new(hover: COLORREF, close_hover: COLORREF, symbol: COLORREF, idle: COLORREF) -> Self {
        Self { hover, close_hover, symbol, symbol_idle: idle }
    }
}

/// Draw the three buttons. `hover` is driven by the main window's
/// `WM_NCMOUSEMOVE` (see ui/mod.rs) — the tab bar never sees mouse input there,
/// because it deliberately reports the area as `HTTRANSPARENT`.
pub unsafe fn paint(
    hdc: HDC,
    bar_width: i32,
    hover: Option<Button>,
    maximized: bool,
    active: bool,
    c: &Colors,
) {
    let left = left_edge(bar_width);
    for b in [Button::Minimize, Button::Maximize, Button::Close] {
        let x = left + b.slot() * BUTTON_W;
        let rc = RECT { left: x, top: 0, right: x + BUTTON_W, bottom: HEIGHT };
        if hover == Some(b) {
            let bg = if b == Button::Close { c.close_hover } else { c.hover };
            fill(hdc, &rc, bg);
        }
        let hot_close = hover == Some(Button::Close);
        // On the red hover the glyph must be white, whatever the theme says.
        let symbol = if hot_close {
            COLORREF(0x00FF_FFFF)
        } else if active {
            c.symbol
        } else {
            c.symbol_idle
        };
        glyph(hdc, b, &rc, symbol, maximized);
    }
}

unsafe fn fill(hdc: HDC, rc: &RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let _ = FillRect(hdc, rc, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));
}

/// Glyphs are 1px GDI lines, not font text: the stock UI font renders "×" and
/// "□" with a baseline offset that never lines up across DPI, and Segoe MDL2
/// assets are not worth a font dependency for three shapes.
unsafe fn glyph(hdc: HDC, b: Button, rc: &RECT, color: COLORREF, maximized: bool) {
    let pen = CreatePen(PS_SOLID, 1, color);
    let old = SelectObject(hdc, HGDIOBJ(pen.0));
    let cx = (rc.left + rc.right) / 2;
    let cy = (rc.top + rc.bottom) / 2;
    match b {
        Button::Minimize => line(hdc, cx - 5, cy, cx + 6, cy),
        Button::Maximize if maximized => {
            // "Restore": a small front box plus the corner of the box behind.
            box_outline(hdc, cx - 4, cy - 5, cx + 5, cy + 4);
            line(hdc, cx - 5, cy - 2, cx - 5, cy + 5);
            line(hdc, cx - 5, cy + 5, cx + 2, cy + 5);
            line(hdc, cx + 2, cy + 5, cx + 2, cy + 2);
        }
        Button::Maximize => box_outline(hdc, cx - 5, cy - 5, cx + 5, cy + 5),
        Button::Close => {
            line(hdc, cx - 5, cy - 5, cx + 6, cy + 6);
            line(hdc, cx + 5, cy - 5, cx - 6, cy + 6);
        }
    }
    SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(pen.0));
}

unsafe fn line(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32) {
    let _ = MoveToEx(hdc, x1, y1, None);
    let _ = LineTo(hdc, x2, y2);
}

unsafe fn box_outline(hdc: HDC, l: i32, t: i32, r: i32, b: i32) {
    line(hdc, l, t, r, t);
    line(hdc, r, t, r, b);
    line(hdc, r, b, l, b);
    line(hdc, l, b, l, t);
}
