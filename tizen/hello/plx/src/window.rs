//! The native window, created the way flutter-tizen's embedder does it
//! (`flutter/shell/platform/tizen/tizen_window_ecore_wl2.cc`, `CreateWindow` / `SetWindowOptions` /
//! `Show`): `ecore_wl2_init`, display connect, `ecore_wl2_sync`, screen size, `ecore_wl2_window_new`,
//! an Ecore_Wl2 EGL window, a few window options, then `ecore_wl2_window_show`.
//!
//! Everything here runs on the host's MAIN thread (Ecore is not thread-safe).

use crate::code;
use crate::dl::Ptr;
use crate::log::plog;
use crate::sym::{Ecore, WlEgl};
use std::ffi::CString;
use std::ptr::{null, null_mut};

/// `ECORE_WL2_WINDOW_TYPE_TOPLEVEL` in EFL's `Ecore_Wl2_Window_Type` (NONE = 0, TOPLEVEL = 1).
/// From the upstream header, not from this device: unverified.
const WINDOW_TYPE_TOPLEVEL: i32 = 1;

pub struct Window {
    /// `Ecore_Wl2_Display*`.
    pub display: Ptr,
    /// `wl_display*`: what `eglGetDisplay` is given (the embedder's `GetRenderTargetDisplay`).
    pub wl_display: Ptr,
    /// `Ecore_Wl2_Window*`.
    pub window: Ptr,
    /// What to destroy: an `Ecore_Wl2_Egl_Window*`, or on the fallback path the `wl_egl_window*`.
    egl_handle: Ptr,
    /// `wl_egl_window*`: the `EGLNativeWindowType` for `eglCreateWindowSurface`.
    pub native_window: Ptr,
    via_ecore: bool,
    wlegl: Option<WlEgl>,
    pub width: i32,
    pub height: i32,
}

impl Window {
    /// Steps 1-6 plus the window options; the caller registers key handlers and then calls
    /// [`Window::show`], as the embedder's constructor does.
    pub fn create(ecore: &Ecore, wlegl: Option<WlEgl>) -> Result<Window, i32> {
        // SAFETY (whole function): the calls are the embedder's, with the embedder's arguments;
        // every returned handle is null-checked before use.
        let inits = unsafe { (ecore.wl2_init)() };
        plog!("ecore_wl2_init -> {inits}");
        if inits < 1 {
            return Err(code::ECORE_INIT);
        }

        let display = unsafe { (ecore.wl2_display_connect)(null()) };
        plog!("ecore_wl2_display_connect(NULL) -> {display:p}");
        if display.is_null() {
            return Err(code::DISPLAY);
        }
        let wl_display = unsafe { (ecore.wl2_display_get)(display) };
        plog!("ecore_wl2_display_get -> wl_display {wl_display:p}");
        if wl_display.is_null() {
            return Err(code::DISPLAY);
        }
        unsafe { (ecore.wl2_sync)() };

        let (mut w, mut h) = (0i32, 0i32);
        unsafe { (ecore.wl2_display_screen_size_get)(display, &mut w, &mut h) };
        plog!("screen size {w} x {h}");
        if w <= 0 || h <= 0 {
            return Err(code::SCREEN_SIZE);
        }

        let window = unsafe { (ecore.wl2_window_new)(display, null_mut(), 0, 0, w, h) };
        plog!("ecore_wl2_window_new(0,0,{w},{h}) -> {window:p}");
        if window.is_null() {
            return Err(code::WINDOW);
        }

        let (egl_handle, native_window, via_ecore) = Self::make_egl_window(ecore, &wlegl, window, w, h)?;
        let mut win = Window {
            display,
            wl_display,
            window,
            egl_handle,
            native_window,
            via_ecore,
            wlegl,
            width: w,
            height: h,
        };
        win.set_options(ecore);
        Ok(win)
    }

