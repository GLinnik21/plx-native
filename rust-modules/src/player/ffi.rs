// Copyright (c) 2026 Gleb Linnik
// SPDX-License-Identifier: GPL-3.0-or-later
// Own Rust mirror of src/starfish.h, written from the public ABI contract.
//! player::ffi — the starfish.c seam (sf_* / acb_* verbs). These stay C (the mangled-C++ + ACB
//! ABI). The library thread calls back into our sf_on_event / acb_on_event (defined in mod.rs).
//! Signatures mirror `src/starfish.h` exactly; `long` is 32-bit on the arm target -> c_long.
//!
//! **Everything here but `load` takes a [`MainThread`]**, because the seam has no locking of
//! its own: the ACB bind order is a bare call sequence, and `sf_feed` races the pump's own
//! bookkeeping. The raw declarations live in a private `sys` module so the token is the only way
//! to reach them — a `use super::ffi::sys` from elsewhere in `player/` does not compile, which is
//! what makes this a guarantee rather than a note.
//!
//! The wrappers are the methods of [`StarfishSink`], the television's `tv::sink::VideoSink`; the
//! port table installs it (`port.rs`) and `player::sink()` reaches it. They stay `unsafe fn`,
//! one-for-one with the declarations. Several take raw pointers, and the pointer-free ones still
//! carry ordering preconditions the C side does not check (`acb_start` before the bind completes,
//! `sf_play` after `sf_destroy`). Calling any of them remains something to think about; the token
//! only says *where* from.
//!
//! This module is not compiled for the `hostsim` build or for host tests: the simulator's own
//! `VideoSink` is `player::ffi_host`, so the test binary references no Starfish externs at all.
use crate::task::MainThread;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long, c_uint};

/// The declarations themselves — private ON PURPOSE. See the module doc.
mod sys {
    use std::os::raw::{c_char, c_int, c_long, c_uint};

    extern "C" {
        pub(super) fn sf_load(payload: *const c_char, epoch: c_uint) -> c_int;
        pub(super) fn sf_ready() -> c_int;
        pub(super) fn sf_is_load_completed() -> c_int;
        pub(super) fn sf_play() -> c_int;
        pub(super) fn sf_pause() -> c_int;
        pub(super) fn sf_flush() -> c_int;
        pub(super) fn sf_push_eos() -> c_int;
        pub(super) fn sf_set_time_to_decode(position_ns: i64) -> c_int;
        pub(super) fn sf_set_content_info(position_ns: i64) -> c_int;
        pub(super) fn sf_send_segment() -> c_int;
        pub(super) fn sf_feed(p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char;
        pub(super) fn sf_unload();
        pub(super) fn sf_callback_gate_retire() -> c_int;
        pub(super) fn sf_callback_intercepts() -> c_uint;
        pub(super) fn sf_destroy() -> c_int;
        pub(super) fn sf_quarantine();

        pub(super) fn vp_mode() -> c_int;
        pub(super) fn vp_create_window() -> *const c_char;
        pub(super) fn vp_window_id() -> *const c_char;
        pub(super) fn vp_place(
            src_w: c_int,
            src_h: c_int,
            dst_x: c_int,
            dst_y: c_int,
            dst_w: c_int,
            dst_h: c_int,
        ) -> c_int;
        pub(super) fn vp_destroy_window();

        pub(super) fn acb_create(app_id: *const c_char, player_type: c_int) -> c_long;
        pub(super) fn acb_bind(media_id: *const c_char);
        pub(super) fn acb_send_video_data(source_info: *const c_char) -> c_int;
        pub(super) fn acb_send_atmos(media_id: *const c_char) -> c_int;
        pub(super) fn acb_start(x: c_long, y: c_long, w: c_long, h: c_long);
        pub(super) fn acb_unload();
        pub(super) fn acb_pause();
        pub(super) fn acb_resume();
    }
}

/// The television's video sink: each method is the one seam verb of the same name, through the
/// private `sys` declarations above.
pub(crate) struct StarfishSink;

impl crate::tv::sink::VideoSink for StarfishSink {
    /// **The one verb of this seam that is NOT main-thread, and the missing token is how you can
    /// tell.** `Load` blocks for the pipeline construction and the library owns its own GMainContext
    /// behind it, so it runs on the media worker (`threads::load_thread`) by design — putting it on
    /// the main thread would stall the frame loop for the whole load. What keeps that safe is NOT
    /// `sf_ready()` — that reads true the moment the object is constructed, before the real Load call
    /// returns (issue #74) — but the C seam's own `g_load_returned` gate inside `sf_ready_object()`,
    /// which refuses the other Starfish verbs until this call has returned. Rust tracks the same
    /// epoch boundary in Shared and the pump bounds both waits with `NATIVE_LOAD_BUDGET`.
    #[inline]
    unsafe fn load(&self, payload: *const c_char, epoch: u32) -> c_int {
        sys::sf_load(payload, epoch)
    }

