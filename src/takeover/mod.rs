//! Takeover module — BHO architecture (clover/analysis/Clover_Absorb_Mechanism.md).
//!
//! The BHO (bho crate, revolc_bho.dll) registers itself per-user (HKCU) as an
//! Explorer Browser Helper Object. Explorer reports every folder window to
//! Revolc via WM_APP_ABSORB. This module only ensures the BHO is registered;
//! there is no hook, no polling, no condition chain.

use std::os::windows::ffi::OsStrExt as _;
use windows::core::PCWSTR;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// Ensure the BHO DLL is registered for the current user (idempotent).
/// The DLL ships next to revolc.exe as revolc_bho.dll.
pub fn ensure_bho_registered() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "cannot locate exe");
            return;
        }
    };
    let Some(exe_dir) = exe.parent() else { return };
    let dll = exe_dir.join("revolc_bho.dll");
    if !dll.exists() {
        tracing::warn!(dll = %dll.display(), "bho dll missing, takeover unavailable");
        return;
    }

    unsafe {
        let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
        let Ok(module) = LoadLibraryW(PCWSTR::from_raw(wide.as_ptr())) else {
            tracing::error!("LoadLibrary(bho) failed");
            return;
        };
        let Some(proc) = GetProcAddress(module, windows::core::s!("DllRegisterServer")) else {
            tracing::error!("DllRegisterServer export missing");
            let _ = FreeLibrary(module);
            return;
        };
        let register: extern "system" fn() -> i32 = std::mem::transmute(proc);
        let hr = register();
        let _ = FreeLibrary(module);
        if hr == 0 {
            tracing::info!("bho registered (per-user)");
        } else {
            tracing::error!(hr, "bho registration failed");
        }
    }
}
