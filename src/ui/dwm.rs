//! Tiny DWM helpers. Windows 11 only; every call is best-effort because the
//! same binary targets Win7 (spec §2: 特性缺失只影响视觉效果).

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};

/// Ask DWM to round the frame corners.
///
/// A window that strips `WS_CAPTION` loses the "this is a normal app window"
/// hint DWM uses to pick its corner treatment, so an integrated-title-bar
/// window shows up square on Win11 while every native app is rounded. Failures
/// (Win10 and older, or DWM disabled) are ignored: the fallback is a square
/// corner, which is also the Win7 look.
pub fn round_corners(hwnd: HWND) {
    let pref = DWMWCP_ROUND;
    let r = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&pref) as u32,
        )
    };
    if let Err(e) = r {
        tracing::debug!(error = %e, "DWM corner preference not supported");
    }
}
