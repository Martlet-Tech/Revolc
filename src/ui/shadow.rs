//! Self-drawn drop shadow for the frameless main window.
//!
//! DWM's automatic shadow is unavailable: the window is WS_POPUP with no
//! caption, so DWM treats it as a borderless tool window and projects nothing.
//! This module draws its own shadow into a layered, click-through popup owned
//! by the main window, so it follows the window's Z-order, minimize and close
//! automatically.
//!
//! The popup is created *before* the tab bar / tooltip popups, so among the
//! owner's siblings it sits lowest — just above the main window. Its centre
//! is fully transparent (only the area outside the window is painted), so it
//! never covers the window's own content or the chrome popups above it.
//!
//! The shadow is a 32bpp DIB whose alpha is a smooth falloff from the edge of
//! the window's rounded rectangle. Computing it on the CPU keeps it independent
//! of DWM quirks and theme colours.

use std::ptr;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BI_RGB,
    DIB_RGB_COLORS, HBRUSH, HBITMAP, HDC, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, AC_SRC_ALPHA,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, ShowWindow,
    UpdateLayeredWindow, CS_HREDRAW, CS_VREDRAW, SW_HIDE, SW_SHOWNA, ULW_ALPHA, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Pixels of shadow visible outside the window on each side.
const PAD: i32 = 10;
/// Peak alpha at the window edge. Lower = fainter.
const MAX_ALPHA: f32 = 60.0;

const CLASS_NAME: windows::core::PCWSTR = w!("RevolcShadow");

/// Owns the layered shadow popup and its DIB.
pub struct ShadowWindow {
    hwnd: HWND,
    hdc_mem: HDC,
    hbm: HBITMAP,
    /// Bits of the DIB (BGRA, bottom-up), owned by `hbm`.
    bits: *mut u8,
    /// Size of the current DIB in pixels.
    bm_w: i32,
    bm_h: i32,
    /// Whether the popup is currently visible.
    visible: bool,
}

// All access stays on the UI thread; the raw DIB pointer is owned by hbm.
unsafe impl Send for ShadowWindow {}

impl ShadowWindow {
    fn ensure_class() {
        let hinstance = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(shadow_wndproc),
            hInstance: hinstance.into(),
            hbrBackground: HBRUSH(ptr::null_mut()),
            lpszClassName: CLASS_NAME,
            style: CS_HREDRAW | CS_VREDRAW,
            ..Default::default()
        };
        if unsafe { RegisterClassExW(&wc) } != 0 {
            return;
        }
        let err = windows::core::Error::from_win32();
        if (err.code().0 as u32 & 0xffff) != 1410 {
            tracing::warn!(error = %err, "register shadow class failed");
        }
    }

    /// Create the layered popup owned by `owner`.
    pub fn create(owner: HWND) -> Option<Self> {
        Self::ensure_class();
        let hinstance = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                CLASS_NAME,
                None,
                WS_POPUP,
                0,
                0,
                1,
                1,
                owner,
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                hinstance,
                None,
            )
        }
        .ok()?;

        Some(Self {
            hwnd,
            hdc_mem: unsafe { CreateCompatibleDC(HDC(ptr::null_mut())) },
            hbm: HBITMAP(ptr::null_mut()),
            bits: ptr::null_mut(),
            bm_w: 0,
            bm_h: 0,
            visible: false,
        })
    }

    /// Rebuild the DIB if the size changed, then draw and position the popup.
    /// `win` is the main window's *window* rect (screen coords).
    pub fn update(&mut self, win: RECT, show: bool) {
        if !show {
            if self.visible {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.visible = false;
            }
            return;
        }

        let w = win.right - win.left;
        let h = win.bottom - win.top;
        if w <= 0 || h <= 0 {
            return;
        }
        let bw = w + 2 * PAD;
        let bh = h + 2 * PAD;
        if bw != self.bm_w || bh != self.bm_h {
            self.rebuild_dib(bw, bh);
        }
        if self.bits.is_null() {
            return;
        }

        unsafe {
            paint_shadow(self.bits, bw, bh, PAD);
            let blend = BLENDFUNCTION {
                BlendOp: 0,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let size = SIZE { cx: bw, cy: bh };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT {
                x: win.left - PAD,
                y: win.top - PAD,
            };
            let _ = UpdateLayeredWindow(
                self.hwnd,
                HDC(ptr::null_mut()),
                Some(&dst),
                Some(&size),
                self.hdc_mem,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );

            if !self.visible {
                // SW_SHOWNA: show without activating or stealing Z-order.
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                self.visible = true;
            }
        }
    }

    fn rebuild_dib(&mut self, w: i32, h: i32) {
        unsafe {
            if !self.hbm.0.is_null() {
                let _ = DeleteObject(std::mem::replace(&mut self.hbm, HBITMAP(ptr::null_mut())));
            }
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: h, // positive = bottom-up DIB
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = ptr::null_mut();
            if let Ok(hbm) = CreateDIBSection(
                self.hdc_mem,
                &bi,
                DIB_RGB_COLORS,
                &mut bits,
                HANDLE(ptr::null_mut()),
                0,
            ) {
                let _ = SelectObject(self.hdc_mem, hbm);
                self.hbm = hbm;
                self.bits = bits as *mut u8;
                self.bm_w = w;
                self.bm_h = h;
            }
        }
    }

    pub fn destroy(&mut self) {
        unsafe {
            if !self.hbm.0.is_null() {
                let _ = DeleteObject(std::mem::replace(&mut self.hbm, HBITMAP(ptr::null_mut())));
            }
            if !self.hdc_mem.0.is_null() {
                let _ = DeleteDC(std::mem::replace(&mut self.hdc_mem, HDC(ptr::null_mut())));
            }
            let _ = DestroyWindow(self.hwnd);
        }
        self.bits = ptr::null_mut();
        self.visible = false;
    }
}

unsafe extern "system" fn shadow_wndproc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Fill the BGRA (bottom-up) DIB with a soft shadow around the window bounds.
unsafe fn paint_shadow(bits: *mut u8, w: i32, h: i32, pad: i32) {
    let right = w - pad;
    let bottom = h - pad;
    let fade = pad as f32;

    for y in 0..h {
        // Row 0 of a bottom-up DIB is the bottom of the image.
        let row = bits.add((y as usize) * (w as usize) * 4);
        let img_y = (h - 1 - y) as f32;
        for x in 0..w {
            // Distance from the window rect [pad,right) x [pad,bottom).
            let dx = if x < pad {
                (pad - x) as f32
            } else if x >= right {
                (x - right + 1) as f32
            } else {
                0.0
            };
            let dy = if img_y < pad as f32 {
                pad as f32 - img_y
            } else if img_y >= bottom as f32 {
                img_y - bottom as f32 + 1.0
            } else {
                0.0
            };
            // Outside distance; inside (both zero) gets no shadow.
            let dist = if dx > 0.0 && dy > 0.0 {
                (dx * dx + dy * dy).sqrt()
            } else {
                dx.max(dy)
            };
            let alpha = if dist <= 0.0 {
                0.0
            } else {
                let t = (dist / fade).clamp(0.0, 1.0);
                let eased = 1.0 - t;
                MAX_ALPHA * eased * eased
            };
            let px = row.add((x as usize) * 4);
            *px = 0; // B
            *px.add(1) = 0; // G
            *px.add(2) = 0; // R
            *px.add(3) = alpha.round() as u8;
        }
    }
}
