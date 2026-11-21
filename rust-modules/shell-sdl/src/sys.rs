//! Every SDL2 symbol this crate calls, declared once. A PLAIN link, not `dynlib!`: the rule in
//! `plx_base::dynlib` is for libraries whose SONAME moves, and these are stock public SDL2 API:
//! every name declared outside the `hostsim` block is exported by all 14 firmware inventories
//! (`tools/fwcompat.py --lib libSDL2-2.0.so.0`), so there is nothing here for a runtime bind to
//! tolerate. The one `hostsim` name the inventories do not all carry is `SDL_GetDisplayUsableBounds`:
//! 1.2.0 through 3.9.2 lack it and 4.4.2 onward have it, and the ARM binary never imports it.
//!
//! The Wayland and `SDL_SysWMinfo` calls are NOT here and never may be: the television's SDL fork
//! writes a larger `SDL_SysWMinfo` than the headers declare, and that over-allocated buffer
//! lives in the webOS platform crate's `system.rs` with the rest of the LG half (a pin in `lib.rs`
//! keeps this file from naming it).
#![allow(non_snake_case)]
use std::os::raw::{c_char, c_int, c_void};

extern "C" {
    pub(crate) fn SDL_SetMainReady();
    pub(crate) fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    pub(crate) fn SDL_Init(flags: u32) -> c_int;
    pub(crate) fn SDL_GetCurrentVideoDriver() -> *const c_char;
    pub(crate) fn SDL_GL_SetAttribute(attr: c_int, value: c_int) -> c_int;
    pub(crate) fn SDL_CreateWindow(
        title: *const c_char,
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        flags: u32,
    ) -> *mut c_void;
    pub(crate) fn SDL_DestroyWindow(win: *mut c_void);
    pub(crate) fn SDL_GL_CreateContext(win: *mut c_void) -> *mut c_void;
    pub(crate) fn SDL_GL_SetSwapInterval(interval: c_int) -> c_int;
    pub(crate) fn SDL_Delay(ms: u32);
    pub(crate) fn SDL_PollEvent(event: *mut c_void) -> c_int;
    pub(crate) fn SDL_PushEvent(event: *const c_void) -> c_int;
    pub(crate) fn SDL_GL_SwapWindow(win: *mut c_void);
    pub(crate) fn SDL_Quit();
}

// Desktop-only window management. Apart for the mirror-image reason: a television owns the whole
// panel and never asks how big a display is, so on that build these would be dead code — which
// `[workspace.lints.rust] warnings = "deny"` makes a build failure, not a warning.
#[cfg(feature = "hostsim")]
extern "C" {
    /// `SDL_GetDisplayUsableBounds` — the display minus the menu bar and the Dock, which is what
    /// a window may actually occupy. The out parameter is an `SDL_Rect`: exactly four `c_int`.
    pub(crate) fn SDL_GetDisplayUsableBounds(display: c_int, rect: *mut c_int) -> c_int;
    /// `SDL_SetWindowSize` / `SDL_SetWindowPosition` — `window::fit_window_to_density` re-sizes
    /// and re-centres the window once the display's pixel density has been measured from it.
    pub(crate) fn SDL_SetWindowSize(win: *mut c_void, w: c_int, h: c_int);
    pub(crate) fn SDL_SetWindowPosition(win: *mut c_void, x: c_int, y: c_int);
}

// The system on-screen keyboard (`text`). The text-input family is stock public SDL2 API.
//
// Under `cfg(test)` and the `test-support` feature the four calls a host test could reach are
// swapped for `text::host_test_sdl`, so these declarations lose their last use there and warn; the
// `allow(dead_code)` is narrower than it looks: a real orphan would be silent in every
// configuration, and these are live on device.
extern "C" {
    #[allow(dead_code)]
    pub(crate) fn SDL_StartTextInput();
    #[allow(dead_code)]
    pub(crate) fn SDL_StopTextInput();
    pub(crate) fn SDL_IsTextInputActive() -> c_int;
    #[allow(dead_code)]
    pub(crate) fn SDL_HasScreenKeyboardSupport() -> c_int;
    /// LG's `WebOSIsScreenKeyboardShown`, the fourth of the four hooks its Wayland driver installs.
    /// Exported by all 14 inventories, and **it does not answer the question its name asks** on this
    /// firmware — the app's `textinput` note has the measurement and what replaced it. Declared,
    /// unused, and kept so the next person finds the finding before they find the symbol.
    #[allow(dead_code)]
    pub(crate) fn SDL_IsScreenKeyboardShown(w: *mut c_void) -> c_int;
    #[allow(dead_code)]
    pub(crate) fn SDL_GetWindowFlags(w: *mut c_void) -> u32;
    /// Turn the DRIVER's own tracing on for one category. LG's `WebOSShowScreenKeyboard` /
    /// `Hide` / `TextModelLeave` / `TextModelInputPanelState` all log through SDL at
    /// `SDL_LOG_CATEGORY_INPUT`, which is silent at the default priority — so this is how the
    /// keyboard's real lifecycle becomes readable without patching SDL.
    #[allow(dead_code)] // test build only — see the note above `SDL_StartTextInput`
    pub(crate) fn SDL_LogSetPriority(category: c_int, priority: c_int);
}
