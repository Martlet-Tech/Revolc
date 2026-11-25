pub mod caption;
pub mod chrome;
pub mod dwm;
pub mod folder_icon;
pub mod gdi;
pub mod reorder;
pub mod settings;
pub mod tabbar;
pub mod tooltip;
pub mod view;

pub use revolc_common::icon;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GetDC, GetMonitorInfoW, InvalidateRect,
    MonitorFromWindow, RedrawWindow, ReleaseDC, ScreenToClient, SelectObject, SetWindowRgn,
    UpdateWindow, HBRUSH, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW, SRCCOPY,
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
    WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
    WM_SETICON, WM_SIZE, WM_TIMER, IDOK, MB_ICONQUESTION, MB_OKCANCEL, MB_SETFOREGROUND,
    MINMAXINFO, SM_CXDRAG, SW_SHOWNA, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME, WNDCLASSEXW,
};

use crate::bookmarks::BookmarkStore;
use crate::config::ConfigHandle;
use crate::shell::host::{self, Absorbed};
use crate::tabs::events::{self, Action, Binding};
use crate::tabs::TabList;
use revolc_common::ledger::Ledger;

use self::view::{PaintedPage, View};

pub const WM_APP_CONFIG_CHANGED: u32 = 0x8000; // posted by config-watcher thread
/// Quit for real: restore every absorbed window and exit. Shutdown paths
/// (tray, console handler, `--shutdown`) post this; WM_CLOSE only hides when
/// the close action is "hide".
pub const WM_APP_QUIT: u32 = 0x8010;
/// Tray -> hidden window: show it again. Must match common::messages.
pub const WM_APP_RESTORE: u32 = 0x8011;
/// Tray -> window: close all tabs (their Explorer panes) and the frame.
pub const WM_APP_CLOSE_TABS: u32 = 0x8012;
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
/// Bookmark/toolbar row below the tabs. Part of the same chrome popup since
/// the unification change — not a separately hideable window any more.
const BOOKMARK_HEIGHT: i32 = 28;
/// Total height of the unified chrome popup (tabs + toolbar). The Explorer
/// view starts here.
const CHROME_HEIGHT: i32 = TABBAR_HEIGHT + BOOKMARK_HEIGHT;
/// Right-edge toolbar button that opens the settings tab.
const SETTINGS_BTN_W: i32 = 28;
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
/// Owned popup for the hover tooltip. A separate top-level window because the
/// card hangs below the toolbar row, past the strip's CHROME_HEIGHT, and must
/// float above the absorbed Explorer pane like the strip does.
const TOOLTIP_CLASS: PCWSTR = w!("RevolcTooltip");
/// Context-menu command IDs. Kept in the WM_USER range so they cannot collide
/// with accelerators (which use 1..=keymap.len()).
const CMD_TAB_NEW_WINDOW: usize = 0x8001;
const CMD_TAB_CLOSE: usize = 0x8002;
const CMD_TAB_CLOSE_OTHERS: usize = 0x8003;
const CMD_TAB_CLOSE_RIGHT: usize = 0x8004;
const CMD_TAB_NEW_AFTER: usize = 0x8005;
const CMD_TAB_BOOKMARK: usize = 0x8006;
const CMD_BOOKMARK_OPEN: usize = 0x8007;
const CMD_BOOKMARK_REMOVE: usize = 0x8008;

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
    /// Spawned by the tray to adopt an already-open folder window. Start with
    /// zero tabs so that folder becomes the first tab, not an extra one.
    adopt_only: bool,
    /// Tray "打开设置": open the control panel even when takeover is on.
    settings_only: bool,
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
    /// One content slot per tab, by index. `Pending` = waiting for a foreign
    /// window; `Hosted` = an Explorer pane; `Painted` = drawn by us.
    views: Vec<View>,
    /// HWNDs we deliberately left on the desktop (closed tabs), with the
    /// instant they were released. Sweep ignores them until EXCLUDE_TTL.
    excluded: Vec<(isize, Instant)>,
    /// Reentrancy guard: absorb moves windows around, which can re-enter
    /// this app's message handlers.
    sweeping: bool,
    /// True between WM_ENTERSIZEMOVE and WM_EXITSIZEMOVE. Freezes all
    /// cross-process geometry/repaint work on the absorbed Explorer pane
    /// while the user drags the frame -- see Absorbed::place_client.
    in_sizemove: bool,
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
    /// BHO navigation pushes for windows this process has not absorbed yet.
    /// Explorer fires NavigateComplete2 the moment a new folder window
    /// navigates — long before our sweep absorbs it — so the initial path of
    /// every tab arrives here. Consumed by `absorb_window`; expires with the
    /// entry TTL so vanished windows do not accumulate.
    nav_cache: Vec<(isize, String, Instant)>,
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
    /// Real child window that owns the chrome pixels and hits (tabs + toolbar
    /// in one popup). Kept above every Explorer pane so their overhang cannot
    /// cover it.
    tabbar_hwnd: HWND,
    main_hwnd: HWND,
    /// TME_LEAVE/TME_NONCLIENT registered: the leave message arrives once per
    /// run, and it is the only way to drop a hover that the cursor walked out
    /// of without a further hit test.
    hover_tracking: bool,
    /// Left button went down on a tab and the pointer has not yet crossed the
    /// system drag threshold. `None` once a click is dispatched or a drag
    /// starts.
    tab_press: Option<TabPress>,
    /// State of the painted settings tab (selected category). Only meaningful
    /// while a Painted(Settings) view is active.
    settings_panel: settings::SettingsPanel,
    /// Flat list of saved folders, painted on the toolbar row and toggled from
    /// the tab context menu. Loaded at startup, saved atomically on change.
    bookmarks: BookmarkStore,
    /// Pending press / active drag for toolbar bookmark chips. Shares the tab
    /// strip's `reorder` state machine — only the slot geometry differs.
    bookmark_press: Option<reorder::Press>,
    bookmark_drag: Option<reorder::Drag>,
    /// Chip under the pointer on the toolbar row; painted with the hover fill.
    bookmark_hover: Option<usize>,
    /// Whether the right-end settings gear is under the pointer.
    settings_hover: bool,
    /// Owned popup that paints the hover tooltip. Kept above the Explorer
    /// panes; click-through so it never steals input from the strip.
    tooltip_hwnd: HWND,
    /// Current tooltip content, or None when nothing is hovered.
    tooltip_content: Option<tooltip::Content>,
    /// Measured card, computed when the hovered target changes.
    tooltip_layout: Option<tooltip::Layout>,
}

pub struct MainWindow {
    hwnd: HWND,
}

impl MainWindow {
    /// The single window owned by this `revolc.exe` process.
    pub fn new(
        cfg: Arc<ConfigHandle>,
        initial_path: Option<String>,
        adopt_only: bool,
        settings_only: bool,
    ) -> windows::core::Result<Self> {
        Self::create(cfg, initial_path, adopt_only, settings_only)
    }

    fn create(
        cfg: Arc<ConfigHandle>,
        initial_path: Option<String>,
        adopt_only: bool,
        settings_only: bool,
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
                adopt_only,
                settings_only,
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

/// Class for the owned tooltip popup. Same tolerate-already-exists rule.
fn ensure_tooltip_class() {
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tooltip_wndproc),
        hInstance: unsafe { GetModuleHandleW(None) }.unwrap_or_default().into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        hbrBackground: HBRUSH(std::ptr::null_mut()),
        lpszClassName: TOOLTIP_CLASS,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&wc) } != 0 {
        return;
    }
    let err = windows::core::Error::from_win32();
    if (err.code().0 as u32 & 0xffff) != 1410 {
        tracing::warn!(error = %err, "register tooltip class failed");
    }
}

