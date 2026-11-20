//! Crash capture: a minidump plus a one-paragraph signature next to the log.
//!
//! Revolc has no business dying silently. Before this module the only trace of
//! a crash was Event Viewer's "faulting module uxtheme.dll+0xe9f6", which says
//! nothing about *our* code (2026-11-17: crashed right after "open containing
//! folder" from a browser download). Everything here is deliberately
//! allocation-light and independent of `tracing`: the crashing thread may
//! already hold the log writer, so the signature goes to its own file, written
//! with plain `std::fs`.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows::Win32::System::Diagnostics::Debug::{
    MiniDumpNormal, MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump,
    EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId};

/// Where dumps and signatures land: `%APPDATA%\revolc\crash`.
fn crash_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    let dir = std::path::PathBuf::from(base).join("revolc").join("crash");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `module.dll` + its load address, resolved from a faulting address. The
/// offset inside the module is what makes two crashes comparable; the module
/// base is needed to compute it.
fn module_of(addr: usize) -> (String, usize) {
    if addr == 0 {
        return ("<null>".into(), 0);
    }
    unsafe {
        let mut hmod = Default::default();
        let ok = GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr as *const u16),
            &mut hmod,
        );
        if ok.is_err() || hmod.is_invalid() {
            return ("<unknown>".into(), 0);
        }
        let mut buf = [0u16; 512];
        let n = GetModuleFileNameW(hmod, &mut buf);
        let full = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        let name = full.rsplit(['\\', '/']).next().unwrap_or(&full).to_string();
        (name, hmod.0 as usize)
    }
}

fn write_minidump(
    info: *const EXCEPTION_POINTERS,
    dir: &std::path::Path,
    id: u64,
) -> Option<PathBuf> {
    let path = dir.join(format!("revolc-{id}.dmp"));
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0])
        .collect();
    unsafe {
        let Ok(file) = CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) else {
            return None;
        };
        let mut md = MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: GetCurrentThreadId(),
            ExceptionPointers: info as *mut EXCEPTION_POINTERS,
            ClientPointers: false.into(),
        };
        let kind = MiniDumpNormal | MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules;
        let ok = MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file,
            kind,
            Some(&mut md),
            None,
            None,
        );
        let _ = CloseHandle(file);
        ok.is_ok().then_some(path)
    }
}

/// Walk the crashing thread's stack and return one `module+0xoffset function
/// (file:line)` line per frame. Best effort: dbghelp without a PDB still gives
/// module+offset, which is enough to compare a crash against the breadcrumbs
/// in the log; with the PDB next to the exe (`debug = 1` in the release
/// profile) it gives the actual Rust function and line.
fn stack_lines(info: *const EXCEPTION_POINTERS) -> Vec<String> {
    use std::ffi::CStr;

    use windows::Win32::System::Diagnostics::Debug::{
        SymFromAddr, SymFunctionTableAccess64, SymGetLineFromAddr64, SymGetModuleBase64,
        SymInitializeW, SymSetOptions, StackWalk64, IMAGEHLP_LINE64, STACKFRAME64, SYMBOL_INFO,
        SYMOPT_DEFERRED_LOADS, SYMOPT_LOAD_LINES, SYMOPT_UNDNAME,
    };
    use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;

    unsafe extern "system" fn fta(h: windows::Win32::Foundation::HANDLE, a: u64) -> *mut core::ffi::c_void {
        SymFunctionTableAccess64(h, a)
    }
    unsafe extern "system" fn gmb(h: windows::Win32::Foundation::HANDLE, a: u64) -> u64 {
        SymGetModuleBase64(h, a)
    }

    const MAX_FRAMES: usize = 24;
    let mut out = Vec::new();
    if info.is_null() {
        return out;
    }
    unsafe {
        if (*info).ContextRecord.is_null() {
            return out;
        }
        let process = GetCurrentProcess();
        let thread = windows::Win32::System::Threading::GetCurrentThread();
        let ctx = (*info).ContextRecord;
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
        // Search path = the exe's own directory, so `revolc.pdb` sitting next
        // to `revolc.exe` is found without _NT_SYMBOL_PATH. Without the PDB the
        // walk still returns module+offset, which is what matters.
        let mut exe = [0u16; 1024];
        let n = GetModuleFileNameW(None, &mut exe) as usize;
        let full = String::from_utf16_lossy(&exe[..n.max(0)]);
        let dir = full.rsplit_once(['\\', '/']).map(|(d, _)| d).unwrap_or("");
        let dir_w: Vec<u16> = dir.encode_utf16().chain([0]).collect();
        if SymInitializeW(process, PCWSTR(dir_w.as_ptr()), true).is_err() {
            return out;
        }

        let mut frame = STACKFRAME64::default();
        frame.AddrPC.Offset = (*ctx).Rip;
        frame.AddrPC.Mode = windows::Win32::System::Diagnostics::Debug::ADDRESS_MODE(3); // AddrModeFlat
        frame.AddrFrame.Offset = (*ctx).Rbp;
        frame.AddrFrame.Mode = windows::Win32::System::Diagnostics::Debug::ADDRESS_MODE(3);
        frame.AddrStack.Offset = (*ctx).Rsp;
        frame.AddrStack.Mode = windows::Win32::System::Diagnostics::Debug::ADDRESS_MODE(3);

        for _ in 0..MAX_FRAMES {
            let ok = StackWalk64(
                IMAGE_FILE_MACHINE_AMD64.0 as u32,
                process,
                thread,
                &mut frame,
                ctx as *mut _,
                None,
                Some(fta),
                Some(gmb),
                None,
            );
            if !ok.as_bool() || frame.AddrPC.Offset == 0 {
                break;
            }
            let addr = frame.AddrPC.Offset;
            let (module, base) = module_of(addr as usize);
            let offset = addr.saturating_sub(base as u64);

            // SYMBOL_INFO is variable length: one name buffer behind it.
            let mut buf = vec![0u64; (std::mem::size_of::<SYMBOL_INFO>() + 512 + 7) / 8];
            let sym = buf.as_mut_ptr() as *mut SYMBOL_INFO;
            (*sym).SizeOfStruct = std::mem::size_of::<SYMBOL_INFO>() as u32;
            (*sym).MaxNameLen = 480;
            let mut disp = 0u64;
            let name = if SymFromAddr(process, addr, Some(&mut disp), sym).is_ok() {
                let p = (*sym).Name.as_ptr() as *const i8;
                CStr::from_ptr(p).to_string_lossy().into_owned()
            } else {
                String::new()
            };
            let mut line = String::new();
            let mut li = IMAGEHLP_LINE64 {
                SizeOfStruct: std::mem::size_of::<IMAGEHLP_LINE64>() as u32,
                ..Default::default()
            };
            let mut ldisp = 0u32;
            if SymGetLineFromAddr64(process, addr, &mut ldisp, &mut li).is_ok() && !li.FileName.is_null() {
                let f = CStr::from_ptr(li.FileName.0 as *const i8).to_string_lossy();
                let short = f.rsplit(['\\', '/']).next().unwrap_or(&f).to_string();
                line = format!(" ({short}:{})", li.LineNumber);
            }
            out.push(format!(
                "{module}+0x{offset:x} {name}{line}"
            ));
        }
    }
    out
}

