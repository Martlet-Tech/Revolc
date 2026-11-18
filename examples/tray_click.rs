//! Dev diagnostic: simulate a tray **left click** and report whether the main
//! window actually took the foreground.
//! Usage: `cargo run --example tray_click` (with Revolc running).
//!
//! Posts the same message the Shell posts (`WM_APP_TRAY` + `WM_LBUTTONUP`) and
//! re-reads `GetForegroundWindow` afterwards, so "clicking the tray icon does
//! nothing" can be told apart from "the message arrives but activation is
//! refused by the foreground lock".

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetClassNameW, GetForegroundWindow, GetWindowTextW, IsIconic, IsZoomed,
    PostMessageW, WM_LBUTTONUP,
};
use windows::core::w;

const WM_APP_TRAY: u32 = 0x8004;

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn title_of(h: HWND) -> String {
    let mut buf = [0u16; 260];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn report(label: &str, main: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        println!(
            "{label}: foreground={} class={} title={:?} | main iconic={} zoomed={} is_fg={}",
            fg.0 as isize,
            class_of(fg),
            title_of(fg),
            IsIconic(main).as_bool(),
            IsZoomed(main).as_bool(),
            fg == main
        );
    }
}

fn main() {
    unsafe {
        let main = match FindWindowW(w!("RevolcMain"), None) {
            Ok(h) if !h.0.is_null() => h,
            _ => {
                println!("RevolcMain not found — is Revolc running?");
                return;
            }
        };
        println!("RevolcMain hwnd = {}", main.0 as isize);
        report("before", main);
        let _ = PostMessageW(main, WM_APP_TRAY, WPARAM(0), LPARAM(WM_LBUTTONUP as isize));
        std::thread::sleep(std::time::Duration::from_millis(400));
        report("after ", main);
    }
}
