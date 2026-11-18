//! Absorb host: re-parent real Explorer windows into Revolc's tab area.
//! Clover architecture (clover/analysis/Clover_Absorb_Mechanism.md):
//! tab content = a real Explorer window (ribbon, address bar, selection).
//!
//! The BHO (bho crate) tells us when a folder window is born; we re-parent
//! it under our host. No injection, no subclassing of Explorer internals.

use windows::core::{Interface as _, Result, VARIANT};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, InvalidateRect, SetWindowRgn, HGDIOBJ, HRGN,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_LOCAL_SERVER};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::{self as shell, IShellWindows, IWebBrowser2};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowTextW, GetWindowThreadProcessId, GetWindowRect, IsWindow, IsWindowVisible,
    SetForegroundWindow,
    SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED,
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    GA_PARENT, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, HWND_TOP, SW_HIDE, SW_SHOW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_DLGMODALFRAME, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_VISIBLE,
    WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};
use windows::Win32::Foundation::{BOOL, LPARAM};

/// Caption/frame styles stripped on absorb, restored on release.
const STRIP_STYLE: u32 = WS_CAPTION.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0
    | WS_SYSMENU.0 | WS_BORDER.0;
const STRIP_EXSTYLE: u32 =
    WS_EX_WINDOWEDGE.0 | WS_EX_DLGMODALFRAME.0 | WS_EX_CLIENTEDGE.0;

/// How long to wait for Explorer to show a pre-show frame before showing it
/// ourselves. Measured create→show gap on this machine is ~850ms, so this is
/// "one missed show", not "a slow disk".
const PRE_SHOW_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

/// Placement inputs measured from a live absorbed window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// Non-client padding around the client rect: (l, t, r, b).
    pub nc: (i32, i32, i32, i32),
    /// Reserved-but-unpainted strip at the top of the client area.
    pub band: i32,
}

/// A real Explorer window absorbed as tab content. MAIN THREAD ONLY.
pub struct Absorbed {
    pub top: HWND,
    orig_style: isize,
    orig_exstyle: isize,
    /// Non-client metrics **before** we stripped WS_CAPTION: (l, t, r, b).
    /// Explorer keeps reserving the caption band *inside* its client area
    /// (custom frame), so this is the only cheap hint we get about it.
    nc_pre: (i32, i32, i32, i32),
    /// Explorer has shown this frame at least once.
    ///
    /// A window adopted while still hidden is a *pre-show* shell frame: it
    /// exists and navigates, but Explorer has not run its own ShowWindow yet.
    /// We position it and leave visibility alone until that happens; showing
    /// it ourselves would take Explorer's start-up show path out of the loop.
    shown: bool,
    /// When we adopted it, for the pre-show grace period.
    adopted_at: std::time::Instant,
}

