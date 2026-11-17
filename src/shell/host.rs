//! Absorb host: re-parent real Explorer windows into Revolc's tab area.
//! Clover architecture (clover/analysis/Clover_Absorb_Mechanism.md):
//! tab content = a real Explorer window (ribbon, address bar, selection).
//!
//! The BHO (bho crate) tells us when a folder window is born; we re-parent
//! it under our host. No injection, no subclassing of Explorer internals.

use windows::core::{Interface as _, Result, VARIANT};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_LOCAL_SERVER};
use windows::Win32::UI::Shell::{self as shell, IShellWindows, IWebBrowser2};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetWindowTextW, GetWindowLongPtrW, IsWindowVisible, MoveWindow,
    SetParent, SetWindowLongPtrW, SetWindowPos, ShowWindow, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, GA_PARENT, GA_ROOT,
    GWL_EXSTYLE, GWL_STYLE, HWND_TOP, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_DLGMODALFRAME, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_THICKFRAME,
};
use windows::Win32::Foundation::{BOOL, LPARAM};

/// Caption/frame styles stripped on absorb, restored on release.
const STRIP_STYLE: u32 = WS_CAPTION.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0
    | WS_SYSMENU.0 | WS_BORDER.0;
const STRIP_EXSTYLE: u32 =
    WS_EX_WINDOWEDGE.0 | WS_EX_DLGMODALFRAME.0 | WS_EX_CLIENTEDGE.0;

/// A real Explorer window absorbed as tab content. MAIN THREAD ONLY.
pub struct Absorbed {
    pub top: HWND,
    orig_style: isize,
    orig_exstyle: isize,
}

impl Absorbed {
    /// Re-parent `top` under `host`, strip its window chrome.
    pub unsafe fn absorb(top: HWND, host: HWND) -> Result<Self> {
        if is_absorbed(top, host) {
            return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_ABORT));
        }
        let orig_style = GetWindowLongPtrW(top, GWL_STYLE);
        let orig_exstyle = GetWindowLongPtrW(top, GWL_EXSTYLE);

        // Failure here (e.g. UIPI block) must not be silent, or a duplicate
        // notify will absorb the same window again into a second tab.
        SetParent(top, host).map_err(|e| {
            tracing::error!(error = %e, ?top, "SetParent failed");
            e
        })?;
        let stripped = ((orig_style as u32) & !STRIP_STYLE) | WS_CHILD.0;
        SetWindowLongPtrW(top, GWL_STYLE, stripped as isize);
        SetWindowLongPtrW(top, GWL_EXSTYLE, ((orig_exstyle as u32) & !STRIP_EXSTYLE) as isize);
        SetWindowPos(
            top,
            HWND_TOP,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
        );
        tracing::info!(?top, "explorer window absorbed");
        Ok(Self { top, orig_style, orig_exstyle })
    }

    /// Put the window back as a normal desktop window.
    pub unsafe fn restore(&self) {
        if let Err(e) = SetParent(self.top, None) {
            tracing::error!(error = %e, top = ?self.top, "restore SetParent failed");
        }
        SetWindowLongPtrW(self.top, GWL_STYLE, self.orig_style);
        SetWindowLongPtrW(self.top, GWL_EXSTYLE, self.orig_exstyle);
        SetWindowPos(
            self.top,
            HWND_TOP,
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
        );
        tracing::info!(?self.top, "explorer window restored");
    }

    pub fn resize(&self, rect: RECT) {
        unsafe {
            let _ = MoveWindow(self.top, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top, true);
        }
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe {
            let _ = ShowWindow(self.top, if visible { SW_SHOW } else { SW_HIDE });
        }
    }
}

/// True if `top` is already a child of our host (absorbed earlier).
/// GA_ROOT: robust even if something re-parented `top` in between.
pub fn is_absorbed(top: HWND, host: HWND) -> bool {
    unsafe { GetAncestor(top, GA_ROOT).0 == host.0 }
}

/// Read the folder path of an Explorer window via IShellWindows (main thread).
/// Brief retries: the window exists before it navigates.
pub fn read_folder_path(top: HWND, retries: usize) -> Option<String> {
    unsafe {
        let svc: IShellWindows =
            CoCreateInstance(&shell::ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
        for _ in 0..retries {
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
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    }
}

/// Find a folder-window top-level HWND that Revolc hasn't absorbed yet.
/// Called from the BHO notify handler. The BHO never touches the site object
/// during SetSite (QI on it corrupted explorerframe — crash loop 2026-11-17).
/// Fast path: plain EnumWindows (in-proc, sub-ms) — absorbed windows are
/// WS_CHILD and invisible to EnumWindows, `known` covers strays. IShellWindows
/// fallback was dropped: the out-of-proc enumeration (25 × 60ms retries) made
/// every absorb flash a full Explorer window for seconds first.
pub fn find_new_folder_window(known: &[isize], retries: usize) -> Option<HWND> {
    unsafe {
        for _ in 0..retries {
            let mut found: Option<HWND> = None;
            let ctx = FindCtx { known, found: &mut found };
            EnumWindows(
                Some(enum_find_proc),
                LPARAM(&ctx as *const FindCtx as isize),
            );
            if let Some(top) = found {
                return Some(top);
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        None
    }
}

struct FindCtx<'a> {
    known: &'a [isize],
    found: *mut Option<HWND>,
}

unsafe extern "system" fn enum_find_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &*(lparam.0 as *const FindCtx);
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if class != "CabinetWClass" && class != "ExploreWClass" {
        return BOOL(1);
    }
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    if ctx.known.contains(&(hwnd.0 as isize)) {
        return BOOL(1);
    }
    *ctx.found = Some(hwnd);
    BOOL(0)
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
