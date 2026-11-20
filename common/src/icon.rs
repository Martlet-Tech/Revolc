//! The application icon.
//!
//! `build.rs` embeds `assets/icon.ico` as resource id 1 in the executable, so
//! the shell (Explorer, taskbar, Alt-Tab) already shows it without any code.
//! This module loads that *same* resource for the things the shell does not do
//! for us: the `RevolcMain` window class and the notification-area icon.
//!
//! Sizes come from the system metrics, not from hard-coded pixels, so the tray
//! icon stays crisp at 125%/150% scaling (16 -> 20 -> 24 px).
//!
//! If the resource is missing (build without `rc.exe`) everything falls back to
//! the stock application icon — an icon must never crash or block startup.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadIconW, LoadImageW, HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR,
    LR_SHARED, SM_CXICON, SM_CXSMICON, SYSTEM_METRICS_INDEX,
};

/// Resource id assigned by `build.rs` (`1 ICON "icon.ico"`).
const IDI_APP: usize = 1;

static BIG: AtomicIsize = AtomicIsize::new(0);
static SMALL: AtomicIsize = AtomicIsize::new(0);

/// Large window/taskbar icon (`SM_CXICON`, 32 px at 100%).
pub fn big() -> HICON {
    cached(&BIG, SM_CXICON)
}

/// Small icon: title bar and notification area (`SM_CXSMICON`, 16 px at 100%).
pub fn small() -> HICON {
    cached(&SMALL, SM_CXSMICON)
}

fn cached(slot: &AtomicIsize, metric: SYSTEM_METRICS_INDEX) -> HICON {
    let v = slot.load(Ordering::Relaxed);
    if v != 0 {
        return HICON(v as *mut _);
    }
    let h = unsafe { load(metric) };
    slot.store(h.0 as isize, Ordering::Relaxed);
    h
}

unsafe fn load(metric: SYSTEM_METRICS_INDEX) -> HICON {
    let px = GetSystemMetrics(metric);
    let inst = GetModuleHandleW(None).unwrap_or_default();
    // LR_SHARED: the handle is owned by the system and must not be destroyed;
    // it is also what RegisterClassExW expects for the class icons.
    match LoadImageW(
        inst,
        PCWSTR(IDI_APP as *const u16),
        IMAGE_ICON,
        px,
        px,
        LR_DEFAULTCOLOR | LR_SHARED,
    ) {
        Ok(h) if !h.is_invalid() => HICON(h.0),
        _ => {
            tracing::warn!("app icon resource unavailable, using the system icon");
            LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
        }
    }
}
