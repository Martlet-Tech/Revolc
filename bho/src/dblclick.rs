//! Double-click on the empty file-list area navigates to the parent folder.
//!
//! Ported from Clover's TabHelper64.dll (REA evidence:
//! `docs/Clover-双击空白返回上级-调研.md`, functions 0x18000e080 / 0x18000de60 /
//! 0x18000e250). Two file-list flavours exist:
//!
//! * `SysListView32` — sends a real `WM_LBUTTONDBLCLK`; empty space is
//!   `LVM_HITTEST` returning no item.
//! * `DirectUIHWND` — Win10 22H2 (and Win11). It never delivers
//!   `WM_LBUTTONDBLCLK`, so Clover synthesises one from `WM_LBUTTONDOWN`
//!   timing (`GetDoubleClickTime` + 5 px) and tests emptiness with
//!   UIAutomation: the element under the point has class name
//!   `UIItemsView` or `UIGroupItem` (group headers count as empty).
//!
//! Both flavours end the same way: `WM_COMMAND 0xA022` ("Up one folder",
//! FCIDM_SHVIEW_UPONELEVEL) to `ShellTabWindowClass`.
//!
//! Revolc's BHO must never touch the site object (crash loop, see lib.rs),
//! so we cannot use `IWebBrowser2::get_HWND` like Clover does. Instead we
//! scan all windows owned by *this* process: after Revolc absorbs a folder
//! window it does `SetParent` + `WS_CHILD`, so the frame is no longer a
//! top-level window and `EnumThreadWindows` would miss it. We walk
//! `EnumWindows` -> `EnumChildWindows` and filter by owning PID.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, IUIAutomationElement};
use windows::Win32::UI::Controls::{LVM_HITTEST, LVHITTESTINFO};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, FindWindowExW, GetClassNameW, GetParent, GetWindowThreadProcessId,
    SendMessageW, SetTimer, WM_COMMAND, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_NCDESTROY,
};

/// FCIDM_BROWSER command "Up one folder" (Clover sends exactly 0xA022).
const UP_ONE_LEVEL: WPARAM = WPARAM(0xA022);
/// Rescan interval. Cheap, and catches file lists created after `SetSite`.
const SCAN_MS: u32 = 750;
/// Clover's synthetic double-click tolerance (TabHelper64 0x18000e080).
const DBLCLICK_SLACK: i32 = 5;

/// Per-view state, leaked as the subclass `dwRefData`. Lives on Explorer's
/// UI thread only.
struct Ctx {
    /// Where `WM_COMMAND 0xA022` goes: the `ShellTabWindowClass` ancestor
    /// when present, else the `CabinetWClass` ancestor.
    target: HWND,
    /// The subclassed view window (SysListView32 or DirectUIHWND).
    view: HWND,
    /// `true` = SysListView32 (`LVM_HITTEST`), `false` = DirectUIHWND (UIA).
    syslist: bool,
    /// Cached UIAutomation COM pointer for the DirectUI path.
    uia: Option<IUIAutomation>,
    /// Synthetic double-click tracker for the DirectUI path.
    last_down_tick: u32,
    last_down_pt: POINT,
}

/// HWND values we already subclassed, so the periodic scan is idempotent.
static ATTACHED: OnceLock<Mutex<HashSet<isize>>> = OnceLock::new();
/// One timer per Explorer process.
static TIMER_ARMED: OnceLock<()> = OnceLock::new();

/// Called from `SetSite`. Schedule the scan once; Explorer's UI thread has a
/// message loop, so the timer callback will run there.
pub fn arm() {
    if crate::revolc_disabled() {
        return;
    }
    crate::bho_log("dblclick: arm()");
    // Synchronous first pass: catch file lists already up.
    scan();
    // Timer fallback for file lists created later on the UI thread.
    TIMER_ARMED.get_or_init(|| unsafe {
        let _ = SetTimer(HWND::default(), 0, SCAN_MS, Some(timer_proc));
    });
}

unsafe extern "system" fn timer_proc(_hwnd: HWND, _msg: u32, _id: usize, _tick: u32) {
    scan();
}

fn scan() {
    if crate::revolc_disabled() {
        return;
    }
    unsafe {
        // Walk every top-level window, then recurse into children. Revolc
        // re-parents absorbed frames under its own window (WS_CHILD), so
        // they never show up in EnumThreadWindows / EnumWindows directly.
        let _ = EnumWindows(Some(enum_top), LPARAM(0));
    }
}

