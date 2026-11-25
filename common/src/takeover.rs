//! Takeover module — BHO architecture (clover/analysis/Clover_Absorb_Mechanism.md).
//!
//! Three independent concerns live here, and the settings page exposes them as
//! three separate rows:
//!
//! 1. **System registration** (`install_bho` / `uninstall_bho_keys`): writes or
//!    removes the HKLM/HKCU BHO + CLSID keys. This is the only part that needs
//!    UAC, and the only part that survives a reboot.
//! 2. **Takeover switch** (`set_takeover_enabled`): just writes/removes
//!    `%APPDATA%\revolc\disabled`. No registry write, no UAC, safe to flip
//!    while the app is running. The BHO checks the marker at SetSite and does
//!    nothing when it is present.
//! 3. **Path repair** (`repair_bho_path_if_installed`): a portable build can be
//!    moved after registration. If HKLM still points at the old DLL path, we
//!    re-register once with elevation. We deliberately do *not* install a BHO
//!    that was never installed — uninstall must stay uninstalled.

use std::io::ErrorKind;
use std::path::Path;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, REG_VALUE_TYPE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOASYNC};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const BHO_CLSID_PATH: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Browser Helper Objects\\{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}";
const BHO_CLSID_REG_PATH: &str =
    "Software\\Classes\\CLSID\\{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}";
const BHO_INPROC_PATH: &str =
    "Software\\Classes\\CLSID\\{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}\\InprocServer32";

/// `%APPDATA%\revolc\disabled`. The BHO checks this at SetSite and does
/// nothing when present. This is the *temporary* takeover switch — it is not
/// an uninstall, and it never touches the registry.
fn disabled_marker() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .map(|p| p.join("revolc").join("disabled"))
}

/// Flip the temporary takeover switch. `true` removes the marker (BHO active),
/// `false` writes it (BHO loaded but inert). No elevation, no registry write.
pub fn set_takeover_enabled(enabled: bool) {
    let Some(path) = disabled_marker() else { return };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let result = if enabled {
        // Two processes can flip this at once. Losing a remove race to the
        // other process (or to the file simply being absent) still means the
        // marker is gone — no need to log a scary ERROR for that.
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied) => Ok(()),
            Err(e) => Err(e),
        }
    } else {
        let mut last = None;
        for _ in 0..3 {
            match std::fs::write(&path, b"1") {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        match last {
            Some(e) => Err(e),
            None => Ok(()),
        }
    };
    match result {
        Ok(()) => tracing::info!(enabled, "takeover switch updated (marker only)"),
        Err(e) => tracing::error!(error = %e, "could not update takeover switch"),
    }
}

/// Current state of the temporary takeover switch.
pub fn is_takeover_enabled() -> bool {
    !disabled_marker().map(|p| p.exists()).unwrap_or(false)
}

/// True when the machine-level BHO keys exist. Explorer only reads the HKLM
/// list, so HKCU-only registration does not count as installed.
pub fn is_bho_installed() -> bool {
    hklm_bho_present()
}

fn current_dll() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dll = exe.parent()?.join("revolc_bho.dll");
    dll.exists().then_some(dll)
}

/// Install (or refresh) the BHO registration. Always goes through an elevated
/// `regsvr32` so HKLM is written directly; the settings row promises UAC.
pub fn install_bho() -> Result<(), String> {
    let dll = current_dll().ok_or_else(|| "revolc_bho.dll not found next to the exe".to_string())?;
    elevate(Path::new("regsvr32.exe"), &format!("/s \"{}\"", dll.display()))?;
    wait_for_install(&dll)
}

/// If the BHO is already installed but HKLM points at a different DLL path,
/// re-register once. Called on tray startup so moving a portable folder heals
/// itself without turning an uninstalled system back into an installed one.
pub fn repair_bho_path_if_installed() {
    if !hklm_bho_present() {
        return;
    }
    let Some(dll) = current_dll() else { return };
    let stale = match hklm_inproc_path() {
        Some(p) => !same_dll_path(Path::new(&p), &dll),
        None => true,
    };
    if !stale {
        return;
    }
    tracing::warn!(current = %dll.display(), "HKLM BHO path is stale, re-registering");
    let cmd = format!("/s \"{}\"", dll.display());
    match elevate(Path::new("regsvr32.exe"), &cmd).and_then(|_| wait_for_install(&dll)) {
        Ok(()) => tracing::info!("HKLM BHO path repaired"),
        Err(e) => tracing::error!(error = %e, "could not repair HKLM BHO path"),
    }
}

