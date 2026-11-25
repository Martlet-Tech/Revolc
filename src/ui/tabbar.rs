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
    SetTextColor, HDC, HFONT, HGDIOBJ, DT_CENTER, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    DEFAULT_GUI_FONT, RGN_AND, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL};

use crate::config::Config;
use crate::ui::folder_icon::FolderIcons;
use crate::ui::reorder::Drag;
use crate::ui::{caption, icon};
use super::gdi::rgb;

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
/// Folder icon's leading pad inside a tab, and its gap to the label.
const TAB_ICON_PAD: i32 = 4;
const TAB_ICON_GAP: i32 = 3;
/// Once tabs are squeezed to half their maximum width, the X on inactive
/// tabs goes away and every remaining pixel belongs to the label.
const CLOSE_SQUEEZE_AT: i32 = TAB_MAX_W / 2;
const CLOSE_BOX: i32 = 16; // per-tab close (X) button size
const PLUS_SIZE: i32 = 26; // trailing "+" button size
/// Vertical centre of the bare band; used by the leading app icon.
const CENTER: i32 = caption::HEIGHT / 2;
/// The "+" pill is centred on the *tabs*, not on the raw band. Tabs start at
/// `TAB_TOP`, so their centre is 2px below `CENTER` — using `CENTER` made the
/// "+" read as if it had floated up (measured 2026-11-25).
const PLUS_CENTER: i32 = (TAB_TOP + TAB_BOTTOM) / 2;
/// Gap between the "+" and the caption-button block. Once every tab slot is
/// full this is the only bare strip left to grab the window by on the right,
/// so it is deliberately as wide as the "+" itself. Was a bare 8 (too tight
/// to drag from), raised to 16, then to a full PLUS_SIZE (measured
/// 2026-11-25).
const PLUS_TRAILING: i32 = PLUS_SIZE;

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

/// `Drag` now lives in `ui::reorder`, shared with the bookmark bar. The tab
/// strip's slot geometry (origin `GUTTER`, width `tab_w`, gap `TAB_GAP`) is
/// supplied at every call site, so behaviour is unchanged.
/// Tab strip state + painter.
pub struct TabBar {
    /// Tab ids, parallel to `labels`. Callbacks report ids, not indices: the
    /// list can shrink between a click and its dispatch.
    ids: Vec<u64>,
    labels: Vec<String>,
    /// Folder path each tab is showing; empty means "use the app icon".
    paths: Vec<String>,
    /// Shell icons keyed by path, so reordering tabs never refetches one.
    icons: FolderIcons,
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
    /// Whether the trailing "+" is under the pointer. Tracked separately from
    /// `hover_tab` so sliding from a tab onto "+" swaps the highlight cleanly.
    hover_plus: bool,
    /// Set while a tab is being dragged inside the band (reorder only; pulling
    /// a tab out into a new window is a later stage).
    drag: Option<Drag>,
    maximized: bool,
    window_active: bool,
    caption_colors: caption::Colors,
}

