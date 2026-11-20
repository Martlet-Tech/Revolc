//! Message IDs shared by the tray, window processes and the BHO.

use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;

/// "Quit for real" — restore all absorbed windows, then exit.
pub const WM_APP_QUIT: u32 = 0x8010;
/// Tray icon callback (private to the tray window).
pub const WM_APP_TRAY: u32 = 0x8004;
/// BHO → Revolc: "a folder window was created". Registered so every process
/// sees the same ID, including the low-integrity Explorer/BHO process.
pub fn absorb_msg() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("RevolcAbsorbExplorer")) })
}
