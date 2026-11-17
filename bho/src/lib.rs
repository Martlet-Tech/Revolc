//! revolc_bho: Explorer Browser Helper Object — the Clover mechanism
//! (clover/analysis/Clover_Absorb_Mechanism.md).
//!
//! Explorer loads registered BHOs into every folder window and calls
//! SetSite(IWebBrowser2). We hand the window's top-level HWND to the Revolc
//! main window via a registered window message. No hooks, no polling:
//! Explorer reports windows to us by itself.
//!
//! Per-user registration (HKCU) so takeover never needs admin rights.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, WPARAM};
use windows::core::{implement, w, GUID, IUnknown, Interface, HRESULT, HSTRING, PCWSTR, Ref};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Ole::{IObjectWithSite, IObjectWithSite_Impl};
use windows::Win32::UI::Shell::IWebBrowser2;
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetAncestor, GetClassNameW, PostMessageW, RegisterWindowMessageW, GA_ROOT,
};

/// Our COM object CLSID (also the BHO key leaf).
const CLSID: GUID = GUID::from_u128(0x9f1c4e8b_6a2d_4e7b_8c13_5d04f7a2b916);

static DLL_INSTANCE: AtomicIsize = AtomicIsize::new(0);

// ---------------------------------------------------------------- DllMain

#[no_mangle]
extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut core::ffi::c_void) -> BOOL {
    if reason == 1 {
        DLL_INSTANCE.store(hinst.0 as isize, Ordering::SeqCst);
    }
    BOOL(1)
}

fn dll_path() -> Option<String> {
    let hinst = HINSTANCE(DLL_INSTANCE.load(Ordering::SeqCst) as *mut _);
    if hinst.0.is_null() {
        return None;
    }
    unsafe {
        let mut buf = [0u16; 512];
        let n = GetModuleFileNameW(hinst, &mut buf);
        if n == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    }
}

// ---------------------------------------------------------------- BHO object

#[implement(IObjectWithSite)]
struct TabHelper;

impl IObjectWithSite_Impl for TabHelper_Impl {
    // Explorer calls this once per folder window with the window's
    // IWebBrowser2. Forward the top-level HWND to Revolc.
    fn SetSite(&self, psite: Option<&IUnknown>) -> windows::core::Result<()> {
        // NEVER touch the site object here. QI(IWebBrowser2) on the site
        // during window creation corrupts explorerframe state: Explorer
        // later faults in a COM identity-compare helper on a dangling
        // pointer (crash loop explorerframe.dll+0x4d29a, 2026-11-17, 79
        // crashes). BHO reports "a shell window was created"; Revolc
        // discovers the new window itself via IShellWindows (out-of-proc).
        if psite.is_some() {
            bho_log("SetSite (notify only)");
            notify_revolc();
        }
        Ok(())
    }

    fn GetSite(
        &self,
        _riid: *const GUID,
        _ppvsite: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        // MUST return a real failure. Error::empty() carries S_OK(0): Explorer
        // would read its uninitialized ppvsite and later fault on the dangling
        // pointer (explorerframe.dll+0x4d29a crash loop, 2026-11-17).
        Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_FAIL,
        ))
    }
}

/// Post the Explorer top-level HWND to the Revolc main window.
/// Revolc not running → no-op (pure BHO, zero side effects).
/// Debug log so we can tell "DLL not loaded" from "notify failed".
fn bho_log(msg: &str) {
    let Some(dir) = std::env::var_os("APPDATA") else { return };
    let dir = std::path::Path::new(&dir).join("revolc");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
        .open(dir.join("bho.log")) else { return };
    use std::io::Write as _;
    let _ = writeln!(f, "[{}] {}", std::process::id(), msg);
}

fn notify_revolc() {
    unsafe {
        static MSG_ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let msg = *MSG_ID.get_or_init(|| RegisterWindowMessageW(w!("RevolcAbsorbExplorer")));

        let Ok(target) = FindWindowW(w!("RevolcMain"), None) else {
            return; // Revolc not running
        };
        if target.0.is_null() {
            return;
        }
        // wParam unused: Revolc finds the new window via IShellWindows.
        let _ = PostMessageW(target, msg, WPARAM(0), LPARAM(0));
    }
}

// ---------------------------------------------------------------- Class factory

#[implement(IClassFactory)]
struct BhoFactory;

impl IClassFactory_Impl for BhoFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if punkouter.is_some() {
            return Err(windows::core::Error::from_hresult(HRESULT(0x80040110u32 as i32))); // CLASS_E_NOAGGREGATION
        }
        let object: IUnknown = TabHelper.into();
        unsafe { object.query(riid, ppvobject) }.ok()
    }

    fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------- Exports

