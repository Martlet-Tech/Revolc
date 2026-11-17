//! Application orchestration: COM init, config handle + watcher, window, message loop.

use std::sync::atomic::{AtomicIsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use notify::Watcher;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{self, ConfigHandle};
use crate::shell;
use crate::takeover;
use crate::ui::{self, MainWindow};

/// One RevolcMain window must exist per session. Two instances break the
/// BHO notify: FindWindowW("RevolcMain") routes the Explorer HWND to an
/// arbitrary instance, and a second instance then absorbs a window that
/// already belongs to the first (observed: same HWND absorbed twice,
/// 2026-11-17). Second launch exits; the running instance keeps absorbing.
fn ensure_single_instance() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        // Handle intentionally leaks: must stay alive for the process lifetime.
        let _mutex: HANDLE = match CreateMutexW(None, false, w!("Local\\RevolcSingleInstance")) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "CreateMutexW failed, allowing startup");
                return true;
            }
        };
        if GetLastError().0 == 183 {
            // ERROR_ALREADY_EXISTS
            tracing::warn!("another Revolc instance owns the session, exiting");
            return false;
        }
    }
    true
}

/// Main window hwnd for the console ctrl handler (set once after creation).
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// Console Ctrl+C / console-close handler (debug builds only — release has no
/// console). Routes termination through WM_CLOSE so absorbed Explorer windows
/// are restored via WM_DESTROY instead of being destroyed inside our window
/// tree (the 2026-11-17 crash loop came from exactly that kind of kill).
unsafe extern "system" fn console_ctrl_handler(ctrl: u32) -> windows::Win32::Foundation::BOOL {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;
    // CTRL_C_EVENT=0, CTRL_BREAK_EVENT=1, CTRL_CLOSE_EVENT=2, CTRL_LOGOFF=5, CTRL_SHUTDOWN=6
    if ctrl <= 2 || (5..=6).contains(&ctrl) {
        let hwnd = MAIN_HWND.load(AtomicOrdering::SeqCst);
        if hwnd != 0 {
            let _ = PostMessageW(HWND(hwnd as _), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        // Logoff/shutdown: system waits ~5s; WM_CLOSE (restore + quit) is fast.
        return BOOL(if (5..=6).contains(&ctrl) { 0 } else { 1 });
    }
    BOOL(0)
}

pub fn run(initial_path: Option<String>) -> i32 {
    if !ensure_single_instance() {
        return 0;
    }

    if let Err(e) = shell::init_com() {
        tracing::error!(error = %e, "CoInitializeEx failed");
        return 1;
    }

    let cfg = Arc::new(ConfigHandle::new(config::config_path()));

    let window = match MainWindow::new(cfg.clone(), initial_path) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "window creation failed");
            return 1;
        }
    };

    let watcher = spawn_config_watcher(cfg.clone(), window.hwnd_raw());
    takeover::ensure_bho_registered(&cfg.get().takeover.mode);

    // Console termination routes through WM_CLOSE → WM_DESTROY → restore.
    #[cfg(debug_assertions)]
    unsafe {
        use windows::Win32::System::Console::SetConsoleCtrlHandler;
        MAIN_HWND.store(window.hwnd_raw() as isize, AtomicOrdering::SeqCst);
        if SetConsoleCtrlHandler(Some(console_ctrl_handler), true).is_err() {
            tracing::warn!("SetConsoleCtrlHandler failed");
        }
    }

    let code = window.run_message_loop();

    drop(watcher);
    code
}

/// config-watcher thread (design §2: the ONLY non-main thread in M0).
/// File change → reload config on the watcher thread (ArcSwap is thread-safe)
/// → notify the main thread via a posted message (no cross-thread HWND use).
fn spawn_config_watcher(cfg: Arc<ConfigHandle>, hwnd_raw: usize) -> Option<notify::RecommendedWatcher> {
    let path = cfg.path().clone();
    let watch_dir = path.parent().map(std::path::Path::to_path_buf);

    let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        match res {
            Ok(ev) => {
                tracing::debug!(?ev, "fs event");
                cfg.reload();
                unsafe {
                    let _ = PostMessageW(
                        HWND(hwnd_raw as _),
                        ui::WM_APP_CONFIG_CHANGED,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            }
            Err(e) => tracing::error!(error = %e, "config watcher error"),
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "config watcher init failed");
            return None;
        }
    };

    if let Some(dir) = watch_dir {
        if let Err(e) = watcher.watch(&dir, notify::RecursiveMode::NonRecursive) {
            tracing::error!(error = %e, ?dir, "config watch failed");
            return None;
        }
    }
    tracing::info!("config watcher armed");
    Some(watcher)
}
