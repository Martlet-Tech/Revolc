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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use revolc_common::config::{config_path, ConfigHandle};
use revolc_common::ledger::Ledger;
use revolc_common::messages::{absorb_msg, WM_APP_QUIT, WM_APP_RESTORE, WM_APP_TRAY};
use revolc_common::takeover::ensure_bho_registered;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    BOOL, HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM, GetLastError,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, ChangeWindowMessageFilterEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, EnumWindows, FindWindowW, GetClassNameW, GetCursorPos,
    GetMessageW, GetWindowTextW, IsWindow, LoadCursorW, MF_SEPARATOR, MF_STRING,
    MSG, MSGFLT_ALLOW, PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    TrackPopupMenuEx, TranslateMessage, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
    WNDCLASSEXW, TPM_RETURNCMD, IDC_ARROW,
};

const TRAY_CLASS: PCWSTR = w!("RevolcResident");
const TRAY_ID: u32 = 1;
const CMD_NEW_WINDOW: usize = 3;
const CMD_WINDOW_LIST: usize = 4;
const CMD_CLOSE_ALL: usize = 5;
const CMD_EXIT: usize = 2;
const LIST_CMD_BASE: usize = 100;
const MAX_LIST: usize = 32;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
static HOOK: AtomicIsize = AtomicIsize::new(0);
static EXE_DIR: OnceLock<PathBuf> = OnceLock::new();
/// Set when the config watcher asks the tray to hide, so WM_APP_QUIT knows to
/// keep BHO registration (takeover stays on) instead of tearing everything down.
static HIDDEN_BY_CONFIG: AtomicBool = AtomicBool::new(false);

fn main() {
    revolc_common::crash::install();
    revolc_common::logging::init("revolc-tray");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--shutdown") {
        std::process::exit(shutdown());
    }

    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let _ = EXE_DIR.set(dir);

    let cfg = Arc::new(ConfigHandle::new(config_path()));
    if !cfg.get().tray.enabled {
        tracing::info!("tray disabled in config, exiting");
        return;
    }

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

    // One owner for BHO registration: the tray. A window process must never
    // touch the registry (it may be launched while another window is closing).
    if cfg.get().takeover.mode == "redirect" {
        ensure_bho_registered("redirect");
        // Navigation tracking (tab context menu) is gated by a marker file the
        // BHO checks at window creation: `[bookmarks] enabled` drives it, so
        // the config stays the single source of truth (hot-reloaded by the
        // window's watcher only restarts windows; the marker persists).
        let marker = std::env::var_os("APPDATA")
            .map(|d| std::path::Path::new(&d).join("revolc").join("bho.nav"));
        if let Some(m) = marker {
            if cfg.get().bookmarks.enabled {
                let _ = std::fs::write(&m, b"");
            } else {
                let _ = std::fs::remove_file(&m);
            }
        }
    } else {
        ensure_bho_registered("off");
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

/// (hwnd, title) of every RevolcMain on the desktop, in enumeration order.
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

unsafe extern "system" fn enum_windows(hwnd: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<(HWND, String)>);
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    if n > 0 && String::from_utf16_lossy(&cls[..n as usize]) == "RevolcMain" {
        let mut title = [0u16; 256];
        let t = GetWindowTextW(hwnd, &mut title);
        out.push((
            hwnd,
            String::from_utf16_lossy(&title[..t.max(0) as usize]),
        ));
    }
    BOOL(1)
}

fn spawn_window() {
    let dir = EXE_DIR.get().cloned().unwrap_or_default();
    let exe = dir.join("revolc.exe");
    match Command::new(&exe).spawn() {
        Ok(_) => tracing::info!(?exe, "spawned revolc.exe"),
        Err(e) => tracing::error!(error = %e, ?exe, "failed to spawn revolc.exe"),
    }
}