impl Absorbed {
    /// Re-parent `top` under `host`, strip its window chrome.
    pub unsafe fn absorb(top: HWND, host: HWND) -> Result<Self> {
        if is_absorbed(top, host) {
            return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_ABORT));
        }
        let orig_style = GetWindowLongPtrW(top, GWL_STYLE);
        let orig_exstyle = GetWindowLongPtrW(top, GWL_EXSTYLE);
        let nc_pre = nc_metrics(top).unwrap_or((0, 0, 0, 0));
        // Was Explorer already showing this frame? A window that was on screen
        // (Revolc started while folder windows were open, or a second folder
        // opened from one of our own tabs) is **not** a pre-show frame: Explorer
        // will never run its start-up ShowWindow for it again, so if we keep
        // treating it as "waiting for Explorer", nobody ever brings it back and
        // the tab shows the host's empty background — black. Everything below
        // still hides it (that is the crash-safe order); this only records the
        // fact so `set_visible` knows it has to do the showing.
        let was_visible = IsWindowVisible(top).as_bool();

        // ORDER MATTERS, and it is the difference between a crash and a clean
        // reparent (2026-11-17 dump, minidump in %APPDATA%\revolc\crash):
        //
        //   SetParent -> NtUserSetParent -> KiUserCallbackDispatcher
        //             -> user32 DispatchMessage/CallWindowProc -> uxtheme+0xe9f6
        //             ACCESS VIOLATION
        //
        // while the window still carried WS_CAPTION and WS_VISIBLE. Reparenting
        // makes user32 recompute the non-client area for the new parent, and a
        // captioned frame asks uxtheme for a themed border in a context where
        // the theme data is not ours to draw. Clover's absorb (FUN_01e3ba50,
        // clover/analysis) zeroes the style *before* SetParent and only sets
        // WS_VISIBLE afterwards — this is that order.
        //
        // 1. Nothing may be on screen while it is still parentless.
        tracing::info!(?top, step = "hide", "absorb");
        let _ = ShowWindow(top, SW_HIDE);
        // 2. Strip caption/frame and the visible bit *before* the reparent, so
        //    the NC recompute SetParent triggers has nothing to theme.
        tracing::info!(?top, step = "style", "absorb");
        let stripped =
            ((orig_style as u32) & !STRIP_STYLE & !WS_VISIBLE.0) | WS_CHILD.0;
        SetWindowLongPtrW(top, GWL_STYLE, stripped as isize);
        SetWindowLongPtrW(top, GWL_EXSTYLE, ((orig_exstyle as u32) & !STRIP_EXSTYLE) as isize);
        // 3. Now the reparent itself: no style change is needed afterwards, so
        //    there is no second NC recompute with a hostile state.
        tracing::info!(?top, step = "SetParent", "absorb");
        SetParent(top, host).map_err(|e| {
            tracing::error!(error = %e, ?top, "SetParent failed");
            e
        })?;
        // Do NOT show here: the caller decides visibility and geometry once the
        // window is assigned to a tab. SWP_SHOWWINDOW used to flash the window
        // at (0,0) with its old desktop size, on top of the tab bar.
        tracing::info!(?top, step = "framechanged", "absorb");
        let _ = SetWindowPos(
            top,
            HWND_TOP,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED
                | SWP_ASYNCWINDOWPOS,
        );
        tracing::info!(?top, nc = ?nc_pre, was_visible, "explorer window absorbed");
        Ok(Self {
            top,
            orig_style,
            orig_exstyle,
            nc_pre,
            shown: was_visible,
            adopted_at: std::time::Instant::now(),
        })
    }

    /// Explorer re-applies its own frame styles on some navigations, which
    /// brings the caption row back — seen as a band of Explorer chrome
    /// between the tab bar and the ribbon. Re-strip when it happens.
    pub unsafe fn ensure_stripped(&self) -> bool {
        let style = GetWindowLongPtrW(self.top, GWL_STYLE) as u32;
        if style & STRIP_STYLE == 0 {
            return false;
        }
        let want = (style & !STRIP_STYLE) | WS_CHILD.0;
        SetWindowLongPtrW(self.top, GWL_STYLE, want as isize);
        let _ = SetWindowPos(
            self.top,
            HWND_TOP,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        tracing::info!(top = ?self.top, "re-stripped Explorer frame styles");
        true
    }

    /// Put the window back as a normal desktop window.
    pub unsafe fn restore(&self) {
        // Undo clip_above_content: as a desktop window it needs its whole frame.
        let _ = SetWindowRgn(self.top, HRGN::default(), true);
        if let Err(e) = SetParent(self.top, None) {
            tracing::error!(error = %e, top = ?self.top, "restore SetParent failed");
        }
        SetWindowLongPtrW(self.top, GWL_STYLE, self.orig_style);
        SetWindowLongPtrW(self.top, GWL_EXSTYLE, self.orig_exstyle);
        let _ = SetWindowPos(
            self.top,
            HWND_TOP,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED
                | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS,
        );
        tracing::info!(?self.top, "explorer window restored");
    }

    /// Everything the placement math needs, measured **once**.
    ///
    /// `band()` walks the window's whole child tree, and the sweep used to ask
    /// for it 3-4 times per tick (placement key, drift check, placement
    /// itself) at 15ms intervals while a folder was expected. Callers sample
    /// once and pass the value down instead.
    pub fn sample(&self) -> Metrics {
        Metrics {
            nc: nc_metrics(self.top).unwrap_or((0, 0, 0, 0)),
            band: self.band(),
        }
    }

    /// Place the window so that its **visible chrome** (ribbon / command bar,
    /// i.e. the first thing Explorer actually paints) lands exactly on `rect`.
    ///
    /// Two offsets have to be compensated, and only one of them is a real
    /// non-client area:
    ///
    /// 1. `nc_metrics` — the frame around the client rect. Zero after we strip
    ///    `WS_CAPTION`, non-zero if Explorer re-applied its styles.
    /// 2. `band()` — the strip Explorer keeps *reserved inside its own client*
    ///    for the custom titlebar it no longer draws once the caption is
    ///    stripped. Measured from the real child layout (see `content_band`).
    ///
    /// Aligning the client rect instead of the chrome (the 2026-11-17
    /// `25a5fbd` attempt) leaves that band — stale address-bar / column-header
    /// pixels — right under the tab bar.
    pub fn place_client(&self, rect: RECT, m: Metrics) {
        let (nx, ny, nx2, ny2) = m.nc;
        let band = m.band;
        let w = (rect.right - rect.left) + nx + nx2;
        let h = (rect.bottom - rect.top) + ny + ny2 + band;
        unsafe {
            let _ = SetWindowPos(
                self.top,
                HWND_TOP,
                rect.left - nx,
                rect.top - ny - band,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // The padding above the content is only there to push the ribbon
            // onto the view rect. It must not exist as a *window*, or it covers
            // the host's caption strip (paint + hit test both).
            clip_above_content(self.top, ny + band, w, h);
        }
        tracing::info!(
            top = ?self.top,
            band,
            clip_top = ny + band,
            nc = ?(nx, ny, nx2, ny2),
            view = ?(rect.left, rect.top, rect.right, rect.bottom),
            "placed absorbed view"
        );
    }

    /// Does the window actually sit where `place_client(rect)` would put it?
    ///
    /// Explorer shows its own frame (and may move it) when it finishes
    /// navigating, so the geometry we set at absorb time is not authoritative
    /// — the `laid_out` de-dup key cannot see that drift because *we* did not
    /// change anything. One GetWindowRect per view per sweep.
    pub fn is_placed(&self, rect: RECT, m: Metrics) -> bool {
        let (nx, ny, nx2, ny2) = m.nc;
        let band = m.band;
        let want = (
            rect.left - nx,
            rect.top - ny - band,
            rect.right + nx2,
            rect.bottom + ny2,
        );
        let got = rect_in_parent(self.top);
        let want = [want.0, want.1, want.2, want.3];
        let got = [got.0, got.1, got.2, got.3];
        want.iter().zip(got.iter()).all(|(w, g)| (w - g).abs() <= 1)
    }

    /// Height of the reserved-but-unpainted strip at the top of the absorbed
    /// window's client area.
    fn band(&self) -> i32 {
        content_band(self.top).unwrap_or(self.nc_pre.1).clamp(0, 80)
    }

    pub fn set_visible(&mut self, visible: bool) {
        if visible {
            // Record Explorer showing the frame by itself; that is what makes
            // the tab content "ready".
            if is_visible(self.top) {
                self.shown = true;
            } else if self.waiting_for_explorer() {
                // Pre-show frame inside the grace: Explorer's start-up path
                // (navigation, ribbon init) expects to run the first show, and
                // letting it is what makes the adoption flash-free. It appears
                // in the tab when that happens. After the grace this branch is
                // skipped and we show it ourselves — see waiting_for_explorer.
                return;
            }
        }
        let was = is_visible(self.top);
        unsafe {
            let _ = ShowWindow(self.top, if visible { SW_SHOW } else { SW_HIDE });
        }
        // A window that was hidden and comes back keeps its DWM redirection
        // surface, but the show alone does not make its thread paint again —
        // the tab renders as an empty (black) rectangle until something forces
        // a paint. Invalidate on the hidden→visible edge only (a window that is
        // already up must not be repainted just because its tab was clicked).
        if visible && !was {
            nudge_paint(self.top);
        }
    }

    /// Still waiting for Explorer to run the show we deliberately skipped?
    ///
    /// Only for the first PRE_SHOW_GRACE: Explorer creates a folder window
    /// ~850ms before showing it, and letting its own start-up path do the show
    /// is what makes the adoption flash-free. But that show does not always
    /// arrive — the window can be adopted *after* Explorer already decided it
    /// was shown, or its show path bails out once the frame has a new parent.
    /// A frame still hidden after the grace is ours to show: otherwise the tab
    /// is a black rectangle that survives maximize, restore and clicking the
    /// tab (2026-11-18: three windows opened before Revolc started, absorbed at
    /// startup with nc=(8,0,8,8) and never shown again).
    pub fn waiting_for_explorer(&self) -> bool {
        !self.shown && self.adopted_at.elapsed() < PRE_SHOW_GRACE
    }

    /// Called from the sweep: Explorer's own ShowWindow is what promotes a
    /// pre-show frame to ready. Readiness is a fact about the window, so the
    /// sweep samples it instead of inferring it from our own calls.
    pub fn mark_shown(&mut self) {
        if is_visible(self.top) {
            self.shown = true;
        }
    }
}

/// Cut the part of an absorbed window that sits above the tab content out of
/// its window region.
///
/// `place_client` deliberately makes the child **taller than the content** and
/// pushes it up by `band` pixels, so Explorer's ribbon starts exactly on the
/// view rect. That overhang lands on top of the host's caption strip — and a
/// child window always wins over its parent's painting *and* hit testing, so
/// the title bar turns into Explorer's unpainted black band and every click on
/// it (tabs, `+`, min/max/close, drag) is delivered to Explorer instead. That
/// is intermittent only because `band` is 0 until Explorer applies its own
/// frame: measured 0 right after absorb, 31 a moment later.
///
/// A window region makes the overhang not part of the child at all, so those
/// pixels and those clicks belong to the host again.
unsafe fn clip_above_content(top: HWND, clip: i32, w: i32, h: i32) {
    if clip <= 0 {
        // No overhang: a leftover region from a previous placement would eat
        // the ribbon, so clear it explicitly.
        let _ = SetWindowRgn(top, HRGN::default(), true);
        return;
    }
    let rgn = CreateRectRgn(0, clip, w, h);
    if rgn.0.is_null() {
        return;
    }
    // SetWindowRgn takes ownership of the region only when it succeeds.
    if SetWindowRgn(top, rgn, true) == 0 {
        let _ = DeleteObject(HGDIOBJ(rgn.0));
        tracing::warn!(?top, clip, "SetWindowRgn failed, caption strip may be covered");
    }
}

/// Measure the strip Explorer reserves at the top of its own client area for
/// the custom titlebar / tab strip it stops painting once `WS_CAPTION` is gone.
///
/// Probing the real child layout beats trusting a constant or the pre-strip NC
/// metrics: the band follows DPI, theme and Explorer version, and it is *not*
/// non-client area any more (`GetWindowRect - GetClientRect == 0`).
///
/// The known top-of-content bands are all we need — the smallest visible one
/// wins, since everything else in the tree sits below the command bar.
fn content_band(top: HWND) -> Option<i32> {
    const BAND_CLASSES: [&str; 4] = [
        "UIRibbonCommandBarDock",
        "UIRibbonCommandBar",
        "RebarWindow32",
        "NUIPane",
    ];
    let (_, ny, _, _) = nc_metrics(top)?;
    let mut wr = RECT::default();
    unsafe { GetWindowRect(top, &mut wr).ok()? };
    let client_top = wr.top + ny;

    let mut best: Option<i32> = None;
    for child in descendants(top) {
        if !unsafe { IsWindowVisible(child).as_bool() } {
            continue;
        }
        if !BAND_CLASSES.contains(&class_name(child).as_str()) {
            continue;
        }
        let mut cr = RECT::default();
        if unsafe { GetWindowRect(child, &mut cr) }.is_err() || cr.bottom <= cr.top {
            continue;
        }
        let band = cr.top - client_top;
        if !(0..=80).contains(&band) {
            continue;
        }
        best = Some(best.map_or(band, |b| b.min(band)));
    }
    best
}

/// Is `pid` the shell (explorer.exe)?
///
/// Only used to vet **hidden** adoption candidates. If the process cannot be
/// opened (higher integrity, protected process) the answer is `true`: that is
/// exactly the previous behaviour, and refusing would lose the flash-free path
/// for windows we are not allowed to inspect.
fn is_shell_process(pid: u32) -> bool {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true; // cannot look: keep the old accept-by-default
        };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if ok.is_err() {
            return true;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        let file = full.rsplit(['\\', '/']).next().unwrap_or(&full);
        file.eq_ignore_ascii_case("explorer.exe")
    }
}

