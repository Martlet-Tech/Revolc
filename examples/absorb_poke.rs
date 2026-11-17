//! Dev diagnostic: post the "RevolcAbsorbExplorer" registered message to the
//! running Revolc main window, targeting an Explorer top-level window.
//! Usage: `cargo run --example absorb_poke [-- <hwnd>]` (hwnd in decimal).


use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, PostMessageW, RegisterWindowMessageW,
};
use windows::core::w;

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

static mut FOUND: Option<Vec<isize>> = None;

unsafe extern "system" fn enum_cb(h: HWND, _l: LPARAM) -> BOOL {
    let c = class_of(h);
    if c == "CabinetWClass" || c == "ExploreWClass" {
        println!("cabinet hwnd={} class={c}", h.0 as isize);
        FOUND.get_or_insert_with(Vec::new).push(h.0 as isize);
    }
    BOOL(1)
}

fn main() {
    unsafe {
        let target = FindWindowW(w!("RevolcMain"), None).unwrap();
        println!("RevolcMain hwnd = {}", target.0 as isize);
        if target.0.is_null() {
            println!("revolc window not found");
            return;
        }
        let msg = RegisterWindowMessageW(w!("RevolcAbsorbExplorer"));
        println!("msg id = {msg:#x}");

        EnumWindows(Some(enum_cb), LPARAM(0)).ok();
        let found = FOUND.take().unwrap_or_default();

        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(|a| a == "close").unwrap_or(false) {
            // `absorb_poke close <tab-idx>`: post WM_APP_TAB_CLOSE (lParam = idx)
            let idx: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let ok = PostMessageW(target, 0x8002, WPARAM(0), LPARAM(idx as isize));
            println!("posted WM_APP_TAB_CLOSE idx={idx} -> {ok:?}");
            return;
        }
        let pick: Option<isize> = args.first().and_then(|s| s.parse().ok())
            .or_else(|| found.first().copied());
        match pick {
            Some(v) => {
                let ok = PostMessageW(target, msg, WPARAM(v as usize), LPARAM(0));
                println!("posted hwnd={v} -> {ok:?}");
            }
            None => println!("no cabinet window found"),
        }
    }
}
