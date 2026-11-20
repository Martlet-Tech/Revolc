pub mod caption;
pub mod dwm;
pub mod tabbar;

pub use revolc_common::icon;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
    DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetMonitorInfoW, GetStockObject,
    InvalidateRect, MonitorFromWindow, RedrawWindow, ScreenToClient, SelectObject, SetBkMode,
    SetTextColor, UpdateWindow, HBRUSH, HBITMAP, HDC, HGDIOBJ, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE,
    RDW_UPDATENOW, SRCCOPY, TRANSPARENT, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateAcceleratorTableW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyAcceleratorTable, DestroyMenu, DestroyWindow, DispatchMessageW, EnumWindows,
    GetAncestor, GetClientRect, GetParent, GetMessageW, GetClassNameW, GetSystemMetrics,
    GetWindowLongPtrW, IsIconic, IsWindow, IsWindowVisible, IsZoomed, LoadCursorW, MessageBoxW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TrackPopupMenuEx,
    TranslateAcceleratorW, TranslateMessage, ACCEL, CREATESTRUCTW, CS_DBLCLKS, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GA_ROOT, GWLP_USERDATA, HACCEL,
    HTCAPTION, HTCLIENT, HTBOTTOM,
    HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, HWND_TOP,
    ICON_BIG, ICON_SMALL, IDC_ARROW, MF_SEPARATOR, MF_STRING, MSG, SW_HIDE, SW_RESTORE, SW_SHOW,
    SW_MINIMIZE, SWP_NOACTIVATE, SW_SHOWMAXIMIZED, SW_SHOWNORMAL, SWP_NOZORDER, TPM_RETURNCMD,
    WM_CAPTURECHANGED,
    WM_CLOSE, WM_CREATE, WM_DESTROY, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOVE, WM_MBUTTONUP, WM_NCACTIVATE,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN,
    WM_NCMOUSELEAVE,     WM_PAINT, WM_RBUTTONUP, WM_COPYDATA,
    WM_SETICON, WM_SIZE, WM_TIMER, IDOK, MB_ICONQUESTION, MB_OKCANCEL, MB_SETFOREGROUND,
    MINMAXINFO, SM_CXDRAG, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME, WNDCLASSEXW,
};

use crate::config::ConfigHandle;
use crate::shell::host::{self, Absorbed};
use crate::tabs::events::{self, Action, Binding};
use crate::tabs::TabList;
use revolc_common::ledger::Ledger;

pub const WM_APP_CONFIG_CHANGED: u32 = 0x8000; // posted by config-watcher thread
/// Quit for real: restore every absorbed window and exit. WM_CLOSE only hides
/// to the tray in redirect mode, so shutdown paths (tray is direct, console
/// handler and `--shutdown` post this) need their own message.
pub const WM_APP_QUIT: u32 = 0x8010;
/// WM_COPYDATA tag from the BHO for navigation events ("RNAV"). Payload is
/// UTF-8 "HWND<TAB>url"; must match NAV_COPYDATA_TAG in bho/src/nav.rs.
const NAV_COPYDATA_TAG: usize = 0x524E4156;
/// wParam = Explorer top-level HWND. Sent by the BHO via the registered
/// window message "RevolcAbsorbExplorer" (system-assigned id, identical
/// in every process of the session).
pub(crate) fn absorb_msg() -> u32 {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *ID.get_or_init(|| unsafe {
        windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(w!("RevolcAbsorbExplorer"))
    })
}

const TABBAR_HEIGHT: i32 = caption::HEIGHT;
/// Frame between the window edge and the Explorer view. The view is inset by
/// this much so a bare strip of *our* window is left along the left, right and
/// bottom edges — that is the only place WM_NCHITTEST reaches us (a pointer on
/// the child pane gets HTCLIENT from the child, never a resize code from us).
/// 1px was invisible and unreachable; 8px reads as a frame and is comfortable
/// to grab. Painted by `paint_frame`, hit-tested by the BORDER arm below.
/// Maximized windows use `border()` = 0 instead, to stay flush with rcWork.
const WINDOW_BORDER: i32 = 8;
/// Top resize strip stays thinner than `TABBAR_HEIGHT`: the tab strip needs its
/// own top rows for tabs, and `TAB_TOP` starts at 4. Growing this past TAB_TOP
/// would steal the top edge of every tab for resizing.
const TOP_BORDER: i32 = 4;
const CLASS_NAME: PCWSTR = w!("RevolcMain");
/// Owned sibling of every absorbed Explorer pane. Painted and hit-tested by
/// us, and always raised above those panes, so a foreign process can never
/// erase our title bar again.
const TABBAR_CLASS: PCWSTR = w!("RevolcOwnedTabBar");
/// Owned popup for the bookmark bar, sitting between the tab strip and the
/// absorbed Explorer pane (feat/tab-context-menu phase 2).
const BOOKMARK_CLASS: PCWSTR = w!("RevolcOwnedBookmarkBar");
const BOOKMARK_HEIGHT: i32 = 28;
/// Context-menu command IDs. Kept in the WM_USER range so they cannot collide
/// with accelerators (which use 1..=keymap.len()).
const CMD_TAB_NEW_WINDOW: usize = 0x8001;
const CMD_TAB_CLOSE: usize = 0x8002;
const CMD_TAB_CLOSE_OTHERS: usize = 0x8003;
const CMD_TAB_CLOSE_RIGHT: usize = 0x8004;
const CMD_TAB_NEW_AFTER: usize = 0x8005;

/// The window the user is working in right now (0 = none yet).
///
/// Phase 2: one `revolc.exe` per window, so this lives in a shared file
/// mapping written by `revolc-tray.exe`'s foreground hook (and echoed by each
/// window on activation for the no-tray case). Every window's sweep asks the
/// ledger "whose folder is this" instead of racing its own timer.
static LEDGER: OnceLock<Ledger> = OnceLock::new();

fn ledger() -> Option<&'static Ledger> {
    if LEDGER.get().is_none() {
        let l = match Ledger::open_or_create() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "shared ledger unavailable");
                return None;
            }
        };
        let _ = LEDGER.set(l);
    }
    LEDGER.get()
}

/// Cross-process lock held from "enumerate stray folders" through the last
/// SetParent. Without it two window processes can pick the same Explorer
/// HWND in the same few ms and both re-parent it (2026-11-20 double-absorb
/// → ntdll crash on teardown). Abandoned means the previous owner died;
/// take over rather than skipping forever.
struct SweepLock(HANDLE);

impl SweepLock {
    /// Try, never wait. Sweeps are frequent (15ms burst / 200ms idle); a
    /// contended tick can simply be skipped. Blocking the UI thread here
    /// would freeze dragging while another process is mid-SetParent.
    fn acquire() -> Option<Self> {
        unsafe {
            let h = CreateMutexW(None, false, w!("Local\\RevolcSweepLock")).ok()?;
            // WAIT_OBJECT_0 = 0, WAIT_ABANDONED = 0x80, WAIT_TIMEOUT = 0x102.
            let r = WaitForSingleObject(h, 0).0;
            if r == 0 || r == 0x80 {
                Some(Self(h))
            } else {
                let _ = CloseHandle(h);
                None
            }
        }
    }
}

impl Drop for SweepLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// All RevolcMain windows on the desktop, across processes.
fn all_main_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_main_window),
            LPARAM(&mut out as *mut Vec<HWND> as isize),
        );
    }
    out
}

unsafe extern "system" fn enum_main_window(hwnd: HWND, lp: LPARAM) -> windows::Win32::Foundation::BOOL {
    let out = &mut *(lp.0 as *mut Vec<HWND>);
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    if n > 0 && String::from_utf16_lossy(&cls[..n as usize]) == "RevolcMain" {
        out.push(hwnd);
    }
    windows::Win32::Foundation::BOOL(1)
}

/// Windows alive in this process. Closing the last one is what quits the
/// loop; closing any other one is just closing a window.
static LIVE_WINDOWS: AtomicUsize = AtomicUsize::new(0);

/// HWNDs released back to the desktop by a *dying* window (`release_all`).
///
/// Per-window `excluded` lists cannot see this: the window that released the
/// Explorer panes is being destroyed, and the surviving windows' sweeps would
/// immediately re-adopt its panes ("FE 被吸走"). This table is process-global
/// and every sweep merges it in; entries expire after the same EXCLUDE_TTL.
static RELEASED_WINDOWS: std::sync::Mutex<Vec<(isize, Instant)>> =
    std::sync::Mutex::new(Vec::new());

/// Park an HWND we are deliberately letting go of (closed tab, dying window,
/// failed absorb) in the process-global table so **no** window's sweep picks
/// it back up until it expires.
fn park_released(top: HWND) {
    if let Ok(mut g) = RELEASED_WINDOWS.lock() {
        g.push((top.0 as isize, Instant::now()));
    }
}

/// Stray-window sweep. The BHO notify is a hint, not a guarantee: SetSite
/// fires before the frame is shown and some folder windows never notify at
/// all, so the timer has to exist. But it does not have to be one constant
/// rate — an EnumWindows pass plus one SetParent per hit is the whole cost:
///
/// * idle:    one pass every 200ms — the safety net for windows whose
///            notification was lost (BHO not loaded, notify posted while we
///            were busy). One EnumWindows pass over a few hundred windows is
///            sub-millisecond, so this is well under 1% CPU.
/// * burst:   15ms while a folder is expected (right after a notify or after
///            we spawned Explorer ourselves), so the absorb lands before the
///            FE window is even painted in the common case.
const SWEEP_TIMER_ID: usize = 0x5EED;
const SWEEP_IDLE_MS: u32 = 200;
const SWEEP_FAST_MS: u32 = 15;
/// How long a notify keeps us in fast mode.
const BURST_WINDOW: Duration = Duration::from_millis(1500);
/// A window we just restored (tab closed) stays parked this long so the
/// sweep does not grab it back while Explorer finishes destroying it.
const EXCLUDE_TTL: Duration = Duration::from_secs(8);
/// A tab whose Explorer window never arrived is dropped after this. Explorer
/// can refuse to spawn (no shell running at that moment), the spawn can fail,
/// and a slow network path can take seconds — but a reserved tab that never
/// fills is not cosmetic: while any tab has no view, the sweep treats *every*
/// hidden folder window as "the one we are waiting for" and steals it.
const RESERVE_TTL: Duration = Duration::from_secs(6);
/// Hard cap: a runaway explorer.exe storm must not eat the desktop.
const MAX_TABS: usize = 40;

struct WindowArgs {
    cfg: Arc<ConfigHandle>,
    initial_path: Option<String>,
    /// Not a preference: built per window by the creator; the loop translates
    /// with the table of whichever window owns the focused control, so this
    /// cannot be shared.
    accel: HACCEL,
}

/// A left button pressed on a tab, before the pointer has moved far enough to
/// call it a drag. The tab is identified by id, not index: a sweep can reorder
/// or remove tabs between the press and the first mouse move.
#[derive(Clone, Copy)]
struct TabPress {
    id: u64,
    /// Pointer x when the button went down (client coords).
    x: i32,
}

