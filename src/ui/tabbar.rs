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
    CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, FillRgn, GetStockObject,
    SelectObject, SetBkMode, SetTextColor, HDC, HFONT, HGDIOBJ, DT_CENTER, DT_END_ELLIPSIS,
    DT_SINGLELINE, DT_VCENTER, DEFAULT_GUI_FONT, TRANSPARENT,
};

use crate::config::Config;
use crate::ui::caption;

pub const TAB_MIN_W: i32 = 90;
pub const TAB_MAX_W: i32 = 190;
/// Tabs stop this far above the band's bottom edge; the strip above keeps the
/// rounded top corners off the window's own rounding and is the drag area.
const TAB_TOP: i32 = 4;
const TAB_BOTTOM: i32 = caption::HEIGHT;
const TAB_GAP: i32 = 4;
const PAD_LEFT: i32 = 4;
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
    inactive_bg: COLORREF,
    text: COLORREF,
    text_active: COLORREF,
    radius: i32,
    /// Per-tab width, recomputed from the client width so the tabs fill the
    /// band instead of being stuck at one hard-coded size.
    tab_w: i32,
    hover: Option<caption::Button>,
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
            inactive_bg: COLORREF(0),
            text: COLORREF(0),
            text_active: COLORREF(0),
            radius: 0,
            tab_w: TAB_MAX_W,
            hover: None,
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

    /// Recompute tab widths for a new client width. Returns true on change.
    pub fn fit(&mut self, width: i32) -> bool {
        let n = self.labels.len().max(1) as i32;
        let avail = width - PAD_LEFT - PLUS_SIZE - 8 - caption::WIDTH - n * TAB_GAP;
        let want = (avail / n).clamp(TAB_MIN_W, TAB_MAX_W);
        if want == self.tab_w {
            return false;
        }
        self.tab_w = want;
        true
    }

    fn tab_x(&self, i: usize) -> i32 {
        PAD_LEFT + i as i32 * (self.tab_w + TAB_GAP)
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
        if x < PAD_LEFT || y < TAB_TOP || n == 0 {
            return Hit::Band;
        }
        let i = (x - PAD_LEFT) / (self.tab_w + TAB_GAP);
        if i < 0 || i >= n {
            return Hit::Band;
        }
        let tab_left = self.tab_x(i as usize);
        if x > tab_left + self.tab_w {
            return Hit::Band; // in the gap between tabs
        }
        // Close box occupies the tab's right edge.
        let close_left = tab_left + self.tab_w - CLOSE_BOX - 4;
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

            let font: HFONT = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
            let old_font = SelectObject(hdc, HGDIOBJ(font.0));
            let _ = SetBkMode(hdc, TRANSPARENT);

            for (i, label) in self.labels.iter().enumerate() {
                let x = self.tab_x(i);
                let tab_rc = RECT {
                    left: x,
                    top: TAB_TOP,
                    right: x + self.tab_w,
                    bottom: TAB_BOTTOM,
                };
                let bg = if i == self.active { self.active_bg } else { self.inactive_bg };
                fill_round(hdc, &tab_rc, bg, self.radius, true);

                // Close (X) on the tab's right edge.
                let cy = (tab_rc.top + tab_rc.bottom) / 2;
                let close_rc = RECT {
                    left: tab_rc.right - CLOSE_BOX - 4,
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

                // Label, clipped to leave room for the X.
                let mut wide = to_wide(label);
                let mut text_rc = RECT {
                    left: tab_rc.left + 8,
                    right: close_rc.left - 2,
                    ..tab_rc
                };
                let _ = DrawTextW(
                    hdc,
                    &mut wide,
                    &mut text_rc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                );
            }

            // Trailing "+": its own pill so it reads as a button.
            let plus_x = self.plus_x();
            let plus_rc = RECT {
                left: plus_x,
                top: CENTER - PLUS_SIZE / 2,
                right: plus_x + PLUS_SIZE,
                bottom: CENTER + PLUS_SIZE / 2,
            };
            fill_round(hdc, &plus_rc, self.inactive_bg, self.radius, false);
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

/// Rounded fill. `square_bottom` is the Chrome tab shape: rounding on the top
/// corners only, the tab sits flush on the content area. The region is drawn
/// taller than the rect and clipped by the band, which squares the bottom
/// corners without a second region.
unsafe fn fill_round(hdc: HDC, rc: &RECT, color: COLORREF, radius: i32, square_bottom: bool) {
    let brush = CreateSolidBrush(color);
    if radius <= 0 {
        let _ = FillRect(hdc, rc, brush);
    } else {
        let bottom = if square_bottom { rc.bottom + radius + 1 } else { rc.bottom + 1 };
        let rgn = CreateRoundRectRgn(rc.left, rc.top, rc.right + 1, bottom, radius * 2, radius * 2);
        let _ = FillRgn(hdc, rgn, brush);
        let _ = DeleteObject(HGDIOBJ(rgn.0));
    }
    let _ = DeleteObject(HGDIOBJ(brush.0));
}

fn to_wide(s: &str) -> Vec<u16> {
    windows::core::HSTRING::from(s).as_wide().to_vec()
}
