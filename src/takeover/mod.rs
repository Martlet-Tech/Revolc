//! Takeover module — B1 "redirect" mode (doc/Takeover_Research.md §3/§4).
//!
//! WinEventHook watches for new Explorer folder windows (CabinetWClass).
//! On detection: read its path via IShellWindows COM → close the Explorer
//! window → launch a Revolc instance with that path.
//!
//! Zero registry writes. Toggle = config `takeover.mode`:
//!   "off"      — callback no-ops, system Explorer untouched (取消接管即时生效)
//!   "redirect" — intercept as above
//!
//! Runs on a dedicated thread (the ONLY other thread besides config-watcher;
//! hook callbacks are delivered through this thread's message pump).

use std::sync::Arc;

use windows::core::Interface as _;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Shell::{self as shell, IShellWindows, IWebBrowser2};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetClassNameW, IsWindowVisible, PostMessageW, TranslateMessage,
    EVENT_OBJECT_SHOW, MSG, OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WM_CLOSE,
};

use crate::config::ConfigHandle;

/// Explorer folder window class (Win7+; "This PC" uses it too but is filtered
/// out by path validation below).
const EXPLORER_CLASS: &str = "CabinetWClass";

static CFG: std::sync::OnceLock<Arc<ConfigHandle>> = std::sync::OnceLock::new();
static HANDLED: std::sync::Mutex<Option<std::collections::HashSet<isize>>> =
    std::sync::Mutex::new(None);

pub fn spawn(cfg: Arc<ConfigHandle>) {
    let _ = CFG.set(cfg);
    std::thread::Builder::new()
        .name("takeover-watcher".into())
        .spawn(watch_thread)
        .expect("spawn takeover thread");
}

fn watch_thread() {
    unsafe {
        // MTA is fine: IShellWindows is a free-threaded shell service.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let hook = SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_SHOW,
            windows::Win32::Foundation::HMODULE::default(), // WINEVENT_OUTOFCONTEXT requires null hmod
            Some(winevent_callback),
            0, // all processes
            0, // all threads
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.0.is_null() {
            tracing::error!("SetWinEventHook failed");
            return;
        }
        tracing::info!("takeover watcher armed (EVENT_OBJECT_CREATE..SHOW)");

        // Hook callbacks are delivered through this thread's message pump.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWinEvent(hook);
        tracing::info!("takeover watcher stopped");
    }
}

unsafe extern "system" fn winevent_callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    idobject: i32,
    _idchild: i32,
    _thread: u32,
    _time: u32,
) {
    // Diagnostics: sample the first events to prove the hook is alive.
    static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seen = SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if seen < 5 {
        tracing::info!(event, ?hwnd, idobject, "winevent sample");
    }

    // Fast-path filters first: mode check, object type, visibility, class.
    let Some(cfg) = CFG.get() else {
        tracing::warn!("winevent: CFG not set");
        return;
    };
    let mode = cfg.get().takeover.mode.clone();
    if mode != "redirect" {
        if seen < 5 {
            tracing::info!(%mode, "winevent skipped: mode off");
        }
        return;
    }
    if idobject != OBJID_WINDOW.0 || hwnd.0.is_null() {
        return;
    }

    // Class check FIRST (logging it proves we can see Explorer windows).
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if class != EXPLORER_CLASS {
        return;
    }
    tracing::info!(?hwnd, %class, "explorer window class hit");

    if !IsWindowVisible(hwnd).as_bool() {
        tracing::info!(?hwnd, "explorer window not visible yet, skipping (will retry on next SHOW)");
        // Not marked handled: a later SHOW event for this window retries.
        return;
    }

    // Handle each window once (SHOW fires repeatedly).
    let key = hwnd.0 as isize;
    {
        let mut slot = HANDLED.lock().unwrap();
        let handled = slot.get_or_insert_with(Default::default);
        if !handled.insert(key) {
            return;
        }
    }

    tracing::info!(?hwnd, "explorer window detected, reading path");
    match read_folder_path(hwnd) {
        Some(path) => {
            tracing::info!(path = %path, "redirecting to revolc");
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            launch_revolc(&path);
        }
        None => {
            tracing::debug!(?hwnd, "no resolvable folder path; leaving window alone");
            // Virtual locations (This PC / Control Panel / Recycle Bin) are
            // deliberately NOT intercepted — spec §7.2.
            if let Some(set) = HANDLED.lock().unwrap().as_mut() {
                set.remove(&key);
            }
        }
    }
}

/// Read the folder path of an Explorer window via IShellWindows.
/// Retries briefly: the window is created before it navigates.
fn read_folder_path(hwnd: HWND) -> Option<String> {
    unsafe {
        let windows_svc: IShellWindows =
            CoCreateInstance(&shell::ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;

        for _ in 0..30 {
            let count = windows_svc.Count().unwrap_or(0);
            for i in 0..count {
                let Ok(dispatch) = windows_svc.Item(&windows::core::VARIANT::from(i)) else {
                    continue;
                };
                let Ok(browser) = dispatch.cast::<IWebBrowser2>() else {
                    continue;
                };
                let Ok(whwnd) = browser.HWND() else { continue };
                if whwnd != windows::Win32::Foundation::SHANDLE_PTR(hwnd.0 as isize) {
                    continue;
                }
                if let Ok(url) = browser.LocationURL() {
                    if let Some(path) = file_url_to_path(&url.to_string()) {
                        return Some(path);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    }
}

/// "file:///D:/dir%20name" -> "D:\dir name". Returns None for non-file URLs.
fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file:///")?;
    // Percent-decode.
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let v = u8::from_str_radix(hex, 16).ok()?;
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(out).ok()?.replace('/', "\\");
    // Filter virtual/invalid locations (spec §7.2): must be a real drive path
    // like "D:\..." — "This PC", Recycle Bin etc. produce non-drive URLs.
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' {
        return None;
    }
    Some(path)
}

fn launch_revolc(path: &str) {
    use std::os::windows::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "cannot locate current exe");
            return;
        }
    };
    // CREATE_NO_WINDOW: suppress inherited console (debug builds would flash
    // a black window otherwise; release has no console anyway).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new(exe)
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_) => tracing::info!(path, "revolc launched"),
        Err(e) => tracing::error!(error = %e, path, "launch failed"),
    }
}
