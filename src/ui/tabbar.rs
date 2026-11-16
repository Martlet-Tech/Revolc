//! Tab bar: self-drawn child window (M0 design §1 ui/tabbar.rs).
//! Main-thread only. State updates flow main window -> update() -> invalidate.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DrawTextW, EndPaint, FillRect, GetStockObject, InvalidateRect,
    SelectObject, SetBkMode, SetTextColor, HBRUSH, HFONT,     PAINTSTRUCT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DEFAULT_GUI_FONT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetParent, GetWindowLongPtrW, LoadCursorW, PostMessageW,
    RegisterClassExW, SetWindowLongPtrW, IDC_ARROW, WM_LBUTTONDOWN, WM_MBUTTONUP, WM_PAINT,
    WNDCLASSEXW, GWLP_USERDATA, HMENU, WS_CHILD, WS_VISIBLE,
};

use crate::config::Config;
use crate::ui::{WM_APP_TAB_ACTIVATED, WM_APP_TAB_CLOSE, WM_APP_TAB_NEW};

const TAB_WIDTH: i32 = 150;
const TAB_HEIGHT: i32 = 22;
const TAB_GAP: i32 = 4;
const CLOSE_BOX: i32 = 16; // per-tab close (X) button size
const PLUS_SIZE: i32 = 20; // trailing "+" button size
const CLASS_NAME: PCWSTR = w!("RevolcTabBar");

/// What a click landed on.
enum Hit {
    Tab(usize),
    Close(usize),
    Plus,
    None,
}

/// Snapshot of what to draw; rebuilt on every update() from the config + tabs.
struct State {
    labels: Vec<String>,
    active: usize,
    bg: COLORREF,
    active_bg: COLORREF,
    inactive_bg: COLORREF,
    text: COLORREF,
    text_active: COLORREF,
}

impl State {
    fn new(cfg: &Config) -> Self {
        let c = |f: &dyn Fn(&crate::config::Theme) -> &str| {
            let (r, g, b) = cfg.color(f);
            COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32) // COLORREF = 0x00BBGGRR
        };
        Self {
            labels: Vec::new(),
            active: 0,
            bg: c(&|t| &t.tabbar_background),
            active_bg: c(&|t| &t.tab_active),
            inactive_bg: c(&|t| &t.tab_inactive),
            text: c(&|t| &t.tab_text),
            text_active: c(&|t| &t.tab_text_active),
        }
    }
}

/// Create the tab bar child window. Returns its HWND.
pub fn create(parent: HWND, cfg: &Config) -> HWND {
    unsafe {
        register_class();
        let state = Box::into_raw(Box::new(State::new(cfg)));
        let hinstance = HINSTANCE(GetModuleHandleW(None).unwrap().0);
        CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            w!(""),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            800,
            28,
            parent,
            HMENU::default(),
            hinstance,
            Some(state as _),
        )
        .expect("tabbar create failed")
    }
}

/// Push a new snapshot into the tab bar and repaint.
pub fn update(hwnd: HWND, labels: &[String], active: usize, cfg: &Config) {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return;
        }
        // Mutate in place: the allocation is owned by GWLP_USERDATA, not by us.
        let st = &mut *ptr;
        let colors = State::new(cfg);
        st.labels = labels.to_vec();
        st.active = active;
        st.bg = colors.bg;
        st.active_bg = colors.active_bg;
        st.inactive_bg = colors.inactive_bg;
        st.text = colors.text;
        st.text_active = colors.text_active;
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe fn state_from(hwnd: HWND) -> &'static mut State {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    &mut *ptr
}

