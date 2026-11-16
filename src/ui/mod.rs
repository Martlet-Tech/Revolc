pub mod tabbar;
pub mod tray;

use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateAcceleratorTableW, CreateWindowExW, DefWindowProcW, DispatchMessageW,
    DestroyWindow, GetClientRect, GetMessageW, GetWindowLongPtrW, LoadCursorW, MoveWindow,
    PostQuitMessage, RegisterClassExW, SetWindowLongPtrW, ShowWindow, TranslateAcceleratorW,
    TranslateMessage, ACCEL, CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA,
    HACCEL, IDC_ARROW, MSG, SW_HIDE, SW_SHOW, SW_SHOWNORMAL, WNDCLASSEXW, WM_CLOSE, WM_CREATE,
    WM_DESTROY,
    WM_KEYDOWN, WM_SIZE, WS_OVERLAPPEDWINDOW,
};

use crate::config::ConfigHandle;
use crate::shell::explorer::ExplorerView as View;
use crate::tabs::events::{self, Action, Binding};
use crate::tabs::TabList;

pub const WM_APP_CONFIG_CHANGED: u32 = 0x8000; // posted by config-watcher thread
pub const WM_APP_TAB_ACTIVATED: u32 = 0x8001; // lParam = tab index
pub const WM_APP_TAB_CLOSE: u32 = 0x8002; // lParam = tab index
pub const WM_APP_TAB_NEW: u32 = 0x8003; // lParam unused
pub(crate) use tray::WM_APP_TRAY;

const TABBAR_HEIGHT: i32 = 28;
const CLASS_NAME: PCWSTR = w!("RevolcMain");

struct WindowArgs {
    cfg: Arc<ConfigHandle>,
    initial_path: Option<String>,
}

pub(crate) struct App {
    cfg: Arc<ConfigHandle>,
    keymap: Vec<(Binding, Action)>,
    tabs: TabList,
    views: Vec<Option<View>>,
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
                HWND::default(), // no parent
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                hinstance,
                Some(args as _),
            )?;

            // Accelerator table built from the event-driven keymap
            // (design review ①: event-driven, no GetAsyncKeyState polling).
            let mut accels: Vec<ACCEL> = Vec::new();
            for (i, (b, _)) in events::default_keymap().iter().enumerate() {
                accels.push(ACCEL {
                    fVirt: windows::Win32::UI::WindowsAndMessaging::ACCEL_VIRT_FLAGS(b.fvirt()),
                    key: b.vk,
                    cmd: (i as u16) + 1,
                });
            }
            let accel = CreateAcceleratorTableW(&accels)?;

            // The window must be shown explicitly; CreateWindowExW without
            // WS_VISIBLE leaves it hidden (this is why no window appeared).
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
                // Event-driven accelerator dispatch; nonzero = key consumed.
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
        // Resident tray in redirect mode (takeover needs a living process).
        if app.cfg.get().takeover.mode == "redirect" {
            tray::add(hwnd);
        }
        app
    }

    /// Show window; if no tabs remain (was hidden in tray), open a home tab.
    pub(crate) fn open_main(&mut self) {
        unsafe {
            let _ = ShowWindow(self.main_hwnd, SW_SHOW);
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

    fn new_tab(&mut self, path: String) {
        match View::new(self.main_hwnd, &self.view_rect(), &path) {
            Ok(view) => {
                self.tabs.push(path);
                self.views.push(Some(view));
                self.sync_tabbar();
                self.activate(self.tabs.active_index());
            }
            Err(e) => tracing::error!(error = %e, path = %path, "failed to create view"),
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        // Destroy the COM view first (it owns a child window).
        self.views[i] = None;
        self.tabs.remove(i);
        self.views.remove(i);
        if self.tabs.is_empty() {
            // spec §7: in redirect mode stay resident in the tray so takeover
            // keeps working; otherwise exit (M0 skeleton behavior).
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
                if let Ok(h) = v.hwnd() {
                    unsafe {
                        let _ = ShowWindow(h, if j == i { SW_SHOW } else { SW_HIDE });
                        if j == i {
                            let _ = SetFocus(h);
                        }
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
            let _ = v.set_rect(rc);
        }
    }
}

fn home_dir() -> String {
    // Win7-compatible: USERPROFILE env (M0 design §5).
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into())
}

fn short_label(path: &str) -> String {
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