pub(crate) struct App {
    cfg: Arc<ConfigHandle>,
    keymap: Vec<(Binding, Action)>,
    /// Accelerator table for **this** window. `TranslateAcceleratorW` posts the
    /// resulting WM_COMMAND to the HWND it is handed, so the table cannot live
    /// on the frame: with two windows, the second one's Ctrl+T would be
    /// delivered to the first one's tab list.
    accel: HACCEL,
    tabs: TabList,
    views: Vec<Option<Absorbed>>,
    /// HWNDs we deliberately left on the desktop (closed tabs), with the
    /// instant they were released. Sweep ignores them until EXCLUDE_TTL.
    excluded: Vec<(isize, Instant)>,
    /// Reentrancy guard: absorb moves windows around, which can re-enter
    /// this app's message handlers.
    sweeping: bool,
    /// Fast-poll window: a folder is expected until this instant.
    burst_until: Option<Instant>,
    /// Explorer thread whose input queue we are currently attached to (0 =
    /// none). Attached only while our window is the active window.
    attached_tid: u32,
    /// Our window is the foreground/active window.
    active: bool,
    /// The active tab's window was absorbed *before* Explorer showed it (the
    /// flash-free path), so SetFocus could not land yet. Retried by the sweep
    /// the moment it becomes visible.
    focus_pending: bool,
    /// HWNDs whose folder path we already tried to resolve. A window adopted
    /// before it navigated has no LocationURL yet; without this the sweep
    /// would ask IShellWindows (out-of-proc, 50ms) on every pass.
    path_done: Vec<isize>,
    /// When the oldest still-empty tab was reserved (`new_tab`). Used to drop
    /// tabs whose window never showed up — see RESERVE_TTL.
    reserve_deadline: Option<Instant>,
    /// Windows that belong to a tab the user closed *before* its Explorer
    /// window was absorbed. We spawned that window; adopting it later would
    /// make the closed tab reappear. Matched by title, with a timestamp.
    discard: Vec<(String, Instant)>,
    /// (tab index, l, t, r, b, measured placement metrics) of the last
    /// geometry pushed to an absorbed window. Guards enforce_views() against
    /// re-issuing a cross-process SetWindowPos on every 200ms tick — but the
    /// measured metrics are part of the key: Explorer changes them on
    /// maximize/restore/navigation without the view rect changing.
    laid_out: Option<(usize, i32, i32, i32, i32, ((i32, i32, i32, i32), i32))>,
    /// Client size the current layout was computed for. WM_SIZE is
    /// authoritative, but a resize can race the cross-process re-parent (or be
    /// swallowed while our queue is attached to Explorer's); the sweep
    /// compares this and re-lays out instead of leaving the view at its old
    /// size under a newly sized host.
    last_client: (i32, i32),
    /// The title bar: painted here, hit-tested here (see ui/tabbar.rs).
    tabbar: tabbar::TabBar,
    /// Real child window that owns the strip's pixels and hits. Kept above
    /// every Explorer pane so their overhang cannot cover it.
    tabbar_hwnd: HWND,
    /// Owned bookmark-bar popup below the tab strip (may be null when disabled).
    bookmarkbar_hwnd: HWND,
    main_hwnd: HWND,
    /// TME_LEAVE/TME_NONCLIENT registered: the leave message arrives once per
    /// run, and it is the only way to drop a hover that the cursor walked out
    /// of without a further hit test.
    hover_tracking: bool,
    /// Left button went down on a tab and the pointer has not yet crossed the
    /// system drag threshold. `None` once a click is dispatched or a drag
    /// starts.
    tab_press: Option<TabPress>,
}

pub struct MainWindow {
    hwnd: HWND,
}

impl MainWindow {
    /// The single window owned by this `revolc.exe` process.
    pub fn new(cfg: Arc<ConfigHandle>, initial_path: Option<String>) -> windows::core::Result<Self> {
        Self::create(cfg, initial_path)
    }

    fn create(
        cfg: Arc<ConfigHandle>,
        initial_path: Option<String>,
    ) -> windows::core::Result<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            ensure_class(hinstance.into())?;

            // Accelerator table from the event-driven keymap
            // (event-driven, no GetAsyncKeyState polling). Built before the
            // window so WM_CREATE can hand it to the per-window App.
            let mut accels: Vec<ACCEL> = Vec::new();
            for (i, (b, _)) in events::default_keymap().iter().enumerate() {
                accels.push(ACCEL {
                    fVirt: windows::Win32::UI::WindowsAndMessaging::ACCEL_VIRT_FLAGS(b.fvirt()),
                    key: b.vk,
                    cmd: (i as u16) + 1,
                });
            }
            let accel = CreateAcceleratorTableW(&accels)?;

            let args = Box::into_raw(Box::new(WindowArgs {
                cfg,
                initial_path,
                accel,
            }));

            let hwnd = CreateWindowExW(
                Default::default(),
                CLASS_NAME,
                w!("Revolc"),
                // Chrome-style integrated title bar (spec §4.2.1): no
                // WS_CAPTION at all. WS_THICKFRAME is kept only for the
                // resize/snap behaviour and the DWM shadow — WM_NCCALCSIZE
                // hands the entire frame to us, so the frame is invisible.
                // WS_CLIPCHILDREN: erasing the window must not paint over the
                // absorbed Explorer window (the strip is ours to paint).
                WS_POPUP
                    | WS_THICKFRAME
                    | WS_MINIMIZEBOX
                    | WS_MAXIMIZEBOX
                    | WS_SYSMENU
                    | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1000,
                700,
                HWND::default(),
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                hinstance,
                Some(args as _),
            )?;

            // Frameless windows get square corners from DWM on Win11 unless
            // asked otherwise; every native app here is rounded.
            dwm::round_corners(hwnd);

            // Allow the BHO notify message through UIPI: when Revolc runs
            // elevated (launched from an admin terminal), Explorer's lower-IL
            // PostMessageW would be silently dropped (observed err 5, 10:36).
            let m = absorb_msg();
            let _ = windows::Win32::UI::WindowsAndMessaging::ChangeWindowMessageFilterEx(
                hwnd, m, windows::Win32::UI::WindowsAndMessaging::MSGFLT_ALLOW, None,
            );

            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);

            LIVE_WINDOWS.fetch_add(1, Ordering::SeqCst);
            Ok(Self { hwnd })
        }
    }

    pub fn hwnd_raw(&self) -> usize {
        self.hwnd.0 as usize
    }
}

/// Register the window class, once.
///
/// `RegisterClassExW` fails with `ERROR_CLASS_ALREADY_EXISTS` for the second
/// window; treating that as fatal is what kept this process single-window.
fn ensure_class(hinstance: windows::Win32::Foundation::HINSTANCE) -> windows::core::Result<()> {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        // Class icons: what the title bar and Alt-Tab read before the window
        // has painted or handled WM_SETICON.
        hIcon: icon::big(),
        hIconSm: icon::small(),
        lpszClassName: CLASS_NAME,
        // CS_DBLCLKS: double-click on the empty strip toggles maximize, which
        // only works if the class receives WM_LBUTTONDBLCLK.
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } != 0 {
        return Ok(());
    }
    let err = windows::core::Error::from_win32();
    // 1410 = ERROR_CLASS_ALREADY_EXISTS. The class is per module, so this only
    // means "a window of ours was created before".
    if (err.code().0 as u32 & 0xffff) == 1410 {
        return Ok(());
    }
    Err(err)
}

/// Class for the owned strip. Registered once per process, same tolerate-
/// already-exists rule as the main class (multiple windows are normal now).
fn ensure_tabbar_class() {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tabbar_wndproc),
        hInstance: unsafe { GetModuleHandleW(None) }.unwrap_or_default().into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        lpszClassName: TABBAR_CLASS,
        style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } != 0 {
        return;
    }
    let err = windows::core::Error::from_win32();
    if (err.code().0 as u32 & 0xffff) != 1410 {
        tracing::warn!(error = %err, "register tab bar class failed");
    }
}

/// Class for the bookmark bar popup. Same tolerates-already-exists rule.
fn ensure_bookmark_class() {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(bookmark_wndproc),
        hInstance: unsafe { GetModuleHandleW(None) }.unwrap_or_default().into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        lpszClassName: BOOKMARK_CLASS,
        // No CS_HREDRAW/VREDRAW: SetWindowPos in every sync would otherwise
        // invalidate the whole strip on each move/resize. Content is static
        // in phase 2; phase 3 will double-buffer it when items go in.
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } != 0 {
        return;
    }
    let err = windows::core::Error::from_win32();
    if (err.code().0 as u32 & 0xffff) != 1410 {
        tracing::warn!(error = %err, "register bookmark bar class failed");
    }
}

