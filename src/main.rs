// Release build: no console window. Debug keeps the console for live logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod crash;
mod logging;
mod shell;
mod tabs;
mod takeover;
mod ui;

use std::time::Instant;

use windows::core::w;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

/// `revolc.exe --shutdown`: ask the running instance to quit the polite way.
/// Never kill the process — absorbed Explorer windows are its children and
/// die with it (2026-11-17 crash loop). WM_APP_QUIT tears down every window
/// and the resident (docs/双进程架构设计.md Phase 1).
fn shutdown_running() -> i32 {
    unsafe {
        // Resident-only sessions (all windows closed, redirect+tray) have no
        // RevolcMain, so try the resident class as well.
        let target = match FindWindowW(w!("RevolcMain"), None) {
            Ok(h) if !h.0.is_null() => h,
            _ => match FindWindowW(w!("RevolcResident"), None) {
                Ok(h) if !h.0.is_null() => h,
                _ => {
                    eprintln!("Revolc is not running");
                    return 1;
                }
            },
        };
        let _ = PostMessageW(target, ui::WM_APP_QUIT, WPARAM(0), LPARAM(0));
        0
    }
}

fn main() {
    let started = Instant::now();
    // Before anything else: a crash must leave a dump, not just an Event
    // Viewer line naming a system DLL.
    crash::install();
    if std::env::var_os("REVOLC_CRASH_SELFTEST").is_some() {
        crash::selftest();
    }
    logging::init();

    // Command line: revolc.exe [path] | --shutdown
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
