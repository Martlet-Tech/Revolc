//! Self-drawn tooltip card for the chrome strip.
//!
//! A top-level owned popup (see `ui/mod.rs`) paints a single line of text
//! below the pointer: the full path for bookmark chips and tabs, a short
//! label for the caption buttons and the settings gear. One font throughout —
//! the stock GUI font — so long paths stay readable.
//!
//! Layout is split into pure functions (`card_size`, `place`) so the geometry
//! can be unit-tested without a window. `measure` is the only part that needs
//! an HDC (font metrics); `paint` consumes the measured `Layout`, keeping the
//! text rectangle identical between measuring and drawing.
//!
//! Theme colours are borrowed from the existing palette — no new config keys.
//! Contrast is derived from the card background so every bundled preset stays
//! readable, using the same rule as the rest of the chrome.

use windows::Win32::Foundation::{COLORREF, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, FillRgn, FrameRgn,
    GetStockObject, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_CALCRECT,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HGDIOBJ, HFONT,
    HRGN, TRANSPARENT,
};

use crate::config::Config;

use super::gdi::{self, rgb};

/// Horizontal/vertical padding inside the card.
const PAD_X: i32 = 10;
const PAD_Y: i32 = 7;
/// Extra width beyond the measured text. `DrawTextW` with DT_END_ELLIPSIS
/// treats "text width == rect width" as overflow and drops glyphs for the
/// ellipsis; a slack keeps short labels intact without a visible gap.
const TEXT_SLACK: i32 = 12;
/// Corner radius of the rounded card.
const RADIUS: i32 = 6;
/// Upper bound on card width; very long paths get an ellipsis instead of a
/// screen-wide banner.
const MAX_W: i32 = 480;
/// Lower bound so an empty label still yields a visible card.
const MIN_W: i32 = 64;
/// Offset from the pointer to the card's top-left corner, before edge flips.
const OFFSET: i32 = 16;
/// Extra inset when a card is flipped to avoid the monitor edge.
const EDGE: i32 = 4;

/// What the tooltip shows — always a single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content(pub String);

/// The one font used by the card (system-owned, never destroyed).
unsafe fn font() -> HFONT {
    HFONT(GetStockObject(DEFAULT_GUI_FONT).0)
}

/// Measured card: outer size plus the text rectangle, shared by paint.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub size: SIZE,
    pub text: RECT,
}

/// Pure: derive the card size from the measured text extent.
pub fn card_size(text: SIZE) -> SIZE {
    SIZE {
        cx: (text.cx + PAD_X * 2 + TEXT_SLACK).clamp(MIN_W, MAX_W),
        cy: PAD_Y + text.cy + PAD_Y,
    }
}

/// Pure: screen position of the card given the pointer, its size, and the
/// monitor work area. Flips to the other side of the pointer near an edge,
/// then clamps so the card never leaves the work area.
pub fn place(anchor: POINT, size: SIZE, work: RECT) -> POINT {
    let mut x = anchor.x + OFFSET;
    let mut y = anchor.y + OFFSET;
    if x + size.cx > work.right {
        x = anchor.x - size.cx - OFFSET;
    }
    if y + size.cy > work.bottom {
        y = anchor.y - size.cy - OFFSET;
    }
    x = x.clamp(work.left + EDGE, (work.right - size.cx - EDGE).max(work.left + EDGE));
    y = y.clamp(work.top + EDGE, (work.bottom - size.cy - EDGE).max(work.top + EDGE));
    POINT { x, y }
}

/// Measure with the very same `DrawTextW` used to paint, so the two can never
/// disagree about glyph widths. DT_CALCRECT returns the required rect without
/// drawing; the flags match `paint` minus the ellipsis (which must not affect
/// the measurement of a string that is going to fit).
unsafe fn text_size(hdc: HDC, text: &str) -> SIZE {
    let f = font();
    let old = SelectObject(hdc, HGDIOBJ(f.0));
    let mut wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut rc = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let _ = DrawTextW(
        hdc,
        &mut wide,
        &mut rc,
        DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_CALCRECT,
    );
    SelectObject(hdc, old);
    SIZE { cx: rc.right - rc.left, cy: rc.bottom - rc.top }
}

