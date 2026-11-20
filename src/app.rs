//! Application orchestration for one `revolc.exe` process = one window.
//!
//! Session-level concerns (tray icon, BHO registration, foreground ledger,
//! launcher) live in `revolc-tray.exe` — see docs/双进程架构设计.md Phase 2.
//! This process only owns its window, config view and message loop.

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicIsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::Watcher;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{self, ConfigHandle};
use crate::shell;
use crate::ui::{self, MainWindow};

/// Main window hwnd for the console ctrl handler (set once after creation).
#[cfg(debug_assertions)]
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// Console Ctrl+C / console-close handler (debug builds only — release has no
/// console). Routes termination through WM_APP_QUIT so absorbed Explorer
/// windows are restored via WM_DESTROY instead of being destroyed inside our
/// window tree (the 2026-11-17 crash loop came from exactly that kind of kill).
#[cfg(debug_assertions)]
unsafe extern "system" fn console_ctrl_handler(ctrl: u32) -> windows::Win32::Foundation::BOOL {
    use windows::Win32::Foundation::BOOL;
    if ctrl <= 2 || (5..=6).contains(&ctrl) {
        let hwnd = MAIN_HWND.load(AtomicOrdering::SeqCst);
        if hwnd != 0 {
            let _ = PostMessageW(HWND(hwnd as _), ui::WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
        return BOOL(if (5..=6).contains(&ctrl) { 0 } else { 1 });
    }
    BOOL(0)
}

pub fn run(initial_path: Option<String>) -> i32 {
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

    #[cfg(debug_assertions)]
    unsafe {
        use windows::Win32::System::Console::SetConsoleCtrlHandler;
        MAIN_HWND.store(window.hwnd_raw() as isize, AtomicOrdering::SeqCst);
        if SetConsoleCtrlHandler(Some(console_ctrl_handler), true).is_err() {
            tracing::warn!("SetConsoleCtrlHandler failed");
        }
    }

    let code = ui::run_message_loop();

    drop(watcher);
    drop(window);
    code
}

/// Watch only `revolc.toml` (never the whole directory) and debounce bursts.
/// The old directory watcher fired several reloads per second and drowned
/// `revolc.log` (found during the 2026-11-20 log review).
fn spawn_config_watcher(cfg: Arc<ConfigHandle>, hwnd_raw: usize) -> Option<notify::RecommendedWatcher> {
    let path = cfg.path().clone();
    let mut last = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or(Instant::now());

    let mut watcher = match notify::recommended_watcher(
        move |res: Result<notify::Event, notify::Error>| match res {
            Ok(_) => {
                let now = Instant::now();
                if now.duration_since(last) < Duration::from_millis(500) {
                    return;
                }
                last = now;
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
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "config watcher init failed");
            return None;
        }
    };

    if let Err(e) = watcher.watch(&path, notify::RecursiveMode::NonRecursive) {
        tracing::error!(error = %e, ?path, "config watch failed");
        return None;
    }
    tracing::info!("config watcher armed (single file, 500ms debounce)");
    Some(watcher)
}
