//! Tray icon: resident mode for takeover (spec §1 主程序可常驻; user request:
//! "彻底关了就没接管" → redirect 模式下进程缩托盘继续拦截).
//! Minimal: default icon, right-click menu, left-double-click to restore.

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, GetCursorPos, LoadIconW, SetForegroundWindow,
    TrackPopupMenuEx, HMENU, IDI_APPLICATION, MF_STRING, SW_SHOW, TPM_RETURNCMD,
    WM_LBUTTONDBLCLK, WM_RBUTTONUP,
};

pub const WM_APP_TRAY: u32 = 0x8004;
const TRAY_ID: u32 = 1;
const CMD_OPEN: usize = 1;
const CMD_EXIT: usize = 2;

/// Add the tray icon. Returns false on failure (non-fatal).
pub fn add(hwnd: HWND) -> bool {
    unsafe {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = LoadIconW(None, IDI_APPLICATION).unwrap_or_default();
        let tip = w!("Revolc (文件夹接管中)");
        let tip_bytes: Vec<u16> = tip.as_wide().iter().take(127).copied().collect();
        for (i, c) in tip_bytes.into_iter().enumerate() {
            nid.szTip[i] = c;
        }
        let ok = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
        if ok {
            tracing::info!("tray icon added");
        } else {
            tracing::error!("tray icon add failed");
        }
        ok
    }
}

pub fn remove(hwnd: HWND) {
    unsafe {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        tracing::info!("tray icon removed");
    }
}

/// Handle WM_APP_TRAY mouse events. Returns Some(LRESULT) if consumed.
pub unsafe fn on_tray_message(hwnd: HWND, lp: LPARAM, app: &mut crate::ui::App) -> Option<LRESULT> {
    let m = lp.0 as u32;
    if m != WM_RBUTTONUP && m != WM_LBUTTONDBLCLK {
        return None;
    }
    if m == WM_LBUTTONDBLCLK {
        app.open_main();
        return Some(LRESULT(0));
    }
    // Right-click: context menu.
    let menu = CreatePopupMenu().ok()?;
    let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("打开 Revolc"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD.0, pt.x, pt.y, hwnd, None).0 as usize;
    match cmd {
        CMD_OPEN => {
            app.open_main();
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(hwnd, SW_SHOW);
            Some(LRESULT(0))
        }
        CMD_EXIT => {
            remove(hwnd);
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            Some(LRESULT(0))
        }
        _ => Some(LRESULT(0)),
    }
}
