//! Diagnostic: dump the child-window tree under every RevolcMain frame,
//! looking for the Explorer shell view parts the BHO needs:
//!   CabinetWClass -> ... -> SHELLDLL_DefView -> SysListView32
//! Answers "can the BHO find and subclass the file list after absorption?".
//!
//! Usage: cargo run --release --example probe_shell_view

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
};

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid)); }
    pid
}

unsafe extern "system" fn child_proc(hwnd: HWND, _l: LPARAM) -> BOOL {
    let class = class_of(hwnd);
    if class == "SHELLDLL_DefView"
        || class == "SysListView32"
        || class == "DirectUIHWND"
        || class == "ShellTabWindowClass"
        || class == "CabinetWClass"
        || class == "ExploreWClass"
    {
        println!(
            "    hwnd=0x{:x} pid={} visible={} class={}",
            hwnd.0 as usize,
            pid_of(hwnd),
            IsWindowVisible(hwnd).as_bool(),
            class
        );
    }
    BOOL(1)
}

unsafe extern "system" fn top_proc(hwnd: HWND, _l: LPARAM) -> BOOL {
    let class = class_of(hwnd);
    if class == "RevolcMain" {
        println!("RevolcMain hwnd=0x{:x} pid={}:", hwnd.0 as usize, pid_of(hwnd));
        let _ = EnumChildWindows(hwnd, Some(child_proc), LPARAM(0));
    }
    BOOL(1)
}

fn main() {
    unsafe { let _ = EnumWindows(Some(top_proc), LPARAM(0)); }
}