/// The owned strip's window procedure. Painting stays here; every input and
/// hit-test message is forwarded to the frame, which already knows how to
/// turn a client coordinate into a tab / button / drag action. Because the
/// strip is a sibling above the Explorer panes, nothing foreign can paint
/// over it or steal its clicks.
unsafe extern "system" fn tabbar_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    let parent = GetParent(hwnd).unwrap_or_default();
    let app = if parent.0.is_null() {
        None
    } else {
        app_opt(parent)
    };
    match msg {
        WM_PAINT => {
            let Some(app) = app else { return DefWindowProcW(hwnd, msg, wp, lp) };
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            app.paint_band(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => LRESULT(0),
        WM_NCHITTEST => {
            // Let the frame decide: resize borders, caption drag, tabs and
            // buttons all already implemented there.
            if parent.0.is_null() {
                LRESULT(HTCLIENT as isize)
            } else {
                SendMessageW(parent, WM_NCHITTEST, wp, lp)
            }
        }
        WM_MOUSEMOVE => {
            if let Some(app) = app {
                let (x, y) = point_from_lparam(lp);
                app.update_hover(x, y, app.client().right);
            }
            if parent.0.is_null() { LRESULT(0) } else { SendMessageW(parent, msg, wp, lp) }
        }
        WM_LBUTTONDOWN
        | WM_LBUTTONUP
        | WM_LBUTTONDBLCLK
        | WM_MBUTTONUP
        | WM_RBUTTONUP
        | WM_MOUSELEAVE
        | WM_NCMOUSELEAVE
        | WM_NCLBUTTONDOWN
        | WM_NCLBUTTONDBLCLK
        | WM_CAPTURECHANGED => {
            if parent.0.is_null() {
                LRESULT(0)
            } else {
                if msg == WM_LBUTTONDOWN || msg == WM_NCLBUTTONDOWN {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(parent);
                }
                SendMessageW(parent, msg, wp, lp)
            }
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Bookmark bar popup. Phase 2 is UI-only: paint a placeholder strip; clicks
/// are forwarded to the frame but do nothing until phase 3 wires in items.
unsafe extern "system" fn bookmark_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    let parent = GetParent(hwnd).unwrap_or_default();
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let cfg = app_opt(parent).map(|a| a.cfg.clone());
            let (r, g, b) = cfg
                .as_ref()
                .map(|c| c.get().color(|t| &t.bookmark_bar_background))
                .unwrap_or((0x20, 0x20, 0x20));
            let brush = CreateSolidBrush(COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32));
            let _ = FillRect(hdc, &rc, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            // Placeholder caption. Phase 3 replaces this with real bookmarks.
            let font: HFONT = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
            let old_font = SelectObject(hdc, HGDIOBJ(font.0));
            let _ = SetBkMode(hdc, TRANSPARENT);
            let tr = (r as i32 / 2 + 0x50).min(0xff) as u8;
            let tg = (g as i32 / 2 + 0x50).min(0xff) as u8;
            let tb = (b as i32 / 2 + 0x50).min(0xff) as u8;
            let _ = SetTextColor(hdc, COLORREF((tb as u32) << 16 | (tg as u32) << 8 | tr as u32));
            let mut text = RECT { left: 10, top: 0, right: rc.right - 10, bottom: rc.bottom };
            let mut label: Vec<u16> = "收藏夹".encode_utf16().chain(Some(0)).collect();
            let _ = DrawTextW(hdc, &mut label, &mut text, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            let _ = SelectObject(hdc, old_font);

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => LRESULT(0),
        // Clicks/hover belong to the frame until phase 3 gives items meaning.
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEMOVE => {
            if parent.0.is_null() {
                LRESULT(0)
            } else {
                SendMessageW(parent, msg, wp, lp)
            }
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The one message loop, shared by every window in the process.
///
/// `TranslateAcceleratorW` posts WM_COMMAND to the HWND it is given, so the
/// old "always translate against the first window" meant Ctrl+T pressed in a
/// second window edited the first window's tabs. Resolve the owner per message
/// instead: keyboard messages carry the *focused* HWND, which is very often an
/// absorbed Explorer child — `GetAncestor(GA_ROOT)` walks back to our frame.
pub fn run_message_loop() -> i32 {
    unsafe {
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                return msg.wParam.0 as i32;
            }
            if let Some(owner) = owner_window(msg.hwnd) {
                if let Some(app) = app_opt(owner) {
                    if TranslateAcceleratorW(owner, app.accel, &msg) != 0 {
                        continue;
                    }
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Owning Revolc frame of a (possibly deeply nested, possibly cross-process)
/// window.
fn owner_window(hwnd: HWND) -> Option<HWND> {
    if hwnd.0.is_null() {
        return None;
    }
    unsafe {
        let root = GetAncestor(hwnd, GA_ROOT);
        app_opt(root).is_some().then_some(root)
    }
}

/// Is any Revolc window on screen? Used as the last-resort fallback when the
/// ledger has no valid entry.
fn any_visible_window() -> bool {
    all_main_windows()
        .iter()
        .any(|h| unsafe { IsWindowVisible(*h).as_bool() })
}

unsafe fn app_opt(hwnd: HWND) -> Option<&'static mut App> {
    if hwnd.0.is_null() {
        return None;
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    (!ptr.is_null()).then(|| &mut *ptr)
}

unsafe fn app_from(hwnd: HWND) -> &'static mut App {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    &mut *ptr
}

impl App {
    fn build(hwnd: HWND, args: Box<WindowArgs>) -> Box<App> {
        let tabbar = tabbar::TabBar::new(&args.cfg.get());
        let mut app = Box::new(App {
            cfg: args.cfg,
            keymap: events::default_keymap(),
            accel: args.accel,
            tabs: TabList::new(),
            views: Vec::new(),
            excluded: Vec::new(),
            sweeping: false,
            burst_until: None,
            attached_tid: 0,
            active: true,
            focus_pending: false,
            path_done: Vec::new(),
            reserve_deadline: None,
            discard: Vec::new(),
            laid_out: None,
            last_client: (0, 0),
            tabbar,
            tabbar_hwnd: HWND::default(),
            bookmarkbar_hwnd: HWND::default(),
            main_hwnd: hwnd,
            hover_tracking: false,
            tab_press: None,
        });
        let first = args.initial_path.unwrap_or_else(home_dir);
        app.new_tab(first);
        app
    }

    /// Make sure the frame is on screen **without ever touching WS_MAXIMIZE**.
    ///
    /// SW_SHOWNORMAL and SW_RESTORE both mean "if maximized or minimized,
    /// restore to the original size and position", so calling either as a
    /// plain "make it visible" is destructive: absorbing a window (i.e. every
    /// `+` click) un-maximized Revolc. Only act when the frame is actually
    /// hidden or minimized, and pick a command that keeps the zoom state.
    fn surface(&self) {
        unsafe {
            if IsIconic(self.main_hwnd).as_bool() {
                // Minimized: restore returns to the pre-minimize state, which
                // is maximized if it was maximized.
                let _ = ShowWindow(self.main_hwnd, SW_RESTORE);
            } else if !IsWindowVisible(self.main_hwnd).as_bool() {
                let cmd = if IsZoomed(self.main_hwnd).as_bool() {
                    SW_SHOWMAXIMIZED
                } else {
                    SW_SHOW
                };
                let _ = ShowWindow(self.main_hwnd, cmd);
            }
            // Visible and not minimized: do nothing at all.
        }
        self.sync_tabbar_window();
    }

    /// Where the absorbed Explorer window goes: inside the hairline frame,
    /// below the tab strip and the (optional) bookmark bar.
    fn view_rect(&self) -> RECT {
        let rc = self.client();
        let b = self.border();
        let top = TABBAR_HEIGHT + self.bookmark_height();
        RECT {
            left: b,
            top,
            right: (rc.right - b).max(b),
            bottom: (rc.bottom - b).max(top),
        }
    }

    /// A maximized window sits flush against the work area; a hairline there
    /// only draws a stray dark line on the screen edge (Chrome does the same).
    fn border(&self) -> i32 {
        if self.is_maximized() {
            0
        } else {
            WINDOW_BORDER
        }
    }

    /// Frame around the client. The band is ours, and the view is inset, so
    /// nothing covers it. Doubles as the resize-grab strip.
    fn paint_frame(&self, hdc: HDC) {
        let b = self.border();
        if b == 0 {
            return;
        }
        let rc = self.client();
        let (r, g, bl) = self.cfg.get().color(|t| &t.window_border);
        let color = COLORREF((bl as u32) << 16 | (g as u32) << 8 | r as u32);
        let strips = [
            RECT { left: 0, top: 0, right: rc.right, bottom: b },
            RECT { left: 0, top: rc.bottom - b, right: rc.right, bottom: rc.bottom },
            RECT { left: 0, top: 0, right: b, bottom: rc.bottom },
            RECT { left: rc.right - b, top: 0, right: rc.right, bottom: rc.bottom },
        ];
        unsafe {
            let brush = CreateSolidBrush(color);
            for s in strips.iter() {
                let _ = FillRect(hdc, s, brush);
            }
            let _ = DeleteObject(HGDIOBJ(brush.0));
        }
    }

    fn client(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.main_hwnd, &mut rc);
        }
        rc
    }

    /// Paint the caption band into an off-screen bitmap, then blit it in one go.
    ///
    /// One band repaint is `FillRect(band, bg)` followed by N tab fills and
    /// their labels. Straight on the window DC every one of those intermediate
    /// states is on screen, so a strip repainted at pointer rate blinks as a
    /// whole — measured while the pointer crossed the caption buttons: 59 of
    /// 184 recorded frames caught a bare or half-drawn band, always in paint
    /// order (background, then tab 1, then tab 2 …). Buffering costs one
    /// BitBlt and makes the repaint rate invisible instead of fixing it one
    /// message at a time.
    fn paint_band(&self, hdc: HDC) {
        let rc = self.client();
        let w = rc.right.max(1);
        let h = TABBAR_HEIGHT.min(rc.bottom.max(1));
        unsafe {
            let mem = CreateCompatibleDC(hdc);
            let bmp = if mem.is_invalid() {
                HBITMAP::default()
            } else {
                CreateCompatibleBitmap(hdc, w, h)
            };
            if bmp.is_invalid() {
                // No memory for a back buffer: a flashing band beats no band.
                if !mem.is_invalid() {
                    let _ = DeleteDC(mem);
                }
                self.tabbar.paint(hdc, w);
                return;
            }
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            self.tabbar.paint(mem, w);
            let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
        }
    }

    fn sync_tabbar(&mut self) {
        let ids: Vec<u64> = self.tabs.iter_all().map(|t| t.id).collect();
        let labels: Vec<String> = self
            .tabs
            .iter_all()
            .map(|t| {
                if t.label.is_empty() {
                    short_label(&t.path)
                } else {
                    t.label.clone()
                }
            })
            .collect();
        let width = self.client().right;
        self.tabbar
            .update(&ids, &labels, self.tabs.active_index(), &self.cfg.get(), width);
        self.invalidate_band();
        // With no caption there is nothing left that shows the current folder
        // outside the strip itself, so the taskbar tooltip / Alt-Tab entry take
        // the active tab's name — same as a browser.
        let title = if labels.is_empty() {
            "Revolc".to_string()
        } else {
            labels[self.tabs.active_index().min(labels.len() - 1)].clone()
        };
        unsafe {
            let _ = SetWindowTextW(self.main_hwnd, &HSTRING::from(title));
        }
        self.sync_chrome();
    }

    fn is_maximized(&self) -> bool {
        unsafe { IsZoomed(self.main_hwnd).as_bool() }
    }

    /// Minimize / maximize-restore / close, driven by our own click handling.
    fn caption_action(&mut self, b: caption::Button) {
        // One line per click: rare enough to keep at info, and the only way to
        // tell "the click never arrived" from "the action did nothing".
        tracing::info!(?b, maximized = self.is_maximized(), "caption button clicked");
        match b {
            caption::Button::Minimize => unsafe {
                let _ = ShowWindow(self.main_hwnd, SW_MINIMIZE);
            },
            caption::Button::Maximize => self.toggle_maximize(),
            // Same path as the system button: WM_CLOSE means "hide to tray"
            // in redirect mode (Clover semantics), not "exit".
            caption::Button::Close => unsafe {
                let _ = PostMessageW(self.main_hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            },
        }
    }

    /// Maximize/restore without `SW_RESTORE`'s side effect of also undoing a
    /// minimize (see surface()).
    fn toggle_maximize(&mut self) {
        let cmd = if self.is_maximized() { SW_RESTORE } else { SW_SHOWMAXIMIZED };
        unsafe {
            let _ = ShowWindow(self.main_hwnd, cmd);
        }
    }

    /// Repaint only the caption band. Hover and tab changes touch nothing else,
    /// and a full-window invalidate would drag the absorbed Explorer child into
    /// the update twice per mouse move.
    fn invalidate_band(&self) {
        let rc = self.client();
        let band = RECT { left: 0, top: 0, right: rc.right, bottom: TABBAR_HEIGHT };
        unsafe {
            // With WS_CLIPCHILDREN the parent no longer owns these pixels:
            // invalidate the window that actually paints them.
            let target = if self.tabbar_hwnd.0.is_null() { self.main_hwnd } else { self.tabbar_hwnd };
            let _ = InvalidateRect(target, Some(&band), false);
        }
    }

    /// Repaint the strip synchronously on the window that owns its pixels.
    fn refresh_band(&self) {
        self.invalidate_band();
        unsafe {
            let target = if self.tabbar_hwnd.0.is_null() {
                self.main_hwnd
            } else {
                self.tabbar_hwnd
            };
            let _ = UpdateWindow(target);
        }
    }

    /// Create the owned strip. It is a top-level popup owned by the frame, not
    /// a sibling child: a foreign Explorer pane can raise itself among the
    /// frame's children, but it can never get above an owned popup.
    fn create_tabbar_window(&mut self) {
        ensure_tabbar_class();
        let rc = self.client();
        let mut origin = POINT { x: 0, y: 0 };
        unsafe {
            let _ = ClientToScreen(self.main_hwnd, &mut origin);
            let hwnd = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW,
                TABBAR_CLASS,
                None,
                WS_POPUP | WS_CLIPSIBLINGS,
                origin.x,
                origin.y,
                rc.right,
                TABBAR_HEIGHT,
                self.main_hwnd,
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default(),
                None,
            )
            .expect("create owned tab bar popup");
            self.tabbar_hwnd = hwnd;
            self.sync_tabbar_window();
        }
    }

    /// Keep the popup glued to the frame's top-left, sized to its client width,
    /// and hidden whenever the frame is hidden. SWP_NOZORDER is essential: the
    /// popup is a top-level owned window, so raising it here would drag its
    /// owner above the other window and make two frames fight over Z order.
    fn sync_tabbar_window(&self) {
        if self.tabbar_hwnd.0.is_null() {
            return;
        }
        let rc = self.client();
        let mut origin = POINT { x: 0, y: 0 };
        unsafe {
            let _ = ClientToScreen(self.main_hwnd, &mut origin);
            let visible = IsWindowVisible(self.main_hwnd).as_bool();
            let _ = ShowWindow(
                self.tabbar_hwnd,
                if visible { SW_SHOW } else { SW_HIDE },
            );
            let _ = SetWindowPos(
                self.tabbar_hwnd,
                HWND_TOP,
                origin.x,
                origin.y,
                rc.right,
                TABBAR_HEIGHT,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        // Keep the second overlay in step with show/hide and moves.
        self.sync_bookmarkbar_window();
    }

    /// Bookmark bar height: 0 when disabled, so the Explorer view starts right
    /// under the tab strip. Read from config each call for hot reload.
    fn bookmark_height(&self) -> i32 {
        if self.cfg.get().bookmarks.enabled {
            BOOKMARK_HEIGHT
        } else {
            0
        }
    }

    /// Create the bookmark-bar popup just below the tab strip. Created once
    /// (like the strip); `[bookmarks].enabled` only toggles its visibility via
    /// `sync_bookmarkbar_window`, so it can be hot-reloaded.
    fn create_bookmarkbar_window(&mut self) {
        ensure_bookmark_class();
        let rc = self.client();
        let mut origin = POINT { x: 0, y: TABBAR_HEIGHT };
        unsafe {
            let _ = ClientToScreen(self.main_hwnd, &mut origin);
            let hwnd = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW,
                BOOKMARK_CLASS,
                None,
                WS_POPUP | WS_CLIPSIBLINGS,
                origin.x,
                origin.y,
                rc.right,
                BOOKMARK_HEIGHT,
                self.main_hwnd,
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                GetModuleHandleW(None).unwrap_or_default(),
                None,
            )
            .expect("create owned bookmark bar popup");
            self.bookmarkbar_hwnd = hwnd;
            self.sync_bookmarkbar_window();
        }
    }

    /// Glue the bookmark popup under the strip, hide it with the frame or when
    /// disabled. SWP_NOZORDER for the same reason as the tab strip.
    fn sync_bookmarkbar_window(&self) {
        let hwnd = self.bookmarkbar_hwnd;
        if hwnd.0.is_null() {
            return;
        }
        let rc = self.client();
        let enabled = self.cfg.get().bookmarks.enabled;
        let mut origin = POINT { x: 0, y: TABBAR_HEIGHT };
        unsafe {
            let _ = ClientToScreen(self.main_hwnd, &mut origin);
            let visible = IsWindowVisible(self.main_hwnd).as_bool() && enabled;
            let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
            let _ = SetWindowPos(
                hwnd,
                HWND_TOP,
                origin.x,
                origin.y,
                rc.right,
                BOOKMARK_HEIGHT,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }

    /// Pull maximize/active state into the strip (glyphs depend on both).
    fn sync_chrome(&mut self) {
        let maximized = self.is_maximized();
        let active = self.active;
        if self.tabbar.set_chrome(self.tabbar.hover(), maximized, active) {
            self.invalidate_band();
        }
    }

    /// Track which caption button the cursor is over. Driven from
    /// WM_NCHITTEST: that is the one place every mouse move passes through,
    /// whether the point resolves to a client area (tabs) or a non-client one
    /// (caption buttons).
    fn update_hover(&mut self, x: i32, y: i32, width: i32) {
        // While a tab is being dragged the highlight belongs to the tab in
        // hand: letting the pointer repaint hover fills underneath it makes
        // the strip flicker as the drag passes over each slot.
        if self.tabbar.drag().is_some() {
            return;
        }
        let want = caption::hit(x, y, width);
        // A tab is a client-area target, a caption button a non-client one:
        // the two hovers are tracked separately and both repaint the band.
        // The close X belongs to its tab: without that, sliding onto the X
        // dropped the tab's hover fill and the tab flickered.
        let over_tab = match self.tabbar.hit(x, y, width) {
            tabbar::Hit::Tab(i) | tabbar::Hit::Close(i) => Some(i),
            _ => None,
        };
        let maximized = self.is_maximized();
        let active = self.active;
        let mut dirty = self.tabbar.set_chrome(want, maximized, active);
        if self.tabbar.set_hover_tab(over_tab) {
            dirty = true;
        }
        if dirty {
            self.invalidate_band();
        }
        if (want.is_some() || over_tab.is_some()) && !self.hover_tracking {
            self.hover_tracking = true;
            unsafe {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    // Client tracking only: the caption buttons answer HTCLIENT
                    // (see WM_NCHITTEST), and on a window whose whole frame was
                    // handed back by WM_NCCALCSIZE there is no non-client area
                    // to leave — TME_NONCLIENT would fire a leave immediately,
                    // clearing the hover right after it was set.
                    dwFlags: TME_LEAVE,
                    hwndTrack: if self.tabbar_hwnd.0.is_null() {
                        self.main_hwnd
                    } else {
                        self.tabbar_hwnd
                    },
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
        }
    }

    fn clear_hover(&mut self) {
        self.hover_tracking = false;
        let maximized = self.is_maximized();
        let active = self.active;
        let mut dirty = self.tabbar.set_chrome(None, maximized, active);
        if self.tabbar.set_hover_tab(None) {
            dirty = true;
        }
        if dirty {
            self.invalidate_band();
        }
    }

    /// Index of the tab with `id`, if it still exists.
    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter_all().position(|t| t.id == id)
    }

    /// Event-driven navigation from the BHO: `hwnd` is the CabinetWClass the
    /// BHO saw navigate; find the tab holding that exact window and update its
    /// path. Matching by HWND (not URL) keeps same-folder tabs independent.
    fn on_explorer_navigated(&mut self, hwnd: isize, url: &str) {
        let Some(path) = host::file_url_to_path(url) else {
            tracing::debug!(?url, "BHO navigation: non-filesystem URL, ignored");
            return;
        };
        let idx = self.views.iter().position(|v| {
            v.as_ref().map(|v| v.top.0 as isize == hwnd).unwrap_or(false)
        });
        let Some(i) = idx else {
            // Window may not be absorbed yet (BHO fires during creation), or
            // belongs to another Revolc window — both are normal. INFO so a
            // hand-test can tell "arrived but unmatched" from "never sent".
            tracing::info!(?hwnd, ?path, "BHO navigation: no matching tab in this window");
            return;
        };
        let old = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
        if old == path {
            return;
        }
        tracing::info!(tab = i, ?hwnd, ?old, new = ?path, "tab navigated (BHO event)");
        self.tabs.set_path(i, path);
    }

    // ---- tab drag: reorder inside the band -------------------------------

    /// Left button went down on tab `i`. A browser activates a tab on press,
    /// so a drag always carries the tab you were looking at; the drag itself
    /// only starts once the pointer crosses the system threshold, otherwise
    /// every click on a tab would nudge its neighbours.
    fn begin_tab_press(&mut self, i: usize, x: i32) {
        let Some(id) = self.tabbar.ids().get(i).copied() else {
            return;
        };
        let Some(idx) = self.index_of(id) else {
            return;
        };
        self.activate(idx);
        self.tab_press = Some(TabPress { id, x });
    }

    /// Mouse moved while the left button is down. Returns true when the band
    /// was repainted (i.e. the caller must not also handle hover).
    fn tab_drag_track(&mut self, x: i32) -> bool {
        if let Some(press) = self.tab_press {
            // SM_CXDRAG is "how far can the pointer move before it is a drag"
            // — the same constant user32 uses for its own drag detection, so
            // the feel matches the rest of the shell.
            let threshold = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(1);
            if (x - press.x).abs() < threshold {
                return false;
            }
            self.tab_press = None;
            let Some(i) = self.index_of(press.id) else {
                return false;
            };
            // Grab from the press point, not from here: the tab must not jump
            // under the cursor the moment the drag is recognised.
            self.tabbar.begin_drag(i, press.x);
            unsafe {
                SetCapture(self.main_hwnd);
            }
        }
        if self.tabbar.drag().is_none() {
            return false;
        }
        self.tabbar.drag_to(x);
        if let Some(to) = self.tabbar.target() {
            let from = self.tabbar.drag().map(|d| d.from).unwrap_or(to);
            self.move_tab(from, to);
        }
        self.invalidate_band();
        true
    }

    /// Drop the dragged tab. Reorder inside the band only: pulling a tab out
    /// into its own window is a later stage, so there is nothing to decide on
    /// release — the model already holds the final order.
    fn end_tab_drag(&mut self) {
        self.tab_press = None;
        if self.tabbar.drag().is_none() {
            return;
        }
        self.tabbar.end_drag();
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate_band();
    }

    /// Move tab `from` to `to`, keeping tabs, views and the active index in
    /// step. Everything that is keyed by tab index (`views`, the layout cache)
    /// has to move with it or the wrong folder would answer for the tab.
    fn move_tab(&mut self, from: usize, to: usize) {
        if !self.tabs.move_to(from, to) {
            return;
        }
        let view = self.views.remove(from);
        self.views.insert(to, view);
        // The drag holds a tab, not a slot: follow it.
        self.tabbar.set_drag_from(to);
        // `laid_out` is keyed by the active index. Retarget it instead of
        // clearing it: tab slots are uniform, so a reorder moves no window —
        // clearing the cache would cost one cross-process SetWindowPos on the
        // next sweep tick for nothing.
        if let Some(k) = self.laid_out.as_mut() {
            if k.0 == from {
                k.0 = to;
            } else if from < k.0 && to >= k.0 {
                k.0 -= 1;
            } else if from > k.0 && to <= k.0 {
                k.0 += 1;
            }
        }
        self.sync_tabbar();
        self.invalidate_band();
    }

    /// Open a folder: push a reserved tab, spawn Explorer, absorb on the
    /// notify or on the next fast sweep tick, whichever comes first.
    fn new_tab(&mut self, path: String) {
        let at = self.tabs.len();
        self.tabs.push(path.clone());
        self.views.insert(at, None);
        self.launch_tab_view(at, path);
    }

    /// Context menu "new tab to the right": insert after `after` and open it.
    fn new_tab_after(&mut self, after: usize, path: String) {
        let at = (after + 1).min(self.tabs.len());
        self.tabs.insert_after(after, path.clone());
        self.views.insert(at, None);
        self.launch_tab_view(at, path);
    }

    /// Shared tail of new_tab / new_tab_after: arm the reserve clock, show and
    /// activate the new tab, then spawn the Explorer window to absorb.
    fn launch_tab_view(&mut self, at: usize, path: String) {
        self.reserve_deadline = Some(Instant::now() + RESERVE_TTL);
        self.sync_tabbar();
        self.activate(at);
        self.arm_burst();
        host::spawn_explorer(&path);
    }

    /// Context menu "open in new window": MOVE the tab — close it here, then
    /// launch a fresh revolc.exe with the tab's *live* folder as startup path.
    /// The new process absorbs its own Explorer window. Keep the close after
    /// the spawn: if this is the last tab, close_tab destroys this window and
    /// we must have captured the path and started the replacement first.
    fn move_tab_to_new_window(&mut self, i: usize) {
        let path = self.folder_path_of(i);
        let exe = std::env::current_exe().unwrap_or_default();
        match std::process::Command::new(exe).arg(&path).spawn() {
            Ok(_) => {
                tracing::info!(?path, tab = i, "moved tab to new window");
                self.close_tab(i);
            }
            Err(e) => {
                // Spawn failed: leave the tab where it is — never lose it.
                tracing::error!(error = %e, ?path, "failed to spawn new window, tab kept");
            }
        }
    }

    /// Close every tab except `keep`. Iterate in reverse so removing a tab
    /// never shifts the index of one we have not visited yet.
    fn close_other_tabs(&mut self, keep: usize) {
        if keep >= self.tabs.len() {
            return;
        }
        for i in (0..self.tabs.len()).rev() {
            if i != keep {
                self.close_tab(i);
            }
        }
    }

    /// Close every tab to the right of `i` (again, reverse order).
    fn close_tabs_to_right(&mut self, i: usize) {
        let start = i + 1;
        for j in (start..self.tabs.len()).rev() {
            self.close_tab(j);
        }
    }

    /// Attach an Explorer window to the first reserved (view-less) tab,
    /// or a brand-new tab if none is reserved.
    fn absorb_window(&mut self, top: HWND) {
        if host::is_absorbed(top, self.main_hwnd) {
            return;
        }
        // A tab the user closed while its window was still on its way: the
        // window is not wanted any more. Close it instead of resurrecting it.
        if self.discard_window(top) {
            return;
        }
        // ABSORB FIRST. Resolving the path before SetParent blocked the main
        // thread ~1s (20×50ms retries) — every absorb flashed a full Explorer
        // window for 1.6s (2026-11-17). Clover semantics: re-parent the window
        // immediately; metadata (tab label/path) is filled in after.
        let idx = match self.views.iter().position(|v| v.is_none()) {
            Some(i) => i,
            None => {
                self.tabs.push(String::new());
                self.views.push(None);
                self.views.len() - 1
            }
        };

        let view = match unsafe { Absorbed::absorb(top, self.main_hwnd) } {
            Ok(view) => view,
            Err(e) => {
                // Park it too: a failed absorb (e.g. cross-integrity SetParent
                // blocked by UIPI) must not be retried every sweep tick.
                tracing::error!(error = %e, ?top, "absorb failed, parking window");
                self.excluded.push((top.0 as isize, Instant::now()));
                park_released(top);
                return;
            }
        };
        self.views[idx] = Some(view);
        // Absorbing into a hidden/minimized Revolc looks like "nothing
        // happened" - surface the window so the user sees the result. Must not
        // un-maximize: this runs on every `+` click.
        self.surface();
        // The window is usually adopted while Explorer still keeps it hidden
        // (that is the whole point — see stray_folder_windows). Everything
        // below that needs a *live, navigated* browser is therefore deferred.
        let visible = host::is_visible(top);
        tracing::info!(?top, idx, visible, "absorbed into tab");

        // Best-effort path only for reserved tabs (Ctrl+T / stray adoption).
        // When we spawned the window ourselves the tab already carries the
        // path — never stall the UI thread on IShellWindows (out-of-proc COM,
        // 50ms per retry) for information we already have. A window that has
        // not navigated yet has no path at all, and asking would burn the
        // retries for nothing: resolve it on a later sweep instead.
        let need_path = self.tabs.get(idx).map(|t| t.path.is_empty()).unwrap_or(true);
        if need_path && visible {
            let store = host::read_folder_path(top, 1, false).unwrap_or_default();
            if !store.is_empty() {
                self.tabs.set_path(idx, store);
            }
        }
        // Display text comes from the window title so it follows navigation.
        let title = host::window_title(top);
        if !title.is_empty() {
            self.tabs.set_label(idx, title);
        } else if self.tabs.get(idx).map(|t| t.label.is_empty()).unwrap_or(false) {
            let fallback = short_label(&self.tabs.get(idx).map(|t| t.path.clone()).unwrap_or_default());
            self.tabs.set_label(idx, fallback);
        }

        self.sync_tabbar();
        self.activate(idx);
        self.layout();
    }

    /// BHO said "a shell window was created". Treat it as a hint: sweep
    /// immediately, and stay in fast-poll mode while the window finishes
    /// appearing (SetSite fires before the frame is shown).
    fn on_notify(&mut self) {
        tracing::info!("absorb notify received");
        self.arm_burst();
        self.sweep();
    }

    /// Switch the sweep timer into 15ms mode for BURST_WINDOW.
    fn arm_burst(&mut self) {
        if self.cfg.get().takeover.mode != "redirect" {
            return;
        }
        self.burst_until = Some(Instant::now() + BURST_WINDOW);
        unsafe {
            SetTimer(self.main_hwnd, SWEEP_TIMER_ID, SWEEP_FAST_MS, None);
        }
    }

    /// Back off to the idle rate (one pass per second). Called from sweep so
    /// the transition needs no extra timer bookkeeping.
    fn arm_idle(&mut self) {
        if self.cfg.get().takeover.mode != "redirect" {
            return;
        }
        self.burst_until = None;
        unsafe {
            SetTimer(self.main_hwnd, SWEEP_TIMER_ID, SWEEP_IDLE_MS, None);
        }
    }

    fn known_tops(&self) -> Vec<isize> {
        self.views.iter().flatten().map(|v| v.top.0 as isize).collect()
    }

    fn prune_excluded(&mut self) {
        let now = Instant::now();
        self.excluded.retain(|(raw, at)| {
            now.duration_since(*at) < EXCLUDE_TTL
                && host::is_window_alive(HWND(*raw as *mut core::ffi::c_void))
        });
    }

    /// Drop tabs whose Explorer window never arrived — see RESERVE_TTL.
    fn expire_reserved(&mut self) {
        let Some(deadline) = self.reserve_deadline else { return };
        if Instant::now() < deadline {
            return;
        }
        let empty: Vec<usize> = self
            .views
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.is_none().then_some(i))
            .collect();
        self.reserve_deadline = None;
        if empty.is_empty() {
            return;
        }
        tracing::warn!(
            count = empty.len(),
            "reserved tab never got its window, dropping"
        );
        for i in empty.into_iter().rev() {
            self.close_tab(i);
        }
    }

    /// Is `top` the window of a tab the user closed before absorption?
    /// Returns true when it was consumed (hidden and asked to close).
    fn discard_window(&mut self, top: HWND) -> bool {
        if self.discard.is_empty() {
            return false;
        }
        let now = Instant::now();
        self.discard.retain(|(_, at)| now.duration_since(*at) < EXCLUDE_TTL);
        if self.discard.is_empty() {
            return false;
        }
        let title = host::window_title(top);
        let hit = if title.is_empty() {
            // A shell frame that has not navigated yet has no title. Only take
            // it when nothing else is waiting for a window of its own, and
            // only while it is still invisible — that is the shape of the
            // window we just asked Explorer to open.
            let waiting = self.views.iter().any(|v| v.is_none());
            if waiting || host::is_visible(top) {
                None
            } else {
                self.discard.first().map(|(l, _)| l.clone())
            }
        } else {
            self.discard
                .iter()
                .find(|(l, _)| *l == title)
                .map(|(l, _)| l.clone())
        };
        let Some(label) = hit else { return false };
        self.discard.retain(|(l, _)| *l != label);
        // Park it so the sweep does not try again while it closes.
        self.excluded.push((top.0 as isize, Instant::now()));
        park_released(top);
        tracing::info!(
            ?top,
            title,
            label,
            "closing window of a tab closed before absorb"
        );
        unsafe {
            let _ = ShowWindow(top, SW_HIDE);
            let _ = PostMessageW(top, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        true
    }

    /// Drop tabs whose Explorer window died on its own (its own X button, a
    /// crashed browser process). Without this the tab keeps a dead HWND and
    /// the layout/activate paths keep poking it.
    fn drop_dead_views(&mut self) {
        let dead: Vec<usize> = self
            .views
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| match slot {
                Some(v) if !host::is_window_alive(v.top) => Some(i),
                _ => None,
            })
            .collect();
        if dead.is_empty() {
            return;
        }
        for i in dead.into_iter().rev() {
            tracing::warn!(idx = i, "absorbed window vanished, dropping tab");
            self.views.remove(i);
            self.tabs.remove(i);
        }
        if self.tabs.is_empty() {
            // Every window closes for real (docs/双进程架构设计.md Phase 1);
            // the resident window keeps redirect takeover alive.
            unsafe {
                let _ = DestroyWindow(self.main_hwnd);
            }
            return;
        }
        self.sync_tabbar();
        self.activate(self.tabs.active_index());
        self.layout();
    }

    /// Keep tab labels in sync with the Explorer windows. GetWindowTextW on a
    /// foreign window returns its cached caption (no WM_GETTEXT round-trip),
    /// so one pass over all tabs is microseconds. This is what makes a tab
    /// say "此电脑" after the user navigates away from "zt".
    fn refresh_labels(&mut self) -> bool {
        let mut changed = false;
        // Collect first: track_navigation needs &mut self, so we cannot call
        // it while `views` / `tabs` are borrowed in the loop body.
        let mut navigated: Vec<(usize, HWND, String)> = Vec::new();
        for (i, slot) in self.views.iter().enumerate() {
            if let Some(v) = slot {
                let title = host::window_title(v.top);
                if !title.is_empty() && self.tabs.set_label(i, title.clone()) {
                    changed = true;
                    navigated.push((i, v.top, title));
                }
            }
        }
        for (i, top, title) in navigated {
            // A changed Explorer title almost always means the user navigated.
            // Capture the live folder now so Tab.path stops being a stale
            // snapshot of where the tab was opened.
            self.track_navigation(i, top, &title);
        }
        changed
    }

    /// Navigation tracking (feat/tab-context-menu): when a tab's window title
    /// changes, read its live folder path and write it back to Tab.path, with
    /// a log line so navigation is observable. Uses the diagnosing reader so
    /// a failure also records why (IShellWindows enumeration + pid match).
    fn track_navigation(&mut self, i: usize, top: HWND, title: &str) {
        let old = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
        match host::read_folder_path(top, 1, true) {
            Some(path) if path != old => {
                tracing::info!(tab = i, ?old, new = ?path, title, "tab navigated");
                self.tabs.set_path(i, path);
            }
            Some(_) => {
                tracing::debug!(tab = i, ?old, title, "tab title changed, path unchanged");
            }
            None => {
                tracing::warn!(tab = i, ?old, title, ?top,
                    "tab title changed but live path unreadable; Tab.path stays stale");
            }
        }
    }

    /// Fill in the folder path of tabs that were absorbed before their window
    /// had navigated (the flash-free path). Once per window, only when it is
    /// on screen and titled — IShellWindows is out-of-proc and costs 50ms.
    fn resolve_pending_paths(&mut self) {
        for i in 0..self.views.len() {
            let (top, titled, visible) = match self.views[i].as_ref() {
                Some(v) => (v.top, !host::window_title(v.top).is_empty(), host::is_visible(v.top)),
                None => continue,
            };
            let want = self
                .tabs
                .get(i)
                .map(|t| t.path.is_empty())
                .unwrap_or(false);
            if !want || !titled || !visible || self.path_done.contains(&(top.0 as isize)) {
                continue;
            }
            self.path_done.push(top.0 as isize);
            if let Some(p) = host::read_folder_path(top, 1, false) {
                self.tabs.set_path(i, p);
            }
        }
        let known = self.known_tops();
        self.path_done.retain(|h| known.contains(h));
    }

    /// Folder a tab is actually showing right now. `Tab::path` is only what
    /// the tab was opened with; the user may have navigated since. Used by the
    /// context menu (which can target a non-active tab) and by `+` / shortcuts.
    fn folder_path_of(&self, i: usize) -> String {
        let label = self.tabs.get(i).map(|t| t.label.clone()).unwrap_or_default();
        let stored = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
        if let Some(v) = self.views.get(i).and_then(|s| s.as_ref()) {
            if let Some(p) = host::read_folder_path(v.top, 1, true) {
                if p != stored {
                    tracing::info!(tab = i, ?label, live = ?p, ?stored,
                        "folder_path_of: live path differs from stored (navigated since open)");
                }
                return p;
            }
            tracing::warn!(tab = i, ?label, ?stored, top = ?v.top,
                "folder_path_of: live read FAILED, falling back to stored path");
        } else {
            tracing::warn!(tab = i, ?label, ?stored,
                "folder_path_of: no view for tab, falling back to stored path");
        }
        self.tabs
            .get(i)
            .map(|t| t.path.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(home_dir)
    }

    fn active_folder_path(&self) -> String {
        self.folder_path_of(self.tabs.active_index())
    }

    /// May this window adopt a folder window that just appeared?
    ///
    /// "Put it in the window I was just using" is a process-wide question, and
    /// every window's sweep asks it independently. `LAST_ACTIVE` answers it.
    /// The escape clauses matter just as much as the answer: a tray-resident
    /// window must not keep intercepting (it would pop itself up under the
    /// cursor while the user works in another window), but it must when it is
    /// the only thing left — that is what "resident takeover" means.
    fn may_adopt(&self) -> bool {
        // A reserved tab is a folder *we* spawned; nobody else may take it.
        if self.views.iter().any(|v| v.is_none()) {
            return true;
        }
        let me = self.main_hwnd;
        let me_visible = unsafe { IsWindowVisible(me).as_bool() };
        let last = ledger().and_then(|l| l.last_active());
        if let Some(other) = last {
            let usable = unsafe { IsWindow(other).as_bool() && IsWindowVisible(other).as_bool() };
            if usable {
                // The ledger is authoritative: the user touched `other` last.
                return other == me;
            }
        }
        // No valid ledger entry (tray not installed / just launched): pick a
        // deterministic owner instead of letting whichever timer fires first
        // win. Smallest live HWND among visible RevolcMain windows.
        let owner = all_main_windows()
            .into_iter()
            .filter(|h| unsafe { IsWindowVisible(*h).as_bool() })
            .min_by_key(|h| h.0 as usize);
        match owner {
            Some(h) => h == me,
            None => me_visible || !any_visible_window(),
        }
    }

    /// Absorb every desktop folder window Revolc doesn't own yet.
    fn sweep(&mut self) {
        if self.sweeping || self.cfg.get().takeover.mode != "redirect" {
            return;
        }
        // Phase 2: one process per window. Each sweep enumerates top-level
        // CabinetWClass windows and then spends hundreds of ms in cross-process
        // SetParent. Two processes that enumerate in the same few ms both see
        // the same folder and both adopt it (observed 2026-11-20: same HWND
        // absorbed twice, then a ntdll crash during teardown). Serialise the
        // whole enumerate+adopt section across processes; the loser sees the
        // window already re-parented and skips it.
        let Some(_sweep_lock) = SweepLock::acquire() else {
            // Another window is mid-absorb; next tick sees the settled state.
            return;
        };
        self.sweeping = true;
        self.prune_excluded();
        self.drop_dead_views();
        self.expire_reserved();

        // Local excludes plus everything any dying window released back to
        // the desktop (the "FE 被吸走" fix). Both expire on the same TTL.
        let mut excl: Vec<(isize, Instant)> = self.excluded.clone();
        if let Ok(g) = RELEASED_WINDOWS.lock() {
            excl.extend(g.iter().copied());
        }
        excl.retain(|(_, t)| t.elapsed() < EXCLUDE_TTL);
        let excluded: Vec<isize> = excl.iter().map(|(h, _)| *h).collect();
        // Take windows while they are still hidden whenever a folder is
        // expected. Explorer creates the frame **~850ms before showing it**
        // (log/probe_hidden.ps1), so this is the difference between "absorbed
        // before it was ever painted" and "one frame of a stray Explorer
        // window" — the flash the user reports. Outside that window we keep
        // the old visible-only rule: an unrelated hidden CabinetWClass is
        // none of our business.
        let expecting = self.burst_until.is_some() || self.views.iter().any(|v| v.is_none());
        // Every window runs this timer. Only the one the user last touched may
        // adopt — otherwise the destination of "open this folder" is decided by
        // whichever timer fired first, which reads as a random window.
        let strays = if self.may_adopt() {
            host::stray_folder_windows(&self.known_tops(), &excluded, expecting)
        } else {
            Vec::new()
        };
        if !strays.is_empty() {
            tracing::info!(
                count = strays.len(),
                expecting,
                "sweep adopting stray folder windows"
            );
        }
        for top in strays {
            if self.tabs.len() >= MAX_TABS {
                tracing::warn!(?top, "tab limit reached, leaving window on the desktop");
                continue;
            }
            self.absorb_window(top);
        }
        self.sweeping = false;

        if self.refresh_labels() {
            self.sync_tabbar();
        }
        self.resolve_pending_paths();
        self.enforce_views();

        // Adaptive rate: keep hammering while a folder is expected, drop to
        // the once-a-second safety net otherwise.
        if self.burst_until.map(|t| Instant::now() < t).unwrap_or(false) {
            unsafe {
                SetTimer(self.main_hwnd, SWEEP_TIMER_ID, SWEEP_FAST_MS, None);
            }
        } else {
            self.arm_idle();
        }
    }

    /// Browser-style "close all tabs?" confirm. Only called with 2+ tabs.
    fn confirm_close_all_tabs(&self) -> bool {
        let n = self.tabs.len();
        let text = format!("要关闭全部 {n} 个标签页吗？\r\n每个标签页的文件夹窗口会一并关闭。");
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let r = unsafe {
            MessageBoxW(
                self.main_hwnd,
                PCWSTR(wide.as_ptr()),
                w!("Revolc"),
                MB_OKCANCEL | MB_ICONQUESTION | MB_SETFOREGROUND,
            )
        };
        r == IDOK
    }

    /// Close every tab's FE window (hidden, then WM_CLOSE — the same path as
    /// `close_tab`, so Explorer dies quietly instead of growing a title bar on
    /// the desktop), leaving nothing for another window's sweep to re-adopt.
    fn close_all_tabs(&mut self) {
        for slot in self.views.iter_mut() {
            if let Some(v) = slot.take() {
                self.excluded.push((v.top.0 as isize, Instant::now()));
                park_released(v.top);
                unsafe {
                    let _ = ShowWindow(v.top, SW_HIDE);
                    let _ = PostMessageW(v.top, WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        // Tabs whose Explorer window never arrived: nothing to close, but the
        // late window must not land in some other window as a fresh tab.
        for t in self.tabs.iter_all() {
            if !t.label.is_empty() {
                self.discard.push((t.label.clone(), Instant::now()));
            }
        }
        self.views.clear();
        self.tabs.clear();
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        // Closing a tab whose window has not arrived yet: the Explorer process
        // we spawned is still starting and will show its window seconds later.
        // Remember the title so the sweep closes that window instead of
        // adopting it back as a brand-new tab (which made a closed tab
        // reappear).
        if self.views.get(i).map(|v| v.is_none()).unwrap_or(false) {
            if let Some(t) = self.tabs.get(i) {
                let label = if t.label.is_empty() {
                    short_label(&t.path)
                } else {
                    t.label.clone()
                };
                if !label.is_empty() {
                    self.discard.push((label, Instant::now()));
                }
            }
        }
        // Close the Explorer window where it is: hidden, still our child.
        if let Some(v) = self.views[i].take() {
            // Park it: the sweep must not grab the window back while it is
            // still alive during WM_CLOSE teardown.
            self.excluded.push((v.top.0 as isize, Instant::now()));
            park_released(v.top);
            // `restore()` re-parents the window to the *desktop* and shows it
            // (SWP_SHOWWINDOW) before the close lands, so every closed tab
            // painted one frame of a plain native Explorer window — the
            // "关闭文件夹时 FE 闪一下" report. Explorer shuts down just as
            // happily while hidden, and then there is nothing to see.
            unsafe {
                let _ = ShowWindow(v.top, SW_HIDE);
                let _ = PostMessageW(v.top, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        self.tabs.remove(i);
        self.views.remove(i);
        if self.tabs.is_empty() {
            // Every window closes for real (docs/双进程架构设计.md Phase 1);
            // the resident window keeps redirect takeover alive.
            tracing::info!("last tab closed, closing window");
            unsafe {
                let _ = DestroyWindow(self.main_hwnd);
            }
            return;
        }
        self.sync_tabbar();
        self.activate(self.tabs.active_index());
    }

    fn activate(&mut self, i: usize) {
        if !self.tabs.activate(i) {
            return;
        }
        // POSITION BEFORE SHOWING. A freshly absorbed window still carries its
        // desktop size and origin, so showing it first paints it at (0,0) over
        // the tab bar for one frame — the "content flashes at the wrong place"
        // half of the glitch. Placing first also means a window Explorer has
        // not shown yet becomes visible exactly where the tab expects it.
        self.laid_out = None;
        self.place_active();
        // Show FIRST, hide others second. The old ascending loop hid j<i
        // before showing j==i, so switching to a tab on the right left one
        // frame with no visible child at all — the host then erased the whole
        // client to the tab-bar background (the green flash). Switching left
        // happened to show the lower-index target before hiding the old pane,
        // which is why the bug was direction-dependent.
        if let Some(v) = self.views.get_mut(i).and_then(|slot| slot.as_mut()) {
            v.set_visible(true);
        }
        for (j, slot) in self.views.iter_mut().enumerate() {
            if j != i {
                if let Some(v) = slot {
                    v.set_visible(false);
                }
            }
        }
        self.redraw_all();
        self.refresh_band();
        // Keyboard focus has to be handed to the other process explicitly, or
        // the address bar takes clicks but no keystrokes. A window adopted
        // before Explorer showed it cannot take focus yet: the sweep retries
        // as soon as it appears (`focus_pending`).
        self.focus_pending = match self.views.get(i) {
            Some(Some(v)) if host::is_visible(v.top) => {
                focus_view(self.main_hwnd, self.attached_tid, v);
                false
            }
            Some(Some(_)) => true,
            _ => false,
        };
        self.sync_input_queue();
        self.sync_tabbar();
    }

    /// Keep our input queue attached to the active tab's Explorer thread while
    /// our window is the active window, and detached otherwise: merged queues
    /// also share hangs, so a stuck Explorer would freeze Revolc too.
    fn sync_input_queue(&mut self) {
        // One process = one window = one UI thread now. The Phase 1 guard
        // ("did *this* window lose focus?") used a process-local LAST_ACTIVE;
        // in Phase 2 the shared ledger already points at the *new* window by
        // the time WM_ACTIVATE(WA_INACTIVE) arrives, so it returned early and
        // never detached — three windows stayed attached to three Explorer
        // input queues: drag stuttered and taskbar clicks stopped switching.
        // Each process owns only its own attachment; always tear it down.
        let want = if self.active {
            self.views
                .get(self.tabs.active_index())
                .and_then(|v| v.as_ref())
                .map(|v| host::thread_of(v.top))
                .unwrap_or(0)
        } else {
            0
        };
        if want == self.attached_tid {
            return;
        }
        if self.attached_tid != 0 {
            host::attach_input(self.attached_tid, false);
        }
        self.attached_tid = want;
        if want != 0 {
            host::attach_input(want, true);
        }
    }

    fn action(&mut self, a: Action) {
        tracing::debug!(?a, "action dispatched");
        match a {
            Action::NewTab => {
                // Reuse where the user actually is, not where the tab was
                // opened (a tab navigated to This PC must not reopen "zt").
                let path = self.active_folder_path();
                self.new_tab(path);
            }
            Action::CloseTab => self.close_tab(self.tabs.active_index()),
            Action::NextTab => {
                self.tabs.cycle(true);
                self.activate(self.tabs.active_index());
            }
            Action::PrevTab => {
                self.tabs.cycle(false);
                self.activate(self.tabs.active_index());
            }
        }
    }

    fn layout(&mut self) {
        // Width for the strip and the drift check come from the *client*, not
        // from the view rect: the view is inset by the hairline frame.
        let rc = self.client();
        self.last_client = (rc.right, rc.bottom);
        // Tab widths follow the client width; the band itself is painted by our
        // own WM_PAINT, so there is no child to move or raise any more (the old
        // HWND_TOP shuffle against the absorbed Explorer child is gone with it).
        self.tabbar.fit(rc.right);
        if !self.tabbar_hwnd.0.is_null() {
            self.sync_tabbar_window();
            self.invalidate_band();
        }
        self.laid_out = None;
        self.place_active();
        // The strip must be painted **now**, synchronously: leaving it to the
        // message queue is what produced the black rectangle right of the tabs
        // after a maximize (host already grown and erased, band not yet drawn).
        self.redraw_all();
        self.refresh_band();
    }

    /// Resize the active view, skipping the call when nothing changed.
    ///
    /// `sampled` lets callers that already measured the active view (the
    /// sweep does, for its drift check) hand the value down: `sample()` walks
    /// the window's child tree, and this used to happen three times per sweep
    /// tick at 15ms intervals during a burst.
    fn place_active(&mut self) {
        self.place_active_sampled(None);
    }

    fn place_active_sampled(&mut self, sampled: Option<host::Metrics>) {
        let idx = self.tabs.active_index();
        let rc = self.view_rect();
        let metrics = match self.views.get(idx).and_then(|v| v.as_ref()) {
            Some(v) => Some(sampled.unwrap_or_else(|| v.sample())),
            None => None,
        };
        let key = metrics.map(|m| (idx, rc.left, rc.top, rc.right, rc.bottom, (m.nc, m.band)));
        if key.is_some() && self.laid_out == key {
            return;
        }
        if let (Some(Some(v)), Some(m)) = (self.views.get(idx), metrics) {
            // Align the *visible chrome* (ribbon onward) to the view rect.
            v.place_client(rc, m);
        }
        self.laid_out = key;
    }

    /// Repaint the whole client including children. Needed after any geometry
    /// change: with WS_CLIPCHILDREN our own invalidate cannot touch the area a
    /// shrunken child vacated, so it keeps the previous frame (the "残影" band
    /// between the tab bar and the ribbon).
    fn redraw_all(&self) {
        unsafe {
            let _ = RedrawWindow(
                self.main_hwnd,
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
            // Same-thread children (our tab bar) are painted by the call
            // above; the host's own erase still has to be flushed now.
            let _ = UpdateWindow(self.main_hwnd);
        }
    }

    /// Re-assert visibility/geometry of every tab every sweep. Explorer
    /// re-shows its frame while it finishes navigating, and a re-shown
    /// inactive tab lands on top of the active one at its old size — exactly
    /// the "two ribbons stacked" glitch. One IsWindowVisible per view.
    fn enforce_views(&mut self) {
        // Self-heal a missed resize first: everything below assumes the layout
        // matches the current client size.
        let mut cr = RECT::default();
        unsafe {
            let _ = GetClientRect(self.main_hwnd, &mut cr);
        }
        if self.last_client != (0, 0) && (cr.right, cr.bottom) != self.last_client {
            tracing::warn!(
                client = ?(cr.right, cr.bottom),
                laid_out_for = ?self.last_client,
                "client size drifted from layout, re-laying out"
            );
            self.layout();
        }
        let active = self.tabs.active_index();
        let rc = self.view_rect();
        let mut restyled = false;
        let mut became_visible = false;
        let mut show: Option<HWND> = None;
        let mut hide: Vec<HWND> = Vec::new();
        for (i, slot) in self.views.iter().enumerate() {
            let Some(v) = slot else { continue };
            // Explorer can restore its caption on navigation; that is the band
            // of foreign chrome that shows up between tab bar and ribbon.
            if unsafe { v.ensure_stripped() } {
                restyled = true;
            }
            let want = i == active;
            let vis = host::is_visible(v.top);
            if want && v.waiting_for_explorer() {
                // Pre-show shell frame, still inside the grace period:
                // Explorer's own ShowWindow does the showing (and runs its
                // start-up path). After the grace we stop waiting — see
                // waiting_for_explorer(): no show ever arrives for some
                // windows and the tab would stay black forever.
                if vis {
                    became_visible = true;
                }
                // else: still inside the grace, keep waiting.
            } else if want && !vis {
                // Includes "grace expired and Explorer never showed it": the
                // tab must not stay black forever.
                show = Some(v.top);
            } else if !want && vis {
                hide.push(v.top);
            }
        }
        // Measure the active view **once** and reuse it for the drift check and
        // the placement below. `sample()` walks the window's child tree; this
        // used to happen three times per tick (15ms apart during a burst).
        // Measured after ensure_stripped(), because re-stripping the caption is
        // exactly what changes the non-client metrics.
        let metrics = self
            .views
            .get(active)
            .and_then(|v| v.as_ref())
            .map(|v| v.sample());
        // Explorer moves and resizes its own frame when it finally shows it;
        // the `laid_out` key cannot notice that because *we* changed nothing.
        let drifted = match (self.views.get(active).and_then(|v| v.as_ref()), metrics) {
            (Some(v), Some(m)) => !v.is_placed(rc, m),
            _ => false,
        };
        if restyled || became_visible || drifted {
            self.laid_out = None;
        }
        // Place BEFORE re-asserting visibility: a window that is about to be
        // shown must already sit at the tab geometry, or its first frame lands
        // at the desktop position it was born with.
        self.place_active_sampled(metrics);
        if became_visible {
            // Explorer's ShowWindow may have restored its own geometry.
            self.laid_out = None;
            self.place_active();
        }
        if let Some(top) = show {
            host::force_visible(top, true);
        }
        for top in hide {
            host::force_visible(top, false);
        }
        // No re-raise here: the strip is a top-level owned popup, so an
        // Explorer child pane can never cover it, and raising it every sweep
        // would drag this frame above the other one (Z-order fight).
        for slot in self.views.iter_mut() {
            if let Some(v) = slot {
                v.mark_shown();
            }
        }

        // A tab absorbed before Explorer showed it could not take focus; hand
        // it over now that it is on screen.
        if self.focus_pending {
            match self.views.get(active).and_then(|v| v.as_ref()) {
                Some(v) if host::is_visible(v.top) => {
                    focus_view(self.main_hwnd, self.attached_tid, v);
                    self.focus_pending = false;
                }
                None => self.focus_pending = false,
                _ => {}
            }
        }

        // Anything else parented to us is a leftover from an earlier lifetime
        // (a window explored after its tab was dropped, a restored window that
        // came back). It paints over the active view as a band of foreign
        // Explorer chrome between the tab bar and the content.
        let active_top = self
            .views
            .get(active)
            .and_then(|v| v.as_ref())
            .map(|v| v.top.0 as isize)
            .unwrap_or(0);
        for child in host::child_windows(self.main_hwnd) {
            let raw = child.0 as isize;
            if self.views.iter().flatten().any(|v| v.top.0 as isize == raw) {
                continue; // tracked; visibility already handled above
            }
            if raw != active_top && host::is_visible(child) {
                tracing::warn!(
                    ?child,
                    class = %host::class_name(child),
                    rect = ?host::rect_in_parent(child),
                    "hiding untracked child window"
                );
                host::force_visible(child, false);
            }
        }

        self.place_active();
    }

    /// Layout ground truth in the log: main client, tab bar and every child
    /// window with its rect. Called on resize, where the offsets went wrong.
    fn dump_layout(&self, why: &str) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.main_hwnd, &mut rc);
        }
        tracing::info!(
            why,
            client = ?(rc.left, rc.top, rc.right, rc.bottom),
            views = self.views.len(),
            "layout"
        );
        for child in host::child_windows(self.main_hwnd) {
            tracing::info!(
                why,
                ?child,
                class = %host::class_name(child),
                rect = ?host::rect_in_parent(child),
                nc = ?host::nc_metrics(child),
                visible = host::is_visible(child),
                "layout child"
            );
        }
    }

    /// Release every absorbed window back to the desktop.
    fn release_all(&mut self) {
        for v in self.views.iter_mut() {
            if let Some(v) = v.take() {
                // Park it process-wide: this window is on its way out, and the
                // surviving windows' sweeps must not adopt the pane back.
                park_released(v.top);
                unsafe { v.restore() };
            }
        }
    }
}

/// Hand keyboard focus to an absorbed view without breaking the persistent
/// input-queue attachment (`attached_tid`) — AttachThreadInput is not reference
/// counted, so a blind attach/detach pair would silently undo it.
fn focus_view(main: HWND, attached_tid: u32, v: &Absorbed) {
    let already = attached_tid != 0 && attached_tid == host::thread_of(v.top);
    host::focus_into(main, v.top, already);
}

/// x/y out of a mouse message's lParam, sign-extended (screen coords can be
/// negative on a monitor to the left of the primary).
/// Build and show the tab context menu. Returns the chosen command id, or 0.
///
/// `TrackPopupMenuEx` runs a modal loop that dispatches other messages to this
/// window, so the caller must NOT hold an `&mut App` across it (re-entrant
/// `app_from` would alias). Collect the target first, drop the borrow, show the
/// menu, then re-acquire the app to dispatch.
unsafe fn show_tab_context_menu(hwnd: HWND, screen: POINT, has_tab: bool) -> usize {
    let Ok(menu) = CreatePopupMenu() else { return 0 };

    if has_tab {
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_NEW_WINDOW, w!("在新窗口打开此标签页(&N)"));
        // (moves the tab: closes it here + opens the same folder in a new window)
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_NEW_AFTER, w!("在右侧新建标签页(&A)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_CLOSE, w!("关闭标签页(&C)"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_CLOSE_OTHERS, w!("关闭其他标签页(&O)"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_CLOSE_RIGHT, w!("关闭右侧标签页(&R)"));
    } else {
        // Bare strip: still offer a new tab, nothing tab-specific.
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_NEW_AFTER, w!("新建标签页(&A)"));
    }

    // A menu owned by another window does not dismiss on outside click unless
    // that window is foreground first (classic TrackPopupMenu gotcha).
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenuEx(
        menu,
        TPM_RETURNCMD.0,
        screen.x,
        screen.y,
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);
    cmd.0 as usize
}

fn point_from_lparam(lp: LPARAM) -> (i32, i32) {
    let x = (lp.0 as u16) as i16 as i32;
    let y = ((lp.0 >> 16) as u16) as i16 as i32;
    (x, y)
}

fn home_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
}

fn short_label(path: &str) -> String {
    // Shell-namespace items (LocationURL empty -> stored as CLSID path).
    if path.starts_with("::") {
        return if path.contains("20D04FE0-3AEA-1069-A2D8-08002B30309D") {
            "此电脑".into()
        } else {
            "Shell".into()
        };
    }
    path.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// Monitor metrics for the window (work area included). `None` when the monitor
/// of the window cannot be resolved; the caller then keeps DefWindowProc's
/// defaults.
unsafe fn monitor_info(hwnd: HWND) -> Option<MONITORINFO> {
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if mon.is_invalid() {
        return None;
    }
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !GetMonitorInfoW(mon, &mut mi).as_bool() {
        return None;
    }
    Some(mi)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = lp.0 as *const CREATESTRUCTW;
            let args = Box::from_raw((*cs).lpCreateParams as *mut WindowArgs);
            let app = App::build(hwnd, args);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as _);
            let app = app_from(hwnd);
            app.create_tabbar_window();
            app.create_bookmarkbar_window();
            app.invalidate_band();
            // The class icons cover Alt-Tab and the title bar; the taskbar button
            // and `SendMessage`-based consumers read the *window* icon, which
            // only exists once it is set explicitly. Both come from the same .ico.
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_BIG as usize),
                    LPARAM(icon::big().0 as isize),
                );
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_SMALL as usize),
                    LPARAM(icon::small().0 as isize),
                );
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            app_from(hwnd).release_all();
            // One process = one window now (Phase 2): its destruction ends
            // this process. The tray, if any, lives in a separate exe.
            if let Some(l) = ledger() {
                let _ = l.clear_if(hwnd);
            }
            LIVE_WINDOWS.fetch_sub(1, Ordering::SeqCst);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // Free the per-window App and its accelerator table. Windows can
            // be spawned and closed repeatedly now, so a leaked Box per window
            // would be an actual leak rather than a one-off.
            let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if ptr != 0 {
                let app = Box::from_raw(ptr as *mut App);
                if !app.accel.is_invalid() {
                    let _ = DestroyAcceleratorTable(app.accel);
                }
                drop(app);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        // Maximize to the **work area**, not to the monitor.
        //
        // The window is WS_POPUP (no WS_CAPTION) and DefWindowProc sizes a
        // caption-less popup to the whole monitor, ignoring the taskbar:
        // measured on a 1600x900 screen with a vertical 79px taskbar on the
        // right, the zoomed rect came out as the monitor inflated by the
        // resize frame (-7,-7..1607,907), i.e. the taskbar was buried under
        // the window. Pinning size *and* origin to rcWork is the fix.
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lp.0 as *mut MINMAXINFO);
            if let Some(mi) = monitor_info(hwnd) {
                // ptMaxPosition is monitor-relative.
                mmi.ptMaxPosition.x = mi.rcWork.left - mi.rcMonitor.left;
                mmi.ptMaxPosition.y = mi.rcWork.top - mi.rcMonitor.top;
                mmi.ptMaxSize.x = mi.rcWork.right - mi.rcWork.left;
                mmi.ptMaxSize.y = mi.rcWork.bottom - mi.rcWork.top;
            }
            LRESULT(0)
        }
        // The window has no non-client area at all: the client rect *is* the
        // window rect. Returning 0 (not calling DefWindowProc) is what removes
        // the frame — the tab bar is the caption now.
        //
        // No maximized fixup here any more: WM_GETMINMAXINFO already pins the
        // zoomed rect to rcWork, so no frame hangs off-screen (measured
        // 0,0..1521,900 — exactly the work area). The 8px inset this arm used
        // to apply would now leave an unpainted non-client strip all round.
        WM_NCCALCSIZE => LRESULT(0),
        // Everything the pointer can hit in the caption band is answered here.
        //
        // This has to be the toplevel: a child window cannot produce a
        // non-client hit (`HTTRANSPARENT` only forwards a *client* hit, and
        // user32 then re-asks the child in a loop). Coming from here instead
        // gives native drag (`HTCAPTION`), native double-click maximize and
        // Win11 Snap Layouts (`HTMAXBUTTON`), all via DefWindowProc.
        WM_NCHITTEST => {
            // Hit tests can arrive before WM_CREATE has stored the App (and
            // after WM_DESTROY cleared the pointer): never dereference null
            // from the one message that fires on every single mouse move.
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if ptr.is_null() {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            let app = &mut *ptr;
            let (sx, sy) = point_from_lparam(lp);
            let mut pt = POINT { x: sx, y: sy };
            let _ = ScreenToClient(hwnd, &mut pt);
            let cr = app.client();
            // While a tab is held the whole window answers HTCLIENT. The strip
            // above the tabs answers HTCAPTION, so a drag that wanders up
            // there would switch the capture to non-client and start emitting
            // WM_NCMOUSEMOVE — the tab would freeze mid-air until the button
            // was released.
            if app.tabbar.drag().is_some() {
                return LRESULT(HTCLIENT as isize);
            }
            let hit = app.tabbar.hit(pt.x, pt.y, cr.right);
            // Caption buttons before the resize borders: they are painted on
            // top of the band and, at the right end, on top of the frame too —
            // the last 6px of the right edge and the last 4px of the top edge
            // cut straight through the min/max/close rects. Answering the
            // border first makes a pointer gliding along the top of the strip
            // alternate "resize" / "button" every pixel, and every crossing
            // drops the hover and repaints the whole strip.
            //
            // Caption buttons stay CLIENT on purpose: reporting HTMAXBUTTON
            // hands the button to the shell, which pops the Win11 Snap Layouts
            // flyout on top of it — the flyout lands in the wrong place (a
            // window with no non-client area gives it no anchor), eats the
            // click and leaves a blank white box behind. Measured 2026-11-18:
            // min/max stopped working, white artifact on press. Own the
            // clicks, own the behaviour.
            if let tabbar::Hit::Caption(_) = hit {
                app.update_hover(pt.x, pt.y, cr.right);
                return LRESULT(HTCLIENT as isize);
            }
            // Resize borders are ours to answer now that there is no
            // non-client area left. Skipped while maximized: the frame hangs
            // off-screen there, so the screen edge must not answer "resize".
            if !app.is_maximized() {
                // Same inset as the painted/inset frame: the child pane is
                // pushed in by WINDOW_BORDER, so these pixels are genuinely
                // ours and the resize code reaches us. TOP_BORDER is thinner
                // so it stays above the tabs (TAB_TOP = 4).
                let border = WINDOW_BORDER;
                let left = pt.x < border;
                let right = pt.x >= cr.right - border;
                let top = pt.y < TOP_BORDER;
                let bottom = pt.y >= cr.bottom - border;
                let code = match (left, right, top, bottom) {
                    (true, _, true, _) => Some(HTTOPLEFT),
                    (_, true, true, _) => Some(HTTOPRIGHT),
                    (true, _, _, true) => Some(HTBOTTOMLEFT),
                    (_, true, _, true) => Some(HTBOTTOMRIGHT),
                    (true, _, _, _) => Some(HTLEFT),
                    (_, true, _, _) => Some(HTRIGHT),
                    (_, _, true, _) => Some(HTTOP),
                    (_, _, _, true) => Some(HTBOTTOM),
                    _ => None,
                };
                if let Some(code) = code {
                    // The pointer is on the frame, not on a button or a tab:
                    // feeding the real point keeps the hover honest without a
                    // special "no hit" coordinate.
                    app.update_hover(pt.x, pt.y, cr.right);
                    return LRESULT(code as isize);
                }
            }
            // Hover tracking rides on the hit test: every mouse move gets one,
            // whether the point ends up client or non-client.
            app.update_hover(pt.x, pt.y, cr.right);
            match hit {
                // Empty strip = caption. Below the band there must be no
                // HTCAPTION: while a tab is still reserved there is no child
                // covering the content, and the whole window would drag.
                tabbar::Hit::Band if pt.y < TABBAR_HEIGHT => LRESULT(HTCAPTION as isize),
                _ => LRESULT(HTCLIENT as isize),
            }
        }
        // We paint the caption band ourselves, so the default frame painting
        // must stay out of the way (returning TRUE = "handled").
        WM_NCACTIVATE => {
            let app = app_from(hwnd);
            if wp.0 == 0 {
                app.clear_hover();
            }
            let maximized = app.is_maximized();
            if app.tabbar.set_chrome(app.tabbar.hover(), maximized, wp.0 != 0) {
                app.invalidate_band();
            }
            LRESULT(1)
        }
        // The cursor walked out of the window (or its caption band) without
        // another hit test: drop the hover highlight.
        WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
            app_from(hwnd).clear_hover();
            LRESULT(0)
        }
        // The strip is our own client area now, so its clicks arrive here.
        // Anything that is not a control starts a window drag the native way.
        windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN => {
            let app = app_from(hwnd);
            let (x, y) = point_from_lparam(lp);
            let width = app.client().right;
            match app.tabbar.hit(x, y, width) {
                tabbar::Hit::Tab(i) => app.begin_tab_press(i, x),
                tabbar::Hit::Close(i) => {
                    if let Some(id) = app.tabbar.ids().get(i).copied() {
                        if let Some(i) = app.index_of(id) {
                            app.close_tab(i);
                        }
                    }
                }
                tabbar::Hit::Plus => {
                    let path = app.active_folder_path();
                    app.new_tab(path);
                }
                tabbar::Hit::Band => {
                    // Drag the whole window from the empty strip, the way a
                    // caption would. DefWindowProc owns the move loop.
                    let mut sp = POINT { x, y };
                    unsafe {
                        let _ = ClientToScreen(hwnd, &mut sp);
                        let _ = ReleaseCapture();
                        SendMessageW(
                            hwnd,
                            windows::Win32::UI::WindowsAndMessaging::WM_NCLBUTTONDOWN,
                            WPARAM(HTCAPTION as usize),
                            LPARAM(sp.x as isize | (sp.y as isize) << 16),
                        );
                    }
                }
                tabbar::Hit::Caption(b) => app.caption_action(b),
            }
            LRESULT(0)
        }
        // Double click on the empty strip = maximize / restore, matching what
        // a real caption does (the window class needs CS_DBLCLKS for this).
        WM_LBUTTONDBLCLK => {
            let app = app_from(hwnd);
            let (x, y) = point_from_lparam(lp);
            let width = app.client().right;
            if app.tabbar.hit(x, y, width) == tabbar::Hit::Band {
                app.toggle_maximize();
            }
            LRESULT(0)
        }
        // The empty strip answers HTCAPTION, so a double-click there arrives as
        // WM_NCLBUTTONDBLCLK (never WM_LBUTTONDBLCLK). The owned tab-bar popup
        // forwards it here; toggle maximize/restore exactly like a real title
        // bar. wParam carries the hit code, lp a *screen* point.
        WM_NCLBUTTONDBLCLK => {
            let app = app_from(hwnd);
            if wp.0 == HTCAPTION as usize {
                let (sx, sy) = point_from_lparam(lp);
                let mut pt = POINT { x: sx, y: sy };
                unsafe {
                    let _ = ScreenToClient(hwnd, &mut pt);
                }
                if pt.y < TABBAR_HEIGHT
                    && app.tabbar.hit(pt.x, pt.y, app.client().right) == tabbar::Hit::Band
                {
                    app.toggle_maximize();
                }
            }
            LRESULT(0)
        }
        // Tab drag: press is remembered by WM_LBUTTONDOWN, the threshold is
        // crossed here, the reorder happens on the spot (the model is the
        // truth; the strip only paints the tab under the cursor).
        WM_MOUSEMOVE => {
            let app = app_from(hwnd);
            let (x, _y) = point_from_lparam(lp);
            app.tab_drag_track(x);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            app_from(hwnd).end_tab_drag();
            LRESULT(0)
        }
        // Capture stolen (Alt+Tab, a modal, another window grabbing the
        // mouse): drop the drag instead of leaving a tab glued to the cursor.
        WM_CAPTURECHANGED => {
            app_from(hwnd).end_tab_drag();
            LRESULT(0)
        }
        // Middle click on a tab closes it (spec §3.1).
        WM_MBUTTONUP => {
            let app = app_from(hwnd);
            let (x, y) = point_from_lparam(lp);
            let width = app.client().right;
            if let tabbar::Hit::Tab(i) = app.tabbar.hit(x, y, width) {
                if let Some(id) = app.tabbar.ids().get(i).copied() {
                    if let Some(i) = app.index_of(id) {
                        app.close_tab(i);
                    }
                }
            }
            LRESULT(0)
        }
        // Tab context menu. lp comes from the owned strip, whose client origin
        // equals the frame's, so the same coords drive `TabBar::hit`. The strip
        // forwards WM_RBUTTONUP here; resolving the tab by stable id keeps the
        // target correct even if a sweep reorders views while the menu is up.
        WM_RBUTTONUP => {
            let (x, y) = point_from_lparam(lp);
            // Phase 1: gather target + screen point, then DROP the app borrow.
            let (hit_index, screen) = {
                let app = app_from(hwnd);
                let width = app.client().right;
                let idx = match app.tabbar.hit(x, y, width) {
                    tabbar::Hit::Tab(i) | tabbar::Hit::Close(i) => app
                        .tabbar
                        .ids()
                        .get(i)
                        .copied()
                        .and_then(|id| app.index_of(id)),
                    _ => None,
                };
                let mut pt = POINT { x, y };
                let _ = ClientToScreen(hwnd, &mut pt);
                (idx, pt)
            };
            // Phase 2: modal loop — no &mut App alive here.
            let cmd = show_tab_context_menu(hwnd, screen, hit_index.is_some());
            // Phase 3: re-acquire and dispatch.
            if cmd != 0 {
                let app = app_from(hwnd);
                // Re-resolve by id in case the list shifted during the loop.
                let idx = hit_index;
                match cmd {
                    CMD_TAB_NEW_WINDOW => {
                        if let Some(i) = idx {
                            app.move_tab_to_new_window(i);
                        }
                    }
                    CMD_TAB_NEW_AFTER => {
                        let path = idx
                            .map(|i| app.folder_path_of(i))
                            .unwrap_or_else(|| app.active_folder_path());
                        let anchor = idx.unwrap_or_else(|| app.tabs.len().saturating_sub(1));
                        app.new_tab_after(anchor, path);
                    }
                    CMD_TAB_CLOSE => {
                        if let Some(i) = idx {
                            app.close_tab(i);
                        }
                    }
                    CMD_TAB_CLOSE_OTHERS => {
                        if let Some(i) = idx {
                            app.close_other_tabs(i);
                        }
                    }
                    CMD_TAB_CLOSE_RIGHT => {
                        if let Some(i) = idx {
                            app.close_tabs_to_right(i);
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        // The caption band is the only part of the client we paint; every other
        // pixel belongs to the absorbed Explorer child (WS_CLIPCHILDREN keeps
        // the two apart).
        WM_PAINT => {
            let app = app_from(hwnd);
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            app.paint_band(hdc);
            app.paint_frame(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOVE => {
            app_from(hwnd).sync_tabbar_window();
            LRESULT(0)
        }
        WM_SIZE => {
            let app = app_from(hwnd);
            app.sync_tabbar_window();
            app.layout();
            app.dump_layout("WM_SIZE");
            // Maximize state drives the restore glyph in the caption buttons.
            app.sync_chrome();
            // The main window has no background brush, so anything the tab
            // area does not cover keeps the *previous* frame's pixels: after
            // maximize/restore that shows up as a duplicate tab bar and black
            // bands. Erase + repaint on every resize, synchronously.
            unsafe {
                let _ = InvalidateRect(hwnd, None, true);
                let _ = UpdateWindow(hwnd);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_ERASEBKGND => {
            let app = app_from(hwnd);
            let (r, g, b) = app.cfg.get().color(|t| &t.tabbar_background);
            unsafe {
                let brush = CreateSolidBrush(COLORREF((b as u32) << 16 | (g as u32) << 8 | r as u32));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let _ = FillRect(HDC(wp.0 as *mut _), &rc, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
            LRESULT(1)
        }
        WM_KEYDOWN => {
            let app = app_from(hwnd);
            let vk = wp.0 as u16;
            let ctrl = GetKeyState(0x11) < 0;
            let shift = GetKeyState(0x10) < 0;
            let alt = GetKeyState(0x12) < 0;
            if let Some(a) = events::match_key(&app.keymap, ctrl, shift, alt, vk) {
                app.action(a);
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        // TranslateAccelerator converts matching key events into WM_COMMAND
        // targeted at the main window (works even when the Shell view has focus).
        windows::Win32::UI::WindowsAndMessaging::WM_COMMAND => {
            let cmd = (wp.0 & 0xffff) as usize; // accel cmd = keymap index + 1
            let app = app_from(hwnd);
            if cmd >= 1 {
                if let Some((_, action)) = app.keymap.get(cmd - 1) {
                    app.action(*action);
                    return LRESULT(0);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // Browser semantics (docs/双进程架构设计.md): several tabs → ask
            // first; confirming closes every FE window, then the frame. The
            // released-panes table keeps any survivor from re-adopting them.
            // One tab (or no tabs): just close, like every other window.
            let app = app_from(hwnd);
            if app.tabs.len() > 1 && !app.confirm_close_all_tabs() {
                return LRESULT(0);
            }
            app.close_all_tabs();
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        m if m == WM_APP_QUIT => {
            // Tray/CLI broadcasts this to every window process; here it means
            // "restore my panes and close this window" (WM_DESTROY does both).
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        // WM_ENDSESSION (logoff/shutdown): restore absorbed windows NOW —
        // once we're killed mid-session, Explorer keeps dangling refs to the
        // re-parented/destroyed windows (2026-11-17 crash loop).
        windows::Win32::UI::WindowsAndMessaging::WM_ENDSESSION => {
            let app = app_from(hwnd);
            if wp.0 != 0 {
                app.release_all();
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATE => {
            let app = app_from(hwnd);
            // low word: WA_INACTIVE(0) / WA_ACTIVE(1) / WA_CLICKACTIVE(2)
            app.active = (wp.0 & 0xffff) as u32 != 0;
            if app.active {
                // Echo into the ledger so takeover works even without the
                // optional tray exe. The tray hook writes the same value.
                if let Some(l) = ledger() {
                    l.note_active(hwnd);
                }
            }
            app.sync_input_queue();
            app.sync_chrome();
            if app.active {
                if let Some(Some(v)) = app.views.get(app.tabs.active_index()) {
                    focus_view(app.main_hwnd, app.attached_tid, v);
                }
            }
            LRESULT(0)
        }
        m if m == WM_APP_CONFIG_CHANGED => {
            let app = app_from(hwnd);
            app.sync_tabbar();
            // [bookmarks].enabled may have flipped: re-sync the popup and
            // re-lay out so the Explorer view starts below/above it.
            app.sync_bookmarkbar_window();
            if !app.bookmarkbar_hwnd.0.is_null() {
                let _ = InvalidateRect(app.bookmarkbar_hwnd, None, true);
            }
            app.laid_out = None;
            app.layout();
            LRESULT(0)
        }
        m if m == WM_TIMER && wp.0 == SWEEP_TIMER_ID => {
            app_from(hwnd).sweep();
            LRESULT(0)
        }
        m if m == absorb_msg() => {
            // BHO now broadcasts to every RevolcMain. Only the ledger's
            // chosen window enters fast-sweep mode; the rest ignore it so
            // they don't fight over the same stray folder window.
            let app = app_from(hwnd);
            if app.may_adopt() {
                app.on_notify();
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            // Event-driven navigation from the BHO. Payload: "HWND\turl" UTF-8.
            // Validate the tag and length; a random WM_COPYDATA from another
            // app must never touch our tabs.
            let cds = lp.0 as *const COPYDATASTRUCT;
            if !cds.is_null() && (*cds).dwData == NAV_COPYDATA_TAG {
                let len = (*cds).cbData as usize;
                let ptr = (*cds).lpData as *const u8;
                tracing::info!(len, "WM_COPYDATA nav received");
                if !ptr.is_null() && len > 0 && len <= 16 * 1024 {
                    let payload = std::slice::from_raw_parts(ptr, len);
                    if let Ok(text) = std::str::from_utf8(payload) {
                        if let Some((hwnd_str, url)) = text.split_once('\t') {
                            if let Ok(explorer_hwnd) = hwnd_str.parse::<isize>() {
                                app_from(hwnd).on_explorer_navigated(explorer_hwnd, url);
                            }
                        }
                    }
                }
            }
            LRESULT(1) // non-zero = data accepted (WM_COPYDATA convention)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
