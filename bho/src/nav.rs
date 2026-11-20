//! Event-driven navigation tracking (feat/tab-context-menu).
//!
//! Revolc cannot learn in-tab navigation from outside: once a folder window is
//! re-parented under our frame, IShellWindows no longer enumerates it, so a
//! polled `LocationURL` always reads the wrong window. Instead, the BHO (which
//! lives inside each Explorer folder window) subscribes to
//! `DWebBrowserEvents2::NavigateComplete2` and pushes the live URL to Revolc.
//!
//! SAFETY (2026-11-17 crash loop): SetSite must NEVER call methods on the site
//! (QI/HWND there corrupted explorerframe). We only `cast` the site to
//! IWebBrowser2 and STORE it; the connection-point subscription and every
//! method call happen later, on the Explorer UI thread's timer/event loop.
//!
//! Step 1 is verification-only: create `%APPDATA%\revolc\bho.navtrace` and the
//! sink logs each navigation to bho.log WITHOUT sending anything to Revolc.
//! Confirm Explorer does not crash and events arrive before enabling delivery.

use std::sync::Mutex;

use windows::core::{implement, GUID, IUnknown, Interface, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    IConnectionPoint, IConnectionPointContainer, IDispatch, IDispatch_Impl, ITypeInfo,
    DISPATCH_FLAGS, DISPPARAMS, EXCEPINFO,
};
use windows::Win32::UI::Shell::IWebBrowser2;
use windows::Win32::UI::WindowsAndMessaging::SetTimer;

/// COM interfaces are !Send, but all our uses are confined to Explorer's UI
/// thread. The unsafe impl only lets them live in a process-wide static.
struct SendCom<T>(T);
unsafe impl<T> Send for SendCom<T> {}
unsafe impl<T> Sync for SendCom<T> {}

/// DISPID of DWebBrowserEvents2::NavigateComplete2.
const DISPID_NAVIGATECOMPLETE2: i32 = 250;
/// IID of DWebBrowserEvents2 (outgoing dispinterface).
const DIID_DWEBBROWSEREVENTS2: GUID =
    GUID::from_u128(0x34a715a0_6587_11d0_924a_0020afc7ac4d);

static NAV_TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn trace() -> bool {
    *NAV_TRACE.get_or_init(|| {
        std::env::var_os("APPDATA")
            .map(|d| {
                std::path::Path::new(&d)
                    .join("revolc")
                    .join("bho.navtrace")
                    .exists()
            })
            .unwrap_or(false)
    })
}

fn log(msg: &str) {
    crate::bho_log(&format!("[nav] {msg}"));
}

/// One subscription per Explorer folder window. Stored for the DLL's lifetime
/// so the sink and connection point stay alive (dropping them unsubscribes).
static SUBS: Mutex<Vec<SendCom<NavigationSink>>> = Mutex::new(Vec::new());

/// Sites queued by SetSite (which must not call COM methods). The timer
/// callback drains this and subscribes on the Explorer UI thread.
static PENDING: Mutex<Vec<SendCom<IUnknown>>> = Mutex::new(Vec::new());

/// Called from SetSite. Only clones the site pointer into a queue — no method
/// calls on it here (2026-11-17 crash rule). The actual subscribe runs later
/// from `pump()`, on a timer, far away from window creation.
pub fn queue_site(site: &IUnknown) {
    if crate::revolc_disabled() {
        return;
    }
    if let Ok(mut q) = PENDING.lock() {
        q.push(SendCom(site.clone()));
    }
    arm_timer();
}

static TIMER_ARMED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
fn arm_timer() {
    let _ = TIMER_ARMED.get_or_init(|| unsafe {
        let _ = SetTimer(HWND::default(), 0, 300, Some(timer_proc));
    });
}

unsafe extern "system" fn timer_proc(_hwnd: HWND, _msg: u32, _id: usize, _tick: u32) {
    pump();
}

