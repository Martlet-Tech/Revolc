//! HKCU Run entry for `revolc-tray.exe`.
//!
//! Task Manager → Startup apps reads `HKCU\Software\Microsoft\Windows\
//! CurrentVersion\Run`, so an entry here is visible to the user and can be
//! disabled/removed from there without going through Revolc. `set_enabled`
//! adds or removes exactly our named value and leaves every other startup
//! entry untouched.

use windows::core::HSTRING;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_WRITE, REG_SZ, REG_VALUE_TYPE,
};

const RUN_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// Stable name shown in Task Manager. Prefixed so the user can tell what it is.
const VALUE_NAME: &str = "RevolcTray";

/// Whether our autostart entry is present.
pub fn is_enabled() -> bool {
    let path = HSTRING::from(RUN_PATH);
    let name = HSTRING::from(VALUE_NAME);
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, windows::core::PCWSTR(path.as_ptr()), 0, KEY_QUERY_VALUE, &mut key).is_err() {
            return false;
        }
        let mut kind = REG_VALUE_TYPE(0);
        let r = RegQueryValueExW(
            key,
            windows::core::PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            None,
            None,
        );
        let _ = RegCloseKey(key);
        r.is_ok() && kind == REG_SZ
    }
}

/// Add or remove the entry. `command` is the full command line, quoted so
/// paths with spaces work (e.g. `"C:\Program Files\Revolc\revolc-tray.exe"`).
pub fn set_enabled(enabled: bool, command: &str) -> Result<(), String> {
    let path = HSTRING::from(RUN_PATH);
    let name = HSTRING::from(VALUE_NAME);
    unsafe {
        let mut key = HKEY::default();
        let opened = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(path.as_ptr()),
            0,
            KEY_WRITE,
            &mut key,
        );
        if opened.is_err() {
            return Err(format!("open Run key: {:?}", opened));
        }

        let result = if enabled {
            let bytes: Vec<u8> = command
                .encode_utf16()
                .chain(Some(0))
                .flat_map(|u| u.to_le_bytes())
                .collect();
            let r = RegSetValueExW(
                key,
                windows::core::PCWSTR(name.as_ptr()),
                0,
                REG_SZ,
                Some(bytes.as_slice()),
            );
            if r.is_err() { Err(format!("set Run value: {:?}", r)) } else { Ok(()) }
        } else {
            let r = RegDeleteValueW(key, windows::core::PCWSTR(name.as_ptr()));
            // Deleting a value that is not there is fine (idempotent).
            if r.is_err() && r.0 != 2 { Err(format!("delete Run value: {:?}", r)) } else { Ok(()) }
        };
        let _ = RegCloseKey(key);
        result
    }
}

/// The command line written to the Run key. Starts the main window process,
/// which absorbs folders on its own; the tray is optional and is spawned by
/// the main process only when `[tray].enabled` is on. One startup entry, no
/// background resident when the user has turned the tray off.
pub fn launch_command(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}