/// Every descendant of `parent` (EnumChildWindows is recursive).
fn descendants(parent: HWND) -> Vec<HWND> {
    unsafe extern "system" fn proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<HWND>);
        out.push(hwnd);
        BOOL(1)
    }
    let mut out: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(
            parent,
            Some(proc),
            LPARAM(&mut out as *mut Vec<HWND> as isize),
        );
    }
    out
}

/// True if `top` is already a child of our host (absorbed earlier).
/// GA_ROOT: robust even if something re-parented `top` in between.
pub fn is_absorbed(top: HWND, host: HWND) -> bool {
    unsafe { GetAncestor(top, GA_ROOT).0 == host.0 }
}

/// Read the folder path of an Explorer window via IShellWindows (main thread).
/// Brief retries: the window exists before it navigates.
///
/// This runs on the UI thread, so the sleep between attempts is real freeze
/// time for the user — hence "between", not "after each": the old loop slept
/// 50ms even after its last attempt, i.e. every call paid a full frame stall
/// for nothing.
pub fn read_folder_path(top: HWND, retries: usize) -> Option<String> {
    unsafe {
        let svc: IShellWindows =
            CoCreateInstance(&shell::ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
        for attempt in 0..retries.max(1) {
            let count = svc.Count().unwrap_or(0);
            for i in 0..count {
                let Ok(dispatch) = svc.Item(&VARIANT::from(i)) else { continue };
                let Ok(browser) = dispatch.cast::<IWebBrowser2>() else { continue };
                let Ok(whwnd) = browser.HWND() else { continue };
                if GetAncestor(HWND(whwnd.0 as *mut _), GA_PARENT).0 != top.0 {
                    continue;
                }
                if let Ok(url) = browser.LocationURL() {
                    if let Some(p) = file_url_to_path(&url.to_string()) {
                        return Some(p);
                    }
                }
            }
            if attempt + 1 < retries {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        None
    }
}

/// All top-level folder windows Revolc neither owns nor deliberately parked.
/// Called from the BHO notify handler AND from the periodic sweep timer. The
/// BHO never touches the site object during SetSite (QI on it corrupted
/// explorerframe — crash loop 2026-11-17).
///
/// Why a sweep and not just the notify: SetSite fires when the frame is
/// created, but the window becomes visible only after Explorer finishes
/// navigating — sometimes seconds later, and sometimes with no notification
/// at all (BHO absent from that window/process). A notify-only design drops
/// those windows on the desktop as plain Explorer windows (observed
/// 2026-11-17: "no new folder window found after notify").
///
/// `include_hidden` is what makes a new tab flash-free. Measured on this
/// machine (log/probe_hidden.ps1): a folder window exists **851ms before
/// Explorer shows it** — created hidden, navigates, then `ShowWindow`. A
/// visible-only filter therefore *guarantees* adopting the window after it is
/// already on screen: one painted frame of a stray Explorer window, which is
/// the "系统自带 FE 闪一下" the user sees. Adopting it while it is still
/// hidden means it is ours before it is ever composited (Explorer's later
/// ShowWindow simply shows it inside our tab).
pub fn stray_folder_windows(
    known: &[isize],
    excluded: &[isize],
    include_hidden: bool,
) -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    let ctx = FindCtx { known, excluded, include_hidden, out: &mut out };
    unsafe {
        let _ = EnumWindows(Some(enum_find_proc), LPARAM(&ctx as *const FindCtx as isize));
    }
    out
}

struct FindCtx<'a> {
    known: &'a [isize],
    excluded: &'a [isize],
    include_hidden: bool,
    out: &'a mut Vec<HWND>,
}

unsafe extern "system" fn enum_find_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindCtx);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if class != "CabinetWClass" && class != "ExploreWClass" {
        return BOOL(1);
    }
    // Hidden is not a rejection reason when we are expecting a folder (see
    // `include_hidden`): it is the whole point — a pre-show frame is free to
    // steal. Never adopt our own windows though (we create none, but a
    // future in-process browser would look identical).
    if !ctx.include_hidden && !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let raw = hwnd.0 as isize;
    if ctx.known.contains(&raw) || ctx.excluded.contains(&raw) {
        return BOOL(1);
    }
    if ctx.include_hidden && !IsWindowVisible(hwnd).as_bool() {
        // A hidden frame may be an Explorer window that is still navigating or
        // one that is about to be torn down (never shown at all). Only take it
        // when it is not owned by us and it really is the shell: an invisible
        // CabinetWClass from some other process (a host that creates shell
        // views off-screen) is nobody's tab.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return BOOL(1);
        }
        if !is_shell_process(pid) {
            return BOOL(1);
        }
    }
    ctx.out.push(hwnd);
    BOOL(1)
}

