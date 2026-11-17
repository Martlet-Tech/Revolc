//! Diagnostic: list every top-level folder window (CabinetWClass /
//! ExploreWClass) with PID, visibility, parent class and title.
//! Answers "did Explorer create a window Revolc failed to absorb?".
//!
//! Usage: cargo run --release --example list_windows

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetParent, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn proc(hwnd: HWND, _l: LPARAM) -> BOOL {
    let class = class_of(hwnd);
    if class != "CabinetWClass" && class != "ExploreWClass" && class != "RevolcMain" {
        return BOOL(1);
    }
    let mut t = [0u16; 260];
    let tn = GetWindowTextW(hwnd, &mut t);
    let title = String::from_utf16_lossy(&t[..tn.max(0) as usize]);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let parent = GetParent(hwnd).unwrap_or_default();
    let pclass = if parent.0.is_null() { "-".to_string() } else { class_of(parent) };
    println!(
        "hwnd=0x{:x} pid={} visible={} parent=0x{:x}({}) class={} title={}",
        hwnd.0 as usize,
        pid,
        IsWindowVisible(hwnd).as_bool(),
        parent.0 as usize,
        pclass,
        class,
        title
    );
    BOOL(1)
}

fn main() {
    unsafe {
        let _ = EnumWindows(Some(proc), LPARAM(0));
    }
}
