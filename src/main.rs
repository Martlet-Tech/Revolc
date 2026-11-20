// Release build: no console window. Debug keeps the console for live logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod crash;
mod logging;
mod shell;
mod tabs;
mod ui;

use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, PostMessageW,
};

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
    let initial_path = args.first().cloned();
    tracing::info!(?initial_path, "starting");

    let exit_code = app::run(initial_path);
    tracing::info!(elapsed = ?started.elapsed(), exit_code, "exited");
    std::process::exit(exit_code);
}