/// Direct children of `parent`, z-order top first.
pub fn child_windows(parent: HWND) -> Vec<HWND> {
    unsafe extern "system" fn proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<HWND>);
        out.push(hwnd);
        BOOL(1)
    }
    let mut out: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(
            parent,
            Some(proc),
            LPARAM(&mut out as *mut Vec<HWND> as isize),
        );
    }
    // EnumChildWindows is recursive; keep only direct children.
    out.retain(|h| unsafe { GetAncestor(*h, GA_PARENT).0 == parent.0 });
    out
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Window rect in parent-client coordinates, for layout diagnostics.
pub fn rect_in_parent(top: HWND) -> (i32, i32, i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut rc = RECT::default();
        if GetWindowRect(top, &mut rc).is_err() {
            return (0, 0, 0, 0);
        }
        let parent = GetAncestor(top, GA_PARENT);
        let mut tl = POINT { x: rc.left, y: rc.top };
        let mut br = POINT { x: rc.right, y: rc.bottom };
        if !parent.0.is_null() {
            let _ = ScreenToClient(parent, &mut tl);
            let _ = ScreenToClient(parent, &mut br);
        }
        (tl.x, tl.y, br.x, br.y)
    }
}

/// Non-client metrics of `top`: (left, top, right, bottom) padding of the
/// window rect around its client rect, in pixels.
pub fn nc_metrics(top: HWND) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::MapWindowPoints;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};
    unsafe {
        let mut wr = RECT::default();
        let mut cr = RECT::default();
        GetWindowRect(top, &mut wr).ok()?;
        GetClientRect(top, &mut cr).ok()?;
        let mut pts = [
            POINT { x: cr.left, y: cr.top },
            POINT { x: cr.right, y: cr.bottom },
        ];
        // hwndFrom = top, hwndTo = NULL → screen coordinates.
        MapWindowPoints(top, None, &mut pts);
        Some((
            pts[0].x - wr.left,
            pts[0].y - wr.top,
            wr.right - pts[1].x,
            wr.bottom - pts[1].y,
        ))
    }
}

