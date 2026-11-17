pub mod tabbar;
pub mod tray;

use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateAcceleratorTableW, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    DestroyWindow, GetClientRect, GetMessageW, GetParent, GetWindowLongPtrW, LoadCursorW,
    MoveWindow, PostMessageW, PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, ShowWindow,
    TranslateAcceleratorW, TranslateMessage, ACCEL, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWLP_USERDATA, HACCEL, IDC_ARROW, MSG, SW_HIDE, SW_SHOWNORMAL, WNDCLASSEXW,
    WM_CLOSE, WM_CREATE, WM_DESTROY, WM_KEYDOWN, WM_SIZE, WM_USER, WS_OVERLAPPEDWINDOW,
};

use crate::config::ConfigHandle;
use crate::shell::host::{self, Absorbed};
use crate::tabs::events::{self, Action, Binding};
use crate::tabs::TabList;

pub const WM_APP_CONFIG_CHANGED: u32 = 0x8000; // posted by config-watcher thread
pub const WM_APP_TAB_ACTIVATED: u32 = 0x8001; // lParam = tab index
pub const WM_APP_TAB_CLOSE: u32 = 0x8002; // lParam = tab index
pub const WM_APP_TAB_NEW: u32 = 0x8003; // lParam unused
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

struct WindowArgs {
    cfg: Arc<ConfigHandle>,
    initial_path: Option<String>,
}

struct App {
    cfg: Arc<ConfigHandle>,
    keymap: Vec<(Binding, Action)>,
    tabs: TabList,
    views: Vec<Option<Absorbed>>,
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
                WS_OVERLAPPEDWINDOW,
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
            tabbar_hwnd,
            main_hwnd: hwnd,
        });
        let first = args.initial_path.unwrap_or_else(home_dir);
        app.new_tab(first);
        if app.cfg.get().takeover.mode == "redirect" {
            tray::add(hwnd);
        }
        app
    }

    /// Show window; if no tabs remain (was hidden in tray), open a home tab.
    pub(crate) fn open_main(&mut self) {
        unsafe {
            let _ = ShowWindow(self.main_hwnd, SW_SHOWNORMAL);
        }
        if self.tabs.is_empty() {
            self.new_tab(home_dir());
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
        let labels: Vec<String> = self.tabs.iter_all().map(|t| short_label(&t.path)).collect();
        tabbar::update(self.tabbar_hwnd, &labels, self.tabs.active_index(), &self.cfg.get());
    }

    /// Open a folder: push a reserved tab, spawn Explorer, absorb on BHO notify.
    fn new_tab(&mut self, path: String) {
        self.tabs.push(path.clone());
        self.views.push(None);
        self.sync_tabbar();
        self.activate(self.tabs.active_index());
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
                tracing::error!(error = %e, ?top, "absorb failed");
                return;
            }
        };
        self.views[idx] = Some(view);
        // Absorbing into a hidden/minimized Revolc looks like "nothing
        // happened" - surface the window so the user sees the result.
        unsafe {
            let _ = ShowWindow(self.main_hwnd, SW_SHOWNORMAL);
        }
        tracing::info!(?top, idx, "absorbed into tab");

        // Best-effort path for Tab-New reuse (~0.3s); the label falls back to
        // the window title, which Explorer sets as soon as it navigates.
        let store = host::read_folder_path(top, 6)
            .or_else(|| {
                let t = host::window_title(top);
                if t.is_empty() { None } else { Some(t) }
            })
            .unwrap_or_else(|| "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string());
        self.tabs.set_path(idx, store);

        self.sync_tabbar();
        self.activate(idx);
        self.layout();
    }

    /// BHO notified "a shell window was created". Find it via IShellWindows
    /// (the BHO never touches the site object — that crashed explorerframe)
    /// and absorb the first folder window we don't already own.
    fn absorb_new_window(&mut self) {
        tracing::info!("absorb notify received");
        let known: Vec<isize> = self
            .views
            .iter()
            .flatten()
            .map(|v| v.top.0 as isize)
            .collect();
        tracing::debug!(known = known.len(), "searching for new folder window");
        match host::find_new_folder_window(&known, 25) {
            Some(top) => self.absorb_window(top),
            None => tracing::warn!("no new folder window found after notify"),
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        // Restore the Explorer window first, then let it close itself.
        if let Some(v) = self.views[i].take() {
            unsafe {
                v.restore();
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
        for (j, slot) in self.views.iter().enumerate() {
            if let Some(v) = slot {
                v.set_visible(j == i);
                if j == i {
                    unsafe {
                        let _ = SetFocus(v.top);
                    }
                }
            }
        }
        self.sync_tabbar();
    }

    fn action(&mut self, a: Action) {
        tracing::debug!(?a, "action dispatched");
        match a {
            Action::NewTab => {
                let path = self.tabs.active().map(|t| t.path.clone()).unwrap_or_else(home_dir);
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
        unsafe {
            let _ = MoveWindow(self.tabbar_hwnd, 0, 0, rc.right, TABBAR_HEIGHT, true);
        }
        if let Some(Some(v)) = self.views.get(self.tabs.active_index()) {
            v.resize(rc);
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
            app_from(hwnd).layout();
            LRESULT(0)
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
            // spec §3.2 (M1): persist session here. M0: pass through.
            DefWindowProcW(hwnd, msg, wp, lp)
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
            let path = app.tabs.active().map(|t| t.path.clone()).unwrap_or_else(home_dir);
            app.new_tab(path);
            LRESULT(0)
        }
        m if m == absorb_msg() => {
            // wParam unused: BHO reports "window created" without touching
            // the site object; we discover the HWND via IShellWindows.
            let app = app_from(hwnd);
            if app.cfg.get().takeover.mode == "redirect" {
                app.absorb_new_window();
            }
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
