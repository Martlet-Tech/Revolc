//! Tray icon: resident mode for takeover (spec §1 主程序可常驻; user request:
//! "彻底关了就没接管" → redirect 模式下进程缩托盘继续拦截).
//! Minimal: our icon, right-click menu, left click to bring the window back.

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    TrackPopupMenuEx, MF_STRING, TPM_RETURNCMD, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
};

pub const WM_APP_TRAY: u32 = 0x8004;
const TRAY_ID: u32 = 1;
const CMD_OPEN: usize = 1;
const CMD_EXIT: usize = 2;
const CMD_NEW_WINDOW: usize = 3;

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

/// Handle WM_APP_TRAY mouse events. Returns Some(LRESULT) if consumed.
///
/// Left click = "show me the window", like every other tray app: one click,
/// one action. It never toggles (no hide-on-click) and never touches the zoom
/// state — `App::open_main` restores only what was minimized/hidden and then
/// takes the foreground. Double click needs no separate path: the
/// `WM_LBUTTONUP` before `WM_LBUTTONDBLCLK` already did the work, and the
/// second call is idempotent.
pub unsafe fn on_tray_message(hwnd: HWND, lp: LPARAM, app: &mut crate::ui::App) -> Option<LRESULT> {
    let m = lp.0 as u32;
    match m {
        WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
            app.open_main();
            return Some(LRESULT(0));
        }
        WM_RBUTTONUP => {}
        _ => return None,
    }
    // Right-click: context menu.
    let menu = CreatePopupMenu().ok()?;
    let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("打开 Revolc"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_NEW_WINDOW, w!("新建窗口"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD.0, pt.x, pt.y, hwnd, None).0 as usize;
    match cmd {
        CMD_OPEN => {
            app.open_main();
            Some(LRESULT(0))
        }
        CMD_NEW_WINDOW => {
            app.spawn_window();
            Some(LRESULT(0))
        }
        CMD_EXIT => {
            // Destroy every window through the shared quit path; killing just
            // this one would leave the others running with no tray icon.
            let _ = PostMessageW(hwnd, crate::ui::WM_APP_QUIT, WPARAM(0), LPARAM(0));
            Some(LRESULT(0))
        }
        _ => Some(LRESULT(0)),
    }
}
