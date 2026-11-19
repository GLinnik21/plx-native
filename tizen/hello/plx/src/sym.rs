//! The C symbols this library binds, copied verbatim (names and argument order) from the
//! flutter-tizen embedder it mirrors: `tizen_window_ecore_wl2.cc` (window, key handlers) and
//! `tizen_renderer_egl.cc` (EGL display/config/context/surface). Nothing here was run against a
//! real Tizen set; the log's `sym ...` lines are what settle which names exist on which firmware.
//!
//! `required` is what the hello world cannot work without; `optional` is nicety or an alternative
//! path (see `window.rs` for the wl_egl_window fallback).

use crate::dl::{symtable, Ptr};
use std::ffi::{c_char, c_int, c_uint};

/// `Eina_Bool (*Ecore_Event_Handler_Cb)(void *data, int type, void *event)`. `Eina_Bool` is an
/// `unsigned char`; `ECORE_CALLBACK_PASS_ON` is EINA_TRUE (1), `ECORE_CALLBACK_DONE` EINA_FALSE (0).
pub type EventCb = unsafe extern "C" fn(Ptr, c_int, Ptr) -> u8;

symtable! {
    /// libecore_wl2 + libecore (+ libecore_input's init). Opaque handles are all `Ptr`.
    pub struct Ecore {
        required {
            wl2_init: unsafe extern "C" fn() -> c_int = "ecore_wl2_init",
            wl2_display_connect: unsafe extern "C" fn(*const c_char) -> Ptr = "ecore_wl2_display_connect",
            wl2_display_get: unsafe extern "C" fn(Ptr) -> Ptr = "ecore_wl2_display_get",
            wl2_sync: unsafe extern "C" fn() = "ecore_wl2_sync",
            wl2_display_screen_size_get: unsafe extern "C" fn(Ptr, *mut i32, *mut i32) = "ecore_wl2_display_screen_size_get",
            wl2_window_new: unsafe extern "C" fn(Ptr, Ptr, i32, i32, i32, i32) -> Ptr = "ecore_wl2_window_new",
            wl2_window_show: unsafe extern "C" fn(Ptr) = "ecore_wl2_window_show",
            event_handler_add: unsafe extern "C" fn(c_int, EventCb, *const std::ffi::c_void) -> Ptr = "ecore_event_handler_add",
        }
        optional {
            // The embedder's path to a wl_egl_window; `wl2_window_surface_get` + libwayland-egl's
            // `wl_egl_window_create` is the logged fallback when these are absent.
            wl2_egl_window_create: unsafe extern "C" fn(Ptr, c_int, c_int) -> Ptr = "ecore_wl2_egl_window_create",
            wl2_egl_window_native_get: unsafe extern "C" fn(Ptr) -> Ptr = "ecore_wl2_egl_window_native_get",
            wl2_egl_window_destroy: unsafe extern "C" fn(Ptr) = "ecore_wl2_egl_window_destroy",
            wl2_window_surface_get: unsafe extern "C" fn(Ptr) -> Ptr = "ecore_wl2_window_surface_get",
            // Window options the embedder sets after creation.
            wl2_window_type_set: unsafe extern "C" fn(Ptr, c_int) = "ecore_wl2_window_type_set",
            wl2_window_position_set: unsafe extern "C" fn(Ptr, c_int, c_int) = "ecore_wl2_window_position_set",
            wl2_window_aux_hint_add: unsafe extern "C" fn(Ptr, c_int, *const c_char, *const c_char) = "ecore_wl2_window_aux_hint_add",
            wl2_window_alpha_set: unsafe extern "C" fn(Ptr, u8) = "ecore_wl2_window_alpha_set",
            wl2_window_available_rotations_set: unsafe extern "C" fn(Ptr, *const c_int, c_uint) = "ecore_wl2_window_available_rotations_set",
            // Teardown, as in the embedder's DestroyWindow().
            wl2_window_free: unsafe extern "C" fn(Ptr) = "ecore_wl2_window_free",
            wl2_display_disconnect: unsafe extern "C" fn(Ptr) = "ecore_wl2_display_disconnect",
            wl2_shutdown: unsafe extern "C" fn() -> c_int = "ecore_wl2_shutdown",
            event_handler_del: unsafe extern "C" fn(Ptr) -> *mut std::ffi::c_void = "ecore_event_handler_del",
            // ECORE_EVENT_* ids are allocated by libecore_input's init; ecore_wl2_init normally
            // runs it. Called only if the ids still read 0 afterwards.
            event_init: unsafe extern "C" fn() -> c_int = "ecore_event_init",
        }
    }
}

