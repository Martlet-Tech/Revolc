//! Application orchestration: COM init, config handle + watcher, window, message loop.

use std::sync::Arc;

use notify::Watcher;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::{self, ConfigHandle};
use crate::shell;
use crate::takeover;
use crate::ui::{self, MainWindow};

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
    takeover::spawn(cfg.clone());

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