unsafe fn popup_menu(hwnd: HWND) -> usize {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let _ = AppendMenuW(menu, MF_STRING, CMD_NEW_WINDOW, w!("新建窗口"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_WINDOW_LIST, w!("窗口列表"));
    let _ = AppendMenuW(menu, MF_STRING, CMD_CLOSE_ALL, w!("全部关闭"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("退出接管"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    TrackPopupMenuEx(menu, TPM_RETURNCMD.0, pt.x, pt.y, hwnd, None).0 as usize
}

/// Real windows, not a canned list: titles are read at menu time.
unsafe fn popup_window_list(hwnd: HWND) -> Option<HWND> {
    let list = our_windows();
    if list.is_empty() {
        return None;
    }
    let menu = CreatePopupMenu().ok()?;
    for (i, (_, title)) in list.iter().take(MAX_LIST).enumerate() {
        let label: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            LIST_CMD_BASE + i,
            PCWSTR(label.as_ptr()),
        );
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD.0, pt.x, pt.y, hwnd, None).0 as usize;
    if cmd >= LIST_CMD_BASE {
        list.get(cmd - LIST_CMD_BASE).map(|(h, _)| *h)
    } else {
        None
    }
}

/// Tear down the tray. The two flags are independent:
/// - `unregister_bho`: menu `--shutdown` disables takeover; a config-driven
///   hide or logoff must leave the registration intact.
/// - `quit_windows`: tell window processes to restore their panes. A config
///   hide must NOT do this (the user's folders stay open), but logoff/shutdown
///   must (otherwise Explorer keeps dangling refs to re-parented windows).
unsafe fn quit(hwnd: HWND, unregister_bho: bool, quit_windows: bool) {
    tray_icon_remove(hwnd);
    unhook();
    if unregister_bho {
        ensure_bho_registered("off");
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
                    tracing::info!(%new_mode, "takeover mode changed, re-registering BHO");
                    ensure_bho_registered(&new_mode);
                    mode = new_mode;
                }
                // Tray hidden from the settings page: leave the process so the
                // icon and foreground hook disappear. The main process will
                // spawn us again if the user turns it back on.
                if !snapshot.tray.enabled {
                    tracing::info!("tray disabled in config, exiting");
                    HIDDEN_BY_CONFIG.store(true, Ordering::SeqCst);
                    // Win32 windows are thread-affine: ask the tray thread to
                    // quit via its message loop, never call DestroyWindow here.
                    // HWND is not Send, so read it from the atomic instead of
                    // capturing it in this watcher closure.
                    let raw = TRAY_HWND.load(Ordering::SeqCst);
                    if raw != 0 {
                        unsafe {
                            let _ = PostMessageW(HWND(raw as _), WM_APP_QUIT, WPARAM(0), LPARAM(0));
                        }
                    }
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
            spawn_window();
        }
        return LRESULT(0);
    }
    match msg {
        m if m == WM_APP_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => spawn_window(),
                WM_RBUTTONUP => match popup_menu(hwnd) {
                    CMD_NEW_WINDOW => spawn_window(),
                    CMD_WINDOW_LIST => {
                        if let Some(target) = popup_window_list(hwnd) {
                            if IsWindow(target).as_bool() {
                                // Ask the window to surface itself: it knows
                                // whether it is hidden (close="hide") or
                                // minimized, and keeps its maximized state.
                                let _ = PostMessageW(
                                    target,
                                    WM_APP_RESTORE,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }
                        }
                    }
                    CMD_CLOSE_ALL => {
                        for (h, _) in our_windows() {
                            let _ = PostMessageW(h, WM_APP_QUIT, WPARAM(0), LPARAM(0));
                        }
                    }
                    CMD_EXIT => quit(hwnd, true, true),
                    _ => {}
                },
                _ => {}
            }
            LRESULT(0)
        }
        m if m == WM_APP_QUIT => {
            // CLI --shutdown / menu exit tears down takeover (unregister BHO);
            // a config-driven hide only removes the icon, leaving takeover on.
            let hidden = HIDDEN_BY_CONFIG.swap(false, Ordering::SeqCst);
            quit(hwnd, !hidden, !hidden);
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
