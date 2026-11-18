//! Diagnostic: prove the app icon really is embedded and loadable the way
//! `src/ui/icon.rs` loads it (RT_GROUP_ICON id 1 at the system icon metrics).
//!
//! `ExtractAssociatedIcon` in Explorer only proves *some* icon exists, and the
//! runtime loader silently falls back to the stock icon, so a wrong resource id
//! would otherwise only be visible as "the tray icon looks generic".
//!
//! Usage: cargo run --release --example icon_probe [path\to\revolc.exe]

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, LoadLibraryExW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadImageW, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON,
};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        format!(
            "{}\\target\\release\\revolc.exe",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let module = match LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        ) {
            Ok(h) => h,
            Err(e) => {
                println!("FAIL cannot open {path}: {e}");
                std::process::exit(1);
            }
        };
        println!("opened {}", path);
        println!(
            "own module handle: {:?}",
            GetModuleHandleW(None).unwrap_or(HMODULE::default())
        );

        for (label, metric) in [("big (SM_CXICON)", SM_CXICON), ("small (SM_CXSMICON)", SM_CXSMICON)]
        {
            let px = GetSystemMetrics(metric);
            // id 1 == the resource id build.rs assigns ("1 ICON \"icon.ico\"").
            match LoadImageW(
                module,
                PCWSTR(1 as *const u16),
                IMAGE_ICON,
                px,
                px,
                LR_DEFAULTCOLOR,
            ) {
                Ok(h) if !h.is_invalid() => println!("  {label}: OK {px}x{px} (handle {:?})", h.0),
                Ok(_) => println!("  {label}: loaded an invalid handle at {px}px"),
                Err(e) => println!("  {label}: FAIL {e}"),
            }
        }
    }
}
