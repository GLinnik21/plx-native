//! textinput — the system on-screen keyboard, and the text it commits.
//!
//! **STUB.** The seam exists so the search screen can be built against it; the body lands with the
//! VKB unit. `start`/`stop` are no-ops and `drain` yields nothing, so a caller written today
//! behaves exactly as it will once this is real — an empty field that never fills — rather than
//! failing to compile.
//!
//! ## Why this is plain SDL and not a webOS call
//!
//! Stock `SDL_StartTextInput()` / `SDL_StopTextInput()` raise and dismiss the TV's own keyboard.
//! `SDL_webOS.h` misleads by omission — it has no keyboard entry point — but the backend is not in
//! the webOS extension API at all: it is inside LG's Wayland video driver.
//! `Wayland_CreateDevice` writes four real hooks into `SDL_VideoDevice`
//! (`WebOSHasScreenKeyboardSupport`, `Show`, `Hide`, `IsShown`), `SDL_StartTextInput` dispatches to
//! the second, and `WebOSShowScreenKeyboard` is a complete `text_model` IME client. Typed text
//! comes back as an ordinary `SDL_TEXTINPUT` event, so `app.rs`'s existing `SDL_PollEvent` loop
//! already sees it. moonlight-tv 1.5.8 shipped exactly this against the TV's own SDL.
//!
//! Linking is the plain `extern "C"` case, not `dynlib!`: all 14 firmware images in
//! `tools/fwcompat.py`'s inventories export the whole `SDL_*TextInput*` family. That says nothing
//! about whether the PANEL rises, though — those are stock public API in every SDL2 build — so
//! [`available`] probes at runtime rather than assuming.
//!
//! ## Three traps, for whoever fills this in
//!
//! 1. **The event is shifted.** webOS inserts a `Uint32 inputSource` at `+12`, so the UTF-8 text
//!    starts at **+16**, not the `+12` the vendored `include/SDL2/SDL_events.h` declares. That
//!    header is stock 2.0.4 and lies about this build; the NDK sysroot's fork copy is the proof.
//!    Under `hostsim` the offset really is `+12` — desktop SDL2 is stock — so this is a `cfg`, and
//!    a single hard-coded number ships garbage on one of the two platforms.
//! 2. **`SDL_WINDOW_INPUT_FOCUS` is a precondition.** `SDL_StartTextInput` looks for a window
//!    carrying `0x200` before dispatching, and our window is created `OPENGL | FULLSCREEN` with
//!    neither `SHOWN` nor a focus flag. If it is clear the panel never rises, silently and with no
//!    error — so the boot probe logs the flag rather than trusting it.
//! 3. **A wedge we inherit**: the panel cannot be reopened after dismissal (moonlight-tv#435,
//!    reproduced on webOS 7.4). The community fix is in webosbrew's bundled SDL fork; we call the
//!    TV's own, so we get the bug with no patch. `SDL_SetTextInputRect` is a no-op here too —
//!    open and close, no positioning.
//!
//! Two dead ends, so nobody spends a day on them again: the Luna route (only four
//! `com.webos.service.ime/*` methods, all in ACGs this app does not hold — it is granted
//! `["public"]`), and a physical USB or Bluetooth keyboard (`/dev/input` is not mounted into our
//! jail; `remote.rs` documents the general case).
#![allow(dead_code)]

/// Does this build/firmware claim a system keyboard? Probed, never assumed — see the module doc.
/// A `false` here means the field can still be typed into by other means but no panel will rise.
pub(crate) fn available() -> bool {
    false
}

/// Raise the keyboard. Idempotent: calling it while the panel is already up is not an error, and
/// the caller drives it off its own edit state rather than tracking whether it has been called.
pub(crate) fn start() {}

/// Dismiss it. Also idempotent.
pub(crate) fn stop() {}

/// Take everything committed since the last call, in order, and clear the buffer.
///
/// A `Vec<String>` and not a single `String` because one commit is not one character: an IME can
/// commit a whole word at once, and the caller may want to know where one ended and the next
/// began. Returning owned data (rather than lending a static) keeps the caller free to hold it
/// across a frame.
pub(crate) fn drain() -> Vec<String> {
    Vec::new()
}
