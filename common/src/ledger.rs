//! Cross-process foreground-window ledger (docs/双进程架构设计.md Phase 2).
//!
//! With one `revolc.exe` per window there is no process-wide `LAST_ACTIVE`
//! any more. A named file mapping holds the single fact everyone needs:
//! "which RevolcMain did the user touch last". `revolc-tray.exe` owns the
//! write side (`SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`); window processes
//! only read it when deciding whether to adopt a stray folder window.
//!
//! Layout is fixed and 8-byte aligned (the mapping base is page-aligned), so
//! the two `u64`s can be read/written with `read_volatile` plus acquire/release
//! fences. A seqlock-style `generation` (even = stable, odd = write in
//! progress) lets readers retry instead of observing a half-written HWND.
//!
//! When no tray is running the mapping still exists but `last_active == 0`;
//! callers then fall back to a deterministic rule (smallest live HWND), so
//! "no tray installed" never breaks takeover.

use std::mem;
use std::ptr;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE, STILL_ACTIVE,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS,
    PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Session-local: one ledger per logon session, gone at logout.
pub const LEDGER_NAME: PCWSTR = w!("Local\\RevolcWindowLedger");

const MAGIC: u32 = 0x5245_4C44; // "RELD"
const VERSION: u32 = 1;
const ERROR_ALREADY_EXISTS: u32 = 183;

#[repr(C)]
struct LedgerLayout {
    magic: u32,
    version: u32,
    /// Even = stable, odd = writer in progress.
    generation: u64,
    /// Last activated RevolcMain, 0 = none.
    last_active: u64,
    /// PID of the tray that owns the write side, 0 = none.
    tray_pid: u32,
    _pad: u32,
}

/// Owned view of the shared ledger. Both executables open the same mapping.
pub struct Ledger {
    handle: HANDLE,
    mapped: MEMORY_MAPPED_VIEW_ADDRESS,
    view: *mut LedgerLayout,
}

// The mapping is page-aligned and all accesses are volatile + fenced; the
// pointer is never shared with another Rust thread simultaneously.
unsafe impl Send for Ledger {}
// All field access is volatile + acquire/release fenced; callers serialise
// writers through a Mutex where needed (the foreground hook).
unsafe impl Sync for Ledger {}

/// Immutable snapshot of the ledger fields.
#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    pub last_active: HWND,
    pub tray_pid: u32,
    pub generation: u64,
}

impl Ledger {
    /// Open the existing mapping or create it. Safe to call from every
    /// process; the first one initialises the fields.
    pub fn open_or_create() -> windows::core::Result<Self> {
        unsafe {
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                mem::size_of::<LedgerLayout>() as u32,
                LEDGER_NAME,
            )?;
            let created = GetLastError().0 != ERROR_ALREADY_EXISTS;
            let mapped = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            let view = mapped.Value as *mut LedgerLayout;
            if view.is_null() {
                let _ = CloseHandle(handle);
                return Err(windows::core::Error::from_win32());
            }
            let ledger = Self { handle, mapped, view };
            if created || ptr::read_volatile(&(*view).magic) != MAGIC {
                ledger.initialise();
            }
            Ok(ledger)
        }
    }

    /// Consistent read of every field. Retries while the writer is mid-update.
    pub fn snapshot(&self) -> Snapshot {
        unsafe {
            loop {
                let g1 = ptr::read_volatile(&(*self.view).generation);
                if g1 & 1 != 0 {
                    std::hint::spin_loop();
                    continue;
                }
                std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
                let last = ptr::read_volatile(&(*self.view).last_active);
                let pid = ptr::read_volatile(&(*self.view).tray_pid);
                std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
                let g2 = ptr::read_volatile(&(*self.view).generation);
                if g1 == g2 {
                    return Snapshot {
                        last_active: HWND(last as *mut _),
                        tray_pid: pid,
                        generation: g1,
                    };
                }
            }
        }
    }

    pub fn last_active(&self) -> Option<HWND> {
        let h = self.snapshot().last_active;
        (!h.0.is_null()).then_some(h)
    }

    /// Tray write side: claim the foreground window and stamp our PID.
    pub fn set_active(&self, hwnd: HWND, tray_pid: u32) {
        unsafe {
            self.begin_write();
            (*self.view).last_active = hwnd.0 as u64;
            (*self.view).tray_pid = tray_pid;
            self.end_write();
        }
    }

    /// Window process write side: record the activated window without
    /// changing the tray PID (the tray remains the nominal owner).
    pub fn note_active(&self, hwnd: HWND) {
        unsafe {
            self.begin_write();
            (*self.view).last_active = hwnd.0 as u64;
            self.end_write();
        }
    }

    /// Clear the ledger if it still points at `hwnd` (window closed). Returns
    /// true when the entry was actually cleared.
    pub fn clear_if(&self, hwnd: HWND) -> bool {
        unsafe {
            let g = ptr::read_volatile(&(*self.view).generation);
            if g & 1 != 0 || ptr::read_volatile(&(*self.view).last_active) != hwnd.0 as u64 {
                return false;
            }
            self.begin_write();
            let matched = ptr::read_volatile(&(*self.view).last_active) == hwnd.0 as u64;
            if matched {
                (*self.view).last_active = 0;
            }
            self.end_write();
            matched
        }
    }

    /// True when the recorded tray PID still refers to a running process.
    pub fn tray_alive(&self) -> bool {
        let pid = self.snapshot().tray_pid;
        if pid == 0 {
            return false;
        }
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut code = 0u32;
            let alive = GetExitCodeProcess(h, &mut code).is_ok()
                && code as i32 == STILL_ACTIVE.0;
            let _ = CloseHandle(h);
            alive
        }
    }

    unsafe fn initialise(&self) {
        self.begin_write();
        (*self.view).magic = MAGIC;
        (*self.view).version = VERSION;
        (*self.view).last_active = 0;
        (*self.view).tray_pid = 0;
        self.end_write();
    }

    unsafe fn begin_write(&self) {
        let g = ptr::read_volatile(&(*self.view).generation);
        // Odd marks "writer active"; release makes prior stable reads visible.
        ptr::write_volatile(&mut (*self.view).generation, g + 1);
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
    }

    unsafe fn end_write(&self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        let g = ptr::read_volatile(&(*self.view).generation);
        ptr::write_volatile(&mut (*self.view).generation, g + 1);
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        unsafe {
            if !self.view.is_null() {
                let _ = UnmapViewOfFile(self.mapped);
            }
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}
