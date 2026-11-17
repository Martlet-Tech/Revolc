//! Takeover module — BHO architecture (clover/analysis/Clover_Absorb_Mechanism.md).
//!
//! The BHO (bho crate, revolc_bho.dll) registers as an Explorer Browser Helper
//! Object. Explorer reports every folder window to Revolc via WM_APP_ABSORB.
//! No hook, no polling, no condition chain.
//!
//! Registration quirk (evidence: Clover's RGS writes HKLM): Explorer loads the
//! BHO list from HKLM only. DllRegisterServer tries HKLM first and falls back
//! to HKCU; this module detects the HKLM-missing case and retries once with
//! elevation (regsvr32 via UAC).

use std::os::windows::ffi::OsStrExt as _;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOASYNC};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const BHO_CLSID_PATH: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Browser Helper Objects\\{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}";

/// Ensure the BHO DLL is registered (idempotent). The DLL ships next to
/// revolc.exe as revolc_bho.dll.
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

    // 1) per-user registration (CLSID activation + HKCU BHO fallback)
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
            return;
        }
    }

    // 2) Explorer only reads the machine BHO list — if HKLM is missing,
    //    re-register elevated once (UAC prompt).
    if !hklm_bho_present() {
        tracing::warn!("HKLM BHO key missing, requesting elevation via regsvr32");
        elevate_register(&dll);
        if hklm_bho_present() {
            tracing::info!("HKLM BHO key present after elevated registration");
        } else {
            tracing::error!("HKLM BHO key still missing after elevation attempt");
        }
    } else {
        tracing::info!("HKLM BHO key present");
    }
}

fn hklm_bho_present() -> bool {
    let path = HSTRING::from(BHO_CLSID_PATH);
    unsafe {
        let mut hk = HKEY::default();
        let r = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hk,
        );
        if r.is_ok() {
            let _ = RegCloseKey(hk);
            true
        } else {
            false
        }
    }
}

fn elevate_register(dll: &std::path::Path) {
    unsafe {
        let file = HSTRING::from("regsvr32.exe");
        let params = HSTRING::from(format!("/s \"{}\"", dll.display()));
        let mut sei = SHELLEXECUTEINFOW::default();
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOASYNC;
        sei.lpVerb = windows::core::w!("runas");
        sei.lpFile = PCWSTR::from_raw(file.as_ptr());
        sei.lpParameters = PCWSTR::from_raw(params.as_ptr());
        sei.nShow = SW_HIDE.0;
        if ShellExecuteExW(&mut sei).is_ok() {
            tracing::info!("elevated regsvr32 launched");
        } else {
            tracing::error!("elevated regsvr32 failed (user declined UAC?)");
        }
    }
}