/// `SetUnhandledExceptionFilter` target. Runs on the faulting thread with the
/// process already half dead: do the minimum, never panic, never go through
/// `tracing` (its writer lock may be the reason we are here).
unsafe extern "system" fn on_unhandled(info: *const EXCEPTION_POINTERS) -> i32 {
    let (code, addr) = if info.is_null() || (*info).ExceptionRecord.is_null() {
        (0u32, 0usize)
    } else {
        let rec = (*info).ExceptionRecord;
        ((*rec).ExceptionCode.0 as u32, (*rec).ExceptionAddress as usize)
    };
    let (module, base) = module_of(addr);
    let offset = addr.saturating_sub(base);

    let mut text = String::new();
    text.push_str("# revolc crash\n");
    text.push_str(&format!("time_unix = {}\n", stamp()));
    text.push_str(&format!("pid       = {}\n", GetCurrentProcessId()));
    text.push_str(&format!("thread    = {}\n", GetCurrentThreadId()));
    text.push_str(&format!("exception = 0x{code:08x}\n"));
    text.push_str(&format!("address   = 0x{addr:x}\n"));
    text.push_str(&format!("module    = {module}\n"));
    text.push_str(&format!("offset    = 0x{offset:x}\n"));

    if let Some(dir) = crash_dir() {
        let id = stamp();
        if let Some(dump) = write_minidump(info, &dir, id) {
            text.push_str(&format!("minidump  = {}\n", dump.display()));
        }
        let path = dir.join(format!("crash-{id}.txt"));
        // Write the signature BEFORE walking the stack: dbghelp can hang on a
        // half-dead process (it loads a PDB, takes locks), and the signature is
        // the part we cannot afford to lose.
        let _ = std::fs::write(&path, &text);
        text.push_str("\n# stack (innermost first)\n");
        for (i, line) in stack_lines(info).iter().enumerate() {
            text.push_str(&format!("#{i:<2} {line}\n"));
        }
        let _ = std::fs::write(&path, text);
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// Fault on purpose, so the reporter can be exercised end to end:
/// `set REVOLC_CRASH_SELFTEST=1 && revolc.exe`. Never reached in normal runs,
/// but it is the only way to prove a dump is actually written (and that
/// `%APPDATA%\revolc\crash` is writable) without waiting for a real crash.
pub fn selftest() -> ! {
    unsafe { std::ptr::write_volatile(1 as *mut u8, 0) };
    unreachable!("crash selftest did not fault")
}

/// Install the filter and a panic hook. Called once, before any window exists.
pub fn install() {
    unsafe {
        use windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;
        SetUnhandledExceptionFilter(Some(on_unhandled));
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic inside a window procedure is unrecoverable for us (the
        // message loop cannot unwind through user32), but the message must
        // survive: it names the line that is actually wrong. `abort()` after
        // the hook bypasses SetUnhandledExceptionFilter entirely.
        if let Some(dir) = crash_dir() {
            let text = format!(
                "# revolc panic\ntime_unix = {}\npid = {}\nthread = {}\n{info}\n",
                stamp(),
                unsafe { GetCurrentProcessId() },
                unsafe { GetCurrentThreadId() },
            );
            let _ = std::fs::write(dir.join(format!("panic-{}.txt", stamp())), text);
        }
        previous(info);
    }));
}
