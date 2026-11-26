//! Pure geometry for dragging a tab out into its own window.
//!
//! The "separation band" is the horizontal strip around a window's title bar
//! in which a dragged tab stays attached to that window. Left/right are the
//! window's own edges; top/bottom extend half a title-bar height beyond the
//! title bar. All inputs are screen coordinates so the same test works for the
//! window the drag started in and for every other Revolc window on screen.
//!
//! No Win32, no painting — just numbers, so the boundary rules are testable.

/// Title-bar height in pixels (matches `ui::TABBAR_HEIGHT`).
pub const TITLE_H: i32 = 34;
/// How far above/below the title bar the band reaches (half of TITLE_H).
pub const BAND_MARGIN: i32 = TITLE_H / 2;

/// Window rectangle in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Is a screen point inside `win`'s separation band?
pub fn in_band(win: WinRect, x: i32, y: i32) -> bool {
    x >= win.left
        && x <= win.right
        && y >= win.top - BAND_MARGIN
        && y <= win.top + TITLE_H + BAND_MARGIN
}

/// Which window's band contains the point, with `from` excluded (the caller
/// tests the source window separately). First match wins; windows never
/// overlap their bands in practice, and if they do the topmost one in the
/// enumeration order is as good a choice as any.
pub fn band_owner<HWnd: Eq + Copy>(
    windows: impl IntoIterator<Item = (HWnd, WinRect)>,
    from: HWnd,
    x: i32,
    y: i32,
) -> Option<HWnd> {
    windows
        .into_iter()
        .find(|(h, rc)| *h != from && in_band(*rc, x, y))
        .map(|(h, _)| h)
}

/// Where the floating tab's top-left goes: the screen pointer minus the
/// grab offset captured when the press began. No special casing — the same
/// anchor is used inside and outside the band.
pub fn float_origin(screen_x: i32, screen_y: i32, grab_x: i32, grab_y: i32) -> (i32, i32) {
    (screen_x - grab_x, screen_y - grab_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: WinRect = WinRect { left: 100, top: 200, right: 900, bottom: 800 };

    #[test]
    fn band_covers_title_bar_and_half_height_margins() {
        assert!(in_band(W, 100, 200 - BAND_MARGIN));
        assert!(in_band(W, 900, 200 + TITLE_H + BAND_MARGIN));
        assert!(!in_band(W, 100, 200 - BAND_MARGIN - 1));
        assert!(!in_band(W, 900, 200 + TITLE_H + BAND_MARGIN + 1));
        assert!(!in_band(W, 99, 210));
        assert!(!in_band(W, 901, 210));
    }

    #[test]
    fn band_owner_skips_source_window() {
        let other = WinRect { left: 1000, top: 200, right: 1800, bottom: 800 };
        let list = [(1u32, W), (2u32, other)];
        assert_eq!(band_owner(list, 1, 1100, 210), Some(2));
        assert_eq!(band_owner(list, 1, 200, 210), None);
    }

    #[test]
    fn float_origin_keeps_grab_offset() {
        assert_eq!(float_origin(500, 210, 30, 10), (470, 200));
    }
}
