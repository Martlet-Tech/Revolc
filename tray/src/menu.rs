//! Self-drawn tray menu (a Win32 popup menu cannot host an inline button).
//!
//! Rows: one per open window ("窗口N: recent tabs", with a × hit area on the
//! right), a separator, new window, a separator, exit-revolc, close-everything.
//! Clicking a row restores that window; clicking its × closes all its tabs
//! and the frame. Commands are posted to the tray window as private messages.

use std::mem;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject,
    SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics,
    LoadCursorW, PostMessageW, RegisterClassExW, SetForegroundWindow, SetWindowPos,
    ShowWindow, WNDCLASSEXW, WM_ACTIVATEAPP, WM_CANCELMODE, WM_DESTROY, WM_LBUTTONDOWN,
    WM_PAINT, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST, IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN,
    SW_SHOW, SWP_SHOWWINDOW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use revolc_common::messages::{WM_APP_CLOSE_TABS, WM_APP_RESTORE};

const MENU_CLASS: PCWSTR = w!("RevolcTrayMenu");
const ROW_H: i32 = 34;
const SEP_H: i32 = 9;
const MENU_W: i32 = 560;
const PAD: i32 = 12;
const X_W: i32 = 28;

/// Private messages the menu posts to the tray window, which owns spawn/quit.
pub const CMD_NEW_WINDOW: u32 = 0x8020;
pub const CMD_EXIT: u32 = 0x8021;
pub const CMD_CLOSE_ALL: u32 = 0x8022;

#[derive(Clone, Copy)]
enum Action {
    Window(isize),
    CloseWindow(isize),
    NewWindow,
    Exit,
    CloseAll,
}

#[derive(Clone)]
struct Row {
    text: String,
    action: Action,
    separator: bool,
    height: i32,
}

static ROWS: OnceLock<Mutex<Vec<Row>>> = OnceLock::new();
fn rows() -> &'static Mutex<Vec<Row>> { ROWS.get_or_init(|| Mutex::new(Vec::new())) }

/// Show the menu at the cursor. `windows` is (hwnd, "tabA/tabB/tabC") in
/// display order; `tray` receives the command messages.
pub unsafe fn show(tray: HWND, windows: Vec<(HWND, String)>) {
    let mut list: Vec<Row> = Vec::new();
    for (i, (hwnd, label)) in windows.iter().enumerate() {
        list.push(Row {
            text: format!("窗口{}: {}", i + 1, label),
            action: Action::Window(hwnd.0 as isize),
            separator: false,
            height: ROW_H,
        });
    }
    list.push(Row { text: String::new(), action: Action::NewWindow, separator: true, height: SEP_H });
    list.push(Row { text: "新建窗口".into(), action: Action::NewWindow, separator: false, height: ROW_H });
    list.push(Row { text: String::new(), action: Action::Exit, separator: true, height: SEP_H });
    list.push(Row { text: "退出revolc（关托盘+窗口，还原FE）".into(), action: Action::Exit, separator: false, height: ROW_H });
    list.push(Row { text: "关闭全部（啥都关）".into(), action: Action::CloseAll, separator: false, height: ROW_H });
    *rows().lock().unwrap() = list;

    register_class();
    let h = match CreateWindowExW(
        WS_EX_TOOLWINDOW, MENU_CLASS, w!(""), WS_POPUP,
        0, 0, MENU_W, 1,
        HWND::default(),
        windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
        GetModuleHandleW(None).unwrap_or_default(), None,
    ) {
        Ok(h) => h,
        Err(e) => { tracing::error!(error = %e, "menu window failed"); return; }
    };
    let total: i32 = rows().lock().unwrap().iter().map(|r| r.height).sum();
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Clamp to the primary screen so the menu never lands off-display.
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let mut x = pt.x.min(sw - MENU_W).max(0);
    let y = pt.y.min(sh - total).max(0);
    if sw == 3200 { x = x.min(1521 - MENU_W).max(0); } // primary monitor width
    let _ = SetWindowPos(h, HWND_TOPMOST, x, y, MENU_W, total, SWP_SHOWWINDOW);
    let _ = ShowWindow(h, SW_SHOW);
    let _ = SetForegroundWindow(h);
    let _ = tray; // tray hwnd is fetched from the static when posting
}