impl TabBar {
    pub fn new(cfg: &Config) -> Self {
        let mut bar = Self {
            ids: Vec::new(),
            labels: Vec::new(),
            paths: Vec::new(),
            icons: FolderIcons::new(),
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
            hover_plus: false,
            drag: None,
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
            rgb(r, g, b)
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
        paths: &[String],
        active: usize,
        cfg: &Config,
        width: i32,
    ) {
        self.ids = ids.to_vec();
        self.labels = labels.to_vec();
        // Reconcile only on an actual path change: most updates come from a
        // hover or a title tick, and must never touch the shell.
        if self.paths.as_slice() != paths {
            self.paths = paths.to_vec();
            self.icons.sync(paths);
        }
        self.active = active;
        // The tab that was under the pointer may be gone (closed, or the list
        // reordered); never carry a stale highlight into the next frame.
        self.hover_tab = self.hover_tab.filter(|i| *i < self.labels.len());
        // Same for a drag over a tab that no longer exists (the dragged tab
        // closed from under the capture, or a sweep reordered the rest).
        if self.drag.map(|d| d.from >= self.labels.len()).unwrap_or(false) {
            self.drag = None;
        }
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

    /// Highlight the trailing "+" when the pointer is over it. Returns true
    /// on change so the caller repaints exactly once.
    pub fn set_hover_plus(&mut self, hovered: bool) -> bool {
        if self.hover_plus == hovered {
            return false;
        }
        self.hover_plus = hovered;
        true
    }

    // ---- tab drag (reorder inside the band) -----------------------------

    /// Start dragging tab `i`. `x` is the press point in client coords; the
    /// offset inside the tab is kept so the tab does not snap to the cursor.
    pub fn begin_drag(&mut self, i: usize, x: i32) {
        if i >= self.labels.len() {
            return;
        }
        let left = self.tab_x(i);
        self.drag = Some(Drag::new(i, x, left));
        self.hover_tab = None;
        self.hover_plus = false;
    }

    pub fn drag(&self) -> Option<Drag> {
        self.drag
    }

    /// Follow the pointer. The raw edge is kept unclamped so hit-testing stays
    /// symmetric at both ends; `Drag::left` pins it back to the band for
    /// painting.
    pub fn drag_to(&mut self, x: i32) {
        let Some(mut d) = self.drag else { return };
        d.move_to(x);
        self.drag = Some(d);
    }

    /// Adopt a new (post-reorder) index for the tab being dragged.
    pub fn set_drag_from(&mut self, i: usize) {
        if let Some(d) = &mut self.drag {
            d.set_from(i);
        }
    }

    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Where the dragged tab would land, decided by **centre lines** (the way
    /// a browser does it): the tab changes place with its neighbour only once
    /// its own centre has crossed that neighbour's centre.
    ///
    /// The loop yields an *insertion slot* (0..=n), which then has to be
    /// corrected for the tab already occupying a slot: dragging within your own
    /// slot resolves to `slot, slot + 1` and both mean "do not move".
    ///
    /// Returns None when there is nothing to decide (no drag, one tab, or the
    /// pointer has not crossed a centre line yet).
    pub fn target(&self) -> Option<usize> {
        let d = self.drag?;
        d.landing(self.tab_w, TAB_GAP, GUTTER, self.labels.len())
    }

    /// Recompute tab widths for a new client width. Returns true on change.
    ///
    /// Tabs get exactly what the fixed furniture leaves over: gutter, "+",
    /// its trailing drag gutter and the three caption buttons are subtracted
    /// first, so the right end of the strip is always whole — measured
    /// 2026-11-18, a dozen tabs at the old 90px floor pushed the "+" and the
    /// close box clean off the end.
    pub fn fit(&mut self, width: i32) -> bool {
        let n = self.labels.len().max(1) as i32;
        let avail = width - GUTTER - PLUS_SIZE - PLUS_TRAILING - caption::WIDTH - n * TAB_GAP;
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

    /// Close boxes are the first thing to go when the strip gets tight: below
    /// half the maximum tab width only the active tab (or one held in a drag)
    /// keeps its X. The rest spend that space on their label.
    fn close_visible(&self, i: usize) -> bool {
        self.tab_w > CLOSE_SQUEEZE_AT
            || i == self.active
            || self.drag.map(|d| d.from == i).unwrap_or(false)
    }

    fn tab_x(&self, i: usize) -> i32 {
        GUTTER + i as i32 * (self.tab_w + TAB_GAP)
    }

    /// Rectangle of tab `i` in bar-client coords. Shared with the tooltip so
    /// the card anchors to the tab, not to where the pointer crossed its edge.
    pub fn tooltip_rect(&self, i: usize) -> Option<RECT> {
        if i >= self.labels.len() {
            return None;
        }
        let left = self.tab_x(i);
        Some(RECT {
            left,
            top: TAB_TOP,
            right: left + self.tab_w,
            bottom: TAB_BOTTOM,
        })
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
        let plus_top = PLUS_CENTER - PLUS_SIZE / 2;
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
        // Close box occupies the tab's right edge, while there is room for it.
        if self.close_visible(i as usize) {
            let close_left = tab_left + self.tab_w - self.close_w() - 4;
            if x >= close_left {
                return Hit::Close(i as usize);
            }
        }
        Hit::Tab(i as usize)
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
                // The dragged tab is painted last, on top: it overlaps its
                // neighbours while it follows the cursor and must not be cut
                // by whichever slot happens to come later in the loop.
                if self.drag.map(|d| d.from == i).unwrap_or(false) {
                    continue;
                }
                self.paint_tab(hdc, i, self.tab_x(i), label, false);
            }
            if let Some(d) = self.drag {
                if let Some(label) = self.labels.get(d.from) {
                    let max_left = (self.plus_x() - self.tab_w).max(GUTTER);
                    self.paint_tab(hdc, d.from, d.left(GUTTER, max_left), label, true);
                }
            }

            let _ = RestoreDC(hdc, saved);

            // Trailing "+": its own pill so it reads as a button.
            let plus_x = self.plus_x();
            let plus_rc = RECT {
                left: plus_x,
                top: PLUS_CENTER - PLUS_SIZE / 2,
                right: plus_x + PLUS_SIZE,
                bottom: PLUS_CENTER + PLUS_SIZE / 2,
            };
            let plus_bg = if self.hover_plus { self.hover_bg } else { self.inactive_bg };
            fill_round(hdc, &plus_rc, plus_bg, self.radius);
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

    /// One tab: body, close X, label. `dragged` means the pointer is holding
    /// it, so it keeps the active fill (whatever slot is under the cursor does
    /// not get the hover) and is painted above every other tab.
    fn paint_tab(&self, hdc: HDC, i: usize, x: i32, label: &str, dragged: bool) {
        unsafe {
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
            let bg = if dragged || i == self.active {
                self.active_bg
            } else if self.hover_tab == Some(i) {
                self.hover_bg
            } else {
                self.inactive_bg
            };
            fill_tab(hdc, &tab_rc, bg, self.radius);

            // Close (X) on the tab's right edge — only while it has room.
            let cy = (tab_rc.top + tab_rc.bottom) / 2;
            let close_rc = if self.close_visible(i) {
                Some(RECT {
                    left: tab_rc.right - self.close_w() - 4,
                    top: cy - CLOSE_BOX / 2,
                    right: tab_rc.right - 4,
                    bottom: cy + CLOSE_BOX / 2,
                })
            } else {
                None
            };
            let fg = if dragged || i == self.active { self.text_active } else { self.text };
            let _ = SetTextColor(hdc, fg);
            if let Some(rc) = close_rc.as_ref() {
                let _ = DrawTextW(
                    hdc,
                    &mut to_wide("\u{00D7}"), // ×
                    &mut { RECT { ..*rc } },
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            }

            // Folder icon: the shell HICON cached for this tab's path, or our
            // own app icon when the tab has no folder (settings). Always drawn;
            // it only shrinks when the tab is too narrow for it AND the X.
            let icon_size = close_rc
                .as_ref()
                .map(|rc| (rc.left - tab_rc.left - TAB_ICON_PAD - 2).clamp(4, ICON_SIZE))
                .unwrap_or(ICON_SIZE);
            let icon_x = tab_rc.left + TAB_ICON_PAD;
            let icon_y = (tab_rc.top + tab_rc.bottom - icon_size) / 2;
            let path = self.paths.get(i).map(String::as_str).unwrap_or("");
            let has_icon = if let Some(h) = self.icons.get(path) {
                DrawIconEx(hdc, icon_x, icon_y, h, icon_size, icon_size, 0, None, DI_NORMAL).is_ok()
            } else {
                let app = icon::small();
                if !app.0.is_null() {
                    DrawIconEx(hdc, icon_x, icon_y, app, icon_size, icon_size, 0, None, DI_NORMAL)
                        .is_ok()
                } else {
                    false
                }
            };

            // Label: icon + text are one left-aligned group. No ellipsis —
            // on a tight tab the text is simply cut where space runs out, so
            // it shows as much as fits (DrawText clips to the rect by default).
            let text_left = if has_icon {
                icon_x + icon_size + TAB_ICON_GAP
            } else {
                tab_rc.left + TAB_ICON_PAD
            };
            let text_right = close_rc
                .as_ref()
                .map(|rc| rc.left - 2)
                .unwrap_or(tab_rc.right - TAB_ICON_PAD);
            if text_right > text_left {
                let mut wide = to_wide(label);
                let mut text_rc = RECT {
                    left: text_left,
                    right: text_right,
                    ..tab_rc
                };
                let _ = DrawTextW(
                    hdc,
                    &mut wide,
                    &mut text_rc,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::reorder;

    /// Slot geometry used below: GUTTER 28, tab 100, gap 4 -> step 104,
    /// centres at 78 / 182 / 286.
    const W: i32 = 100;

    fn landing(center: i32, tab_w: i32, n: usize, from: usize) -> Option<usize> {
        reorder::landing(center, tab_w, TAB_GAP, GUTTER, n, from)
    }

    #[test]
    fn plus_hover_toggles_once_per_change() {
        let mut bar = TabBar::new(&Config::default());
        assert!(!bar.hover_plus);
        assert!(bar.set_hover_plus(true), "entering + repaints");
        assert!(!bar.set_hover_plus(true), "no redundant repaint");
        assert!(bar.set_hover_plus(false), "leaving + repaints");
        assert!(!bar.hover_plus);
    }

    #[test]
    fn resting_in_your_own_slot_is_not_a_move() {
        for from in 0..3 {
            let center = GUTTER + from as i32 * (W + TAB_GAP) + W / 2;
            assert_eq!(landing(center, W, 3, from), None, "from={from}");
        }
    }

    #[test]
    fn swaps_only_after_crossing_the_neighbour_centre() {
        // Drag the first tab right: one pixel short of tab 1's centre line it
        // must still sit at 0, and the moment it crosses, it is index 1.
        let tab1_mid = GUTTER + (W + TAB_GAP) + W / 2;
        assert_eq!(landing(tab1_mid - 1, W, 3, 0), None);
        assert_eq!(landing(tab1_mid, W, 3, 0), Some(1));
    }

    #[test]
    fn drags_both_ways_and_stops_at_the_ends() {
        let tab0_mid = GUTTER + W / 2;
        let tab1_mid = GUTTER + (W + TAB_GAP) + W / 2;
        // Third tab dragged left: it stays put while its centre is still on
        // tab 1's centre line, and swaps one pixel past it.
        assert_eq!(landing(tab1_mid, W, 3, 2), None);
        assert_eq!(landing(tab1_mid - 1, W, 3, 2), Some(1));
        assert_eq!(landing(GUTTER, W, 3, 2), Some(0));
        // First tab dragged past the ends clamps, it never goes out of range.
        assert_eq!(landing(i32::MIN / 2, W, 3, 0), None);
        assert_eq!(landing(i32::MAX / 2, W, 3, 0), Some(2));
        assert_eq!(landing(tab0_mid, W, 3, 1), None);
    }

    /// Slot 0 must be reachable from the right, and slot n-1 from the left.
    /// Regression: the drag used to clamp the tab's left edge at `GUTTER`
    /// before hit-testing, so a dragged tab's centre could never get past the
    /// first tab's centre — "drag #2 to #1" silently did nothing.
    #[test]
    fn both_end_slots_are_reachable() {
        let tab0_mid = GUTTER + W / 2;
        let last_mid = GUTTER + 2 * (W + TAB_GAP) + W / 2;
        // Second tab, centre one pixel left of tab 0's centre line: it is now
        // the first tab. Anything further left stays there.
        assert_eq!(landing(tab0_mid - 1, W, 3, 1), Some(0));
        assert_eq!(landing(GUTTER - W, W, 3, 1), Some(0));
        // First tab, centre past the last tab's centre line.
        assert_eq!(landing(last_mid + 1, W, 3, 0), Some(2));
    }

    /// The clamp is a painting rule; hit-testing must use the raw edge, or the
    /// pinned left edge would cap how far left a tab can be recognised.
    #[test]
    fn painting_clamps_but_landing_does_not() {
        let mut d = Drag::new(1, GUTTER, GUTTER); // grab = 0
        d.move_to(GUTTER - 40);
        assert_eq!(d.left(GUTTER, 500), GUTTER, "painted inside the band");
        assert_eq!(landing(d.raw + W / 2, W, 3, 1), Some(0), "but decided on raw");
    }

    #[test]
    fn a_single_tab_never_lands_anywhere() {
        assert_eq!(landing(GUTTER, W, 1, 0), None);
        assert_eq!(landing(GUTTER, W, 3, 3), None);
    }

    #[test]
    fn close_boxes_hide_in_narrow_tabs_except_active() {
        let cfg = Config::default();
        let mut bar = TabBar::new(&cfg);
        bar.update(
            &[1, 2],
            &["a".into(), "b".into()],
            &[String::new(), String::new()],
            0,
            &cfg,
            1000,
        );
        bar.tab_w = TAB_MAX_W;
        assert!(bar.close_visible(1), "wide tabs keep every X");
        bar.tab_w = CLOSE_SQUEEZE_AT;
        assert!(bar.close_visible(0), "active keeps its X");
        assert!(!bar.close_visible(1), "inactive loses X when squeezed");
        bar.begin_drag(1, GUTTER);
        assert!(bar.close_visible(1), "a dragged tab keeps its X");
    }
}
