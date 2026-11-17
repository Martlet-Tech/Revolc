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
/// revolc.exe as revolc_bho.dll. With mode != "redirect" the BHO is fully
/// unregistered (HKLM + HKCU) so system Explorer is 100% pristine.
pub fn ensure_bho_registered(mode: &str) {
    if mode != "redirect" {
        unregister_bho();
        return;
    }
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
        tracing::warn!("HKLM BHO key missing, requesting elevation for registration");
        elevate_register(&dll);
        // regsvr32 runs async after UAC consent; wait for it to finish.
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(700));
            if hklm_bho_present() {
                break;
            }
        }
        if hklm_bho_present() {
            tracing::info!("HKLM BHO key present after elevated registration");
        } else {
            tracing::error!("HKLM BHO key still missing after elevation attempt");
        }
    } else {
        tracing::info!("HKLM BHO key present");
    }
}

fn unregister_bho() {
    let exe = std::env::current_exe().ok();
    let dll = exe.and_then(|e| e.parent().map(|d| d.join("revolc_bho.dll")));
    let Some(dll) = dll.filter(|p| p.exists()) else { return };
    unsafe {
        let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
        let Ok(module) = LoadLibraryW(PCWSTR::from_raw(wide.as_ptr())) else { return };
        if let Some(proc) = GetProcAddress(module, windows::core::s!("DllUnregisterServer")) {
            let unreg: extern "system" fn() -> i32 = std::mem::transmute(proc);
            let hr = unreg();
            tracing::info!(hr, "bho unregistered (per-user)");
        }
        let _ = FreeLibrary(module);
    }
    if hklm_bho_present() {
        tracing::warn!("HKLM BHO key present, requesting elevation for unregister");
        elevate(&dll, "/u /s");
    }
    tracing::info!("takeover fully disabled, system Explorer pristine");
}

fn hklm_bho_present() -> bool {
    hklm_key(BHO_CLSID_PATH.to_string())
        && hklm_key(format!(
            "Software\\Classes\\CLSID\\{{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}}\\InprocServer32"
        ))
}

fn hklm_key(path: String) -> bool {
    let path = HSTRING::from(path);
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
    elevate(dll, "/s");
}

fn elevate(dll: &std::path::Path, flags: &str) {
    unsafe {
        let file = HSTRING::from("regsvr32.exe");
        let params = HSTRING::from(format!("{} \"{}\"", flags, dll.display()));
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
