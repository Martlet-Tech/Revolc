//! Tab strip: pure painting + hit testing, owned by the main window.
//!
//! It used to be a child window. It cannot be one any more: the strip is the
//! title bar now (spec §4.2.1), and a child window cannot answer a *non-client*
//! hit test — `HTTRANSPARENT` only forwards to the parent as a **client** hit,
//! so user32 re-asks the child forever (measured: 30 hit tests in 1.5ms) and
//! the click never lands anywhere. Chrome draws its tab strip in the toplevel
//! for the same reason, and so do we: the main window paints the band and
//! answers `HTCAPTION` / `HTMINBUTTON` / `HTMAXBUTTON` / `HTCLOSE` itself,
//! which is also what brings native drag, double-click and Win11 Snap Layouts.
//!
//! Everything here is main-thread only and paint-only (no state machine): the
//! caller decides when to invalidate.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW,
    FillRect, FillRgn, GetStockObject, IntersectClipRect, RestoreDC, SaveDC, SelectObject, SetBkMode,
    SetTextColor, HDC, HFONT, HGDIOBJ, DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER,
    DEFAULT_GUI_FONT, RGN_AND, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL};

use crate::config::Config;
use crate::ui::{caption, icon};

pub const TAB_MAX_W: i32 = 190;
/// Floor for the per-tab width. Tabs share whatever the fixed furniture leaves
/// over — gutter, "+", window buttons — and shrink well below their label size
/// rather than push the right end of the strip off-screen. 24px is about the
/// narrowest tab that still holds a close X and a couple of characters; a
/// browser starts scrolling at this point, we clip the last tab instead.
pub const TAB_MIN_W: i32 = 24;
/// Tabs stop this far above the band's bottom edge; the strip above keeps the
/// rounded top corners off the window's own rounding and is the drag area.
const TAB_TOP: i32 = 4;
const TAB_BOTTOM: i32 = caption::HEIGHT;
const TAB_GAP: i32 = 4;
/// Leading app-icon gutter. Tabs start to its right, so this strip is bare band
/// at all times: once the tabs fill the width there is nowhere else left to
/// grab the window, and a full row of tabs would make it unmovable (and
/// undrag-by-title). Sized 16px icon + 6px breathing room on both sides.
const GUTTER: i32 = 28;
const ICON_SIZE: i32 = 16;
const CLOSE_BOX: i32 = 16; // per-tab close (X) button size
const PLUS_SIZE: i32 = 26; // trailing "+" button size
/// Vertical centre of the band, for the "+" pill.
const CENTER: i32 = caption::HEIGHT / 2;

/// What a point in the band landed on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Tab(usize),
    /// The per-tab X.
    Close(usize),
    /// The trailing "+".
    Plus,
    /// Bare strip background: window drag / double-click maximize.
    Band,
    /// One of the caption buttons; the main window answers with its NC code.
    Caption(caption::Button),
}

/// Tab strip state + painter.
pub struct TabBar {
    /// Tab ids, parallel to `labels`. Callbacks report ids, not indices: the
    /// list can shrink between a click and its dispatch.
    ids: Vec<u64>,
    labels: Vec<String>,
    active: usize,
    bg: COLORREF,
    active_bg: COLORREF,
    hover_bg: COLORREF,
    inactive_bg: COLORREF,
    text: COLORREF,
    text_active: COLORREF,
    radius: i32,
    /// Per-tab width, recomputed from the client width so the tabs fill the
    /// band instead of being stuck at one hard-coded size.
    tab_w: i32,
    hover: Option<caption::Button>,
    /// Index of the tab under the pointer. Independent of `hover`: a tab lives
    /// in the client area, the caption buttons do not.
    hover_tab: Option<usize>,
    maximized: bool,
    window_active: bool,
    caption_colors: caption::Colors,
}

impl TabBar {
    pub fn new(cfg: &Config) -> Self {
        let mut bar = Self {
            ids: Vec::new(),
            labels: Vec::new(),
            active: 0,
            bg: COLORREF(0),
            active_bg: COLORREF(0),
            hover_bg: COLORREF(0),
            inactive_bg: COLORREF(0),
            text: COLORREF(0),
            text_active: COLORREF(0),
            radius: 0,
            tab_w: TAB_MAX_W,
            hover: None,
            hover_tab: None,
            maximized: false,
            window_active: true,
            caption_colors: caption::Colors::new(
                COLORREF(0),
                COLORREF(0),
                COLORREF(0),
                COLORREF(0),
            ),
        };
        bar.restyle(cfg);
        bar
    }

