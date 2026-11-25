#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! `revolc-tray.exe` — optional tray / launcher / window list / foreground
//! ledger (docs/双进程架构设计.md Phase 2).
//!
//! The window processes are self-contained: they never require this exe.
//! This one owns only session-level concerns:
//!   * notification-area icon + menu (new window / window list / close all /
//!     stop takeover),
//!   * BHO registration (so only one process ever touches the registry),
//!   * the shared last-active ledger, fed by a foreground hook,
//!   * BHO fallback when zero windows are open (spawn one to absorb),
//!   * config reload with single-file watch + debounce.

mod menu;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use revolc_common::config::{config_path, ConfigHandle};
use revolc_common::ledger::Ledger;
use revolc_common::messages::{absorb_msg, WM_APP_QUIT, WM_APP_RESTORE, WM_APP_TRAY};
use revolc_common::takeover::{
    repair_bho_path_if_installed, set_takeover_enabled, uninstall_bho_keys,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    BOOL, HANDLE, HWND, LPARAM, LRESULT, WPARAM, GetLastError,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, EnumWindows, FindWindowW, GetClassNameW,
    GetMessageW, GetWindowTextW, HWND_BROADCAST, IsWindow, IsWindowVisible, LoadCursorW,
    MB_ICONINFORMATION, MB_OK, MessageBoxW, MSG, MSGFLT_ALLOW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SendMessageTimeoutW, SMTO_ABORTIFHUNG,
    TranslateMessage, WM_NULL, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
    WNDCLASSEXW, IDC_ARROW,
};

const TRAY_CLASS: PCWSTR = w!("RevolcResident");
const TRAY_ID: u32 = 1;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
static HOOK: AtomicIsize = AtomicIsize::new(0);
static EXE_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Read-only config handle for the tray menu: it needs to know whether to
/// show workbench rows without opening the TOML on every right-click.
static CONFIG: OnceLock<Arc<ConfigHandle>> = OnceLock::new();

/// The tray message window, for menu commands posted back to this process.
pub(crate) fn tray_hwnd() -> HWND {
    let raw = TRAY_HWND.load(Ordering::SeqCst);
    HWND(raw as _)
}