fn register_class() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: CLASS_NAME,
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            tracing::error!("tabbar class registration failed");
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_CREATE => {
            // Receive the State pointer passed via CreateWindowExW lpParam and
            // store it in GWLP_USERDATA. Without this, WM_PAINT would deref null.
            let cs = lp.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            let state = (*cs).lpCreateParams as *mut State;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as _);
            LRESULT(0)
        }
        WM_PAINT => {
            let st = state_from(hwnd);
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT {
                left: ps.rcPaint.left,
                top: ps.rcPaint.top,
                right: ps.rcPaint.right,
                bottom: ps.rcPaint.bottom,
            };
            let _ = FillRect(hdc, &rc, CreateSolidBrush(st.bg));

            let font: HFONT = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
            let old_font = SelectObject(hdc, windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
            let _ = SetBkMode(hdc, TRANSPARENT);

            for (i, label) in st.labels.iter().enumerate() {
                let x = i as i32 * (TAB_WIDTH + TAB_GAP);
                if x + TAB_WIDTH < ps.rcPaint.left || x > ps.rcPaint.right {
                    continue; // clip
                }
                let tab_rc = RECT {
                    left: x,
                    top: 3,
                    right: x + TAB_WIDTH,
                    bottom: TAB_HEIGHT,
                };
                let bg = if i == st.active { st.active_bg } else { st.inactive_bg };
                let _ = FillRect(hdc, &tab_rc, CreateSolidBrush(bg));

                // Close (X) button on the tab's right edge.
                let close_rc = RECT {
                    left: tab_rc.right - CLOSE_BOX - 4,
                    top: 6,
                    right: tab_rc.right - 4,
                    bottom: TAB_HEIGHT - 4,
                };
                let _ = DrawTextW(
                    hdc,
                    &mut to_wide("\u{00D7}"), // ×
                    &mut { RECT { ..close_rc } },
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );

                // Label text, clipped to leave room for the X button.
                let mut wide = to_wide(label);
                let mut text_rc = RECT {
                    left: tab_rc.left + 6,
                    right: close_rc.left,
                    ..tab_rc
                };
                let fg = if i == st.active { st.text_active } else { st.text };
                let _ = SetTextColor(hdc, fg);
                let _ = DrawTextW(hdc, &mut wide, &mut text_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }

            // Trailing "+" (new tab) button after the last tab.
            let n = st.labels.len() as i32;
            let plus_x = n * (TAB_WIDTH + TAB_GAP);
            let plus_rc = RECT {
                left: plus_x,
                top: 4,
                right: plus_x + PLUS_SIZE,
                bottom: TAB_HEIGHT,
            };
            let _ = DrawTextW(
                hdc,
                &mut to_wide("+"),
                &mut { RECT { ..plus_rc } },
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = windows::Win32::Graphics::Gdi::SelectObject(hdc, old_font);            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            match hit_test(lp, state_from(hwnd).labels.len()) {
                Some(Hit::Close(i)) => {
                    let parent = GetParent(hwnd).unwrap();
                    let _ = PostMessageW(parent, WM_APP_TAB_CLOSE, WPARAM(0), LPARAM(i as _));
                }
                Some(Hit::Plus) => {
                    let parent = GetParent(hwnd).unwrap();
                    let _ = PostMessageW(parent, WM_APP_TAB_NEW, WPARAM(0), LPARAM(0));
                }
                Some(Hit::Tab(i)) => {
                    let parent = GetParent(hwnd).unwrap();
                    let _ = PostMessageW(parent, WM_APP_TAB_ACTIVATED, WPARAM(0), LPARAM(i as _));
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_MBUTTONUP => {
            if let Some(Hit::Tab(i)) = hit_test(lp, state_from(hwnd).labels.len()) {
                let parent = GetParent(hwnd).unwrap();
                let _ = PostMessageW(parent, WM_APP_TAB_CLOSE, WPARAM(0), LPARAM(i as _));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// lParam of WM_*BUTTON*: x/y client coords -> hit region.
fn hit_test(lp: LPARAM, tab_count: usize) -> Option<Hit> {
    let x = (lp.0 as u16) as i32; // loword
    let y = ((lp.0 >> 16) as u16) as i32; // hiword
    if x < 0 {
        return None;
    }
    let n = tab_count as i32;
    // Trailing "+" button.
    let plus_x = n * (TAB_WIDTH + TAB_GAP);
    if x >= plus_x && x <= plus_x + PLUS_SIZE && y >= 0 && y <= TABBAR_Y + TAB_HEIGHT {
        return Some(Hit::Plus);
    }
    let i = x / (TAB_WIDTH + TAB_GAP);
    if i >= n {
        return None;
    }
    // Close box occupies the tab's right edge.
    let tab_left = i * (TAB_WIDTH + TAB_GAP);
    let close_left = tab_left + TAB_WIDTH - CLOSE_BOX - 4;
    if x >= close_left && y >= 4 && y <= TAB_HEIGHT - 2 {
        Some(Hit::Close(i as usize))
    } else {
        Some(Hit::Tab(i as usize))
    }
}

const TABBAR_Y: i32 = 3;

fn to_wide(s: &str) -> Vec<u16> {
    windows::core::HSTRING::from(s).as_wide().to_vec()
}