    /// Re-read every theme value (config hot reload).
    pub fn restyle(&mut self, cfg: &Config) {
        let c = |f: &dyn Fn(&crate::config::Theme) -> &str| {
            let (r, g, b) = cfg.color(f);
            COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32) // COLORREF = 0x00BBGGRR
        };
        self.bg = c(&|t| &t.tabbar_background);
        self.active_bg = c(&|t| &t.tab_active);
        self.hover_bg = c(&|t| &t.tab_hover);
        self.inactive_bg = c(&|t| &t.tab_inactive);
        self.text = c(&|t| &t.tab_text);
        self.text_active = c(&|t| &t.tab_text_active);
        self.radius = cfg.theme.tab_radius.max(0);
        let symbol = c(&|t| &t.tabbar_text);
        let bg = self.bg;
        self.caption_colors = caption::Colors::new(
            c(&|t| &t.caption_hover),
            c(&|t| &t.caption_close_hover),
            symbol,
            dim(symbol, bg),
        );
    }

    pub fn ids(&self) -> &[u64] {
        &self.ids
    }

    /// Snapshot push from the app (caller invalidates the band afterwards).
    pub fn update(
        &mut self,
        ids: &[u64],
        labels: &[String],
        active: usize,
        cfg: &Config,
        width: i32,
    ) {
        self.ids = ids.to_vec();
        self.labels = labels.to_vec();
        self.active = active;
        // The tab that was under the pointer may be gone (closed, or the list
        // reordered); never carry a stale highlight into the next frame.
        self.hover_tab = self.hover_tab.filter(|i| *i < self.labels.len());
        self.restyle(cfg);
        self.fit(width);
    }

    /// Caption-band state the strip paints but does not own. Returns true when
    /// the caller must repaint.
    pub fn set_chrome(
        &mut self,
        hover: Option<caption::Button>,
        maximized: bool,
        active: bool,
    ) -> bool {
        if self.hover == hover && self.maximized == maximized && self.window_active == active {
            return false;
        }
        self.hover = hover;
        self.maximized = maximized;
        self.window_active = active;
        true
    }

    pub fn hover(&self) -> Option<caption::Button> {
        self.hover
    }

    /// Which tab the pointer is over (driven from the same hit test as the
    /// caption buttons). Returns true when the caller must repaint the band.
    pub fn set_hover_tab(&mut self, index: Option<usize>) -> bool {
        let index = index.filter(|i| *i < self.labels.len());
        if self.hover_tab == index {
            return false;
        }
        self.hover_tab = index;
        true
    }

    /// Recompute tab widths for a new client width. Returns true on change.
    ///
    /// Tabs get exactly what the fixed furniture leaves over: gutter, "+",
    /// its breathing room and the three caption buttons are subtracted first,
    /// so the right end of the strip is always whole — measured 2026-11-18, a
    /// dozen tabs at the old 90px floor pushed the "+" and the close box clean
    /// off the end.
    pub fn fit(&mut self, width: i32) -> bool {
        let n = self.labels.len().max(1) as i32;
        let avail = width - GUTTER - PLUS_SIZE - 8 - caption::WIDTH - n * TAB_GAP;
        let want = (avail / n).clamp(TAB_MIN_W, TAB_MAX_W);
        if want == self.tab_w {
            return false;
        }
        self.tab_w = want;
        true
    }

    /// Close box: 16px on the tab's trailing edge, but never more than half the
    /// tab — a 24px tab still has to answer "select" on its leading half.
    fn close_w(&self) -> i32 {
        CLOSE_BOX.min(self.tab_w / 2).max(8)
    }

    fn tab_x(&self, i: usize) -> i32 {
        GUTTER + i as i32 * (self.tab_w + TAB_GAP)
    }

    fn plus_x(&self) -> i32 {
        self.tab_x(self.labels.len())
    }

    /// Client coords -> hit. `Band` for everything that is neither a tab nor a
    /// button, so callers never re-derive "is this the drag area".
    pub fn hit(&self, x: i32, y: i32, width: i32) -> Hit {
        if let Some(b) = caption::hit(x, y, width) {
            return Hit::Caption(b);
        }
        if !(0..caption::HEIGHT).contains(&y) {
            return Hit::Band;
        }
        let n = self.labels.len() as i32;
        // Trailing "+" button.
        let plus_x = self.plus_x();
        let plus_top = CENTER - PLUS_SIZE / 2;
        if x >= plus_x && x <= plus_x + PLUS_SIZE && (plus_top..plus_top + PLUS_SIZE).contains(&y) {
            return Hit::Plus;
        }
        // Left of the first tab (the icon gutter) is always bare band, i.e. a
        // caption hit; same for the gaps and the strip above the tabs.
        if x < GUTTER || y < TAB_TOP || n == 0 {
            return Hit::Band;
        }
        let i = (x - GUTTER) / (self.tab_w + TAB_GAP);
        if i < 0 || i >= n {
            return Hit::Band;
        }
        let tab_left = self.tab_x(i as usize);
        if x > tab_left + self.tab_w {
            return Hit::Band; // in the gap between tabs
        }
        // Close box occupies the tab's right edge.
        let close_left = tab_left + self.tab_w - self.close_w() - 4;
        if x >= close_left {
            Hit::Close(i as usize)
        } else {
            Hit::Tab(i as usize)
        }
    }

    /// Paint the whole band. `width` is the client width. The caller has
    /// already erased the background; painting the band again keeps it correct
    /// when only the strip is invalidated.
    pub fn paint(&self, hdc: HDC, width: i32) {
        let band = RECT { left: 0, top: 0, right: width, bottom: caption::HEIGHT };
        unsafe {
            let brush = CreateSolidBrush(self.bg);
            let _ = FillRect(hdc, &band, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            // Leading app icon, centred in the gutter. Same resource the
            // taskbar/tray use (ui/icon.rs); it never overlaps a tab.
            let app_icon = icon::small();
            if !app_icon.0.is_null() {
                let _ = DrawIconEx(
                    hdc,
                    (GUTTER - ICON_SIZE) / 2,
                    CENTER - ICON_SIZE / 2,
                    app_icon,
                    ICON_SIZE,
                    ICON_SIZE,
                    0,
                    None,
                    DI_NORMAL,
                );
            }

            let font: HFONT = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
            let old_font = SelectObject(hdc, HGDIOBJ(font.0));
            let _ = SetBkMode(hdc, TRANSPARENT);

            // Overflow guard: however many tabs there are, they never paint
            // over the "+" or the window buttons. `fit` keeps them inside
            // unless the tab count is absurd; clipping is one call and turns
            // that worst case into "last tab cut off" instead of "buttons
            // gone", which is also what a browser looks like mid-scroll.
            let saved = SaveDC(hdc);
            let _ = IntersectClipRect(hdc, 0, 0, self.plus_x(), caption::HEIGHT);

            for (i, label) in self.labels.iter().enumerate() {
                let x = self.tab_x(i);
                let tab_rc = RECT {
                    left: x,
                    top: TAB_TOP,
                    right: x + self.tab_w,
                    bottom: TAB_BOTTOM,
                };
                // Lightness ramp, darkest band to lightest tab:
                // background < inactive < hover < active. The active tab is the
                // brightest thing in the band so the eye lands on it first;
                // hovering lifts a tab off the strip without pretending to be
                // the selected one.
                let bg = if i == self.active {
                    self.active_bg
                } else if self.hover_tab == Some(i) {
                    self.hover_bg
                } else {
                    self.inactive_bg
                };
                fill_tab(hdc, &tab_rc, bg, self.radius);

                // Close (X) on the tab's right edge.
                let cy = (tab_rc.top + tab_rc.bottom) / 2;
                let close_rc = RECT {
                    left: tab_rc.right - self.close_w() - 4,
                    top: cy - CLOSE_BOX / 2,
                    right: tab_rc.right - 4,
                    bottom: cy + CLOSE_BOX / 2,
                };
                let fg = if i == self.active { self.text_active } else { self.text };
                let _ = SetTextColor(hdc, fg);
                let _ = DrawTextW(
                    hdc,
                    &mut to_wide("\u{00D7}"), // ×
                    &mut { RECT { ..close_rc } },
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );

                // Label, clipped to leave room for the X. The leading pad
                // shrinks with the tab: 8px on a full-width tab, 4px on a
                // 24px one, where every pixel goes to characters.
                let mut wide = to_wide(label);
                let pad = (self.tab_w / 6).clamp(4, 8);
                let mut text_rc = RECT {
                    left: tab_rc.left + pad,
                    right: (close_rc.left - 2).max(tab_rc.left + pad),
                    ..tab_rc
                };
                let _ = DrawTextW(
                    hdc,
                    &mut wide,
                    &mut text_rc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
            }

            let _ = RestoreDC(hdc, saved);

            // Trailing "+": its own pill so it reads as a button.
            let plus_x = self.plus_x();
            let plus_rc = RECT {
                left: plus_x,
                top: CENTER - PLUS_SIZE / 2,
                right: plus_x + PLUS_SIZE,
                bottom: CENTER + PLUS_SIZE / 2,
            };
            fill_round(hdc, &plus_rc, self.inactive_bg, self.radius);
            let _ = SetTextColor(hdc, self.text_active);
            let _ = DrawTextW(
                hdc,
                &mut to_wide("+"),
                &mut { RECT { ..plus_rc } },
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );

            let _ = SelectObject(hdc, old_font);
            // Caption buttons last: chrome must never be under a tab that
            // overflowed into their corner.
            caption::paint(
                hdc,
                width,
                self.hover,
                self.maximized,
                self.window_active,
                &self.caption_colors,
            );
        }
    }
}

/// Blend towards the band background: the "window is inactive" glyph colour.
fn dim(fg: COLORREF, bg: COLORREF) -> COLORREF {
    let mix = |f: u32, b: u32| (f * 45 + b * 55) / 100;
    let fg_r = fg.0 & 0xff;
    let fg_g = (fg.0 >> 8) & 0xff;
    let fg_b = (fg.0 >> 16) & 0xff;
    let bg_r = bg.0 & 0xff;
    let bg_g = (bg.0 >> 8) & 0xff;
    let bg_b = (bg.0 >> 16) & 0xff;
    COLORREF(mix(fg_r, bg_r) | mix(fg_g, bg_g) << 8 | mix(fg_b, bg_b) << 16)
}

/// All four corners rounded (the "+" pill).
unsafe fn fill_round(hdc: HDC, rc: &RECT, color: COLORREF, radius: i32) {
    let brush = CreateSolidBrush(color);
    if radius <= 0 {
        let _ = FillRect(hdc, rc, brush);
    } else {
        let rgn = CreateRoundRectRgn(
            rc.left,
            rc.top,
            rc.right + 1,
            rc.bottom + 1,
            radius * 2,
            radius * 2,
        );
        let _ = FillRgn(hdc, rgn, brush);
        let _ = DeleteObject(HGDIOBJ(rgn.0));
    }
    let _ = DeleteObject(HGDIOBJ(brush.0));
}

/// Chrome tab shape: **two rounded top corners, two square bottom ones**, in a
/// single region.
///
/// The previous trick — draw a rounded rect taller than the tab and let the
/// band (or the Explorer child) clip the bottom rounding away — only looked
/// right as long as something opaque sat directly below the tab. With a tab
/// that does not touch the content edge (multi-row tabs, a future tab strip
/// above a bookmark bar) the rounding would show up as two notches.
///
/// Instead: a round rect tall enough that its bottom rounding falls *below*
/// the tab, intersected with the tab's own rect. What survives is the top
/// rounding plus a straight cut at `rc.bottom`.
unsafe fn fill_tab(hdc: HDC, rc: &RECT, color: COLORREF, radius: i32) {
    let brush = CreateSolidBrush(color);
    if radius <= 0 {
        let _ = FillRect(hdc, rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        return;
    }
    let rounded = CreateRoundRectRgn(
        rc.left,
        rc.top,
        rc.right + 1,
        rc.bottom + radius + 1, // bottom corners land below the tab
        radius * 2,
        radius * 2,
    );
    let cut = CreateRectRgn(rc.left, rc.top, rc.right + 1, rc.bottom + 1);
    let shape = CreateRectRgn(0, 0, 0, 0);
    // ERROR (0) / NULLREGION (1) / SIMPLEREGION (2) / COMPLEXREGION (3)
    if CombineRgn(shape, rounded, cut, RGN_AND).0 > 1 {
        let _ = FillRgn(hdc, shape, brush);
    } else {
        // Region ops failed (no memory): a square tab beats no tab at all.
        let _ = FillRect(hdc, rc, brush);
    }
    let _ = DeleteObject(HGDIOBJ(rounded.0));
    let _ = DeleteObject(HGDIOBJ(cut.0));
    let _ = DeleteObject(HGDIOBJ(shape.0));
    let _ = DeleteObject(HGDIOBJ(brush.0));
}

fn to_wide(s: &str) -> Vec<u16> {
    windows::core::HSTRING::from(s).as_wide().to_vec()
}