/// Elevated helper launched from the settings page's 系统注册 row.
/// Order matters: quit every Revolc process first (otherwise their watchers
/// would recreate the config), then remove registry, Run entry and APPDATA.
fn uninstall_flow() -> i32 {
    for (hwnd, _) in our_windows() {
        unsafe {
            let _ = PostMessageW(hwnd, WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    if let Ok(old_tray) = unsafe { FindWindowW(w!("RevolcResident"), None) } {
        if !old_tray.0.is_null() {
            unsafe {
                let _ = PostMessageW(old_tray, WM_APP_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
    for _ in 0..30 {
        let old_tray_gone = match unsafe { FindWindowW(w!("RevolcResident"), None) } {
            Ok(h) => h.0.is_null(),
            Err(_) => true,
        };
        if our_windows().is_empty() && old_tray_gone {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    uninstall_bho_keys();
    let exe = std::env::current_exe().unwrap_or_default();
    let _ = revolc_common::autostart::set_enabled(
        false,
        &revolc_common::autostart::launch_command(&exe),
    );
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let _ = std::fs::remove_dir_all(PathBuf::from(appdata).join("revolc"));
    }

    unsafe {
        let _ = MessageBoxW(
            HWND::default(),
            w!("已清除 Revolc 的注册表项、开机自启和全部配置。\n已打开的 Explorer 进程仍持有 DLL，重启资源管理器或注销后彻底释放。\n现在可以删除软件目录了。"),
            w!("Revolc 卸载完成"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // No logging here: the uninstaller deletes %APPDATA%\revolc, and an open
    // log handle would make that removal fail on Windows.
    if args.iter().any(|a| a == "--uninstall") {
        std::process::exit(uninstall_flow());
    }

    revolc_common::crash::install();
    revolc_common::logging::init("revolc-tray");
    if args.iter().any(|a| a == "--shutdown") {
        std::process::exit(shutdown());
    }

    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _ = EXE_DIR.set(dir);

    let cfg = Arc::new(ConfigHandle::new(config_path()));
    let _ = CONFIG.set(cfg.clone());

    if !ensure_single_instance() {
        return;
    }

    let ledger = match Ledger::open_or_create() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "ledger unavailable, tray exiting");
            return;
        }
    };
    let _ = LEDGER.set(Mutex::new(ledger));

    // Registry install is now an explicit settings action, not a side effect
    // of the takeover switch. On startup we only heal a moved portable folder.
    repair_bho_path_if_installed();
    set_takeover_enabled(cfg.get().takeover.mode == "redirect");
    // Navigation tracking (tab context menu) is gated by a marker file the
    // BHO checks at window creation: `[bookmarks] enabled` drives it, so the
    // config stays the single source of truth and survives takeover toggles.
    let marker = std::env::var_os("APPDATA")
        .map(|d| std::path::Path::new(&d).join("revolc").join("bho.nav"));
    if let Some(m) = marker {
        if cfg.get().bookmarks.enabled {
            let _ = std::fs::write(&m, b"");
        } else {
            let _ = std::fs::remove_file(&m);
        }
    }

    let hwnd = match create_tray_window() {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(error = %e, "tray window creation failed");
            return;
        }
    };
    TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    if !tray_icon_add(hwnd) {
        tracing::warn!("tray icon failed; keyboard/window features still work");
    }

    let hook = unsafe {
        SetWinEventHook(
            windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND,
            windows::Win32::UI::WindowsAndMessaging::EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    HOOK.store(hook.0 as isize, Ordering::SeqCst);
    tracing::info!("foreground ledger hook armed");

    let watcher = spawn_config_watcher(cfg.clone());
    tracing::info!("revolc-tray ready");

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    drop(watcher);
    unhook();
}

/// Second tray launch is a no-op: the first one already owns the icon/ledger.
fn ensure_single_instance() -> bool {
    use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
    unsafe {
        let _mutex: HANDLE = match CreateMutexW(None, false, w!("Local\\RevolcTraySingleInstance")) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "CreateMutexW failed, allowing startup");
                return true;
            }
        };
        if GetLastError().0 == ERROR_ALREADY_EXISTS.0 {
            tracing::warn!("another revolc-tray is running, exiting");
            return false;
        }
    }
    true
}

/// `revolc-tray.exe --shutdown`: ask the tray to quit takeover, and also tell
/// any window processes directly in case the tray died.
fn shutdown() -> i32 {
    unsafe {
        let mut sent = 0;
        for (h, _) in our_windows() {
            let _ = PostMessageW(h, WM_APP_QUIT, WPARAM(0), LPARAM(0));
            sent += 1;
        }
        if let Ok(h) = FindWindowW(TRAY_CLASS, None) {
            if !h.0.is_null() {
                let _ = PostMessageW(h, WM_APP_QUIT, WPARAM(0), LPARAM(0));
                sent += 1;
            }
        }
        if sent == 0 {
            eprintln!("Revolc is not running");
            return 1;
        }
    }
    0
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _idobj: i32,
    _idchild: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.0.is_null() {
        return;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    if n <= 0 || String::from_utf16_lossy(&cls[..n as usize]) != "RevolcMain" {
        return;
    }
    if let Some(ledger) = LEDGER.get() {
        if let Ok(l) = ledger.lock() {
            l.set_active(hwnd, GetCurrentProcessId());
        }
    }
}

fn unhook() {
    let raw = HOOK.swap(0, Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(raw as *mut _));
        }
    }
}

fn create_tray_window() -> windows::core::Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: TRAY_CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            let err = windows::core::Error::from_win32();
            if (err.code().0 as u32 & 0xffff) != 1410 {
                return Err(err);
            }
        }
        let hwnd = CreateWindowExW(
            Default::default(),
            TRAY_CLASS,
            w!("Revolc resident"),
            Default::default(),
            0,
            0,
            0,
            0,
            HWND::default(),
            windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
            hinstance,
            None,
        )?;
        // Explorer/BHO runs at a lower integrity level; without this the
        // absorb notification is silently dropped when the tray is elevated.
        let _ = ChangeWindowMessageFilterEx(hwnd, absorb_msg(), MSGFLT_ALLOW, None);
        Ok(hwnd)
    }
}

fn tray_icon_add(hwnd: HWND) -> bool {
    unsafe {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = revolc_common::icon::small();
        let tip = w!("Revolc (文件夹接管中)");
        for (i, c) in tip.as_wide().iter().take(127).copied().enumerate() {
            nid.szTip[i] = c;
        }
        let ok = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
        if ok {
            tracing::info!("tray icon added");
        } else {
            tracing::error!("tray icon add failed");
        }
        ok
    }
}

fn tray_icon_remove(hwnd: HWND) {
    unsafe {
        let mut nid = NOTIFYICONDATAW::default();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// A window whose UI thread has stopped pumping messages is a zombie: it is
/// still enumerable (the process is alive, the frame hidden via
/// `close_action = "hide"`), but every `PostMessageW` we send it — restore,
/// close, quit — lands in a queue nobody drains. The tray used to list such
/// windows as clickable rows that did nothing. Probe with a synchronous
/// `WM_NULL` under `SMTO_ABORTIFHUNG`; a hung thread times out and is dropped
/// from the list, so the menu only shows windows it can actually control.
/// 300ms is short enough that opening the menu stays snappy.
const PROBE_TIMEOUT_MS: u32 = 300;

unsafe fn window_responds(hwnd: HWND) -> bool {
    let mut result: usize = 0;
    SendMessageTimeoutW(
        hwnd,
        WM_NULL,
        WPARAM(0),
        LPARAM(0),
        SMTO_ABORTIFHUNG,
        PROBE_TIMEOUT_MS,
        Some(&mut result),
    ).0 != 0
}

/// (hwnd, "tab1/tab2/...") for every RevolcMain, in enumeration order.
/// Tab labels come from the absorbed Explorer panes (CabinetWClass /
/// ExploreWClass children), which is exactly what each window's tab strip
/// shows — `refresh_labels` in the main process reads the same window text.
fn our_windows() -> Vec<(HWND, String)> {
    let mut out: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_windows),
            LPARAM(&mut out as *mut Vec<(HWND, String)> as isize),
        );
    }
    out
}

/// Window list for the tray *menu* only. Same enumeration as `our_windows`
/// plus a liveness probe: a zombie frame (hidden process whose UI thread
/// stopped pumping messages) cannot answer restore/close, so listing it just
/// produces a row that silently does nothing when clicked. Lifecycle paths
/// (quit / close-all / foreground switch) keep using `our_windows` unchanged
/// — with a hung thread a posted message is lost either way.
fn menu_windows() -> Vec<(HWND, String)> {
    our_windows().into_iter()
        .filter(|(h, _)| {
            let live = unsafe { window_responds(*h) };
            if !live {
                tracing::warn!(hwnd = ?h, "menu: skipping unresponsive window");
            }
            live
        })
        .collect()
}

unsafe extern "system" fn enum_windows(hwnd: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<(HWND, String)>);
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    if n > 0 && String::from_utf16_lossy(&cls[..n as usize]) == "RevolcMain" {
        let mut panes: Vec<String> = Vec::new();
        let _ = windows::Win32::UI::WindowsAndMessaging::EnumChildWindows(
            hwnd,
            Some(enum_tab_pane),
            LPARAM(&mut panes as *mut Vec<String> as isize),
        );
        // Prefer the MRU labels the window process records on every tab
        // activation ("recently used 3 tabs"), falling back to the pane
        // order when no MRU file exists yet.
        let mut mru = revolc_common::tab_mru::read(hwnd);
        let label = if mru.is_empty() {
            if panes.is_empty() {
                let mut title = [0u16; 256];
                let t = GetWindowTextW(hwnd, &mut title);
                String::from_utf16_lossy(&title[..t.max(0) as usize])
            } else {
                panes.join(" / ")
            }
        } else {
            mru.truncate(3);
            mru.join(" / ")
        };
        out.push((hwnd, label));
    }
    BOOL(1)
}

