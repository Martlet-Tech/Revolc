pub mod tabbar;
pub mod tray;

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, InvalidateRect, RedrawWindow, UpdateWindow, HBRUSH,
    HDC, HGDIOBJ, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateAcceleratorTableW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, GetWindowLongPtrW, IsIconic, IsWindowVisible, IsZoomed, LoadCursorW,
    MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateAcceleratorW, TranslateMessage, ACCEL, CREATESTRUCTW,
    CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HACCEL, HWND_TOP, IDC_ARROW, MSG, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWMAXIMIZED, SW_SHOWNORMAL,
    WNDCLASSEXW, WM_CLOSE, WM_CREATE, WM_DESTROY,
    WM_KEYDOWN, WM_SIZE, WM_TIMER, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};

use crate::config::ConfigHandle;
use crate::shell::host::{self, Absorbed};
use crate::tabs::events::{self, Action, Binding};
use crate::tabs::TabList;

pub const WM_APP_CONFIG_CHANGED: u32 = 0x8000; // posted by config-watcher thread
pub const WM_APP_TAB_ACTIVATED: u32 = 0x8001; // lParam = tab index
pub const WM_APP_TAB_CLOSE: u32 = 0x8002; // lParam = tab index
pub const WM_APP_TAB_NEW: u32 = 0x8003; // lParam unused
/// Quit for real: restore every absorbed window and exit. WM_CLOSE only hides
/// to the tray in redirect mode, so shutdown paths (tray is direct, console
/// handler and `--shutdown` post this) need their own message.
pub const WM_APP_QUIT: u32 = 0x8010;
pub(crate) use tray::WM_APP_TRAY;
/// wParam = Explorer top-level HWND. Sent by the BHO via the registered
/// window message "RevolcAbsorbExplorer" (system-assigned id, identical
/// in every process of the session).
fn absorb_msg() -> u32 {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *ID.get_or_init(|| unsafe {
        windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW(w!("RevolcAbsorbExplorer"))
    })
}

const TABBAR_HEIGHT: i32 = 28;
const CLASS_NAME: PCWSTR = w!("RevolcMain");

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
/// Hard cap: a runaway explorer.exe storm must not eat the desktop.
const MAX_TABS: usize = 40;

struct WindowArgs {
    cfg: Arc<ConfigHandle>,
    initial_path: Option<String>,
}

struct App {
    cfg: Arc<ConfigHandle>,
    keymap: Vec<(Binding, Action)>,
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
    tabbar_hwnd: HWND,
    main_hwnd: HWND,
}

pub struct MainWindow {
    hwnd: HWND,
    accel: HACCEL,
}

