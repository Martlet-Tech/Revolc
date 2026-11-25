// Release build: no console window. Debug keeps the console for live logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bookmarks;
mod config;
mod crash;
mod logging;
mod shell;
mod tabs;
mod ui;

use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::GetFileAttributesW;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, PostMessageW,
};

/// The startup argument becomes the first tab's folder. Reject anything that
/// is not an existing path — `revolc.exe Explorer` used to spawn a literal
/// "Explorer" folder (TODO P0-2). A missing/invalid arg simply falls back to
/// the home directory in `MainWindow::create`.
fn valid_startup_path(arg: Option<String>) -> Option<String> {
    let arg = arg?;
    if arg.is_empty() {
        return None;
    }
    let wide: Vec<u16> = arg.encode_utf16().chain(Some(0)).collect();
    let attrs = unsafe { GetFileAttributesW(windows::core::PCWSTR(wide.as_ptr())) };
    // INVALID_FILE_ATTRIBUTES = u32::MAX means the path does not exist.
    const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    if attrs == INVALID_FILE_ATTRIBUTES {
        tracing::warn!(?arg, "startup path does not exist, ignoring");
        return None;
    }
    if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
        tracing::warn!(?arg, "startup path is not a directory, ignoring");
        return None;
    }
    Some(arg)
}

use revolc_common::messages::WM_APP_QUIT;

/// `revolc.exe --shutdown`: ask every window (and the optional tray) to quit
/// politely. Never kill the process — absorbed Explorer windows are its
/// children and die with it (2026-11-17 crash loop).
fn shutdown_running() -> i32 {
    unsafe {
        let mut sent = 0usize;
        let _ = EnumWindows(
            Some(quit_window),
            LPARAM(&mut sent as *mut usize as isize),
        );
        if let Ok(h) = FindWindowW(w!("RevolcResident"), None) {
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

unsafe extern "system" fn quit_window(hwnd: HWND, lp: LPARAM) -> BOOL {
    let sent = &mut *(lp.0 as *mut usize);
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    if n > 0 && String::from_utf16_lossy(&cls[..n as usize]) == "RevolcMain" {
        let _ = PostMessageW(hwnd, WM_APP_QUIT, WPARAM(0), LPARAM(0));
        *sent += 1;
    }
    BOOL(1)
}

fn main() {
    let started = Instant::now();
    crash::install();
    if std::env::var_os("REVOLC_CRASH_SELFTEST").is_some() {
        crash::selftest();
    }
    logging::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--shutdown") {
        std::process::exit(shutdown_running());
    }
    let adopt_only = args.iter().any(|a| a == "--adopt");
    let settings_only = args.iter().any(|a| a == "--settings");
    let path_arg = args.iter().find(|a| !a.starts_with("--")).cloned();
    let initial_path = if adopt_only || settings_only {
        None
    } else {
        valid_startup_path(path_arg)
    };
    tracing::info!(?initial_path, adopt_only, settings_only, "starting");

    let exit_code = app::run(initial_path, adopt_only, settings_only);
    tracing::info!(elapsed = ?started.elapsed(), exit_code, "exited");
    std::process::exit(exit_code);
}