unsafe extern "system" fn enum_tab_pane(hwnd: HWND, lp: LPARAM) -> BOOL {
    let panes = &mut *(lp.0 as *mut Vec<String>);
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    if n > 0 {
        let class = String::from_utf16_lossy(&cls[..n as usize]);
        if class == "CabinetWClass" || class == "ExploreWClass" {
            let mut title = [0u16; 256];
            let t = GetWindowTextW(hwnd, &mut title);
            let text = String::from_utf16_lossy(&title[..t.max(0) as usize]);
            if !text.is_empty() {
                panes.push(text);
            }
        }
    }
    BOOL(1)
}

fn spawn_window(adopt_only: bool) {
    let dir = EXE_DIR.get().cloned().unwrap_or_default();
    let exe = dir.join("revolc.exe");
    let mut cmd = Command::new(&exe);
    if adopt_only {
        // An existing folder window is waiting to be adopted: start with no
        // Home tab so it becomes the first tab (REA §8 constraint).
        cmd.arg("--adopt");
    }
    match cmd.spawn() {
        Ok(_) => tracing::info!(?exe, "spawned revolc.exe"),
        Err(e) => tracing::error!(error = %e, ?exe, "failed to spawn revolc.exe"),
    }
}

fn spawn_settings_window() {
    let dir = EXE_DIR.get().cloned().unwrap_or_default();
    let exe = dir.join("revolc.exe");
    match Command::new(&exe).arg("--settings").spawn() {
        Ok(_) => tracing::info!(?exe, "spawned settings window"),
        Err(e) => tracing::error!(error = %e, ?exe, "failed to spawn settings window"),
    }
}