symtable! {
    /// libwayland-egl: only the fallback way to make a wl_egl_window from the window's wl_surface.
    pub struct WlEgl {
        required {
            window_create: unsafe extern "C" fn(Ptr, c_int, c_int) -> Ptr = "wl_egl_window_create",
        }
        optional {
            window_destroy: unsafe extern "C" fn(Ptr) = "wl_egl_window_destroy",
        }
    }
}

symtable! {
    /// libEGL.
    pub struct Egl {
        required {
            get_display: unsafe extern "C" fn(Ptr) -> Ptr = "eglGetDisplay",
            initialize: unsafe extern "C" fn(Ptr, *mut i32, *mut i32) -> u32 = "eglInitialize",
            bind_api: unsafe extern "C" fn(u32) -> u32 = "eglBindAPI",
            choose_config: unsafe extern "C" fn(Ptr, *const i32, *mut Ptr, i32, *mut i32) -> u32 = "eglChooseConfig",
            create_context: unsafe extern "C" fn(Ptr, Ptr, Ptr, *const i32) -> Ptr = "eglCreateContext",
            create_window_surface: unsafe extern "C" fn(Ptr, Ptr, Ptr, *const i32) -> Ptr = "eglCreateWindowSurface",
            make_current: unsafe extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32 = "eglMakeCurrent",
            swap_buffers: unsafe extern "C" fn(Ptr, Ptr) -> u32 = "eglSwapBuffers",
            swap_interval: unsafe extern "C" fn(Ptr, i32) -> u32 = "eglSwapInterval",
            query_string: unsafe extern "C" fn(Ptr, i32) -> *const c_char = "eglQueryString",
            get_error: unsafe extern "C" fn() -> i32 = "eglGetError",
        }
        optional {
            get_config_attrib: unsafe extern "C" fn(Ptr, Ptr, i32, *mut i32) -> u32 = "eglGetConfigAttrib",
            destroy_surface: unsafe extern "C" fn(Ptr, Ptr) -> u32 = "eglDestroySurface",
            destroy_context: unsafe extern "C" fn(Ptr, Ptr) -> u32 = "eglDestroyContext",
            terminate: unsafe extern "C" fn(Ptr) -> u32 = "eglTerminate",
        }
    }
}

symtable! {
    /// libGLESv2.
    pub struct Gles {
        required {
            get_string: unsafe extern "C" fn(u32) -> *const u8 = "glGetString",
            clear_color: unsafe extern "C" fn(f32, f32, f32, f32) = "glClearColor",
            clear: unsafe extern "C" fn(u32) = "glClear",
            get_error: unsafe extern "C" fn() -> u32 = "glGetError",
        }
        optional {
            viewport: unsafe extern "C" fn(i32, i32, i32, i32) = "glViewport",
        }
    }
}

// EGL / GL constants (Khronos headers).
pub const EGL_NO_SURFACE: Ptr = std::ptr::null_mut();
pub const EGL_NO_CONTEXT: Ptr = std::ptr::null_mut();
pub const EGL_NONE: i32 = 0x3038;
pub const EGL_DONT_CARE: i32 = -1;
pub const EGL_BUFFER_SIZE: i32 = 0x3020;
pub const EGL_ALPHA_SIZE: i32 = 0x3021;
pub const EGL_BLUE_SIZE: i32 = 0x3022;
pub const EGL_GREEN_SIZE: i32 = 0x3023;
pub const EGL_RED_SIZE: i32 = 0x3024;
pub const EGL_SURFACE_TYPE: i32 = 0x3033;
pub const EGL_VENDOR: i32 = 0x3053;
pub const EGL_VERSION: i32 = 0x3054;
pub const EGL_EXTENSIONS: i32 = 0x3055;
pub const EGL_RENDERABLE_TYPE: i32 = 0x3040;
pub const EGL_CLIENT_APIS: i32 = 0x308D;
pub const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
pub const EGL_WINDOW_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
pub const EGL_OPENGL_ES_API: u32 = 0x30A0;
pub const GL_VENDOR: u32 = 0x1F00;
pub const GL_RENDERER: u32 = 0x1F01;
pub const GL_VERSION: u32 = 0x1F02;
pub const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
