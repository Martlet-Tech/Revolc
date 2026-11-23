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

/// Pick legible ink (near-black or near-white) for an arbitrary background.
/// Theme colours are tuned for their own widget, not for every surface they
/// end up behind — e.g. a light-green `tab_hover` behind the white settings
/// glyph made it vanish. Deriving contrast instead of reusing a theme colour
/// keeps every hover state readable across the bundled presets and user
/// custom colours.
pub fn contrast_text(bg: COLORREF) -> COLORREF {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_uses_bgr_byte_order() {
        assert_eq!(rgb(0x12, 0x34, 0x56).0, 0x00_56_34_12);
    }

    #[test]
    fn light_green_hover_gets_dark_ink() {
        // The exact bug: green preset's tab_hover #c8e6c9 with a white glyph.
        let green_hover = rgb(0xc8, 0xe6, 0xc9);
        assert_eq!(contrast_text(green_hover).0, 0x0010_1010);
    }

    #[test]
    fn dark_background_gets_light_ink() {
        assert_eq!(contrast_text(rgb(0x1b, 0x3d, 0x28)).0, 0x00F5_F5F5);
        assert_eq!(contrast_text(rgb(0x2b, 0x2b, 0x2b)).0, 0x00F5_F5F5);
    }
}