/// An absorbed Explorer window's HWND is still alive. Explorer can destroy it
/// behind our back (its own X button, a crashed browser process, a navigation
/// that spawns a replacement); the tab must then be dropped instead of left
/// holding a dead handle.
pub fn is_window_alive(top: HWND) -> bool {
    unsafe { IsWindow(top).as_bool() }
}

/// Explorer re-shows its own frame while it finishes navigating, so a single
/// SW_HIDE at absorb time does not stick: inactive tabs would reappear on top
/// of the active one at their old geometry. Re-assert state every sweep.
pub fn force_visible(top: HWND, visible: bool) {
    unsafe {
        let flags = if visible {
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER
                | SWP_ASYNCWINDOWPOS
        } else {
            SWP_NOACTIVATE | SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER
                | SWP_ASYNCWINDOWPOS
        };
        let _ = SetWindowPos(top, HWND_TOP, 0, 0, 0, 0, flags);
    }
    if visible {
        // Same as Absorbed::set_visible: re-showing a shell frame does not make
        // it paint, and an unpainted tab area is indistinguishable from a
        // black one.
        nudge_paint(top);
    }
}

/// Ask a foreign window to repaint its whole client now.
///
/// InvalidateRect is allowed across processes (it just marks the region dirty
/// in the owner's queue), unlike RedrawWindow with RDW_UPDATENOW which would
/// block us until the other thread paints.
pub fn nudge_paint(top: HWND) {
    unsafe {
        let _ = InvalidateRect(top, None, true);
    }
}

