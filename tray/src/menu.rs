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
    InvalidateRect, SelectObject, SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_END_ELLIPSIS,
    DT_LEFT, DT_SINGLELINE, DT_VCENTER, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics,
    LoadCursorW, PostMessageW, RegisterClassExW, SetForegroundWindow,
    SetWindowPos, ShowWindow, WNDCLASSEXW, WM_ACTIVATEAPP, WM_CANCELMODE, WM_DESTROY,
    WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, CS_HREDRAW, CS_VREDRAW, HWND_TOPMOST,
    IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SWP_SHOWWINDOW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use revolc_common::messages::{WM_APP_CLOSE_TABS, WM_APP_RESTORE};

const MENU_CLASS: PCWSTR = w!("RevolcTrayMenu");
const ROW_H: i32 = 34;
const SEP_H: i32 = 9;
const MENU_W: i32 = 520;
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

/// Which part of a window row the pointer is over. Left and × highlight
/// independently so the close target is obvious before clicking.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum HoverPart { #[default] None, Left, X }
static HOVER: OnceLock<Mutex<(usize, HoverPart)>> = OnceLock::new();
fn hover() -> &'static Mutex<(usize, HoverPart)> {
    HOVER.get_or_init(|| Mutex::new((usize::MAX, HoverPart::None)))
}

/// Generic shell captions that carry no information in a tab list.
fn short_tab(label: &str) -> Option<&str> {
    let s = label.trim();
    if s.is_empty() || s == "文件资源管理器" || s == "此电脑" || s == "File Explorer" || s == "This PC" {
        None
    } else {
        Some(s)
    }
}

/// Show the menu at the cursor. `windows` is (hwnd, "tabA/tabB/tabC") in
/// display order; `tray` receives the command messages.
pub unsafe fn show(tray: HWND, windows: Vec<(HWND, String)>) {
    let mut list: Vec<Row> = Vec::new();
    for (i, (hwnd, label)) in windows.iter().enumerate() {
        let tabs: Vec<&str> = label.split(" / ").filter_map(short_tab).collect();
        let text = format!("{}#: {}", i + 1, tabs.join(" / "));
        list.push(Row {
            text,
            action: Action::Window(hwnd.0 as isize),
            separator: false,
            height: ROW_H,
        });
    }
    *hover().lock().unwrap() = (usize::MAX, HoverPart::None);
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
        WM_MOUSEMOVE => { on_hover(hwnd, lp); LRESULT(0) }
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

    // One background fill for the WHOLE window first. The window class has no
    // background brush, so any pixel we miss stays white/garbage — that was
    // the white bars between rows in the first build.
    let total: i32 = snapshot.iter().map(|r| r.height).sum();
    let bg = CreateSolidBrush(COLORREF(0x002B2B2B));
    let whole = RECT { left: 0, top: 0, right: MENU_W, bottom: total };
    let _ = FillRect(hdc, &whole, bg);
    let _ = DeleteObject(HGDIOBJ(bg.0));

    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF(0x00E0E0E0));
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let _ = SelectObject(hdc, HGDIOBJ(font.0));

    let (hover_row, hover_part) = *hover().lock().unwrap();
    let mut y = 0i32;
    for (idx, row) in snapshot.iter().enumerate() {
        if row.separator {
            let line = CreateSolidBrush(COLORREF(0x00606060));
            let rc = RECT { left: PAD, top: y + SEP_H / 2, right: MENU_W - PAD, bottom: y + SEP_H / 2 + 1 };
            let _ = FillRect(hdc, &rc, line);
            let _ = DeleteObject(HGDIOBJ(line.0));
        } else {
            let hot = idx == hover_row;
            if hot {
                // Highlight only the hovered half: the row (left) and the
                // close button (right) get separate colours.
                let (left_color, x_color) = match hover_part {
                    HoverPart::X => (COLORREF(0x002B2B2B), COLORREF(0x000030C0)),
                    _ => (COLORREF(0x003A3A3A), COLORREF(0x002B2B2B)),
                };
                let lbrush = CreateSolidBrush(left_color);
                let rc = RECT { left: 0, top: y, right: MENU_W - PAD - X_W, bottom: y + ROW_H };
                let _ = FillRect(hdc, &rc, lbrush);
                let _ = DeleteObject(HGDIOBJ(lbrush.0));
                if matches!(row.action, Action::Window(_)) {
                    let xbrush = CreateSolidBrush(x_color);
                    let xrc0 = RECT { left: MENU_W - PAD - X_W, top: y, right: MENU_W, bottom: y + ROW_H };
                    let _ = FillRect(hdc, &xrc0, xbrush);
                    let _ = DeleteObject(HGDIOBJ(xbrush.0));
                }
            }
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

unsafe fn on_hover(hwnd: HWND, lp: LPARAM) {
    let x = (lp.0 as u32 & 0xffff) as i32;
    let y = ((lp.0 as u32 >> 16) & 0xffff) as i32;
    let snapshot = rows().lock().unwrap().clone();
    let mut next = (usize::MAX, HoverPart::None);
    let mut top = 0i32;
    for (idx, row) in snapshot.iter().enumerate() {
        let bottom = top + row.height;
        if y >= top && y < bottom && !row.separator {
            let part = if matches!(row.action, Action::Window(_)) && x >= MENU_W - PAD - X_W {
                HoverPart::X
            } else {
                HoverPart::Left
            };
            next = (idx, part);
            break;
        }
        top = bottom;
    }
    let mut h = hover().lock().unwrap();
    if *h != next {
        *h = next;
        drop(h);
        let _ = InvalidateRect(hwnd, None, false);
    }
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