/// Drains queued sites and subscribes each. Safe context: Explorer UI thread,
/// long after SetSite returned. This is where all COM method calls happen.
pub fn pump() {
    let sites: Vec<IUnknown> = {
        let Ok(mut q) = PENDING.lock() else { return };
        q.drain(..).map(|s| s.0).collect()
    };
    for site in sites {
        subscribe(site);
    }
}

#[implement(IDispatch)]
struct NavEventSink {
    browser: IWebBrowser2,
}

impl IDispatch_Impl for NavEventSink_Impl {
    fn GetTypeInfoCount(&self) -> Result<u32> {
        Ok(0)
    }
    fn GetTypeInfo(&self, _itinfo: u32, _lcid: u32) -> Result<ITypeInfo> {
        Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }
    fn GetIDsOfNames(
        &self,
        _riid: *const GUID,
        _rgsznames: *const windows::core::PCWSTR,
        _cnames: u32,
        _lcid: u32,
        _rgdispid: *mut i32,
    ) -> Result<()> {
        Err(windows::core::Error::from_hresult(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }
    fn Invoke(
        &self,
        dispidmember: i32,
        _riid: *const GUID,
        _lcid: u32,
        _wflags: DISPATCH_FLAGS,
        _pdispparams: *const DISPPARAMS,
        _pvarresult: *mut windows::core::VARIANT,
        _pexcepinfo: *mut EXCEPINFO,
        _puargerr: *mut u32,
    ) -> Result<()> {
        if dispidmember != DISPID_NAVIGATECOMPLETE2 {
            return Ok(());
        }
        // Event runs on Explorer's UI thread. Reading LocationURL here is
        // safe (we are far past SetSite). No site QI / HWND here.
        let url = unsafe { self.browser.LocationURL() }
            .map(|u| u.to_string())
            .unwrap_or_default();
        if trace() {
            log(&format!("NavigateComplete2 url={url:?}"));
        }
        Ok(())
    }
}

/// A live subscription, kept alive so the connection stays open.
struct NavigationSink {
    _cp: IConnectionPoint,
    _cookie: u32,
    _sink: IDispatch,
    _browser: IWebBrowser2,
}

/// Called from the delayed timer (NOT from SetSite). Subscribes this window's
/// browser to DWebBrowserEvents2. `site` is the IWebBrowser2 stored at SetSite.
pub fn subscribe(site: IUnknown) {
    if crate::revolc_disabled() {
        return;
    }
    let browser: IWebBrowser2 = match site.cast() {
        Ok(b) => b,
        Err(e) => {
            if trace() {
                log(&format!("cast to IWebBrowser2 failed: {e:?}"));
            }
            return;
        }
    };
    unsafe {
        let cpc: IConnectionPointContainer = match browser.cast() {
            Ok(c) => c,
            Err(e) => {
                if trace() {
                    log(&format!("cast to IConnectionPointContainer failed: {e:?}"));
                }
                return;
            }
        };
        let cp: IConnectionPoint = match cpc.FindConnectionPoint(&DIID_DWEBBROWSEREVENTS2) {
            Ok(c) => c,
            Err(e) => {
                if trace() {
                    log(&format!("FindConnectionPoint failed: {e:?}"));
                }
                return;
            }
        };
        let sink: IDispatch = match (NavEventSink { browser: browser.clone() }).try_into() {
            Ok(s) => s,
            Err(e) => {
                if trace() {
                    log(&format!("create sink failed: {e:?}"));
                }
                return;
            }
        };
        let cookie = match cp.Advise(&sink) {
            Ok(c) => c,
            Err(e) => {
                if trace() {
                    log(&format!("Advise failed: {e:?}"));
                }
                return;
            }
        };
        if trace() {
            log(&format!("subscribed, cookie={cookie}"));
        }
        // Keep everything alive for the DLL's lifetime.
        if let Ok(mut subs) = SUBS.lock() {
            subs.push(SendCom(NavigationSink {
                _cp: cp,
                _cookie: cookie,
                _sink: sink,
                _browser: browser,
            }));
        }
    }
}