fn wait_for_install(dll: &Path) -> Result<(), String> {
    // The UAC prompt is user-paced and can sit for a minute. This runs on a
    // worker thread, so waiting does not freeze the settings page.
    for _ in 0..120 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if hklm_bho_present()
            && hklm_inproc_path().map(|p| same_dll_path(Path::new(&p), dll)).unwrap_or(false)
        {
            return Ok(());
        }
    }
    Err("elevated registration did not produce a matching HKLM BHO entry".into())
}

fn same_dll_path(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return x == y;
    }
    let norm = |p: &Path| p.to_string_lossy().trim_matches('"').to_lowercase();
    norm(a) == norm(b)
}

/// Remove every BHO/CLSID key we own, in both hives. Called from the elevated
/// `revolc-tray.exe --uninstall` helper, so HKLM deletion is permitted and no
/// DLL needs to be loaded (important after the user moved or deleted it).
pub fn uninstall_bho_keys() {
    unsafe {
        for (hive, path) in [
            (HKEY_LOCAL_MACHINE, BHO_CLSID_PATH),
            (HKEY_LOCAL_MACHINE, BHO_CLSID_REG_PATH),
            (HKEY_CURRENT_USER, BHO_CLSID_PATH),
            (HKEY_CURRENT_USER, BHO_CLSID_REG_PATH),
        ] {
            let path = HSTRING::from(path);
            let _ = RegDeleteTreeW(hive, PCWSTR::from_raw(path.as_ptr()));
        }
    }
    tracing::info!("BHO registry keys removed (HKLM + HKCU)");
}

/// Launch the elevated uninstall helper. The helper quits all Revolc
/// processes, removes the registry keys, the Run entry and `%APPDATA%\revolc\`.
pub fn launch_uninstaller(tray_exe: &Path) -> Result<(), String> {
    elevate(tray_exe, "--uninstall")
}

/// Generic elevated launch: `file` is the executable, `params` its arguments.
fn elevate(file: &Path, params: &str) -> Result<(), String> {
    let quoted = format!("\"{}\"", file.display());
    let file_h = HSTRING::from(quoted.as_str());
    let param_h = HSTRING::from(params);
    unsafe {
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: windows::core::w!("runas"),
            lpFile: PCWSTR::from_raw(file_h.as_ptr()),
            lpParameters: PCWSTR::from_raw(param_h.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        if ShellExecuteExW(&mut sei).is_ok() {
            Ok(())
        } else {
            Err(format!("elevated launch failed for {}", file.display()))
        }
    }
}

fn hklm_bho_present() -> bool {
    hklm_key(BHO_CLSID_PATH) && hklm_key(BHO_INPROC_PATH)
}

fn hklm_key(path: &str) -> bool {
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

fn hklm_inproc_path() -> Option<String> {
    let path = HSTRING::from(BHO_INPROC_PATH);
    unsafe {
        let mut hk = HKEY::default();
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hk,
        )
        .is_err()
        {
            return None;
        }
        let mut buf = [0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        let mut kind = REG_VALUE_TYPE(0);
        let r = RegQueryValueExW(
            hk,
            PCWSTR::null(),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut len),
        );
        let _ = RegCloseKey(hk);
        if r.is_ok() && kind == REG_SZ && len >= 2 && (len as usize) <= buf.len() * 2 {
            // REG_SZ includes its terminating NUL in `len`. Keeping it would
            // make every path comparison fail and re-trigger UAC on startup.
            Some(trim_nul(String::from_utf16_lossy(
                &buf[..(len as usize / 2)],
            )))
        } else {
            None
        }
    }
}

fn trim_nul(mut value: String) -> String {
    while value.ends_with('\0') {
        value.pop();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{same_dll_path, trim_nul};
    use std::path::Path;

    #[test]
    fn reg_sz_nul_terminator_is_stripped() {
        assert_eq!(trim_nul("D:\\Downloads\\revolc\\revolc_bho.dll\0".into()),
                   "D:\\Downloads\\revolc\\revolc_bho.dll");
    }

    #[test]
    fn path_compare_is_case_insensitive_and_ignores_quotes() {
        assert!(same_dll_path(
            Path::new("\"C:\\Tools\\Revolc\\revolc_bho.dll\""),
            Path::new("c:\\tools\\revolc\\REVOLC_BHO.DLL")
        ));
        assert!(!same_dll_path(
            Path::new("C:\\Tools\\Revolc\\revolc_bho.dll"),
            Path::new("D:\\Tools\\Revolc\\revolc_bho.dll")
        ));
    }
}
