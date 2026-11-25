//! Message IDs shared by the tray, window processes and the BHO.

use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;

/// "Quit for real" — restore all absorbed windows, then exit.
pub const WM_APP_QUIT: u32 = 0x8010;
/// Tray icon callback (private to the tray window).
pub const WM_APP_TRAY: u32 = 0x8004;
/// Tray -> hidden window: show it again (close button set to "hide").
pub const WM_APP_RESTORE: u32 = 0x8011;
/// Tray -> window: close every tab (sending WM_CLOSE to each absorbed
/// Explorer pane) and then close the frame. Unlike WM_APP_QUIT, the panes are
/// closed rather than restored to the desktop.
pub const WM_APP_CLOSE_TABS: u32 = 0x8012;
/// Registry install/uninstall finished in a worker thread -> repaint the
/// settings page so the 系统注册 row shows the new state.
pub const WM_APP_REGISTRY_CHANGED: u32 = 0x8013;
/// BHO → Revolc: "a folder window was created". Registered so every process
/// sees the same ID, including the low-integrity Explorer/BHO process.
pub fn absorb_msg() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("RevolcAbsorbExplorer")) })
}
