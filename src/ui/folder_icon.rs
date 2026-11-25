//! Shell folder icons for tabs: same HICON Explorer uses, cached by path.
//!
//! `SHGetFileInfoW` is synchronous and can block on a network share, so a tab
//! pays for it once per distinct path — never on paint. Handles are owned by
//! us (`SHGFI_ICON`, not `SHGFI_SYSICONINDEX`) and freed when their path leaves
//! the strip or the bar is dropped.

use std::collections::HashMap;

use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ADDOVERLAYS, SHGFI_ICON, SHGFI_SMALLICON,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

/// Path -> icon. `None` is a cached miss: a virtual/empty path must not be
/// retried on every tab update.
#[derive(Default)]
pub struct FolderIcons {
    icons: HashMap<String, Option<HICON>>,
}

impl FolderIcons {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconcile against the current tab paths. Unchanged paths keep their
    /// handle; paths that left the strip release theirs immediately.
    pub fn sync(&mut self, paths: &[String]) {
        let mut next = HashMap::with_capacity(paths.len());
        for path in paths {
            if let Some(cached) = self.icons.remove(path) {
                next.insert(path.clone(), cached);
            } else {
                next.insert(path.clone(), load(path));
            }
        }
        // Whatever is left belonged to a tab that is gone.
        self.release_all();
        self.icons = next;
    }

    /// `None` means: use the app icon (which is LR_SHARED and must not be
    /// destroyed here).
    pub fn get(&self, path: &str) -> Option<HICON> {
        self.icons.get(path).copied().flatten()
    }

    fn release_all(&mut self) {
        for (_, icon) in self.icons.drain() {
            if let Some(h) = icon {
                unsafe {
                    let _ = DestroyIcon(h);
                }
            }
        }
    }
}

impl Drop for FolderIcons {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// Ask the shell for the same small icon the taskbar shows. `::{CLSID}`
/// namespace paths ("此电脑") go through the very same call.
fn load(path: &str) -> Option<HICON> {
    if path.is_empty() {
        return None;
    }
    let wide = HSTRING::from(path);
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            &wide,
            FILE_ATTRIBUTE_DIRECTORY,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON | SHGFI_ADDOVERLAYS,
        )
    };
    if ok != 0 && !info.hIcon.is_invalid() {
        Some(info.hIcon)
    } else {
        None
    }
}
