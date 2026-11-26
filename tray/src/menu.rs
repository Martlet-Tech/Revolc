//! Self-drawn tray menu (a Win32 popup menu cannot host an inline button).
//!
//! Rows: one per open workbench window ("N#: tab/tab/tab", × on the right),
//! then only daily actions — new window / open settings / exit. Takeover
//! toggling is deliberately NOT here: it stays in the settings page.
//! Clicking a window row restores it; clicking its × closes that window.
//!
//! Geometry is derived from the widest row at open time: text width is
//! measured with GetTextExtentPoint32W, so a menu with one short window is
//! narrow instead of a fixed wide box. Every x coordinate comes from `geom()`.

use std::mem;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
    DrawTextW, EndPaint, FillRect, GetDC, GetStockObject, GetTextExtentPoint32W, InvalidateRect,
    Rectangle, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_GUI_FONT, DEFAULT_PITCH, DRAW_TEXT_FORMAT,
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL, HDC, HFONT, HGDIOBJ,
    NULL_BRUSH, OUT_TT_PRECIS, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics, LoadCursorW,
    PostMessageW, RegisterClassExW, SetForegroundWindow, SetWindowPos, ShowWindow, CS_HREDRAW,
    CS_VREDRAW, HWND_TOPMOST, IDC_ARROW, SM_CXSCREEN, SM_CYSCREEN, SWP_SHOWWINDOW, SW_SHOW,
    WM_ACTIVATEAPP, WM_CANCELMODE, WM_DESTROY, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

use revolc_common::messages::{WM_APP_CLOSE_TABS, WM_APP_RESTORE};

const MENU_CLASS: PCWSTR = w!("RevolcTrayMenu");
const ROW_H: i32 = 32;
const SEP_H: i32 = 9;
const PAD: i32 = 12;
const X_BTN_W: i32 = 26; // close-button column
const MIN_W: i32 = 220;
const MAX_W: i32 = 560;

/// Private messages the menu posts to the tray window, which owns spawn/quit.
pub const CMD_NEW_WINDOW: u32 = 0x8020;
pub const CMD_EXIT: u32 = 0x8021;
pub const CMD_OPEN_SETTINGS: u32 = 0x8022;

#[derive(Clone, Copy)]
enum Action {
    /// Non-clickable group caption ("窗口 (N)" / "操作").
    Header,
    Window(isize),
    CloseWindow(isize),
    NewWindow,
    OpenSettings,
    Exit,
}

#[derive(Clone)]
struct Row {
    text: String,
    action: Action,
    separator: bool,
    height: i32,
}

static ROWS: OnceLock<Mutex<Vec<Row>>> = OnceLock::new();
fn rows() -> &'static Mutex<Vec<Row>> {
    ROWS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Menu width, computed once when it opens from the widest row.
static WIDTH: OnceLock<Mutex<i32>> = OnceLock::new();
fn width() -> i32 {
    *WIDTH.get_or_init(|| Mutex::new(MIN_W)).lock().unwrap()
}
fn text_right() -> i32 {
    width() - PAD - X_BTN_W - 6
}
fn x_left() -> i32 {
    width() - PAD - X_BTN_W
}

/// Which part of a window row the pointer is over. Left and × highlight
/// independently so the close target is obvious before clicking.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum HoverPart {
    #[default]
    None,
    Left,
    X,
}
static HOVER: OnceLock<Mutex<(usize, HoverPart)>> = OnceLock::new();
fn hover() -> &'static Mutex<(usize, HoverPart)> {
    HOVER.get_or_init(|| Mutex::new((usize::MAX, HoverPart::None)))
}

/// Generic shell captions that carry no information in a tab list.
fn short_tab(label: &str) -> Option<&str> {
    let s = label.trim();
    if s.is_empty()
        || s == "文件资源管理器"
        || s == "此电脑"
        || s == "File Explorer"
        || s == "This PC"
    {
        None
    } else {
        Some(s)
    }
}

fn has_x(action: Action) -> bool {
    matches!(action, Action::Window(_))
}

/// Measure a row's text width using the menu font.
unsafe fn measure(text: &str) -> i32 {
    let hdc = GetDC(None);
    let font = menu_font(13);
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut size = SIZE::default();
    let w = if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
        size.cx
    } else {
        0
    };
    let _ = SelectObject(hdc, old);
    let _ = DeleteObject(HGDIOBJ(font.0));
    let _ = ReleaseDC(None, hdc);
    w
}

