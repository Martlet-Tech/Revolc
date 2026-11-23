//! Tiny GDI helpers shared by every painter in the chrome popup.
//!
//! `COLORREF` is `0x00BBGGRR` — the byte order is the opposite of the
//! `#RRGGBB` strings in the config. That conversion used to be hand-written at
//! every call site; one helper keeps it correct (and makes the intent clear).

use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{LineTo, MoveToEx, HDC};

/// `(r, g, b)` → `COLORREF` (`0x00BBGGRR`).
pub fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32)
}

/// Draw one 1px line. Glyphs here are GDI lines, not font text: the stock UI
/// font renders symbols with a baseline offset that never lines up across DPI.
pub unsafe fn line(hdc: HDC, x1: i32, y1: i32, x2: i32, y2: i32) {
    let _ = MoveToEx(hdc, x1, y1, None);
    let _ = LineTo(hdc, x2, y2);
}