const CLASS_E_CLASSNOTAVAILABLE: HRESULT = HRESULT(0x80040111u32 as i32);

#[no_mangle]
extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    unsafe {
        if rclsid.as_ref() != Some(&CLSID) {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = BhoFactory.into();
        unsafe { factory.query(riid, ppv) }.ok().into()
    }
}

// ---------------------------------------------------------------- Registry

fn clsid_path() -> HSTRING {
    HSTRING::from(format!("Software\\Classes\\CLSID\\{{{CLSID:?}}}\\InprocServer32"))
}

fn bho_path() -> HSTRING {
    HSTRING::from(format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Browser Helper Objects\\{{{CLSID:?}}}"
    ))
}

fn reg_key(path: &HSTRING) -> Result<HKEY, HRESULT> {
    reg_key_at(HKEY_CURRENT_USER, path)
}

fn reg_key_at(root: HKEY, path: &HSTRING) -> Result<HKEY, HRESULT> {
    unsafe {
        let mut hk = HKEY::default();
        let r = RegCreateKeyExW(
            root,
            path,
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hk,
            None,
        );
        if r.is_ok() { Ok(hk) } else { Err(HRESULT(r.0 as i32)) }
    }
}

fn set_value(hk: &HKEY, name: Option<&HSTRING>, data: &HSTRING) {
    unsafe {
        let name_ptr = name.map(|n| PCWSTR::from_raw(n.as_ptr())).unwrap_or(PCWSTR::null());
        let mut bytes = data.as_wide().iter().copied().collect::<Vec<u16>>();
        bytes.push(0);
        let r = RegSetValueExW(
            *hk,
            name_ptr,
            0,
            REG_SZ,
            Some(std::slice::from_raw_parts(
                bytes.as_ptr() as *const u8,
                bytes.len() * 2,
            )),
        );
        if r.is_err() { tracing_free(); }
    }
}

// registry helpers don't log; placeholder to keep fn sizes stable
fn tracing_free() {}

fn register() -> Result<(), HRESULT> {
    let Some(path) = dll_path() else { return Err(HRESULT(1u32 as i32)) };
    let path_h = HSTRING::from(path);
    let apartment = HSTRING::from("Apartment");
    let threading = HSTRING::from("ThreadingModel");
    let friendly = HSTRING::from("Revolc Tab Explorer Helper");

    // 1) CLSID\{clsid}\InprocServer32. The BHO loader resolves the CLSID
    //    from HKLM\Software\Classes; HKCU-only registration is invisible to
    //    it. HKLM preferred (needs elevation), HKCU fallback for testing.
    let hk = match reg_key_at(HKEY_LOCAL_MACHINE, &clsid_path()) {
        Ok(hk) => { bho_log("CLSID written to HKLM"); hk }
        Err(_) => {
            bho_log("CLSID HKLM denied, using HKCU");
            reg_key(&clsid_path())?
        }
    };
    set_value(&hk, None, &path_h);
    set_value(&hk, Some(&threading), &apartment);
    unsafe { let _ = RegCloseKey(hk); }

    // 2) Explorer BHO list. Explorer reads the machine BHO list; HKCU-only
    //    registration is ignored for folder windows (Clover's RGS used HKLM).
    //    Try HKLM first (needs elevation); fall back to HKCU for IE/testing.
    let hklm = reg_key_at(HKEY_LOCAL_MACHINE, &bho_path());
    let hk = match hklm {
        Ok(hk) => { bho_log("BHO key written to HKLM"); hk }
        Err(_) => {
            bho_log("HKLM denied, BHO key in HKCU only (Explorer will NOT load us)");
            reg_key(&bho_path())?
        }
    };
    set_value(&hk, None, &friendly);
    unsafe { let _ = RegCloseKey(hk); }
    Ok(())
}

fn unregister() -> Result<(), HRESULT> {
    unsafe {
        // HKCU (per-user registration)
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR::from_raw(bho_path().as_ptr()));
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR::from_raw(clsid_path().as_ptr()));
        // HKLM (machine registration; succeeds when running elevated)
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR::from_raw(bho_path().as_ptr()));
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR::from_raw(clsid_path().as_ptr()));
    }
    Ok(())
}

// ---------------------------------------------------------------- Exports (registry)

#[no_mangle]
extern "system" fn DllRegisterServer() -> HRESULT {
    match register() {
        Ok(()) => HRESULT(0),
        Err(e) => e,
    }
}

#[no_mangle]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match unregister() {
        Ok(()) => HRESULT(0),
        Err(e) => e,
    }
}

#[no_mangle]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    HRESULT(1u32 as i32) // S_FALSE: never unload while Explorer holds us
}