/// Measure the card with the given HDC.
///
/// # Safety
/// `hdc` must be valid for the duration of the call.
pub unsafe fn measure(hdc: HDC, content: &Content) -> Layout {
    let text_sz = text_size(hdc, &content.0);
    let size = card_size(text_sz);
    let text = RECT {
        left: PAD_X,
        top: PAD_Y,
        right: size.cx - PAD_X,
        bottom: PAD_Y + text_sz.cy,
    };
    Layout { size, text }
}

/// Linear blend between two `COLORREF`s (`t = 0` → `a`, `t = 1` → `b`).
fn mix(a: COLORREF, b: COLORREF, t: i32) -> COLORREF {
    let ch = |c: COLORREF, shift: u32| -> i32 { ((c.0 >> shift) & 0xff) as i32 };
    let out = |shift: u32| -> u32 {
        let av = ch(a, shift);
        let bv = ch(b, shift);
        ((av + (bv - av) * t / 100).clamp(0, 0xff) as u32) << shift
    };
    COLORREF(out(0) | out(8) | out(16))
}

/// Paint the card into `hdc` at local origin (0,0), sized by `layout`.
///
/// # Safety
/// `hdc` must be valid for the duration of the call.
pub unsafe fn paint(hdc: HDC, content: &Content, cfg: &Config, layout: Layout) {
    let (r, g, b) = cfg.color(|t| &t.tabbar_background);
    let bg = rgb(r, g, b);
    let ink = gdi::contrast_text(bg);
    let border = mix(bg, ink, 35);

    let _ = SetBkMode(hdc, TRANSPARENT);

    let rgn: HRGN = CreateRoundRectRgn(
        0,
        0,
        layout.size.cx + 1,
        layout.size.cy + 1,
        RADIUS * 2,
        RADIUS * 2,
    );
    let bg_brush = CreateSolidBrush(bg);
    let _ = FillRgn(hdc, rgn, HBRUSH(bg_brush.0));
    let border_brush = CreateSolidBrush(border);
    let _ = FrameRgn(hdc, rgn, HBRUSH(border_brush.0), 1, 1);
    let _ = DeleteObject(HGDIOBJ(bg_brush.0));
    let _ = DeleteObject(HGDIOBJ(border_brush.0));
    let _ = DeleteObject(HGDIOBJ(rgn.0));

    let old_font = SelectObject(hdc, HGDIOBJ(font().0));
    let _ = SetTextColor(hdc, ink);
    let mut rc = layout.text;
    let mut wide: Vec<u16> = content.0.encode_utf16().chain(Some(0)).collect();
    let _ = DrawTextW(
        hdc,
        &mut wide,
        &mut rc,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    SelectObject(hdc, old_font);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_is_one_line_tall() {
        let sz = card_size(SIZE { cx: 80, cy: 16 });
        assert_eq!(sz.cy, PAD_Y * 2 + 16);
        assert_eq!(sz.cx, 80 + PAD_X * 2 + TEXT_SLACK);
    }

    #[test]
    fn card_width_is_capped() {
        let sz = card_size(SIZE { cx: 2000, cy: 16 });
        assert_eq!(sz.cx, MAX_W);
        assert!(card_size(SIZE { cx: 1, cy: 16 }).cx >= MIN_W);
    }

    #[test]
    fn place_prefers_bottom_right() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let p = place(POINT { x: 100, y: 100 }, SIZE { cx: 200, cy: 60 }, work);
        assert_eq!((p.x, p.y), (100 + OFFSET, 100 + OFFSET));
    }

    #[test]
    fn place_flips_near_right_and_bottom_edges() {
        let work = RECT { left: 0, top: 0, right: 1000, bottom: 800 };
        let size = SIZE { cx: 200, cy: 60 };
        let p = place(POINT { x: 950, y: 780 }, size, work);
        // Flipped left/up instead of overflowing the work area.
        assert_eq!(p.x, 950 - 200 - OFFSET);
        assert_eq!(p.y, 780 - 60 - OFFSET);
        assert!(p.x + size.cx <= work.right);
        assert!(p.y + size.cy <= work.bottom);
    }

    #[test]
    fn place_clamps_inside_work_area() {
        let work = RECT { left: 0, top: 0, right: 300, bottom: 200 };
        let size = SIZE { cx: 200, cy: 60 };
        let p = place(POINT { x: 10, y: 10 }, size, work);
        assert!(p.x >= work.left + EDGE);
        assert!(p.y >= work.top + EDGE);
        assert!(p.x + size.cx <= work.right - EDGE);
        assert!(p.y + size.cy <= work.bottom - EDGE);
    }
}
