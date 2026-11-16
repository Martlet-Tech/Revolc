//! IExplorerBrowser wrapper: one embedded Explorer view per tab (spec: real Explorer views).

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    self as shell, EBO_NONE, FOLDERSETTINGS, FVM_DETAILS, IExplorerBrowser, IShellItem,
    SBSP_ABSOLUTE, SHCreateItemFromParsingName,
};

/// Normalize path separators to backslash for shell APIs.
pub fn normalize_path(path: &str) -> String {
    path.replace('/', "\\")
}

/// Owns one IExplorerBrowser instance hosted in a child HWND.
///
/// STA-ONLY by design: the object must be created and used on the main STA
/// thread. Deliberately NOT `unsafe impl Send` — moving this type across
/// threads would be silent UB, so the type system must refuse (design review).
pub struct ExplorerView {
    browser: IExplorerBrowser,
}

impl ExplorerView {
    /// Create the browser inside `parent` filling `rect`, then navigate to `path`.
    pub fn new(parent: HWND, rect: &RECT, path: &str) -> Result<Self> {
        unsafe {
            let browser: IExplorerBrowser =
                CoCreateInstance(&shell::ExplorerBrowser, None, CLSCTX_INPROC_SERVER)?;
            tracing::debug!(?rect, "browser instance created");

            browser.Initialize(parent, rect, None).map_err(|e| {
                tracing::error!(error = %e, "Initialize failed");
                e
            })?;
            tracing::debug!("Initialize ok");

            browser.SetOptions(EBO_NONE).map_err(|e| {
                tracing::error!(error = %e, "SetOptions failed");
                e
            })?;

            // Details view by default (fast, no thumbnails).
            // NOTE: SetFolderSettings can return E_INVALIDARG on some systems
            // when called before navigation; treat as non-fatal (view mode is
            // cosmetic) and retry after navigation lands.
            let pfs = FOLDERSETTINGS {
                ViewMode: FVM_DETAILS.0 as u32,
                fFlags: 0,
            };
            let mut retry_viewmode = false;
            if let Err(e) = browser.SetFolderSettings(&pfs) {
                tracing::warn!(error = %e, "SetFolderSettings failed pre-nav (non-fatal)");
                retry_viewmode = true;
            }

            let view = Self { browser };
            view.navigate(path)?;

            if retry_viewmode {
                if let Err(e) = view.browser.SetFolderSettings(&pfs) {
                    tracing::warn!(error = %e, "SetFolderSettings failed post-nav (non-fatal)");
                }
            }
            tracing::debug!(path, "navigated");
            Ok(view)
        }
    }

    pub fn navigate(&self, path: &str) -> Result<()> {
        // Shell item parser rejects forward-slash paths (E_INVALIDARG).
        let path = normalize_path(path);
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(
                &windows::core::HSTRING::from(&path),
                None,
            )
            .map_err(|e| {
                tracing::error!(error = %e, path, "SHCreateItemFromParsingName failed");
                e
            })?;
            self.browser
                .BrowseToObject(&item, SBSP_ABSOLUTE)
                .map_err(|e| {
                    tracing::error!(error = %e, path, "BrowseToObject failed");
                    e
                })
        }
    }

    pub fn set_rect(&self, rect: RECT) -> Result<()> {
        unsafe { self.browser.SetRect(None, rect) }
    }

    /// Host HWND created by the browser inside our parent (used to show/hide tabs).
    pub fn hwnd(&self) -> Result<HWND> {
        use windows::Win32::System::Ole::IOleWindow;
        unsafe { self.browser.cast::<IOleWindow>()?.GetWindow() }
    }
}

impl Drop for ExplorerView {
    fn drop(&mut self) {
        unsafe {
            let _ = self.browser.Destroy();
        }
    }
}