/// Left-click switches to the most recently active Revolc window (hidden or
/// minimized included), matching the browser-style tray. No window = nothing
/// to switch to; right-click still offers "new window".
unsafe fn switch_to_last_active() {
    let target = LEDGER.get()
        .and_then(|l| l.lock().ok().and_then(|g| g.last_active()));
    if let Some(h) = target {
        if IsWindow(h).as_bool() {
            let _ = PostMessageW(h, WM_APP_RESTORE, WPARAM(0), LPARAM(0));
            return;
        }
    }
    // Ledger empty (tray just started): fall back to the first hidden window,
    // then to any window.
    let windows = our_windows();
    if let Some((h, _)) = windows.iter().find(|(h, _)| !IsWindowVisible(*h).as_bool()) {
        let _ = PostMessageW(*h, WM_APP_RESTORE, WPARAM(0), LPARAM(0));
    } else if let Some((h, _)) = windows.first() {
        let _ = PostMessageW(*h, WM_APP_RESTORE, WPARAM(0), LPARAM(0));
    }
}

/// Tear down the tray. The two flags are independent:
/// - `disable_takeover`: a real exit disables the temporary switch; logoff
///   leaves the user's setting alone (the marker is persistent anyway).
/// - `quit_windows`: tell window processes to restore their panes. Logoff /
///   shutdown must do this, otherwise Explorer keeps dangling refs.
unsafe fn quit(hwnd: HWND, disable_takeover: bool, quit_windows: bool) {
    tray_icon_remove(hwnd);
    unhook();
    if disable_takeover {
        set_takeover_enabled(false);
    }
    if quit_windows {
        for (h, _) in our_windows() {
            let _ = PostMessageW(h, WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(l) = LEDGER.get() {
        if let Ok(l) = l.lock() {
            // Ledger survives the tray (window processes may still be closing),
            // just stop claiming to own the write side.
            l.set_active(HWND::default(), 0);
        }
    }
    let _ = DestroyWindow(hwnd);
}

/// Watch only `revolc.toml` (not the whole directory) and debounce bursts —
/// the old directory watcher logged `config reloaded` several times a second.
fn spawn_config_watcher(cfg: Arc<ConfigHandle>) -> Option<RecommendedWatcher> {
    let path = cfg.path().clone();
    let mut last = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or(Instant::now());
    let mut mode = cfg.get().takeover.mode.clone();
    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        match res {
            Ok(_) => {
                let now = Instant::now();
                if now.duration_since(last) < Duration::from_millis(500) {
                    return;
                }
                last = now;
                cfg.reload();
                let snapshot = cfg.get();
                let new_mode = snapshot.takeover.mode.clone();
                if new_mode != mode {
                    tracing::info!(%new_mode, "takeover switch changed (marker only)");
                    set_takeover_enabled(new_mode == "redirect");
                    if new_mode == "redirect" {
                        // Enabling takeover must also adopt folder windows
                        // that were opened while it was paused. The BHO only
                        // reports *new* windows, so nudge every RevolcMain
                        // into one immediate fast sweep, exactly like a BHO
                        // notification. may_adopt() keeps a single owner.
                        unsafe {
                            let _ = PostMessageW(
                                HWND_BROADCAST,
                                absorb_msg(),
                                WPARAM(0),
                                LPARAM(0),
                            );
                        }
                        tracing::info!("takeover enabled: broadcast absorb for existing windows");
                    }
                    mode = new_mode;
                }
            }
            Err(e) => tracing::error!(error = %e, "config watcher error"),
        }
    })
    .ok()?;
    if watcher.watch(&path, RecursiveMode::NonRecursive).is_err() {
        tracing::warn!(?path, "config file watch failed");
        return None;
    }
    tracing::info!("config watcher armed (single file, 500ms debounce)");
    Some(watcher)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == absorb_msg() {
        // BHO fallback: no window process alive to receive the broadcast.
        // Spawn one; its own sweep adopts the folder within one burst tick.
        if our_windows().is_empty() {
            tracing::info!("absorb notify with zero windows, spawning one");
            spawn_window(true);
        }
        return LRESULT(0);
    }
    match msg {
        m if m == WM_APP_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => switch_to_last_active(),
                WM_RBUTTONUP => unsafe {
                    let takeover_on = CONFIG
                        .get()
                        .map(|c| c.get().takeover.mode == "redirect")
                        .unwrap_or(false);
                    menu::show(hwnd, menu_windows(), takeover_on);
                },
                _ => {}
            }
            LRESULT(0)
        }
        m if m == menu::CMD_NEW_WINDOW => { spawn_window(false); LRESULT(0) }
        m if m == menu::CMD_OPEN_SETTINGS => { spawn_settings_window(); LRESULT(0) }
        m if m == menu::CMD_EXIT => { quit(hwnd, true, true); LRESULT(0) }
        m if m == WM_APP_QUIT => {
            // CLI --shutdown, menu exit or the elevated uninstaller: disable
            // the temporary switch and restore absorbed panes to the desktop.
            quit(hwnd, true, true);
            LRESULT(0)
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                // Logoff: keep the BHO registered (it will be inert after
                // reboot anyway) but must tell windows to restore their panes.
                quit(hwnd, false, true);
            }
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_DESTROY => {
            TRAY_HWND.store(0, Ordering::SeqCst);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
