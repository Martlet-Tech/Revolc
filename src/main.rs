// Release build: no console window. Debug keeps the console for live logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod logging;
mod shell;
mod tabs;
mod takeover;
mod ui;

use std::time::Instant;

fn main() {
    let started = Instant::now();
    logging::init();

    // Command line: revolc.exe [path]
    let args: Vec<String> = std::env::args().skip(1).collect();
    let initial_path = args.first().cloned();
    tracing::info!(?initial_path, "starting");

    let exit_code = app::run(initial_path);
    tracing::info!(elapsed = ?started.elapsed(), exit_code, "exited");
    std::process::exit(exit_code);
}