unsafe extern "system" fn enum_top(top: HWND, _lp: LPARAM) -> BOOL {
    inspect_window(top);
    let _ = EnumChildWindows(top, Some(enum_child), LPARAM(0));
    BOOL(1) // keep enumerating
}

unsafe extern "system" fn enum_child(child: HWND, _lp: LPARAM) -> BOOL {
    inspect_window(child);
    BOOL(1) // keep enumerating
}

/// If `hwnd` is the shell's file-list view owned by our process, attach.
///
/// `EnumChildWindows` visits *every* descendant already, so no depth-limited
/// `FindWindowExW` search: the DefView sits 4 levels deep
/// (`CabinetWClass -> ShellTabWindowClass -> DirectUIHWND -> SHELLDLL_DefView`).
unsafe fn inspect_window(hwnd: HWND) {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
    if class != "SHELLDLL_DefView" {
        return;
    }

    // Only touch windows in OUR process. Cross-process subclass is invalid.
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != GetCurrentProcessId() {
        return;
    }

    attach_on_defview(hwnd);
}

/// Walk up from the DefView to find the command target: the
/// `ShellTabWindowClass` ancestor when present, else the closest
/// `CabinetWClass` ancestor.
unsafe fn command_target_for(defview: HWND) -> Option<HWND> {
    let mut cur = GetParent(defview).unwrap_or_default();
    let mut fallback = None;
    while !cur.0.is_null() {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(cur, &mut buf).max(0) as usize;
        let class = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        match class.as_str() {
            "ShellTabWindowClass" => return Some(cur),
            "CabinetWClass" | "ExploreWClass" => fallback = Some(cur),
            _ => {}
        }
        cur = GetParent(cur).unwrap_or_default();
    }
    fallback
}

unsafe fn attach_on_defview(defview: HWND) {
    // Clover 0x18000e250: SysListView32 first, DirectUIHWND as fallback.
    let (view, syslist) = {
        let lv = find_descendant(defview, w!("SysListView32"));
        if !lv.0.is_null() {
            (lv, true)
        } else {
            let dui = find_descendant(defview, w!("DirectUIHWND"));
            if dui.0.is_null() {
                crate::bho_log(&format!(
                    "dblclick: no file-list view under DefView {:?}",
                    defview.0
                ));
                return;
            }
            (dui, false)
        }
    };

    // Command target, mirroring Clover: ShellTabWindowClass when present,
    // else the frame ancestor.
    let Some(target) = command_target_for(defview) else {
        return;
    };

    let set = ATTACHED.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut guard) = set.lock() else { return };
    if !guard.insert(view.0 as isize) {
        return; // already subclassed
    }
    drop(guard);

    let ctx = Box::new(Ctx {
        target,
        view,
        syslist,
        uia: None,
        last_down_tick: 0,
        last_down_pt: POINT::default(),
    });
    let ok = SetWindowSubclass(
        view,
        Some(listview_subclass),
        0,
        Box::into_raw(ctx) as usize, // dwRefData
    );
    if ok.as_bool() {
        crate::bho_log(&format!(
            "dblclick: SUBCLASSED {}={:?} target={:?} defview={:?}",
            if syslist { "SysListView32" } else { "DirectUIHWND" },
            view.0,
            target.0,
            defview.0
        ));
    } else {
        // Roll back so a later scan can retry.
        if let Ok(mut guard) = set.lock() {
            guard.remove(&(view.0 as isize));
        }
    }
}

/// Depth-first search for the first descendant with the given class.
unsafe fn find_descendant(parent: HWND, class: PCWSTR) -> HWND {
    let direct = FindWindowExW(parent, HWND::default(), class, PCWSTR::null()).unwrap_or_default();
    if !direct.0.is_null() {
        return direct;
    }
    // One more level (DefView sometimes sits under a WorkerW sibling).
    let mut child = FindWindowExW(parent, HWND::default(), PCWSTR::null(), PCWSTR::null())
        .unwrap_or_default();
    while !child.0.is_null() {
        let found = FindWindowExW(child, HWND::default(), class, PCWSTR::null()).unwrap_or_default();
        if !found.0.is_null() {
            return found;
        }
        child = FindWindowExW(parent, child, PCWSTR::null(), PCWSTR::null()).unwrap_or_default();
    }
    HWND::default()
}

