//! COM apartment setup. All shell COM objects live on the main STA thread.

pub mod explorer;

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

pub fn init_com() -> Result<(), windows::core::Error> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }
}
