//! Double-click on the empty file-list area navigates to the parent folder.
//!
//! Ported from Clover's TabHelper64.dll (REA evidence:
//! `docs/Clover-双击空白返回上级-调研.md`). The BHO subclasses Explorer's
//! file-list window (`SysListView32` on Win10) and, on `WM_LBUTTONDBLCLK`
//! over empty space (`LVM_HITTEST` => iItem == -1), posts the shell's
//! "Up one level" command (`WM_COMMAND`, 0xA022) to the shell view.
//!
//! Revolc's BHO must never touch the site object (crash loop, see lib.rs),
//! so we cannot use `IWebBrowser2::get_HWND` like Clover does. Instead a
//! timer scans `EnumThreadWindows(GetCurrentThreadId())` — the BHO's SetSite
//! runs on Explorer's UI thread, which also owns all its folder windows.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Controls::{LVM_HITTEST, LVHITTESTINFO};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, FindWindowExW, GetClassNameW, SendMessageW, SetTimer, WM_COMMAND,
    WM_LBUTTONDBLCLK,
};

/// FCIDM_BROWSER command "Up one folder" (Clover sends exactly 0xA022).
const UP_ONE_LEVEL: WPARAM = WPARAM(0xA022);
/// Rescan interval. Cheap (one EnumThreadWindows), and catches windows whose
/// file-list view is created after SetSite fires.
const SCAN_MS: u32 = 750;

/// HWND values we already subclassed, so the periodic scan is idempotent.
static ATTACHED: OnceLock<Mutex<HashSet<isize>>> = OnceLock::new();
/// One timer per Explorer process.
static TIMER_ARMED: OnceLock<()> = OnceLock::new();

/// Called from `SetSite`. Schedule the scan once; Explorer's UI thread has a
/// message loop, so the timer callback will run there.
pub fn arm() {
    // Synchronous first pass: if this window's file list is already up,
    // attach now (even if we're not on a UI thread with a message pump).
    scan();
    // Timer fallback for file lists created later. Explorer's UI thread
    // pumps messages, so the callback will run there.
    TIMER_ARMED.get_or_init(|| unsafe {
        let _ = SetTimer(HWND::default(), 0, SCAN_MS, Some(timer_proc));
    });
}

unsafe extern "system" fn timer_proc(_hwnd: HWND, _msg: u32, _id: usize, _tick: u32) {
    scan();
}

fn scan() {
    unsafe {
        let tid = GetCurrentThreadId();
        let _ = EnumThreadWindows(tid, Some(enum_top), LPARAM(0));
    }
}

unsafe extern "system" fn enum_top(top: HWND, _lp: LPARAM) -> BOOL {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(top, &mut buf).max(0) as usize;
    let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    if class == "CabinetWClass" || class == "ExploreWClass" {
        attach_on_top(top);
    }
    BOOL(1) // keep enumerating
}

unsafe fn attach_on_top(top: HWND) {
    // Win10: SHELLDLL_DefView is a direct child of the CabinetWClass frame.
    let defview = FindWindowExW(top, HWND::default(), w!("SHELLDLL_DefView"), PCWSTR::null())
        .unwrap_or_default();
    if defview.0.is_null() {
        return;
    }

    // Command target, mirroring Clover's context: ShellTabWindowClass when
    // present, else the shell view's parent (the CabinetWClass frame).
    let mut target = FindWindowExW(top, HWND::default(), w!("ShellTabWindowClass"), PCWSTR::null())
        .unwrap_or_default();
    if target.0.is_null() {
        target = defview;
    }

    let listview = FindWindowExW(defview, HWND::default(), w!("SysListView32"), PCWSTR::null())
        .unwrap_or_default();
    if listview.0.is_null() {
        // Win11 uses DirectUIHWND (no LVM_HITTEST); out of scope for this
        // first revision — Win10 is the target here.
        return;
    }

    let set = ATTACHED.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut guard) = set.lock() else { return };
    if !guard.insert(listview.0 as isize) {
        return; // already subclassed
    }
    drop(guard);

    let ok = SetWindowSubclass(
        listview,
        Some(listview_subclass),
        0,
        target.0 as usize, // dwrefdata: where to send WM_COMMAND
    );
    if !ok.as_bool() {
        // Roll back so a later scan can retry.
        if let Ok(mut guard) = set.lock() {
            guard.remove(&(listview.0 as isize));
        }
    }
}

unsafe extern "system" fn listview_subclass(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    target: usize,
) -> LRESULT {
    // Give Explorer its normal double-click handling first (select item etc.).
    let result = DefSubclassProc(hwnd, msg, wp, lp);

    if msg == WM_LBUTTONDBLCLK {
        let x = (lp.0 as u32 & 0xffff) as i16 as i32;
        let y = ((lp.0 as u64 >> 16) & 0xffff) as i16 as i32;
        let mut hit = LVHITTESTINFO {
            pt: POINT { x, y },
            ..Default::default()
        };
        let _ = SendMessageW(
            hwnd,
            LVM_HITTEST,
            WPARAM(0),
            LPARAM(&mut hit as *mut LVHITTESTINFO as isize),
        );
        // No item under the pointer => the user double-clicked empty space.
        if hit.iItem < 0 && target != 0 {
            let _ = SendMessageW(HWND(target as *mut core::ffi::c_void), WM_COMMAND, UP_ONE_LEVEL, LPARAM(0));
        }
    }
    result
}