    /// The embedder's path first (`ecore_wl2_egl_window_create` + `ecore_wl2_egl_window_native_get`),
    /// then libwayland-egl's `wl_egl_window_create(wl_surface, w, h)` as a logged fallback.
    fn make_egl_window(
        ecore: &Ecore,
        wlegl: &Option<WlEgl>,
        window: Ptr,
        w: i32,
        h: i32,
    ) -> Result<(Ptr, Ptr, bool), i32> {
        if let (Some(create), Some(native_get)) =
            (ecore.wl2_egl_window_create, ecore.wl2_egl_window_native_get)
        {
            let handle = unsafe { create(window, w, h) };
            plog!("ecore_wl2_egl_window_create({w},{h}) -> {handle:p}");
            if !handle.is_null() {
                let native = unsafe { native_get(handle) };
                plog!("ecore_wl2_egl_window_native_get -> wl_egl_window {native:p}");
                if !native.is_null() {
                    return Ok((handle, native, true));
                }
            }
            plog!("ecore egl-window path failed; trying libwayland-egl fallback");
        } else {
            plog!("ecore_wl2_egl_window_create/native_get not both present; trying libwayland-egl fallback");
        }
        let (Some(wlegl), Some(surface_get)) = (wlegl, ecore.wl2_window_surface_get) else {
            plog!("no fallback available (libwayland-egl or ecore_wl2_window_surface_get missing)");
            return Err(code::EGL_WINDOW);
        };
        let surface = unsafe { surface_get(window) };
        plog!("ecore_wl2_window_surface_get -> wl_surface {surface:p}");
        if surface.is_null() {
            return Err(code::EGL_WINDOW);
        }
        let native = unsafe { (wlegl.window_create)(surface, w, h) };
        plog!("wl_egl_window_create({w},{h}) -> {native:p}");
        if native.is_null() {
            return Err(code::EGL_WINDOW);
        }
        Ok((native, native, false))
    }

    /// `SetWindowOptions()` from the embedder, minus the phone-only indicator calls. All optional:
    /// a missing symbol is logged by `sym` already, and the window still works without it.
    fn set_options(&mut self, ecore: &Ecore) {
        // SAFETY: valid window handle, embedder's arguments.
        unsafe {
            if let Some(f) = ecore.wl2_window_type_set {
                f(self.window, WINDOW_TYPE_TOPLEVEL);
                plog!("window type set TOPLEVEL({WINDOW_TYPE_TOPLEVEL})");
            }
            if let Some(f) = ecore.wl2_window_position_set {
                f(self.window, 0, 0);
            }
            if let Some(f) = ecore.wl2_window_aux_hint_add {
                let hint = CString::new("wm.policy.win.user.geometry").unwrap_or_default();
                let val = CString::new("1").unwrap_or_default();
                f(self.window, 0, hint.as_ptr(), val.as_ptr());
                plog!("window aux hint wm.policy.win.user.geometry=1");
            }
            if let Some(f) = ecore.wl2_window_alpha_set {
                // The embedder's default (transparent=false). The EGL config still asks for an
                // alpha channel (render.rs): nuvio's "alpha-capable surface" gotcha.
                f(self.window, 0);
                plog!("window alpha set FALSE (opaque)");
            }
            if let Some(f) = ecore.wl2_window_available_rotations_set {
                // TV profile in the embedder: landscape only.
                let rotations: [i32; 1] = [0];
                f(self.window, rotations.as_ptr(), 1);
                plog!("window available rotations {{0}}");
            }
        }
    }

    pub fn show(&self, ecore: &Ecore) {
        // SAFETY: valid window handle.
        unsafe { (ecore.wl2_window_show)(self.window) };
        plog!("ecore_wl2_window_show");
    }

    /// The embedder's `DestroyWindow()`: EGL window, window, display, shutdown. Call after the EGL
    /// surface is gone.
    pub fn destroy(&mut self, ecore: &Ecore) {
        // SAFETY: handles are the ones created above; each is cleared after use.
        unsafe {
            if !self.egl_handle.is_null() {
                if self.via_ecore {
                    if let Some(f) = ecore.wl2_egl_window_destroy {
                        f(self.egl_handle);
                        plog!("ecore_wl2_egl_window_destroy");
                    }
                } else if let Some(f) = self.wlegl.as_ref().and_then(|w| w.window_destroy) {
                    f(self.egl_handle);
                    plog!("wl_egl_window_destroy");
                }
                self.egl_handle = null_mut();
                self.native_window = null_mut();
            }
            if !self.window.is_null() {
                if let Some(f) = ecore.wl2_window_free {
                    f(self.window);
                    plog!("ecore_wl2_window_free");
                }
                self.window = null_mut();
            }
            if !self.display.is_null() {
                if let Some(f) = ecore.wl2_display_disconnect {
                    f(self.display);
                    plog!("ecore_wl2_display_disconnect");
                }
                self.display = null_mut();
            }
            if let Some(f) = ecore.wl2_shutdown {
                let n = f();
                plog!("ecore_wl2_shutdown -> {n}");
            }
        }
    }
}