    #[inline]
    unsafe fn ready(&self, _: &MainThread) -> c_int {
        sys::sf_ready()
    }
    #[inline]
    unsafe fn is_load_completed(&self, _: &MainThread) -> c_int {
        sys::sf_is_load_completed()
    }
    #[inline]
    unsafe fn play(&self, _: &MainThread) -> c_int {
        sys::sf_play()
    }
    #[inline]
    unsafe fn pause(&self, _: &MainThread) -> c_int {
        sys::sf_pause()
    }
    #[inline]
    unsafe fn flush(&self, _: &MainThread) -> c_int {
        sys::sf_flush()
    }
    #[inline]
    unsafe fn push_eos(&self, _: &MainThread) -> c_int {
        sys::sf_push_eos()
    }
    #[inline]
    unsafe fn set_time_to_decode(&self, _: &MainThread, position_ns: i64) -> c_int {
        sys::sf_set_time_to_decode(position_ns)
    }
    #[inline]
    unsafe fn set_content_info(&self, _: &MainThread, position_ns: i64) -> c_int {
        sys::sf_set_content_info(position_ns)
    }
    #[inline]
    unsafe fn send_segment(&self, _: &MainThread) -> c_int {
        sys::sf_send_segment()
    }
    #[inline]
    unsafe fn feed(
        &self,
        _: &MainThread,
        p: *const u8,
        size: c_uint,
        pts: i64,
        es_data: c_int,
    ) -> c_char {
        sys::sf_feed(p, size, pts, es_data)
    }
    #[inline]
    unsafe fn unload(&self, _: &MainThread) {
        sys::sf_unload()
    }
    #[inline]
    unsafe fn callback_gate_retire(&self, _: &MainThread) -> c_int {
        sys::sf_callback_gate_retire()
    }
    #[inline]
    unsafe fn callback_intercepts(&self, _: &MainThread) -> u32 {
        sys::sf_callback_intercepts()
    }
    #[inline]
    unsafe fn destroy(&self, _: &MainThread) -> c_int {
        sys::sf_destroy()
    }
    #[inline]
    unsafe fn quarantine(&self, _: &MainThread) {
        sys::sf_quarantine()
    }

    /// Resolved once inside the seam and cached there, so this is cheap to call repeatedly. Takes no
    /// token: it only reads a memoized int and touches no pipeline state.
    #[inline]
    fn window_mode(&self) -> c_int {
        unsafe { sys::vp_mode() }
    }

    /// The exported windowId the seam holds, or an empty string when none was created. Diagnostics
    /// only (`app::diagnostics`): it answers "did the window this firmware needs ever exist?", which is the
    /// first thing to check when webOS 5+ plays sound over a black screen. Points at the seam's own
    /// long-lived buffer, so it is never NULL and never owned here. No token — it reads a static char[].
    #[inline]
    fn window_id(&self) -> &'static CStr {
        // SAFETY: never NULL and always NUL-terminated (`g_window_id` is a zero-initialised char[64]).
        unsafe { CStr::from_ptr(sys::vp_window_id()) }
    }

    /// `VP_EXPORTED` only. Create the exported window; the returned id must go into the Load payload
    /// as `option.windowId`. Ordering matters — see the `MainThread` note in the module doc, and note
    /// this must happen BEFORE `load`, which runs on the media worker.
    #[inline]
    unsafe fn create_window(&self, _: &MainThread) -> *const c_char {
        sys::vp_create_window()
    }
    #[inline]
    #[allow(clippy::too_many_arguments)]
    unsafe fn place_window(
        &self,
        _: &MainThread,
        src_w: c_int,
        src_h: c_int,
        dst_x: c_int,
        dst_y: c_int,
        dst_w: c_int,
        dst_h: c_int,
    ) -> c_int {
        sys::vp_place(src_w, src_h, dst_x, dst_y, dst_w, dst_h)
    }
    #[inline]
    unsafe fn destroy_window(&self, _: &MainThread) {
        sys::vp_destroy_window()
    }

    #[inline]
    unsafe fn plane_create(
        &self,
        _: &MainThread,
        app_id: *const c_char,
        player_type: c_int,
    ) -> c_long {
        sys::acb_create(app_id, player_type)
    }
    #[inline]
    unsafe fn plane_bind(&self, _: &MainThread, media_id: *const c_char) {
        sys::acb_bind(media_id)
    }
    #[inline]
    unsafe fn plane_send_video_data(&self, _: &MainThread, source_info: *const c_char) -> c_int {
        sys::acb_send_video_data(source_info)
    }
    #[inline]
    unsafe fn plane_send_atmos(&self, _: &MainThread, media_id: *const c_char) -> c_int {
        sys::acb_send_atmos(media_id)
    }
    #[inline]
    unsafe fn plane_start(&self, _: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long) {
        sys::acb_start(x, y, w, h)
    }
    #[inline]
    unsafe fn plane_unload(&self, _: &MainThread) {
        sys::acb_unload()
    }
    #[inline]
    unsafe fn plane_pause(&self, _: &MainThread) {
        sys::acb_pause()
    }
    #[inline]
    unsafe fn plane_resume(&self, _: &MainThread) {
        sys::acb_resume()
    }
}