/// Show the daily-use menu at the cursor. `windows` is (hwnd, "tabA/tabB") in
/// display order; `takeover_on` decides whether workbench actions appear.
/// System-level takeover changes live only in the settings page.
pub unsafe fn show(tray: HWND, windows: Vec<(HWND, String)>, takeover_on: bool) {
    let mut list: Vec<Row> = Vec::new();
    if takeover_on {
        list.push(Row {
            text: format!("窗口 ({})", windows.len()),
            action: Action::Header,
            separator: false,
            height: 24,
        });
        for (hwnd, label) in windows.iter() {
            let tabs: Vec<&str> = label.split(" / ").filter_map(short_tab).collect();
            list.push(Row {
                text: tabs.join(" / "),
                action: Action::Window(hwnd.0 as isize),
                separator: false,
                height: ROW_H,
            });
        }
        list.push(Row {
            text: String::new(),
            action: Action::NewWindow,
            separator: true,
            height: SEP_H,
        });
        list.push(Row {
            text: "新建窗口".into(),
            action: Action::NewWindow,
            separator: false,
            height: ROW_H,
        });
    }
    if !list.is_empty() {
        list.push(Row {
            text: String::new(),
            action: Action::OpenSettings,
            separator: true,
            height: SEP_H,
        });
        list.push(Row {
            text: "操作".into(),
            action: Action::Header,
            separator: false,
            height: 24,
        });
    }
    list.push(Row {
        text: "打开设置".into(),
        action: Action::OpenSettings,
        separator: false,
        height: ROW_H,
    });
    list.push(Row {
        text: "退出 Revolc".into(),
        action: Action::Exit,
        separator: false,
        height: ROW_H,
    });
    *rows().lock().unwrap() = list;
    *hover().lock().unwrap() = (usize::MAX, HoverPart::None);

    // Content-adaptive width: widest text + both paddings + x column if any
    // window row needs it. The long command labels set a sensible floor.
    let widest = rows()
        .lock()
        .unwrap()
        .iter()
        .map(|r| {
            let t = measure(&r.text);
            t + PAD * 2 + if has_x(r.action) { X_BTN_W + 6 } else { 0 }
        })
        .max()
        .unwrap_or(MIN_W);
    *WIDTH.get_or_init(|| Mutex::new(MIN_W)).lock().unwrap() = widest.clamp(MIN_W, MAX_W);

    register_class();
    let w = width();
    let h = match CreateWindowExW(
        WS_EX_TOOLWINDOW,
        MENU_CLASS,
        w!(""),
        WS_POPUP,
        0,
        0,
        w,
        1,
        HWND::default(),
        windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
        GetModuleHandleW(None).unwrap_or_default(),
        None,
    ) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "menu window failed");
            return;
        }
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
    let region = CreateRoundRectRgn(0, 0, w, total, 8, 8);
    if !region.0.is_null() {
        let _ = SetWindowRgn(h, region, true); // ownership transfers to the window
    }
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
        if (e.code().0 as u32 & 0xffff) != 1410 {
            tracing::warn!(error = %e, "menu class failed");
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            on_hover(hwnd, lp);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            on_click(hwnd, lp);
            LRESULT(0)
        }
        WM_ACTIVATEAPP if wp.0 == 0 => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_CANCELMODE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let snapshot = rows().lock().unwrap().clone();
    let w = width();
    let p = crate::current_palette();
    let bg = COLORREF(p.surface_background().to_colorref());
    let hover_fill = COLORREF(p.surface_hover().to_colorref());
    let border = COLORREF(p.border_subtle().to_colorref());
    let ink = COLORREF(p.foreground_primary().to_colorref());
    let ink_dim = COLORREF(p.foreground_secondary().to_colorref());
    let inverse = COLORREF(p.foreground_inverse().to_colorref());
    let danger = COLORREF(p.danger_default().to_colorref());

    let total: i32 = snapshot.iter().map(|r| r.height).sum();
    let brush = CreateSolidBrush(bg);
    let whole = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: total,
    };
    let _ = FillRect(hdc, &whole, brush);
    let _ = DeleteObject(HGDIOBJ(brush.0));
    let _ = SetBkMode(hdc, TRANSPARENT);

    let body = menu_font(13);
    let caption = menu_font(11);
    let left_flags =
        DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0);
    let center_flags = DRAW_TEXT_FORMAT(DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0);

    let (hover_row, hover_part) = *hover().lock().unwrap();
    let mut y = 0i32;
    for (idx, row) in snapshot.iter().enumerate() {
        if row.separator {
            let line = CreateSolidBrush(border);
            let rc = RECT {
                left: PAD,
                top: y + SEP_H / 2,
                right: w - PAD,
                bottom: y + SEP_H / 2 + 1,
            };
            let _ = FillRect(hdc, &rc, line);
            let _ = DeleteObject(HGDIOBJ(line.0));
        } else if matches!(row.action, Action::Header) {
            draw_text(
                hdc,
                RECT {
                    left: PAD,
                    top: y,
                    right: w - PAD,
                    bottom: y + row.height,
                },
                &row.text,
                caption,
                ink_dim,
                left_flags,
            );
        } else {
            let hot = idx == hover_row;
            let x_col = has_x(row.action);
            let x_hot = hot && hover_part == HoverPart::X && x_col;
            let left_right = if x_col { x_left() - 6 } else { w };
            let left_fill = if hot && !x_hot { hover_fill } else { bg };
            let brush = CreateSolidBrush(left_fill);
            let rc = RECT {
                left: 0,
                top: y,
                right: left_right,
                bottom: y + row.height,
            };
            let _ = FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));
            if x_col {
                let brush = CreateSolidBrush(if x_hot { danger } else { bg });
                let xrc = RECT {
                    left: x_left(),
                    top: y,
                    right: w,
                    bottom: y + row.height,
                };
                let _ = FillRect(hdc, &xrc, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
            let row_ink = if x_hot || (matches!(row.action, Action::Exit) && hot) {
                if x_hot {
                    inverse
                } else {
                    danger
                }
            } else {
                ink
            };
            draw_text(
                hdc,
                RECT {
                    left: PAD,
                    top: y,
                    right: text_right(),
                    bottom: y + row.height,
                },
                &row.text,
                body,
                row_ink,
                left_flags,
            );
            if x_col {
                draw_text(
                    hdc,
                    RECT {
                        left: x_left(),
                        top: y,
                        right: w - PAD,
                        bottom: y + row.height,
                    },
                    "×",
                    body,
                    if x_hot { inverse } else { ink_dim },
                    center_flags,
                );
            }
        }
        y += row.height;
    }

    let pen = CreatePen(PS_SOLID, 1, border);
    let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
    let old_brush = SelectObject(hdc, HGDIOBJ(GetStockObject(NULL_BRUSH).0));
    let _ = Rectangle(hdc, 0, 0, w, total);
    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old_pen);
    let _ = DeleteObject(HGDIOBJ(pen.0));
    let _ = DeleteObject(HGDIOBJ(body.0));
    let _ = DeleteObject(HGDIOBJ(caption.0));
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn menu_font(px: i32) -> HFONT {
    let handle = CreateFontW(
        -px,
        0,
        0,
        0,
        FW_NORMAL.0 as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_TT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        DEFAULT_PITCH.0 as u32,
        w!("Microsoft YaHei UI"),
    );
    if handle.is_invalid() {
        HFONT(GetStockObject(DEFAULT_GUI_FONT).0)
    } else {
        handle
    }
}

