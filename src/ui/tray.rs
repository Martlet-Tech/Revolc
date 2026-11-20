//! Tray icon: an optional convenience launcher (docs/双进程架构设计.md).
//! The icon lives on the resident window, never on a RevolcMain: every main
//! window must stay a plain closable window. Menu: 新建窗口 / 退出 — the
//! window list arrives in Phase 2.

use windows::core::w;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenuEx, MF_STRING,
    TPM_RETURNCMD,
};

pub const WM_APP_TRAY: u32 = 0x8004;
const TRAY_ID: u32 = 1;
pub(crate) const CMD_NEW_WINDOW: usize = 3;
pub(crate) const CMD_EXIT: usize = 2;

/// Add the tray icon. Returns false on failure (non-fatal).
pub fn add(hwnd: HWND) -> bool {
    unsafe {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        // Same .ico as the window/exe icon, loaded at SM_CXSMICON so it is
        // sharp on 125%/150% displays instead of a rescaled 16px bitmap.
        nid.hIcon = crate::ui::icon::small();
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

/// Right-click context menu. Returns the picked command, 0 if dismissed.
///
/// `SetForegroundWindow` first: a TrackPopupMenu without a foreground window
/// dismisses itself on the first click elsewhere (documented Win32 trap).
pub unsafe fn popup_menu(hwnd: HWND) -> usize {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let _ = AppendMenuW(menu, MF_STRING, CMD_NEW_WINDOW, w!("新建窗口"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    TrackPopupMenuEx(menu, TPM_RETURNCMD.0, pt.x, pt.y, hwnd, None).0 as usize
}