/// The tooltip popup paints only when the owner has content for it. All input
/// passes through (WS_EX_TRANSPARENT) so it can never steal the hover/click
/// from the chip or tab underneath. The owner drives show/hide and position.
unsafe extern "system" fn tooltip_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let parent = GetParent(hwnd).unwrap_or_default();
            let Some(app) = (if parent.0.is_null() { None } else { app_opt(parent) }) else {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            };
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            app.paint_tooltip(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
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
        let takeover_on = args.cfg.get().takeover.mode == "redirect";
        let adopt_only = args.adopt_only;
        let settings_only = args.settings_only;
        let tabbar = tabbar::TabBar::new(&args.cfg.get());
        let mut app = Box::new(App {
            cfg: args.cfg,
            keymap: events::default_keymap(),
            accel: args.accel,
            tabs: TabList::new(),
            views: Vec::new(),
            excluded: Vec::new(),
            sweeping: false,
            in_sizemove: false,
            burst_until: None,
            attached_tid: 0,
            active: true,
            focus_pending: false,
            path_done: Vec::new(),
            reserve_deadline: None,
            discard: Vec::new(),
            nav_cache: Vec::new(),
            laid_out: None,
            last_client: (0, 0),
            tabbar,
            tabbar_hwnd: HWND::default(),
            main_hwnd: hwnd,
            hover_tracking: false,
            tab_press: None,
            settings_panel: settings::SettingsPanel::new(),
            bookmarks: BookmarkStore::open(crate::bookmarks::bookmarks_path()),
            bookmark_press: None,
            bookmark_drag: None,
            bookmark_hover: None,
            settings_hover: false,
            tooltip_hwnd: HWND::default(),
            tooltip_content: None,
            tooltip_layout: None,
        });
        if settings_only || !takeover_on {
            // Control panel, not an empty file manager. seed_settings_tab()
            // deliberately does not activate: WM_CREATE has not installed the
            // App pointer in GWLP_USERDATA yet (crash 2026-11-25).
            app.seed_settings_tab();
        } else if adopt_only {
            // The BHO is handing us an existing folder window. The first
            // sweep will adopt it into tab 0; spawning a Home tab here would
            // recreate Clover's "one folder = new window + spare tab" artifact.
            tracing::info!("started in adopt-only mode, waiting for stray folder window");
        } else {
            let first = args.initial_path.unwrap_or_else(home_dir);
            app.new_tab(first);
        }
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
        let top = CHROME_HEIGHT;
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
        let (r, g, border_rgb_b) = self.cfg.get().color(|t| &t.window_border);
        let color = gdi::rgb(r, g, border_rgb_b);
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
        let h = CHROME_HEIGHT.min(rc.bottom.max(1));
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
                chrome::paint(
                    hdc,
                    w,
                    TABBAR_HEIGHT,
                    &self.cfg.get(),
                    self.bookmarks.items(),
                    self.chrome_paint_state(w),
                );
                return;
            }
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            self.tabbar.paint(mem, w);
            chrome::paint(
                mem,
                w,
                TABBAR_HEIGHT,
                &self.cfg.get(),
                self.bookmarks.items(),
                self.chrome_paint_state(w),
            );
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
        // A painted tab (settings) has no shell folder: an empty path makes
        // the strip fall back to our own app icon for that tab.
        let paths: Vec<String> = self
            .tabs
            .iter_all()
            .enumerate()
            .map(|(i, t)| {
                let folder = self.views.get(i).map(|v| v.as_hosted().is_some()).unwrap_or(false);
                if folder { t.path.clone() } else { String::new() }
            })
            .collect();
        let width = self.client().right;
        self.tabbar.update(
            &ids,
            &labels,
            &paths,
            self.tabs.active_index(),
            &self.cfg.get(),
            width,
        );
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
        let band = RECT { left: 0, top: 0, right: rc.right, bottom: CHROME_HEIGHT };
        unsafe {
            // With WS_CLIPCHILDREN the parent no longer owns these pixels:
            // invalidate the window that actually paints them.
            let target = if self.tabbar_hwnd.0.is_null() { self.main_hwnd } else { self.tabbar_hwnd };
            let _ = InvalidateRect(target, Some(&band), false);
        }
    }

    /// Gather the toolbar row's transient interaction state in one value,
    /// shared by both paint paths (with and without a back buffer).
    fn chrome_paint_state(&self, width: i32) -> chrome::PaintState {
        chrome::PaintState {
            drag: self.bookmark_drag.map(|d| {
                (d.from, chrome::clamped_drag_left(d.raw, width))
            }),
            hover: self.bookmark_hover,
            settings_hover: self.settings_hover,
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
                CHROME_HEIGHT,
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
                CHROME_HEIGHT,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }

    /// Create the owned tooltip popup. Click-through and non-activating: the
    /// pointer must keep hitting the chip/tab beneath it, and showing a card
    /// must never steal focus from the frame or the absorbed Explorer pane.
    fn create_tooltip_window(&mut self) {
        ensure_tooltip_class();
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                TOOLTIP_CLASS,
                None,
                WS_POPUP,
                0,
                0,
                1,
                1,
                self.main_hwnd,
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                GetModuleHandleW(None).unwrap_or_default(),
                None,
            )
            .expect("create owned tooltip popup");
            self.tooltip_hwnd = hwnd;
        }
    }

    /// Paint the card using the stored layout. The window origin is (0,0) and
    /// the popup is already the right size, so no coordinate translation here.
    fn paint_tooltip(&self, hdc: HDC) {
        let (Some(content), Some(layout)) = (&self.tooltip_content, self.tooltip_layout) else {
            return;
        };
        unsafe {
            tooltip::paint(hdc, content, &self.cfg.get(), layout);
        }
    }

    /// Show a tooltip for a target: its content plus the target's rectangle in
    /// frame-client coordinates. `None` hides it. The card is anchored to the
    /// control (below its bottom edge, left-aligned), never to the pointer —
    /// so it appears in the same place no matter which direction the cursor
    /// entered from. The position is computed once on first appearance and
    /// locked while the same target stays hovered.
    fn set_tooltip(&mut self, target: Option<(tooltip::Content, RECT)>) {
        if self.tooltip_hwnd.0.is_null() {
            return;
        }
        let Some((content, control_client)) = target else {
            self.tooltip_content = None;
            self.tooltip_layout = None;
            unsafe {
                let _ = ShowWindow(self.tooltip_hwnd, SW_HIDE);
            }
            return;
        };
        // Same target -> keep the locked position, do not follow the pointer.
        if self.tooltip_content.as_ref() == Some(&content) {
            return;
        }

        unsafe {
            let hdc = GetDC(self.tooltip_hwnd);
            if hdc.is_invalid() {
                return;
            }
            let layout = tooltip::measure(hdc, &content);
            let _ = ReleaseDC(self.tooltip_hwnd, hdc);

            // Round the popup itself so no square corner shows behind the
            // card. SetWindowRgn takes ownership of the region.
            let rgn = windows::Win32::Graphics::Gdi::CreateRoundRectRgn(
                0, 0, layout.size.cx + 1, layout.size.cy + 1, 12, 12,
            );
            let _ = SetWindowRgn(self.tooltip_hwnd, rgn, true);

            // Control rect: strip client -> frame client (they share an
            // origin) -> screen. ClientToScreen on the frame handles DPI and
            // window placement in one step.
            let mut tl = POINT { x: control_client.left, y: control_client.top };
            let mut br = POINT { x: control_client.right, y: control_client.bottom };
            let _ = ClientToScreen(self.main_hwnd, &mut tl);
            let _ = ClientToScreen(self.main_hwnd, &mut br);
            let control_screen = RECT {
                left: tl.x,
                top: tl.y,
                right: br.x,
                bottom: br.y,
            };

            self.tooltip_content = Some(content);
            self.tooltip_layout = Some(layout);
            self.move_tooltip(layout, control_screen);
            let _ = InvalidateRect(self.tooltip_hwnd, None, true);
            let _ = ShowWindow(self.tooltip_hwnd, SW_SHOWNA);
        }
    }

    /// Position the popup relative to `control` (screen coords), flipping near
    /// the monitor edge and resizing to the measured card. SWP_NOACTIVATE
    /// keeps focus untouched.
    fn move_tooltip(&self, layout: tooltip::Layout, control: RECT) {
        unsafe {
            let monitor = MonitorFromWindow(self.main_hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(monitor, &mut mi);
            let at = tooltip::place(control, layout.size, mi.rcWork);
            let _ = SetWindowPos(
                self.tooltip_hwnd,
                HWND_TOP,
                at.x,
                at.y,
                layout.size.cx,
                layout.size.cy,
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

    /// Build the tooltip target for the current hover state: the one-line
    /// content plus the hovered control's rectangle in strip-client coords.
    /// The rectangle anchors the card to the control, not the pointer, so the
    /// position is identical however the cursor approached it. Paths come
    /// from the tab model — never `folder_path_of`, which consumes cached BHO
    /// pushes and would corrupt navigation on hover.
    fn hover_tooltip_target(
        &self,
        caption_btn: Option<caption::Button>,
        over_tab: Option<usize>,
        over_bookmark: Option<usize>,
        over_settings: bool,
        maximized: bool,
        width: i32,
    ) -> Option<(tooltip::Content, RECT)> {
        if let Some(i) = over_bookmark {
            let bm = self.bookmarks.items().get(i)?;
            let rect = chrome::bookmark_rect(i, width)?;
            return Some((tooltip::Content(bm.path.clone()), rect));
        }
        if let Some(i) = over_tab {
            let tab = self.tabs.get(i)?;
            let rect = self.tabbar.tooltip_rect(i)?;
            return Some((tooltip::Content(tab.path.clone()), rect));
        }
        if over_settings {
            return Some((tooltip::Content("设置".into()), chrome::settings_rect(width)));
        }
        let b = caption_btn?;
        let label = match b {
            caption::Button::Minimize => "最小化",
            caption::Button::Maximize if maximized => "向下还原",
            caption::Button::Maximize => "最大化",
            caption::Button::Close => "关闭",
        };
        Some((tooltip::Content(label.into()), caption::rect(b, width)))
    }

    /// Track which caption button the cursor is over. Driven from
    /// WM_NCHITTEST: that is the one place every mouse move passes through,
    /// whether the point resolves to a client area (tabs) or a non-client one
    /// (caption buttons).
    fn update_hover(&mut self, x: i32, y: i32, width: i32) {
        // While a row is being dragged the highlight belongs to the item in
        // hand: repainting hover fills underneath it makes the band flicker.
        if self.tabbar.drag().is_some() || self.bookmark_drag.is_some() {
            self.set_tooltip(None);
            return;
        }
        let want = caption::hit(x, y, width);
        // The tab strip and the toolbar row are mutually exclusive vertical
        // bands. Resolve one hit and feed each painter only its own state.
        let (over_tab, over_plus, over_bookmark, over_settings) = if y < TABBAR_HEIGHT {
            match self.tabbar.hit(x, y, width) {
                tabbar::Hit::Tab(i) | tabbar::Hit::Close(i) => (Some(i), false, None, false),
                tabbar::Hit::Plus => (None, true, None, false),
                _ => (None, false, None, false),
            }
        } else if (TABBAR_HEIGHT..CHROME_HEIGHT).contains(&y) {
            let total = self.bookmarks.items().len();
            let settings = chrome::hit_settings(x, y, width);
            (
                None,
                false,
                if settings { None } else { chrome::hit_bookmark(x, y, width, total) },
                settings,
            )
        } else {
            (None, false, None, false)
        };
        let maximized = self.is_maximized();
        let active = self.active;
        let mut dirty = self.tabbar.set_chrome(want, maximized, active);
        if self.tabbar.set_hover_tab(over_tab) {
            dirty = true;
        }
        if self.tabbar.set_hover_plus(over_plus) {
            dirty = true;
        }
        if self.bookmark_hover != over_bookmark {
            self.bookmark_hover = over_bookmark;
            dirty = true;
        }
        if self.settings_hover != over_settings {
            self.settings_hover = over_settings;
            dirty = true;
        }
        if dirty {
            self.invalidate_band();
        }

        // Tooltip follows the same hit resolution as the hover fills: caption
        // buttons and the gear get a single line; tabs and bookmark chips get
        // title + full path. Built as owned values so set_tooltip can compare
        // without borrowing self twice.
        let target = self.hover_tooltip_target(
            want, over_tab, over_bookmark, over_settings, maximized, width,
        );
        self.set_tooltip(target);

        if (want.is_some()
            || over_tab.is_some()
            || over_plus
            || over_bookmark.is_some()
            || over_settings)
            && !self.hover_tracking
        {
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
        if self.tabbar.set_hover_plus(false) {
            dirty = true;
        }
        if self.bookmark_hover.take().is_some() {
            dirty = true;
        }
        if self.settings_hover {
            self.settings_hover = false;
            dirty = true;
        }
        if dirty {
            self.invalidate_band();
        }
        self.set_tooltip(None);
    }

    /// Index of the tab with `id`, if it still exists.
    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter_all().position(|t| t.id == id)
    }

    /// Event-driven navigation from the BHO: `hwnd` is the CabinetWClass the
    /// BHO saw navigate; find the tab holding that exact window and update its
    /// path. Matching by HWND (not URL) keeps same-folder tabs independent.
    fn on_explorer_navigated(&mut self, pid: isize, _hwnd: isize, url: &str) {
        let Some(path) = host::file_url_to_path(url) else {
            tracing::debug!(?url, "BHO navigation: non-filesystem URL, ignored");
            return;
        };
        // Match the push to a tab by LABEL — the Explorer window title IS the
        // current folder name and is the one signal that never lies: HWND is
        // rewritten by re-parenting (get_HWND returns RevolcMain), while the
        // pid match proved flaky in hand tests. The push's URL tail segment
        // (".../Downloads" -> "Downloads") equals the tab label of the
        // navigating window. Ambiguity (two tabs on same-named folders) is
        // resolved in favour of the ACTIVE tab; no match -> cache and replay
        // on the next sweep tick, when refresh_labels has caught up.
        if let Some(i) = self.tab_by_label_tail(&path) {
            let old = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
            if old != path {
                tracing::info!(tab = i, ?old, new = ?path, "tab navigated (BHO event)");
                self.tabs.set_path(i, path);
            }
            return;
        }
        // pid fallback (kept as a second opinion; label may lag one sweep).
        let idx = self.views.iter().position(|v| {
            v.as_hosted().map(|v| host::process_id_of(v.top) as isize == pid).unwrap_or(false)
        });
        let Some(i) = idx else {
            self.cache_nav(pid, path);
            return;
        };
        let old = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
        if old != path {
            tracing::info!(tab = i, ?pid, ?old, new = ?path, "tab navigated (BHO event, pid)");
            self.tabs.set_path(i, path);
        }
    }

    /// Last path segment as a folder name ("D:\a\b" -> "b", "D:\" -> "").
    fn label_tail(path: &str) -> &str {
        path.trim_end_matches('\\')
            .rsplit('\\')
            .next()
            .unwrap_or("")
    }

    /// The tab whose label equals `path`'s tail segment. One exact match wins;
    /// several matches resolve to the active tab (the user navigated in the
    /// tab they are looking at); zero or unresolvable -> None.
    fn tab_by_label_tail(&self, path: &str) -> Option<usize> {
        let want = Self::label_tail(path);
        if want.is_empty() {
            return None;
        }
        let hits: Vec<usize> = self
            .tabs
            .iter_all()
            .enumerate()
            .filter(|(i, t)| {
                t.label == want
                    && self.views.get(*i).and_then(|v| v.as_hosted()).is_some()
            })
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            1 => hits.first().copied(),
            _ if hits.contains(&self.tabs.active_index()) => Some(self.tabs.active_index()),
            _ => None,
        }
    }

    /// TTL for cached navigation pushes that could not be matched yet.
    const NAV_CACHE_TTL: Duration = Duration::from_secs(60);

    /// Store the latest unmatched BHO push. Keyed by explorer pid; consumed
    /// by label replay (sweep), absorb seeding, and folder_path_of.
    fn cache_nav(&mut self, pid: isize, path: String) {
        self.nav_cache
            .retain(|(p, _, t)| p != &pid && t.elapsed() < Self::NAV_CACHE_TTL);
        self.nav_cache.push((pid, path.clone(), Instant::now()));
        tracing::debug!(?pid, ?path, "BHO push cached (no label/pid match yet)");
    }

    /// Reconcile cached pushes whose tab label has caught up with the push's
    /// folder name. Runs every sweep: the label lags the navigation by up to
    /// one tick, so a push that matched nothing on arrival matches here once
    /// refresh_labels has run.
    fn replay_nav_cache(&mut self) {
        let mut expired: Vec<usize> = Vec::new();
        for (idx, (_, path, t)) in self.nav_cache.iter().enumerate() {
            if t.elapsed() >= Self::NAV_CACHE_TTL {
                expired.push(idx);
                continue;
            }
            if let Some(i) = self.tab_by_label_tail(path) {
                let old = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
                if old != *path {
                    tracing::info!(tab = i, ?old, new = ?path, "tab navigated (BHO replay)");
                    self.tabs.set_path(i, path.clone());
                }
                expired.push(idx);
            }
        }
        for idx in expired.into_iter().rev() {
            self.nav_cache.remove(idx);
        }
    }

    /// The cached push for a just-absorbed window, by pid (absorb seeding).
    /// Consumes the entry.
    fn take_cached_nav(&mut self, pid: isize) -> Option<String> {
        let idx = self
            .nav_cache
            .iter()
            .position(|(p, _, t)| *p == pid && t.elapsed() < Self::NAV_CACHE_TTL)?;
        let (_, path, _) = self.nav_cache.remove(idx);
        Some(path)
    }

    /// Right-click moment: consume any cached push that matches THIS tab's
    /// label, before falling back to the stored path. Labels are the only
    /// always-correct live signal, and the label-matching replay may not have
    /// run yet when the menu fires.
    fn take_cached_nav_by_label(&mut self, label: &str) -> Option<String> {
        let idx = self.nav_cache.iter().position(|(_, path, t)| {
            t.elapsed() < Self::NAV_CACHE_TTL && Self::label_tail(path) == label && !label.is_empty()
        })?;
        let (_, path, _) = self.nav_cache.remove(idx);
        Some(path)
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
        if self.cfg.get().takeover.mode != "redirect" {
            // Every entry point (shortcut, toolbar +, bookmark) funnels here.
            // No absorption pipeline while uninstalled: show the control panel
            // instead of spawning a system Explorer window we cannot adopt.
            tracing::info!("new tab blocked while takeover is off");
            self.open_settings_tab();
            return;
        }
        let at = self.tabs.len();
        self.tabs.push(path.clone());
        self.views.insert(at, View::Pending);
        self.launch_tab_view(at, path);
    }

    /// Context menu "new tab to the right": insert after `after` and open it.
    fn new_tab_after(&mut self, after: usize, path: String) {
        if self.cfg.get().takeover.mode != "redirect" {
            tracing::info!("new tab blocked while takeover is off");
            self.open_settings_tab();
            return;
        }
        let at = (after + 1).min(self.tabs.len());
        self.tabs.insert_after(after, path.clone());
        self.views.insert(at, View::Pending);
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

    /// Context menu "收藏此标签页": toggle the tab's live folder in the flat
    /// bookmark list. Uses the same real-time path resolution as "open in new
    /// window" (BHO push + label), never the possibly-stale `Tab.path`.
    fn toggle_bookmark(&mut self, i: usize) {
        // Only a real folder (Hosted) can be bookmarked — a Painted settings
        // tab has no path worth saving.
        if !self.views.get(i).map(|v| v.as_hosted().is_some()).unwrap_or(false) {
            return;
        }
        let path = self.folder_path_of(i);
        let already = self.bookmarks.contains(&path);
        if already {
            self.bookmarks.remove(&path);
            tracing::info!(?path, tab = i, "bookmark removed");
        } else {
            let label = self
                .tabs
                .get(i)
                .map(|t| t.label.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| short_label(&path));
            self.bookmarks.add(label, path.clone());
            tracing::info!(?path, tab = i, "bookmark added");
        }
        // Chip count changed: a stale hover index could light up a neighbour.
        self.bookmark_hover = None;
        self.set_tooltip(None);
        self.refresh_band();
    }

    /// Toolbar click on a bookmark: open it in a new tab in this window, at
    /// the same position a "+" click would use. Never replaces the current
    /// tab — bookmarks are navigational shortcuts, not tab moves.
    fn open_bookmark(&mut self, index: usize) {
        if self.cfg.get().takeover.mode != "redirect" {
            self.open_settings_tab();
            return;
        }
        let Some(bm) = self.bookmarks.items().get(index).cloned() else {
            return;
        };
        tracing::info!(path = ?bm.path, "bookmark opened");
        self.new_tab(bm.path);
    }

    /// A left press on a bookmark chip: remember a stable key (the chip index)
    /// and the press x, exactly like `begin_tab_press`. The key is re-resolved
    /// on drag so a reorder mid-press cannot retarget the wrong chip.
    fn begin_bookmark_press(&mut self, index: usize, x: i32) {
        self.bookmark_press = Some(reorder::Press { key: index as u64, x });
    }

    /// Mouse move while a bookmark chip may be held. Mirrors `tab_drag_track`
    /// with the bookmark bar's slot geometry; the actual landing decision is
    /// the same `reorder` code the tab strip uses.
    fn bookmark_drag_track(&mut self, x: i32) -> bool {
        if let Some(press) = self.bookmark_press {
            let threshold = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(1);
            if !reorder::crossed(press.x, x, threshold) {
                return false;
            }
            self.bookmark_press = None;
            let from = press.key as usize;
            let width = self.client().right;
            // Only visible chips can be dragged; guard against a resize that
            // shrank the bar between press and drag.
            let visible = chrome::visible_bookmarks(width, self.bookmarks.items().len());
            if from >= visible {
                return false;
            }
            let left = chrome::slot_x(from);
            self.bookmark_drag = Some(reorder::Drag::new(from, press.x, left));
            unsafe {
                SetCapture(self.main_hwnd);
            }
        }
        let Some(mut d) = self.bookmark_drag else {
            return false;
        };
        d.move_to(x);
        let width = self.client().right;
        let visible = chrome::visible_bookmarks(width, self.bookmarks.items().len());
        if let Some(to) = d.landing(
            chrome::slot_width(),
            chrome::slot_gap(),
            chrome::slot_origin(),
            visible,
        ) {
            let from = d.from;
            if self.bookmarks.move_to(from, to) {
                d.set_from(to);
                tracing::info!(from, to, "bookmark reordered");
            }
        }
        self.bookmark_drag = Some(d);
        self.invalidate_band();
        true
    }

    /// Drop the held bookmark chip. The model already holds the final order.
    fn end_bookmark_drag(&mut self) {
        self.bookmark_press = None;
        if self.bookmark_drag.is_none() {
            return;
        }
        self.bookmark_drag = None;
        self.bookmark_hover = None;
        self.set_tooltip(None);
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate_band();
    }

    /// Bookmark chip context menu "取消收藏": delete the saved item and
    /// repaint the toolbar so the chip disappears immediately.
    fn remove_bookmark(&mut self, index: usize) {
        let Some(bm) = self.bookmarks.items().get(index).cloned() else {
            return;
        };
        if self.bookmarks.remove_at(index) {
            tracing::info!(path = ?bm.path, "bookmark removed via chip menu");
            self.bookmark_hover = None;
            self.set_tooltip(None);
            self.refresh_band();
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
        let idx = match self.views.iter().position(|v| v.is_pending()) {
            Some(i) => i,
            None => {
                self.tabs.push(String::new());
                self.views.push(View::Pending);
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
        self.views[idx] = View::Hosted(view);
        // Absorbing into a hidden/minimized Revolc looks like "nothing
        // happened" - surface the window so the user sees the result. Must not
        // un-maximize: this runs on every `+` click.
        self.surface();
        // The window is usually adopted while Explorer still keeps it hidden
        // (that is the whole point — see stray_folder_windows). Everything
        // below that needs a *live, navigated* browser is therefore deferred.
        let visible = host::is_visible(top);

        // Best-effort path only for reserved tabs (Ctrl+T / stray adoption).
        // When we spawned the window ourselves the tab already carries the
        // path — never stall the UI thread on IShellWindows (out-of-proc COM,
        // 50ms per retry) for information we already have. A window that has
        // not navigated yet has no path at all, and asking would burn the
        // retries for nothing: resolve it on a later sweep instead.
        let need_path = self.tabs.get(idx).map(|t| t.path.is_empty()).unwrap_or(true);
        if need_path && visible {
            // Preferred source: the BHO pushed this window's initial folder
            // URL before we absorbed it (cached by on_explorer_navigated).
            // That read happens INSIDE Explorer and is exact; the IShellWindows
            // fallback below is cross-process and blind to re-parented
            // windows, so it only runs when no push ever arrived (e.g. Revolc
            // started after the window did).
            let pid = host::process_id_of(top) as isize;
            if let Some(cached) = self.take_cached_nav(pid) {
                tracing::info!(?top, ?pid, path = ?cached, "absorb: seeding path from BHO cache");
                self.tabs.set_path(idx, cached);
            } else {
                let store = host::read_folder_path(top, 1).unwrap_or_default();
                if !store.is_empty() {
                    self.tabs.set_path(idx, store);
                }
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
        self.views
            .iter()
            .filter_map(|v| v.as_hosted().map(|h| h.top.0 as isize))
            .collect()
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
            .filter_map(|(i, v)| v.is_pending().then_some(i))
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
            let waiting = self.views.iter().any(|v| v.is_pending());
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
                View::Hosted(v) if !host::is_window_alive(v.top) => Some(i),
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
    ///
    /// Path tracking is NOT here: `Tab.path` is updated by the BHO's
    /// NavigateComplete2/DocumentComplete push (WM_COPYDATA, see
    /// `on_explorer_navigated`), which is event-driven and reads the URL
    /// inside the Explorer process. A title change no longer triggers an
    /// IShellWindows read — that reader cannot see re-parented windows and
    /// cross-wired tabs to whichever shell window matched first.
    fn refresh_labels(&mut self) -> bool {
        let mut changed = false;
        for (i, slot) in self.views.iter().enumerate() {
            if let Some(v) = slot.as_hosted() {
                let title = host::window_title(v.top);
                if title.is_empty() {
                    continue;
                }
                let (old, path) = self.tabs.get(i)
                    .map(|t| (t.label.clone(), t.path.clone()))
                    .unwrap_or_default();
                if self.tabs.set_label(i, title.clone()) {
                    // The tray menu reads labels from the per-window MRU, not
                    // from the tab strip. Keep the MRU in lockstep with the
                    // live Explorer title so navigation ("此电脑", "下载")
                    // shows up there too instead of a stale absorbed name.
                    if old.is_empty() {
                        // On first fill, activate() wrote short_label(path)
                        // into the MRU as a placeholder. Replace that entry.
                        revolc_common::tab_mru::rename(self.main_hwnd, &short_label(&path), &title);
                    } else {
                        revolc_common::tab_mru::rename(self.main_hwnd, &old, &title);
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    /// Fill in the folder path of tabs that were absorbed before their window
    /// had navigated (the flash-free path). Once per window, only when it is
    /// on screen and titled — IShellWindows is out-of-proc and costs 50ms.
    fn resolve_pending_paths(&mut self) {
        for i in 0..self.views.len() {
            let (top, titled, visible) = match self.views[i].as_hosted() {
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
            if let Some(p) = host::read_folder_path(top, 1) {
                self.tabs.set_path(i, p);
            }
        }
        let known = self.known_tops();
        self.path_done.retain(|h| known.contains(h));
    }

    /// Folder a tab is actually showing right now. `Tab::path` is only what
    /// the tab was opened with; the user may have navigated since. Used by the
    /// context menu (which can target a non-active tab) and by `+` / shortcuts.
    /// The tab's live folder. `Tab.path` is kept fresh by the BHO's
    /// navigation push (see `on_explorer_navigated`) — reading IShellWindows
    /// here was REMOVED on purpose: it cannot see re-parented windows and
    /// cross-wired tabs to whichever shell window matched first (the original
    /// "new window opens the wrong folder" bug).
    fn folder_path_of(&mut self, i: usize) -> String {
        // Right-click is a synchronization point: consume any cached BHO push
        // whose folder name equals this tab's label BEFORE falling back to the
        // stored path — the sweep replay may not have run yet.
        let label = self.tabs.get(i).map(|t| t.label.clone()).unwrap_or_default();
        if let Some(cached) = self.take_cached_nav_by_label(&label) {
            tracing::info!(tab = i, ?label, path = ?cached, "folder_path_of: consumed cached push");
            self.tabs.set_path(i, cached);
        }
        let stored = self.tabs.get(i).map(|t| t.path.clone()).unwrap_or_default();
        if stored.is_empty() {
            tracing::warn!(tab = i, "folder_path_of: Tab.path empty (no BHO push seen yet)");
        }
        self.tabs
            .get(i)
            .map(|t| t.path.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(home_dir)
    }

    fn active_folder_path(&mut self) -> String {
        let active = self.tabs.active_index();
        // A Painted tab (e.g. settings) has no filesystem folder. Opening a
        // new tab from there must not try to spawn Explorer at the literal
        // label "设置" (seen in 2026-11-23 logs as
        // `explorer spawned for absorb path="设置"`). Fall back to home.
        if !self.views.get(active).map(|v| v.as_hosted().is_some()).unwrap_or(false) {
            tracing::debug!(tab = active, "active tab is not a folder, new tab uses home");
            return home_dir();
        }
        self.folder_path_of(active)
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
        if self.views.iter().any(|v| v.is_pending()) {
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
        // The global table is pruned HERE, in place (write lock): it only
        // ever grows via park_released, and pruning the local copy alone
        // left the process-global table growing forever (X光体检报告 §3.2).
        let mut excl: Vec<(isize, Instant)> = self.excluded.clone();
        if let Ok(mut g) = RELEASED_WINDOWS.lock() {
            g.retain(|(_, t)| t.elapsed() < EXCLUDE_TTL);
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
        let expecting = self.burst_until.is_some() || self.views.iter().any(|v| v.is_pending());
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
        self.replay_nav_cache();
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
    /// Standalone (no &mut App): MessageBoxW runs a modal loop that dispatches
    /// messages — WM_TIMER(sweep) re-enters app_from and would alias the &mut
    /// App a caller is still holding (X光体检报告 §3.3). Callers drop their
    /// borrow before invoking this, exactly like the WM_RBUTTONUP three-phase
    /// pattern.
    fn confirm_close_all_tabs(host: HWND, n: usize) -> bool {
        let text = format!("要关闭全部 {n} 个标签页吗？\r\n每个标签页的文件夹窗口会一并关闭。");
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let r = unsafe {
            MessageBoxW(
                host,
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
            if let View::Hosted(v) = std::mem::replace(slot, View::Pending) {
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
        if self.views.get(i).map(|v| v.is_pending()).unwrap_or(false) {
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
        // A Painted tab owns no HWND — nothing to close — the remove below is
        // the whole teardown.
        if let View::Hosted(v) = std::mem::replace(&mut self.views[i], View::Pending) {
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
        // Tray menu shows the three most recently used tabs of this window.
        // Use the same label source as the strip; fall back to the short path
        // while a reserved tab is still waiting for its Explorer pane.
        let label = self.tabs.get(i)
            .and_then(|t| if t.label.is_empty() { None } else { Some(t.label.clone()) })
            .unwrap_or_else(|| self.tabs.get(i).map(|t| short_label(&t.path)).unwrap_or_default());
        revolc_common::tab_mru::note_activation(self.main_hwnd, &label);
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
        if let Some(v) = self.views.get_mut(i).and_then(|slot| slot.as_hosted_mut()) {
            v.set_visible(true);
        }
        for (j, slot) in self.views.iter_mut().enumerate() {
            if j != i {
                if let Some(v) = slot.as_hosted_mut() {
                    v.set_visible(false);
                }
            }
        }
        self.redraw_all();
        self.refresh_band();
        // Keyboard focus has to be handed to the other process explicitly, or
        // the address bar takes clicks but no keystrokes. A window adopted
        // before Explorer showed it cannot take focus yet: the sweep retries
        // as soon as it appears (`focus_pending`). A Painted tab has no foreign
        // window: focus stays on our own frame and no queue is attached.
        self.focus_pending = match self.views.get(i).map(|v| v.as_hosted()).flatten() {
            Some(v) if host::is_visible(v.top) => {
                focus_view(self.main_hwnd, self.attached_tid, v);
                false
            }
            Some(_) => true,
            None => false,
        };
        self.sync_input_queue();
        self.sync_tabbar();
        // A Painted tab needs our own frame to own the keyboard focus; there is
        // no Explorer child to hand it to.
        if self.views.get(i).map(|v| v.is_painted()).unwrap_or(false) {
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(self.main_hwnd);
            }
        }
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
                .and_then(|v| v.as_hosted())
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

    /// Create the settings tab without activating it. Used during WM_CREATE,
    /// where activate() -> SetFocus would re-enter WM_NCACTIVATE before the
    /// App pointer is installed in GWLP_USERDATA.
    fn seed_settings_tab(&mut self) {
        let at = self.tabs.len();
        self.tabs.push("设置".to_string());
        self.views.push(View::Painted(PaintedPage::Settings));
        self.sync_tabbar();
        tracing::info!(tab = at, "settings tab opened");
    }

    /// Open the settings tab, or focus it if one is already open (singleton).
    fn open_settings_tab(&mut self) {
        if let Some(i) = self.views.iter().position(|v| v.is_painted()) {
            self.activate(i);
            return;
        }
        let at = self.tabs.len();
        self.tabs.push("设置".to_string());
        self.views.push(View::Painted(PaintedPage::Settings));
        self.sync_tabbar();
        self.activate(at);
        tracing::info!(tab = at, "settings tab opened");
    }

    /// Turn an uninstalled control-panel window into a workbench: drop the
    /// Painted settings tab and open one real folder tab. Scheduled only when
    /// the user switches 接管 from 否 to 是.
    fn enter_workbench(&mut self) {
        let painted: Vec<usize> = self
            .views
            .iter()
            .enumerate()
            .filter(|(_, v)| v.is_painted())
            .map(|(i, _)| i)
            .collect();
        for i in painted.into_iter().rev() {
            self.tabs.remove(i);
            self.views.remove(i);
        }
        if self.tabs.is_empty() {
            self.new_tab(home_dir());
        } else {
            let active = self.tabs.active_index().min(self.tabs.len() - 1);
            self.activate(active);
            self.sync_tabbar();
            self.layout();
        }
    }

    /// Apply a settings-panel click. The value, validation and TOML keys all
    /// come from the `SettingId` registry; this method only reads the current
    /// config, asks the registry for the next value, and writes the file once.
    /// The watcher repaints both the settings tab and the main window via
    /// WM_APP_CONFIG_CHANGED.
    fn handle_settings_hit(&mut self, hit: settings::SettingsHit) {
        use settings::SettingsHit::*;
        match hit {
            Category(i) => {
                // The panel owns its selected category; mutate through a
                // short-lived &mut borrow, then repaint.
                if i < crate::ui::settings::CATEGORY_COUNT {
                    self.settings_panel.select(i);
                    self.invalidate_view();
                }
            }
            Setting(id) => {
                let path = self.cfg.path().clone();
                let text = match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error = %e, "settings: failed to read config");
                        return;
                    }
                };

                if id == revolc_common::settings::SettingId::TakeoverMode {
                    // The user model has no "pause": 是 means work, 否 means
                    // uninstall. UAC is modal, so both branches run off-thread
                    // and post a repaint when done.
                    let enabling = self.cfg.get().takeover.mode != "redirect";
                    let hwnd_raw = self.main_hwnd.0 as usize;
                    if enabling {
                        match revolc_common::settings::apply(&self.cfg.get(), &text, id) {
                            Ok(updated) => {
                                if let Err(e) = std::fs::write(&path, updated) {
                                    tracing::error!(error = %e, "settings: failed to write config");
                                    return;
                                }
                            }
                            Err(label) => {
                                tracing::warn!(setting = label, "settings: invalid value rejected");
                                return;
                            }
                        }
                        // Make the new state visible immediately: sweep and
                        // arm_burst read the config handle, not the file.
                        self.cfg.reload();
                        self.enter_workbench();
                        std::thread::spawn(move || {
                            if !revolc_common::takeover::is_bho_installed() {
                                if let Err(e) = revolc_common::takeover::install_bho() {
                                    tracing::error!(error = %e, "BHO installation failed");
                                }
                            }
                            unsafe {
                                let _ = PostMessageW(
                                    HWND(hwnd_raw as _),
                                    revolc_common::messages::WM_APP_REGISTRY_CHANGED,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }
                        });
                    } else {
                        std::thread::spawn(move || {
                            let exe = std::env::current_exe().unwrap_or_default();
                            let dir = exe.parent().unwrap_or(std::path::Path::new("."));
                            if let Err(e) = revolc_common::takeover::launch_uninstaller(
                                &dir.join("revolc-tray.exe"),
                            ) {
                                tracing::error!(error = %e, "could not launch elevated uninstaller");
                            }
                            unsafe {
                                let _ = PostMessageW(
                                    HWND(hwnd_raw as _),
                                    revolc_common::messages::WM_APP_REGISTRY_CHANGED,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }
                        });
                    }
                    return;
                }

                match revolc_common::settings::apply(&self.cfg.get(), &text, id) {
                    Ok(updated) => {
                        if let Err(e) = std::fs::write(&path, updated) {
                            tracing::error!(error = %e, "settings: failed to write config");
                        }
                    }
                    Err(label) => tracing::warn!(setting = label, "settings: invalid value rejected"),
                }
            }
        }
    }

    /// Invalidate the main window's view area (the settings page lives there).
    fn invalidate_view(&self) {
        let rc = self.view_rect();
        unsafe {
            let _ = InvalidateRect(self.main_hwnd, Some(&rc), true);
            let _ = UpdateWindow(self.main_hwnd);
        }
    }

    fn action(&mut self, a: Action) {
        tracing::debug!(?a, "action dispatched");
        match a {
            Action::NewTab => {
                if self.cfg.get().takeover.mode != "redirect" {
                    // No absorption pipeline while uninstalled: + must not
                    // leave a system Explorer window on the desktop.
                    self.open_settings_tab();
                } else {
                    // Reuse where the user actually is, not where the tab was
                    // opened (a tab navigated to This PC must not reopen "zt").
                    let path = self.active_folder_path();
                    self.new_tab(path);
                }
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
        // NOT during a resize drag, though: RDW_ALLCHILDREN | RDW_UPDATENOW
        // marks the whole child subtree dirty and waits for it, and the
        // absorbed pane lives in explorer.exe -- which also owns the tray and
        // the DWM activation state that other apps' title bars follow. Doing
        // that on every WM_SIZE frame makes focus-sensitive apps stutter.
        if !self.in_sizemove {
            self.redraw_all();
        }
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

    /// Catch the absorbed pane up with the host's final geometry after a
    /// resize drag, then repaint once. This is the single cross-process
    /// update that replaces the dozens that were skipped during the drag.
    fn flush_after_sizing(&mut self) {
        self.laid_out = None;
        self.place_active();
        self.redraw_all();
    }

    fn place_active_sampled(&mut self, sampled: Option<host::Metrics>) {
        let idx = self.tabs.active_index();
        let rc = self.view_rect();
        let hosted = self.views.get(idx).and_then(|v| v.as_hosted());
        let metrics = hosted.map(|v| sampled.unwrap_or_else(|| v.sample()));
        let key = metrics.map(|m| (idx, rc.left, rc.top, rc.right, rc.bottom, (m.nc, m.band)));
        if key.is_some() && self.laid_out == key {
            return;
        }
        if let (Some(v), Some(m)) = (hosted, metrics) {
            // Align the *visible chrome* (ribbon onward) to the view rect.
            v.place_client(rc, m, self.in_sizemove);
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
            let Some(v) = slot.as_hosted() else { continue };
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
        let active_hosted = self.views.get(active).and_then(|v| v.as_hosted());
        let metrics = active_hosted.map(|v| v.sample());
        // Explorer moves and resizes its own frame when it finally shows it;
        // the `laid_out` key cannot notice that because *we* changed nothing.
        let drifted = match (active_hosted, metrics) {
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
            if let Some(v) = slot.as_hosted_mut() {
                v.mark_shown();
            }
        }

        // A tab absorbed before Explorer showed it could not take focus; hand
        // it over now that it is on screen.
        if self.focus_pending {
            match self.views.get(active).and_then(|v| v.as_hosted()) {
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
            .and_then(|v| v.as_hosted())
            .map(|v| v.top.0 as isize)
            .unwrap_or(0);
        for child in host::child_windows(self.main_hwnd) {
            let raw = child.0 as isize;
            if self.views.iter().filter_map(|v| v.as_hosted()).any(|v| v.top.0 as isize == raw) {
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
        for slot in self.views.iter_mut() {
            if let View::Hosted(v) = std::mem::replace(slot, View::Pending) {
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
unsafe fn show_tab_context_menu(
    hwnd: HWND,
    screen: POINT,
    has_tab: bool,
    // None = not a folder tab (painted page): hide the bookmark item.
    bookmarked: Option<bool>,
) -> usize {
    let Ok(menu) = CreatePopupMenu() else { return 0 };

    if has_tab {
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_NEW_WINDOW, w!("在新窗口打开此标签页(&N)"));
        // (moves the tab: closes it here + opens the same folder in a new window)
        let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_NEW_AFTER, w!("在右侧新建标签页(&A)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        if let Some(bookmarked) = bookmarked {
            let bookmark_label = if bookmarked {
                w!("取消收藏此标签页(&B)")
            } else {
                w!("收藏此标签页(&B)")
            };
            let _ = AppendMenuW(menu, MF_STRING, CMD_TAB_BOOKMARK, bookmark_label);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        }
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

/// Context menu for a bookmark chip on the toolbar row. Unlike the tab menu
/// this one manages the saved item itself: open here, open in a new window,
/// or remove (取消收藏). Modal — the caller must not hold `&mut App`.
unsafe fn show_bookmark_context_menu(hwnd: HWND, screen: POINT) -> usize {
    let Ok(menu) = CreatePopupMenu() else { return 0 };
    let _ = AppendMenuW(menu, MF_STRING, CMD_BOOKMARK_OPEN, w!("打开此收藏(&O)"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_BOOKMARK_REMOVE, w!("取消收藏(&R)"));
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
            app.create_tooltip_window();
            // Safe now: GWLP_USERDATA is installed, so the SetFocus a Painted
            // settings tab triggers can re-enter WM_NCACTIVATE without a null
            // App pointer.
            if !app.tabs.is_empty() {
                app.activate(0);
            }
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
            revolc_common::tab_mru::remove(hwnd);
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
            if app.tabbar.drag().is_some() || app.bookmark_drag.is_some() {
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
            // Toolbar row: the right-end settings gear and the bookmark chips
            // are the live targets. Checked before tabbar.hit() so the row
            // never falls into Hit::Band and starts a window drag.
            if chrome::hit_settings(x, y, width) {
                app.open_settings_tab();
                return LRESULT(0);
            }
            if (TABBAR_HEIGHT..CHROME_HEIGHT).contains(&y) {
                let total = app.bookmarks.items().len();
                if let Some(i) = chrome::hit_bookmark(x, y, width, total) {
                    // Remember the press; a click opens on LBUTTONUP, a drag
                    // reorders. Same shape as the tab strip's press/drag.
                    app.begin_bookmark_press(i, x);
                }
                return LRESULT(0);
            }
            // Route by vertical band first. The caption/tab strip always wins
            // over a Painted page — otherwise its X / min / max buttons and
            // tab strip become unclickable while settings is open.
            let painted_active = app
                .views
                .get(app.tabs.active_index())
                .map(|v| v.is_painted())
                .unwrap_or(false);
            if chrome::route_click(y, painted_active) == chrome::ClickRoute::PaintedPage {
                let rc = app.view_rect();
                if let Some(hit) = app.settings_panel.hit(x, y, rc, &app.cfg.get()) {
                    app.handle_settings_hit(hit);
                }
                return LRESULT(0);
            }
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
        // Double click on the empty *chrome* strip = maximize / restore,
        // matching what a real caption does (the class needs CS_DBLCLKS).
        // Only the band ABOVE the view rect counts: a double-click on a
        // Painted page (settings) or in a folder view must not maximize the
        // window. tabbar.hit() reports Band for every y >= TABBAR_HEIGHT, so
        // without the y guard the whole content area toggled maximize.
        WM_LBUTTONDBLCLK => {
            let app = app_from(hwnd);
            let (x, y) = point_from_lparam(lp);
            let width = app.client().right;
            if chrome::is_chrome_double_click(y)
                && app.tabbar.hit(x, y, width) == tabbar::Hit::Band
            {
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
            let (x, _y) = point_from_lparam(lp);
            let app = app_from(hwnd);
            if !app.tab_drag_track(x) {
                // Both rows share the same gesture; only one can be active.
                app.bookmark_drag_track(x);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (_x, y) = point_from_lparam(lp);
            let app = app_from(hwnd);
            // Snapshot the pending press BEFORE the end helpers clear it. A
            // press released without crossing the drag threshold is a click.
            let pending_press = app.bookmark_press.take();
            let was_dragging = app.bookmark_drag.is_some();
            app.end_tab_drag();
            app.end_bookmark_drag();
            if !was_dragging {
                if let Some(press) = pending_press {
                    let width = app.client().right;
                    let index = press.key as usize;
                    if (TABBAR_HEIGHT..CHROME_HEIGHT).contains(&y)
                        && index < chrome::visible_bookmarks(width, app.bookmarks.items().len())
                    {
                        app.open_bookmark(index);
                    }
                }
            }
            LRESULT(0)
        }
        // Capture stolen (Alt+Tab, a modal, another window grabbing the
        // mouse): drop the drag instead of leaving a tab/chip glued to cursor.
        WM_CAPTURECHANGED => {
            let app = app_from(hwnd);
            app.end_tab_drag();
            app.end_bookmark_drag();
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
            // Phase 1: gather target + screen point + menu kind, then DROP the
            // app borrow before the modal menu loop. Two kinds of target:
            //   * a bookmark chip on the toolbar row -> bookmark menu
            //   * a tab (or empty strip)               -> tab menu
            let (hit_index, screen, bookmarked, bm_index) = {
                let app = app_from(hwnd);
                let width = app.client().right;
                // The toolbar row is split from the tab strip: a right click
                // there hits a bookmark chip, not a tab.
                let bm = if (TABBAR_HEIGHT..CHROME_HEIGHT).contains(&y) {
                    chrome::hit_bookmark(x, y, width, app.bookmarks.items().len())
                } else {
                    None
                };
                let idx = if bm.is_none() {
                    match app.tabbar.hit(x, y, width) {
                        tabbar::Hit::Tab(i) | tabbar::Hit::Close(i) => app
                            .tabbar
                            .ids()
                            .get(i)
                            .copied()
                            .and_then(|id| app.index_of(id)),
                        _ => None,
                    }
                } else {
                    None
                };
                // Painted tabs (settings) have no folder — don't offer to
                // bookmark them. Only Hosted tabs are real folders.
                let marked = idx.and_then(|i| {
                    let is_folder = app
                        .views
                        .get(i)
                        .map(|v| v.as_hosted().is_some())
                        .unwrap_or(false);
                    if is_folder {
                        let p = app.folder_path_of(i);
                        Some(app.bookmarks.contains(&p))
                    } else {
                        None
                    }
                });
                let mut pt = POINT { x, y };
                let _ = ClientToScreen(hwnd, &mut pt);
                (idx, pt, marked, bm)
            };
            // Phase 2: modal loop — no &mut App alive here.
            let cmd = if bm_index.is_some() {
                show_bookmark_context_menu(hwnd, screen)
            } else {
                show_tab_context_menu(hwnd, screen, hit_index.is_some(), bookmarked)
            };
            // Phase 3: re-acquire and dispatch.
            if cmd != 0 {
                let app = app_from(hwnd);
                // Re-resolve by id in case the list shifted during the loop.
                let idx = hit_index;
                match cmd {
                    CMD_BOOKMARK_OPEN => {
                        if let Some(i) = bm_index {
                            app.open_bookmark(i);
                        }
                    }
                    CMD_BOOKMARK_REMOVE => {
                        if let Some(i) = bm_index {
                            app.remove_bookmark(i);
                        }
                    }
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
                    CMD_TAB_BOOKMARK => {
                        if let Some(i) = idx {
                            app.toggle_bookmark(i);
                        }
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
            // A Painted tab owns its view rect; Hosted tabs let the Explorer
            // child cover it (WS_CLIPCHILDREN keeps the two apart).
            let is_painted = app
                .views
                .get(app.tabs.active_index())
                .map(|v| v.is_painted())
                .unwrap_or(false);
            if is_painted {
                let rc = app.view_rect();
                app.settings_panel.paint(hdc, rc, &app.cfg.get());
            }
            app.paint_band(hdc);
            app.paint_frame(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOVE => {
            app_from(hwnd).sync_tabbar_window();
            LRESULT(0)
        }
        WM_ENTERSIZEMOVE => {
            app_from(hwnd).in_sizemove = true;
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            let app = app_from(hwnd);
            app.in_sizemove = false;
            // One catch-up + repaint for the final geometry instead of one
            // per resize frame.
            app.flush_after_sizing();
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
                let brush = CreateSolidBrush(gdi::rgb(r, g, b));
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
            // "hide" close action: the window just disappears from view; its
            // tabs and absorbed panes stay alive, and the permanent tray can
            // bring it back with WM_APP_RESTORE. It only makes sense while
            // taking over — an uninstalled hidden window would be an invisible
            // control panel nobody can restore meaningfully.
            let hide = {
                let app = app_from(hwnd);
                let cfg = app.cfg.get();
                cfg.tray.close_action == "hide" && cfg.takeover.mode == "redirect"
            };
            if hide {
                let _ = ShowWindow(hwnd, SW_HIDE);
                // The tab strip is an owned top-level popup, not a child: it
                // stays on screen unless we hide it explicitly. surface()
                // calls the same sync on restore.
                app_from(hwnd).sync_tabbar_window();
                tracing::info!("window hidden to tray");
                return LRESULT(0);
            }

            // Browser semantics (docs/双进程架构设计.md): several tabs → ask
            // first; confirming closes every FE window, then the frame. The
            // released-panes table keeps any survivor from re-adopting them.
            // One tab (or no tabs): just close, like every other window.
            //
            // Three-phase like WM_RBUTTONUP: the MessageBoxW in the confirm
            // runs a modal loop that dispatches WM_TIMER(sweep), which would
            // re-enter app_from and alias the &mut App still held here.
            // Phase 1: count tabs under the borrow, then drop it.
            let (multi, n) = {
                let app = app_from(hwnd);
                (app.tabs.len() > 1, app.tabs.len())
            };
            // Phase 2: modal loop — no &mut App alive here.
            if multi && !App::confirm_close_all_tabs(hwnd, n) {
                return LRESULT(0);
            }
            // Phase 3: re-acquire and dispatch.
            let app = app_from(hwnd);
            app.close_all_tabs();
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        m if m == WM_APP_RESTORE => {
            // Close button set to "hide": the tray asked us to come back.
            let app = app_from(hwnd);
            app.surface();
            let _ = SetForegroundWindow(hwnd);
            LRESULT(0)
        }
        m if m == WM_APP_CLOSE_TABS => {
            // Tray "close this window" / "close all": close every tab, which
            // posts WM_CLOSE to each absorbed Explorer pane (not restore),
            // then tear down the frame. WM_DESTROY still runs release_all for
            // any pane whose close has not landed yet — a restored pane is
            // better than a dangling re-parented window.
            let app = app_from(hwnd);
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
                if let Some(v) = app.views.get(app.tabs.active_index()).and_then(|s| s.as_hosted()) {
                    focus_view(app.main_hwnd, app.attached_tid, v);
                }
            }
            LRESULT(0)
        }
        m if m == WM_APP_CONFIG_CHANGED => {
            let app = app_from(hwnd);
            app.sync_tabbar();
            // The chrome is one popup now (tabs + toolbar). A config change
            // can repaint the toolbar colours; invalidate the whole band.
            app.invalidate_band();
            app.laid_out = None;
            app.layout();
            LRESULT(0)
        }
        m if m == revolc_common::messages::WM_APP_REGISTRY_CHANGED => {
            // Registry install/uninstall ran on a worker thread; refresh the
            // painted settings row with the new state.
            app_from(hwnd).invalidate_view();
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
                tracing::debug!(len, "WM_COPYDATA nav received");
                if !ptr.is_null() && len > 0 && len <= 16 * 1024 {
                    let payload = std::slice::from_raw_parts(ptr, len);
                    if let Ok(text) = std::str::from_utf8(payload) {
                        // "explorer_pid\tbrowser_hwnd\turl"
                        if let Some((pid_str, rest)) = text.split_once('\t') {
                            if let Some((hwnd_str, url)) = rest.split_once('\t') {
                                if let (Ok(pid), Ok(bhwnd)) =
                                    (pid_str.parse::<isize>(), hwnd_str.parse::<isize>())
                                {
                                    app_from(hwnd).on_explorer_navigated(pid, bhwnd, url);
                                }
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