impl MainWindow {
    pub fn new(cfg: Arc<ConfigHandle>, initial_path: Option<String>) -> windows::core::Result<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                lpszClassName: CLASS_NAME,
                style: CS_HREDRAW | CS_VREDRAW,
                hbrBackground: HBRUSH(std::ptr::null_mut()),
                ..Default::default()
            };
            let atom = RegisterClassExW(&wc);
            if atom == 0 {
                return Err(windows::core::Error::from_win32());
            }

            let args = Box::into_raw(Box::new(WindowArgs { cfg, initial_path }));

            let hwnd = CreateWindowExW(
                Default::default(),
                CLASS_NAME,
                w!("Revolc"),
                // WS_CLIPCHILDREN: our WM_ERASEBKGND sweep must not paint over
                // the tab bar or the absorbed Explorer window.
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1000,
                700,
                HWND::default(),
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                hinstance,
                Some(args as _),
            )?;

            // Allow the BHO notify message through UIPI: when Revolc runs
            // elevated (launched from an admin terminal), Explorer's lower-IL
            // PostMessageW would be silently dropped (observed err 5, 10:36).
            let m = absorb_msg();
            let _ = windows::Win32::UI::WindowsAndMessaging::ChangeWindowMessageFilterEx(
                hwnd, m, windows::Win32::UI::WindowsAndMessaging::MSGFLT_ALLOW, None,
            );

            // Accelerator table from the event-driven keymap
            // (event-driven, no GetAsyncKeyState polling).
            let mut accels: Vec<ACCEL> = Vec::new();
            for (i, (b, _)) in events::default_keymap().iter().enumerate() {
                accels.push(ACCEL {
                    fVirt: windows::Win32::UI::WindowsAndMessaging::ACCEL_VIRT_FLAGS(b.fvirt()),
                    key: b.vk,
                    cmd: (i as u16) + 1,
                });
            }
            let accel = CreateAcceleratorTableW(&accels)?;

            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);

            Ok(Self { hwnd, accel })
        }
    }

    pub fn hwnd_raw(&self) -> usize {
        self.hwnd.0 as usize
    }

    pub fn run_message_loop(&self) -> i32 {
        unsafe {
            let mut msg = MSG::default();
            loop {
                if GetMessageW(&mut msg, None, 0, 0).as_bool() == false {
                    return msg.wParam.0 as i32;
                }
                if TranslateAcceleratorW(self.hwnd, self.accel, &msg) != 0 {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

unsafe fn app_from(hwnd: HWND) -> &'static mut App {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    &mut *ptr
}

impl App {
    fn build(hwnd: HWND, args: Box<WindowArgs>) -> Box<App> {
        let tabbar_hwnd = tabbar::create(hwnd, &args.cfg.get());
        let mut app = Box::new(App {
            cfg: args.cfg,
            keymap: events::default_keymap(),
            tabs: TabList::new(),
            views: Vec::new(),
            excluded: Vec::new(),
            sweeping: false,
            burst_until: None,
            attached_tid: 0,
            active: true,
            focus_pending: false,
            path_done: Vec::new(),
            laid_out: None,
            last_client: (0, 0),
            tabbar_hwnd,
            main_hwnd: hwnd,
        });
        let first = args.initial_path.unwrap_or_else(home_dir);
        app.new_tab(first);
        if app.cfg.get().takeover.mode == "redirect" {
            tray::add(hwnd);
            // Startup is a burst too: we just spawned Explorer.
            app.arm_burst();
        }
        app
    }

    /// Show window; if no tabs remain (was hidden in tray), open a home tab.
    pub(crate) fn open_main(&mut self) {
        self.surface();
        if self.tabs.is_empty() {
            self.new_tab(home_dir());
        }
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
    }

    fn view_rect(&self) -> RECT {
        unsafe {
            let mut rc = RECT::default();
            let _ = GetClientRect(self.main_hwnd, &mut rc);
            RECT { top: TABBAR_HEIGHT, ..rc }
        }
    }

    fn sync_tabbar(&self) {
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
        tabbar::update(self.tabbar_hwnd, &labels, self.tabs.active_index(), &self.cfg.get());
    }

    /// Open a folder: push a reserved tab, spawn Explorer, absorb on the
    /// notify or on the next fast sweep tick, whichever comes first.
    fn new_tab(&mut self, path: String) {
        self.tabs.push(path.clone());
        self.views.push(None);
        self.sync_tabbar();
        self.activate(self.tabs.active_index());
        self.arm_burst();
        host::spawn_explorer(&path);
    }

    /// Attach an Explorer window to the first reserved (view-less) tab,
    /// or a brand-new tab if none is reserved.
    fn absorb_window(&mut self, top: HWND) {
        if host::is_absorbed(top, self.main_hwnd) {
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
            let store = host::read_folder_path(top, 2).unwrap_or_default();
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
            if self.cfg.get().takeover.mode == "redirect" {
                unsafe {
                    let _ = ShowWindow(self.main_hwnd, SW_HIDE);
                }
                return;
            }
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
        for (i, slot) in self.views.iter().enumerate() {
            if let Some(v) = slot {
                let title = host::window_title(v.top);
                if !title.is_empty() && self.tabs.set_label(i, title) {
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
            if let Some(p) = host::read_folder_path(top, 1) {
                self.tabs.set_path(i, p);
            }
        }
        let known = self.known_tops();
        self.path_done.retain(|h| known.contains(h));
    }

    /// Folder the active tab is actually showing right now. `Tab::path` is
    /// only what the tab was opened with; the user may have navigated since.
    fn active_folder_path(&self) -> String {
        if let Some(v) = self.views.get(self.tabs.active_index()).and_then(|s| s.as_ref()) {
            if let Some(p) = host::read_folder_path(v.top, 2) {
                return p;
            }
        }
        self.tabs
            .active()
            .map(|t| t.path.clone())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(home_dir)
    }

    /// Absorb every desktop folder window Revolc doesn't own yet.
    fn sweep(&mut self) {
        if self.sweeping || self.cfg.get().takeover.mode != "redirect" {
            return;
        }
        self.sweeping = true;
        self.prune_excluded();
        self.drop_dead_views();

        let excluded: Vec<isize> = self.excluded.iter().map(|(h, _)| *h).collect();
        // Take windows while they are still hidden whenever a folder is
        // expected. Explorer creates the frame **~850ms before showing it**
        // (log/probe_hidden.ps1), so this is the difference between "absorbed
        // before it was ever painted" and "one frame of a stray Explorer
        // window" — the flash the user reports. Outside that window we keep
        // the old visible-only rule: an unrelated hidden CabinetWClass is
        // none of our business.
        let expecting = self.burst_until.is_some() || self.views.iter().any(|v| v.is_none());
        let strays = host::stray_folder_windows(&self.known_tops(), &excluded, expecting);
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

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        // Close the Explorer window where it is: hidden, still our child.
        if let Some(v) = self.views[i].take() {
            // Park it: the sweep must not grab the window back while it is
            // still alive during WM_CLOSE teardown.
            self.excluded.push((v.top.0 as isize, Instant::now()));
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
            // spec §7: in redirect mode stay resident in the tray so takeover
            // keeps working; otherwise exit.
            if self.cfg.get().takeover.mode == "redirect" {
                tracing::info!("last tab closed, hiding to tray (redirect mode)");
                unsafe {
                    let _ = ShowWindow(self.main_hwnd, SW_HIDE);
                }
                return;
            }
            tracing::info!("last tab closed, exiting");
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
        for (j, slot) in self.views.iter_mut().enumerate() {
            if let Some(v) = slot {
                v.set_visible(j == i);
            }
        }
        self.redraw_all();
        // Keyboard focus has to be handed to the other process explicitly, or
        // the address bar takes clicks but no keystrokes. A window adopted
        // before Explorer showed it cannot take focus yet: the sweep retries
        // as soon as it appears (`focus_pending`).
        self.focus_pending = match self.views.get(i) {
            Some(Some(v)) if host::is_visible(v.top) => {
                host::focus_into(self.main_hwnd, v.top);
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
        let rc = self.view_rect();
        self.last_client = (rc.right, rc.bottom);
        unsafe {
            MoveWindow(self.tabbar_hwnd, 0, 0, rc.right, TABBAR_HEIGHT, true);
            // Keep the bar above the absorbed Explorer child: a new child is
            // created with HWND_TOP and would otherwise cover the tabs.
            let _ = SetWindowPos(
                self.tabbar_hwnd,
                HWND_TOP,
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            // Paint the bar NOW. Leaving it to the message queue is what
            // produced the black rectangle on the right of the tabs after a
            // maximize: the host had already grown and erased (clipping out
            // child rects), the tab bar was resized in the same WM_SIZE, and
            // until its WM_PAINT was dispatched that strip had neither a host
            // background nor bar pixels — DWM composites it black.
            let _ = InvalidateRect(self.tabbar_hwnd, None, true);
            let _ = UpdateWindow(self.tabbar_hwnd);
        }
        self.laid_out = None;
        self.place_active();
        self.redraw_all();
    }

    /// Resize the active view, skipping the call when nothing changed.
    fn place_active(&mut self) {
        let idx = self.tabs.active_index();
        let rc = self.view_rect();
        let metrics = match self.views.get(idx).and_then(|v| v.as_ref()) {
            Some(v) => v.placement_key(),
            None => ((0, 0, 0, 0), 0),
        };
        let key = (idx, rc.left, rc.top, rc.right, rc.bottom, metrics);
        if self.laid_out == Some(key) {
            return;
        }
        if let Some(Some(v)) = self.views.get(idx) {
            // Align the *visible chrome* (ribbon onward) to the view rect.
            v.place_client(rc);
        }
        self.laid_out = Some(key);
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
        let mut stale = false;
        let mut became_visible = false;
        let mut show: Option<HWND> = None;
        let mut hide: Vec<HWND> = Vec::new();
        for (i, slot) in self.views.iter().enumerate() {
            let Some(v) = slot else { continue };
            // Explorer can restore its caption on navigation; that is the band
            // of foreign chrome that shows up between tab bar and ribbon.
            if unsafe { v.ensure_stripped() } {
                stale = true;
            }
            // Explorer also moves and resizes its own frame when it finally
            // shows it; the `laid_out` key cannot notice that because we did
            // not change anything.
            if i == active && !v.is_placed(rc) {
                stale = true;
            }
            let want = i == active;
            let vis = host::is_visible(v.top);
            if want && !v.ready() {
                // Pre-show shell frame: positioned already, but Explorer has
                // not shown it yet. Its own ShowWindow does that (and runs its
                // start-up path), so we never force it on screen.
                if vis {
                    became_visible = true;
                }
            } else if want && !vis {
                show = Some(v.top);
            } else if !want && vis {
                hide.push(v.top);
            }
        }
        if stale || became_visible {
            self.laid_out = None;
        }
        // Place BEFORE re-asserting visibility: a window that is about to be
        // shown must already sit at the tab geometry, or its first frame lands
        // at the desktop position it was born with.
        self.place_active();
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
                    host::focus_into(self.main_hwnd, v.top);
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
            if raw == self.tabbar_hwnd.0 as isize {
                continue;
            }
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
        unsafe {
            let _ = SetWindowPos(
                self.tabbar_hwnd,
                HWND_TOP,
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
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
            tabbar = ?host::rect_in_parent(self.tabbar_hwnd),
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
                unsafe { v.restore() };
            }
        }
    }
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = lp.0 as *const CREATESTRUCTW;
            let args = Box::from_raw((*cs).lpCreateParams as *mut WindowArgs);
            let app = App::build(hwnd, args);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(app) as _);
            LRESULT(0)
        }
        WM_DESTROY => {
            app_from(hwnd).release_all();
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_SIZE => {
            let app = app_from(hwnd);
            app.layout();
            app.dump_layout("WM_SIZE");
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
            // redirect mode: closing the window means "hide to the tray" —
            // the takeover keeps running (Clover semantics). Real exit lives
            // in the tray menu and in WM_APP_QUIT (--shutdown).
            let app = app_from(hwnd);
            if app.cfg.get().takeover.mode == "redirect" {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                tracing::info!("main window hidden to tray (redirect mode)");
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        m if m == WM_APP_QUIT => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
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
            app.sync_input_queue();
            if app.active {
                if let Some(Some(v)) = app.views.get(app.tabs.active_index()) {
                    host::focus_into(app.main_hwnd, v.top);
                }
            }
            LRESULT(0)
        }
        m if m == WM_APP_CONFIG_CHANGED => {
            app_from(hwnd).sync_tabbar();
            LRESULT(0)
        }
        m if m == WM_APP_TAB_ACTIVATED => {
            app_from(hwnd).activate(lp.0 as usize);
            LRESULT(0)
        }
        m if m == WM_APP_TAB_CLOSE => {
            app_from(hwnd).close_tab(lp.0 as usize);
            LRESULT(0)
        }
        m if m == WM_APP_TAB_NEW => {
            let app = app_from(hwnd);
            let path = app.active_folder_path();
            app.new_tab(path);
            LRESULT(0)
        }
        m if m == WM_TIMER && wp.0 == SWEEP_TIMER_ID => {
            app_from(hwnd).sweep();
            LRESULT(0)
        }
        m if m == absorb_msg() => {
            // wParam unused: BHO reports "window created" without touching
            // the site object; we discover the HWND by enumerating desktop
            // folder windows and skip the ones we already own.
            let app = app_from(hwnd);
            app.on_notify();
            LRESULT(0)
        }
        m if m == WM_APP_TRAY => {
            let app = app_from(hwnd);
            match tray::on_tray_message(hwnd, lp, app) {
                Some(r) => r,
                None => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