fn register_class() {
    unsafe { register_class_inner() }
}

unsafe fn register_class_inner() {
    let h = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: h.into(),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: MENU_CLASS,
        ..Default::default()
    };
    if RegisterClassExW(&wc) == 0 {
        let e = windows::core::Error::from_win32();
        if (e.code().0 as u32 & 0xffff) != 1410 { tracing::warn!(error = %e, "menu class failed"); }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => { paint(hwnd); LRESULT(0) }
        WM_LBUTTONDOWN => { on_click(hwnd, lp); LRESULT(0) }
        WM_ACTIVATEAPP if wp.0 == 0 => { let _ = DestroyWindow(hwnd); LRESULT(0) }
        WM_CANCELMODE => { let _ = DestroyWindow(hwnd); LRESULT(0) }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let snapshot = rows().lock().unwrap().clone();
    let mut y = 0i32;
    let font = GetStockObject(DEFAULT_GUI_FONT);
    for row in &snapshot {
        if row.separator {
            let brush = CreateSolidBrush(COLORREF(0x00606060));
            let rc = RECT { left: PAD, top: y + SEP_H / 2, right: MENU_W - PAD, bottom: y + SEP_H / 2 + 1 };
            let _ = FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
        } else {
            let brush = CreateSolidBrush(COLORREF(0x002B2B2B));
            let rc = RECT { left: 0, top: y, right: MENU_W, bottom: y + ROW_H };
            let _ = FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, COLORREF(0x00E0E0E0));
            let _ = SelectObject(hdc, HGDIOBJ(font.0));
            let mut rc = RECT { left: PAD, top: y, right: MENU_W - PAD - X_W, bottom: y + ROW_H };
            let mut text: Vec<u16> = row.text.encode_utf16().chain(Some(0)).collect();
            let _ = DrawTextW(hdc, &mut text, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            if matches!(row.action, Action::Window(_)) {
                let mut xrc = RECT { left: MENU_W - PAD - X_W, top: y, right: MENU_W - PAD, bottom: y + ROW_H };
                let mut xtext: Vec<u16> = "×".encode_utf16().chain(Some(0)).collect();
                let _ = DrawTextW(hdc, &mut xtext, &mut xrc, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            }
        }
        y += row.height;
    }
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn on_click(hwnd: HWND, lp: LPARAM) {
    let x = (lp.0 as u32 & 0xffff) as i32;
    let y = ((lp.0 as u32 >> 16) & 0xffff) as i32;
    let snapshot = rows().lock().unwrap().clone();
    let mut top = 0i32;
    for row in &snapshot {
        let bottom = top + row.height;
        if y >= top && y < bottom {
            let action = if x >= MENU_W - PAD - X_W {
                match row.action { Action::Window(h) => Action::CloseWindow(h), other => other }
            } else { row.action };
            let _ = DestroyWindow(hwnd);
            dispatch(action);
            return;
        }
        top = bottom;
    }
    let _ = DestroyWindow(hwnd);
}

unsafe fn dispatch(action: Action) {
    let tray = crate::tray_hwnd();
    match action {
        Action::Window(h) => { let _ = PostMessageW(HWND(h as _), WM_APP_RESTORE, WPARAM(0), LPARAM(0)); }
        Action::CloseWindow(h) => { let _ = PostMessageW(HWND(h as _), WM_APP_CLOSE_TABS, WPARAM(0), LPARAM(0)); }
        Action::NewWindow => { let _ = PostMessageW(tray, CMD_NEW_WINDOW, WPARAM(0), LPARAM(0)); }
        Action::Exit => { let _ = PostMessageW(tray, CMD_EXIT, WPARAM(0), LPARAM(0)); }
        Action::CloseAll => { let _ = PostMessageW(tray, CMD_CLOSE_ALL, WPARAM(0), LPARAM(0)); }
    }
}
