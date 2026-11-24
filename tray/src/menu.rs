//! Self-drawn tray menu (a Win32 popup menu cannot host an inline button).
//!
//! Rows: one per open window ("N#: tab/tab/tab", with a × hit area on the
//! right), a separator, new window, a separator, exit-revolc, close-everything.
//! Clicking a row restores that window; clicking its × closes all its tabs
//! and the frame. Commands are posted to the tray window as private messages.
//!
//! Geometry is derived from the widest row at open time: text width is
//! measured with GetTextExtentPoint32W, so a menu with one short window is
//! narrow instead of a fixed wide box. Every x coordinate comes from `geom()`.

use std::mem;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetStockObject, GetTextExtentPoint32W, InvalidateRect, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, DEFAULT_GUI_FONT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
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
const ROW_H: i32 = 32;
const SEP_H: i32 = 9;
const PAD: i32 = 12;
const X_BTN_W: i32 = 26;   // close-button column
const MIN_W: i32 = 220;
const MAX_W: i32 = 560;

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

/// Menu width, computed once when it opens from the widest row.
static WIDTH: OnceLock<Mutex<i32>> = OnceLock::new();
fn width() -> i32 { *WIDTH.get_or_init(|| Mutex::new(MIN_W)).lock().unwrap() }
fn text_right() -> i32 { width() - PAD - X_BTN_W - 6 }
fn x_left() -> i32 { width() - PAD - X_BTN_W }

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

fn has_x(action: Action) -> bool { matches!(action, Action::Window(_)) }

/// Measure a row's text width using the menu font.
unsafe fn measure(text: &str) -> i32 {
    let hdc = GetDC(None);
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut size = SIZE::default();
    let w = if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
        size.cx
    } else { 0 };
    let _ = SelectObject(hdc, old);
    let _ = ReleaseDC(None, hdc);
    w
}

/// Show the menu at the cursor. `windows` is (hwnd, "tabA/tabB/tabC") in
/// display order; `tray` receives the command messages.
pub unsafe fn show(tray: HWND, windows: Vec<(HWND, String)>) {
    let mut list: Vec<Row> = Vec::new();
    for (i, (hwnd, label)) in windows.iter().enumerate() {
        let tabs: Vec<&str> = label.split(" / ").filter_map(short_tab).collect();
        list.push(Row {
            text: format!("{}#: {}", i + 1, tabs.join(" / ")),
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
    *hover().lock().unwrap() = (usize::MAX, HoverPart::None);

    // Content-adaptive width: widest text + both paddings + x column if any
    // window row needs it. The long command labels set a sensible floor.
    let widest = rows().lock().unwrap().iter().map(|r| {
        let t = measure(&r.text);
        t + PAD * 2 + if has_x(r.action) { X_BTN_W + 6 } else { 0 }
    }).max().unwrap_or(MIN_W);
    *WIDTH.get_or_init(|| Mutex::new(MIN_W)).lock().unwrap() = widest.clamp(MIN_W, MAX_W);

    register_class();
    let w = width();
    let h = match CreateWindowExW(
        WS_EX_TOOLWINDOW, MENU_CLASS, w!(""), WS_POPUP,
        0, 0, w, 1,
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
    let x = pt.x.min(sw - w).max(0);
    let y = pt.y.min(sh - total).max(0);
    let _ = SetWindowPos(h, HWND_TOPMOST, x, y, w, total, SWP_SHOWWINDOW);
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
    let w = width();

    // One background fill for the WHOLE window first. The window class has no
    // background brush, so any pixel we miss stays white/garbage.
    let total: i32 = snapshot.iter().map(|r| r.height).sum();
    let bg = CreateSolidBrush(COLORREF(0x002B2B2B));
    let whole = RECT { left: 0, top: 0, right: w, bottom: total };
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
            let rc = RECT { left: PAD, top: y + SEP_H / 2, right: w - PAD, bottom: y + SEP_H / 2 + 1 };
            let _ = FillRect(hdc, &rc, line);
            let _ = DeleteObject(HGDIOBJ(line.0));
        } else {
            let hot = idx == hover_row;
            let x_col = has_x(row.action);
            if hot {
                // Highlight only the hovered half: label area neutral grey,
                // close button red — the destructive target is unmistakable.
                let (left_color, x_color) = match hover_part {
                    HoverPart::X => (COLORREF(0x002B2B2B), COLORREF(0x000030C0)),
                    _ => (COLORREF(0x003A3A3A), if x_col { COLORREF(0x002B2B2B) } else { COLORREF(0x003A3A3A) }),
                };
                let left_right = if x_col { x_left() - 6 } else { w };
                let lbrush = CreateSolidBrush(left_color);
                let rc = RECT { left: 0, top: y, right: left_right, bottom: y + ROW_H };
                let _ = FillRect(hdc, &rc, lbrush);
                let _ = DeleteObject(HGDIOBJ(lbrush.0));
                if x_col {
                    let xbrush = CreateSolidBrush(x_color);
                    let xrc0 = RECT { left: x_left(), top: y, right: w, bottom: y + ROW_H };
                    let _ = FillRect(hdc, &xrc0, xbrush);
                    let _ = DeleteObject(HGDIOBJ(xbrush.0));
                }
            }
            let mut rc = RECT { left: PAD, top: y, right: text_right(), bottom: y + ROW_H };
            let mut text: Vec<u16> = row.text.encode_utf16().chain(Some(0)).collect();
            let _ = DrawTextW(hdc, &mut text, &mut rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            if x_col {
                // Centre × inside its button column.
                let mut xrc = RECT { left: x_left(), top: y, right: w - PAD, bottom: y + ROW_H };
                let mut xtext: Vec<u16> = "×".encode_utf16().chain(Some(0)).collect();
                let _ = DrawTextW(hdc, &mut xtext, &mut xrc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }
        }
        y += row.height;
    }
    let _ = EndPaint(hwnd, &ps);
}

/// Row index and part under a client-space point.
fn hit_test(x: i32, y: i32) -> (usize, HoverPart) {
    let snapshot = rows().lock().unwrap().clone();
    let mut top = 0i32;
    for (idx, row) in snapshot.iter().enumerate() {
        let bottom = top + row.height;
        if y >= top && y < bottom && !row.separator {
            let part = if has_x(row.action) && x >= x_left() { HoverPart::X } else { HoverPart::Left };
            return (idx, part);
        }
        top = bottom;
    }
    (usize::MAX, HoverPart::None)
}

unsafe fn on_hover(hwnd: HWND, lp: LPARAM) {
    let x = (lp.0 as u32 & 0xffff) as i32;
    let y = ((lp.0 as u32 >> 16) & 0xffff) as i32;
    let next = hit_test(x, y);
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
    let (row, part) = hit_test(x, y);
    let snapshot = rows().lock().unwrap().clone();
    if let Some(r) = snapshot.get(row) {
        let action = if part == HoverPart::X {
            match r.action { Action::Window(h) => Action::CloseWindow(h), other => other }
        } else { r.action };
        let _ = DestroyWindow(hwnd);
        dispatch(action);
        return;
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