pub fn is_visible(top: HWND) -> bool {
    unsafe { IsWindowVisible(top).as_bool() }
}

/// Which thread owns `top` (0 on failure).
pub fn thread_of(top: HWND) -> u32 {
    unsafe { GetWindowThreadProcessId(top, None) }
}

/// Attach/detach our input queue to another thread's.
///
/// Required for the absorbed Explorer window to be *editable*: focus and
/// keyboard state live in the input queue, so a cross-process child can take
/// a click but never a keystroke until the queues are merged. Attach only
/// while our window is the active one — shared queues also share hangs.
pub fn attach_input(tid: u32, attach: bool) {
    unsafe {
        let ours = GetCurrentThreadId();
        if tid != 0 && tid != ours {
            let _ = AttachThreadInput(ours, tid, attach);
        }
    }
}
/// Bring `host` to the foreground and put the keyboard focus inside the
/// absorbed window.
///
/// A re-parented Explorer window that never gets "activated" keeps painting,
/// but its address bar cannot be typed into: focus lives in our input queue
/// and the shell's queue never sees the keystrokes. AttachThreadInput merges
/// the two queues for the duration of the handover (Clover does the same).
///
/// `already_attached` is the persistent attach `App::sync_input_queue` keeps
/// for the active tab. AttachThreadInput is *not* reference counted: attaching
/// an already-attached pair and detaching once afterwards tears the permanent
/// link down, after which the address bar silently stops taking keystrokes.
/// So when the pair is already ours, do not touch it.
pub fn focus_into(host: HWND, top: HWND, already_attached: bool) {
    // Breadcrumb: AttachThreadInput + SetForegroundWindow is the other place
    // where we touch foreign window state from our own thread.
    tracing::info!(?host, ?top, already_attached, "focus_into");
    unsafe {
        let ours = GetCurrentThreadId();
        let target = GetWindowThreadProcessId(top, None);
        let fg = GetForegroundWindow();
        let fg_tid = if fg.0.is_null() { 0 } else { GetWindowThreadProcessId(fg, None) };

        let attach_target = !already_attached && target != 0 && target != ours;
        let attach_fg = fg_tid != 0 && fg_tid != ours && fg_tid != target;

        if attach_target {
            let _ = AttachThreadInput(ours, target, true);
        }
        if attach_fg {
            let _ = AttachThreadInput(fg_tid, ours, true);
        }

        let _ = SetForegroundWindow(host);
        let _ = SetFocus(top);

        if attach_fg {
            let _ = AttachThreadInput(fg_tid, ours, false);
        }
        if attach_target {
            let _ = AttachThreadInput(ours, target, false);
        }
    }
}

/// "file:///D:/dir%20name" -> "D:\dir name"
fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file:///")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(out).ok()?.replace('/', "\\");
    let b = path.as_bytes();
    if b.len() < 3 || b[1] != b':' {
        return None; // virtual locations (This PC etc.) — not real folders
    }
    Some(path)
}

/// Window title text (folder name once Explorer navigates). Instant fallback
/// for the tab label while the real path resolves later.
pub fn window_title(top: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 260];
        let n = GetWindowTextW(top, &mut buf);
        if n <= 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// Spawn a fresh Explorer window (absorbed later via BHO notification).
pub fn spawn_explorer(path: &str) {
    let normalized = path.replace('/', "\\");
    match std::process::Command::new("explorer.exe").arg(&normalized).spawn() {
        Ok(_) => tracing::info!(path, "explorer spawned for absorb"),
        Err(e) => tracing::error!(error = %e, path, "spawn explorer failed"),
    }
}