unsafe fn draw_text(
    hdc: HDC,
    mut rc: RECT,
    label: &str,
    font: HFONT,
    ink: COLORREF,
    flags: DRAW_TEXT_FORMAT,
) {
    let old = SelectObject(hdc, HGDIOBJ(font.0));
    let _ = SetTextColor(hdc, ink);
    let mut wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
    let _ = DrawTextW(hdc, &mut wide, &mut rc, flags);
    let _ = SelectObject(hdc, HGDIOBJ(old.0));
}

/// Row index and part under a client-space point.
fn hit_test(x: i32, y: i32) -> (usize, HoverPart) {
    let snapshot = rows().lock().unwrap().clone();
    let mut top = 0i32;
    for (idx, row) in snapshot.iter().enumerate() {
        let bottom = top + row.height;
        if y >= top && y < bottom && !row.separator && !matches!(row.action, Action::Header) {
            let part = if has_x(row.action) && x >= x_left() {
                HoverPart::X
            } else {
                HoverPart::Left
            };
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
            match r.action {
                Action::Window(h) => Action::CloseWindow(h),
                other => other,
            }
        } else {
            r.action
        };
        let _ = DestroyWindow(hwnd);
        dispatch(action);
        return;
    }
    let _ = DestroyWindow(hwnd);
}

unsafe fn dispatch(action: Action) {
    let tray = crate::tray_hwnd();
    match action {
        Action::Header => {}
        Action::Window(h) => {
            let _ = PostMessageW(HWND(h as _), WM_APP_RESTORE, WPARAM(0), LPARAM(0));
        }
        Action::CloseWindow(h) => {
            let _ = PostMessageW(HWND(h as _), WM_APP_CLOSE_TABS, WPARAM(0), LPARAM(0));
        }
        Action::NewWindow => {
            let _ = PostMessageW(tray, CMD_NEW_WINDOW, WPARAM(0), LPARAM(0));
        }
        Action::OpenSettings => {
            let _ = PostMessageW(tray, CMD_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
        }
        Action::Exit => {
            let _ = PostMessageW(tray, CMD_EXIT, WPARAM(0), LPARAM(0));
        }
    }
}