/// Empty-space test, mirroring Clover `FUN_18000de60`.
unsafe fn point_on_empty_space(ctx: &Ctx, pt: POINT) -> bool {
    if ctx.syslist {
        let mut hit = LVHITTESTINFO {
            pt,
            ..Default::default()
        };
        let _ = SendMessageW(
            ctx.view,
            LVM_HITTEST,
            WPARAM(0),
            LPARAM(&mut hit as *mut LVHITTESTINFO as isize),
        );
        hit.iItem < 0
    } else {
        // UIA: the element under the point must be the list background
        // ("UIItemsView") or a group header ("UIGroupItem").
        let Some(uia) = &ctx.uia else { return false };
        let mut sp = pt;
        let _ = ClientToScreen(ctx.view, &mut sp);
        let Ok(el) = uia.ElementFromPoint(sp) else {
            return false;
        };
        element_is_empty_background(&el)
    }
}

fn element_is_empty_background(el: &IUIAutomationElement) -> bool {
    let name = unsafe { el.CurrentClassName() };
    let Ok(name) = name else {
        return false;
    };
    name == "UIItemsView" || name == "UIGroupItem"
}

/// Obtain (and cache) the UIAutomation pointer for a DirectUI view.
unsafe fn ensure_uia(ctx: &mut Ctx) {
    if ctx.uia.is_none() {
        match CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
            Ok(uia) => ctx.uia = Some(uia),
            Err(e) => crate::bho_log(&format!("dblclick: UIA init failed: {e}")),
        }
    }
}

unsafe extern "system" fn listview_subclass(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        // DefSubclassProc handles subclass teardown; drop our context here
        // and forget the HWND so a recycled handle can be re-subclassed.
        if refdata != 0 {
            drop(Box::from_raw(refdata as *mut Ctx));
        }
        if let Ok(mut guard) = ATTACHED.get_or_init(|| Mutex::new(HashSet::new())).lock() {
            guard.remove(&(hwnd.0 as isize));
        }
        return DefSubclassProc(hwnd, msg, wp, lp);
    }

    if refdata == 0 {
        return DefSubclassProc(hwnd, msg, wp, lp);
    }
    let ctx = &mut *(refdata as *mut Ctx);

    // DirectUI never sends WM_LBUTTONDBLCLK: synthesise one from the
    // WM_LBUTTONDOWN cadence, exactly like Clover 0x18000e080's 0x201 arm.
    if msg == WM_LBUTTONDOWN && !ctx.syslist {
        let result = DefSubclassProc(hwnd, msg, wp, lp);
        let x = (lp.0 as u32 & 0xffff) as i16 as i32;
        let y = ((lp.0 as u64 >> 16) & 0xffff) as i16 as i32;
        let tick = unsafe { GetTickCount() };
        // GetTickCount wraps; wrapping_sub keeps the window sane.
        let too_slow = unsafe { GetDoubleClickTime() } < tick.wrapping_sub(ctx.last_down_tick);
        let pt = POINT { x, y };
        if too_slow {
            // First of a potential pair: remember this press.
            ctx.last_down_tick = tick;
            ctx.last_down_pt = pt;
        } else {
            let dx = x - ctx.last_down_pt.x;
            let dy = y - ctx.last_down_pt.y;
            ctx.last_down_tick = 0;
            if dx.abs() < DBLCLICK_SLACK && dy.abs() < DBLCLICK_SLACK {
                unsafe { ensure_uia(ctx) };
                if unsafe { point_on_empty_space(ctx, pt) } {
                    send_up_one_level(ctx, x, y);
                }
            }
        }
        return result;
    }

    // Give Explorer its normal handling first (selection etc.), like Clover.
    let result = DefSubclassProc(hwnd, msg, wp, lp);

    if msg == WM_LBUTTONDBLCLK {
        let x = (lp.0 as u32 & 0xffff) as i16 as i32;
        let y = ((lp.0 as u64 >> 16) & 0xffff) as i16 as i32;
        let pt = POINT { x, y };
        if !ctx.syslist {
            unsafe { ensure_uia(ctx) };
        }
        if unsafe { point_on_empty_space(ctx, pt) } {
            send_up_one_level(ctx, x, y);
        }
    }
    result
}

unsafe fn send_up_one_level(ctx: &Ctx, x: i32, y: i32) {
    crate::bho_log(&format!(
        "dblclick: EMPTY ({x},{y}) -> WM_COMMAND 0xA022 to {:?}",
        ctx.target.0
    ));
    let _ = SendMessageW(ctx.target, WM_COMMAND, UP_ONE_LEVEL, LPARAM(0));
}
