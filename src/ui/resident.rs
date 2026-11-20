//! Hidden resident window (docs/双进程架构设计.md Phase 1).
//!
//! Redirect takeover needs *something* alive even when every RevolcMain is
//! closed. The old design faked that by making the first window unclosable;
//! the new one keeps the process alive with this window instead: no UI, holds
//! the tray icon, and is the BHO notify fallback when no main window exists.
//!
//! Deliberately NOT in GWLP_USERDATA: `app_opt` treats any non-null userdata
//! as an `App`, and `enum_ours` would then feed this window into
//! `close_all_windows` type-confused. The config lives in a static instead,
//! so `app_opt(hwnd)` returns None here and the resident is invisible to
//! every per-window code path.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, LoadCursorW,
    PostQuitMessage, RegisterClassExW, SetForegroundWindow, HMENU, IDC_ARROW, MSGFLT_ALLOW,
    WM_ENDSESSION, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NCDESTROY, WM_RBUTTONUP,
    WNDCLASSEXW,
};

use super::tray::{self, CMD_EXIT, CMD_NEW_WINDOW, WM_APP_TRAY};
use super::{close_all_windows, MainWindow, WM_APP_QUIT};
use crate::config::ConfigHandle;

pub const RESIDENT_CLASS: PCWSTR = w!("RevolcResident");

static RESIDENT_HWND: AtomicIsize = AtomicIsize::new(0);
static RESIDENT_CFG: Mutex<Option<Arc<ConfigHandle>>> = Mutex::new(None);

/// True while the resident window exists (only in redirect + tray mode).
pub(crate) fn alive() -> bool {
    RESIDENT_HWND.load(Ordering::SeqCst) != 0
}

pub(crate) fn hwnd() -> Option<HWND> {
    let h = RESIDENT_HWND.load(Ordering::SeqCst);
    if h == 0 {
        None
    } else {
        Some(HWND(h as _))
    }
}

/// Create the hidden window and hand it the tray icon. Called once at startup
/// from `app::run` (redirect mode + `[tray] enabled` only).
pub fn create(cfg: Arc<ConfigHandle>) -> windows::core::Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: RESIDENT_CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            let err = windows::core::Error::from_win32();
            // 1410 = ERROR_CLASS_ALREADY_EXISTS: same tolerate rule as every
            // other class in this crate.
            if (err.code().0 as u32 & 0xffff) != 1410 {
                return Err(err);
            }
        }
        // Zero style = created hidden, stays hidden. Still a top-level window,
        // so broadcasts (WM_ENDSESSION) and FindWindowW reach it.
        let hwnd = CreateWindowExW(
            Default::default(),
            RESIDENT_CLASS,
            w!("Revolc resident"),
            Default::default(),
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )?;
        // UIPI: Explorer's BHO posts the absorb notify from a lower-IL
        // process; without this it would be silently dropped when Revolc runs
        // elevated (observed err 5 on the main window, 2026-11-17).
        let m = super::absorb_msg();
        let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);

        *RESIDENT_CFG.lock().expect("resident cfg lock") = Some(cfg);
        RESIDENT_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        tray::add(hwnd);
        tracing::info!("resident window created (redirect takeover keeps running)");
        Ok(())
    }
}

/// Tear everything down: icon out, windows restored, resident destroyed.
/// Our WM_DESTROY posts the quit.
unsafe fn quit(hwnd: HWND) {
    tray::remove(hwnd);
    close_all_windows();
    let _ = DestroyWindow(hwnd);
}

/// No window list yet (Phase 2): one click, one window — same contract as the
/// old tray, minus the ambiguity of "打开 Revolc".
unsafe fn spawn_window() {
    let cfg = RESIDENT_CFG.lock().expect("resident cfg lock").clone();
    let Some(cfg) = cfg else { return };
    match MainWindow::spawn_extra(cfg) {
        Ok(w) => unsafe {
            // The user is about to work *here*; without this the batch still
            // in flight would be routed to the previous window.
            let _ = SetForegroundWindow(HWND(w.hwnd_raw() as _));
        },
        Err(e) => tracing::error!(error = %e, "resident window spawn failed"),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == super::absorb_msg() {
        // BHO fallback: no RevolcMain anywhere, so a folder window just
        // appeared on a bare desktop. Spawn a window; its own sweep adopts
        // the stray within one burst tick.
        tracing::info!("resident notify: spawning a window for the folder");
        spawn_window();
        return LRESULT(0);
    }
    match msg {
        m if m == WM_APP_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => spawn_window(),
                WM_RBUTTONUP => {
                    let cmd = tray::popup_menu(hwnd);
                    match cmd {
                        CMD_NEW_WINDOW => spawn_window(),
                        CMD_EXIT => quit(hwnd),
                        _ => {}
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        m if m == WM_APP_QUIT => {
            quit(hwnd);
            LRESULT(0)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                quit(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            RESIDENT_HWND.store(0, Ordering::SeqCst);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            *RESIDENT_CFG.lock().expect("resident cfg lock") = None;
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
