// Copyright (c) 2026 Gleb Linnik
// SPDX-License-Identifier: GPL-3.0-or-later
// Own Rust mirror of src/starfish.h, written from the public ABI contract.
//! webos::video_sink — the starfish.c and video_plane.c seams (sf_* / acb_* / vp_* verbs). These stay
//! C (the mangled-C++ + ACB ABI). The library thread calls back into the Feed backend's `sf_on_event` (`plx_play_feed`'s
//! `sf_event.rs`) and into this module's own `acb_on_event`, which only records the completion in
//! the plane mailbox that `tv::plane::PlaneOwner` drains.
//! Signatures mirror `src/starfish.h` exactly; `long` is 32-bit on the arm target -> c_long.
//!
//! **Everything here but `load` and `unload` takes a [`MainThread`]** (`unload` takes an
//! `UnloadTicket`, which only a `&MainThread` can issue, so a real stop's Unload can run on the
//! `sf-unload` worker — `plx_play_feed::feed_state::PendingRelease`), because the seam has no locking of
//! its own: the ACB bind order is a bare call sequence, and `sf_feed` races the pump's own
//! bookkeeping. The raw declarations live in a private `sys` module so the token is the only way
//! to reach them — a `use crate::webos::video_sink::sys` from elsewhere does not compile, which is
//! what makes this a guarantee rather than a note.
//!
//! Shared display methods remain available without `feed-sink`; packet-feed declarations and
//! the `VideoSink` implementation require that feature.
//!
//! The wrappers are the methods of [`StarfishSink`], the television's `tv::sink::VideoSink`; the
//! port table installs it (`port.rs`) and the player's composed sink reaches it. They stay `unsafe fn`,
//! one-for-one with the declarations. Several take raw pointers, and the pointer-free ones still
//! carry ordering preconditions the C side does not check (`acb_start` before the bind completes,
//! `sf_play` after `sf_destroy`). Calling any of them remains something to think about; the token
//! only says *where* from.
//!
//! This module is not compiled for the `hostsim` build or for this crate's own tests: the simulator's
//! own `VideoSink` is the media player's `ffi_host`. The application's test binary does carry it
//! (its dependency is compiled without `cfg(test)`), but `port.rs` installs `NoSink` there, so no
//! live code reaches an extern and the linker drops them.
use plx_base::task::MainThread;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long};
#[cfg(feature = "feed-sink")]
use std::os::raw::c_uint;

/// The declarations themselves — private ON PURPOSE. See the module doc.
mod sys {
    use std::os::raw::{c_char, c_int, c_long};
    #[cfg(feature = "feed-sink")]
    use std::os::raw::c_uint;

    #[cfg(feature = "feed-sink")]
    extern "C" {
        pub(super) fn sf_load(payload: *const c_char, epoch: c_uint) -> c_int;
        pub(super) fn sf_ready() -> c_int;
        pub(super) fn sf_is_load_completed() -> c_int;
        pub(super) fn sf_play() -> c_int;
        pub(super) fn sf_pause() -> c_int;
        pub(super) fn sf_set_volume(volume: c_int) -> c_int;
        pub(super) fn sf_flush() -> c_int;
        pub(super) fn sf_push_eos() -> c_int;
        pub(super) fn sf_set_time_to_decode(position_ns: i64) -> c_int;
        pub(super) fn sf_set_content_info(position_ns: i64) -> c_int;
        pub(super) fn sf_send_segment() -> c_int;
        pub(super) fn sf_seek_ms(ms: i64) -> c_int;
        pub(super) fn sf_feed(p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char;
        pub(super) fn sf_feed_abi_is_cow() -> c_int;
        pub(super) fn sf_unload();
        pub(super) fn sf_callback_gate_retire() -> c_int;
        pub(super) fn sf_callback_intercepts() -> c_uint;
        pub(super) fn sf_destroy() -> c_int;
        pub(super) fn sf_quarantine();

    }

    extern "C" {
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
        pub(super) fn acb_select(handle: c_long) -> c_int;
        pub(super) fn acb_initialized() -> c_int;
        #[cfg(feature = "devtriggers")]
        pub(super) fn acb_probe_dass_available() -> c_int;
        pub(super) fn acb_media_id(media_id: *const c_char) -> c_int;
        pub(super) fn acb_loaded() -> c_int;
        #[cfg(feature = "devtriggers")]
        pub(super) fn acb_probe_connect_dass() -> c_int;
        pub(super) fn acb_finish(display_task: *mut c_long, playing_task: *mut c_long) -> c_int;
        pub(super) fn acb_bind(media_id: *const c_char);
        pub(super) fn acb_send_video_data(source_info: *const c_char) -> c_int;
        pub(super) fn acb_send_atmos(media_id: *const c_char) -> c_int;
        pub(super) fn acb_start(x: c_long, y: c_long, w: c_long, h: c_long);
        pub(super) fn acb_unload(task_out: *mut c_long) -> c_int;
        pub(super) fn acb_pause();
        pub(super) fn acb_resume();
    }
}

/// The completion mailbox the firmware's thread fills (`acb_on_event` below) and
/// `PlaneOwner::pump` drains on the main thread. Sized well above the handful of tasks one frame
/// can have in flight; an overflow is counted and surfaces as a `Dropped` marker, never silently.
static COMPLETIONS: std::sync::LazyLock<crate::tv::plane::CompletionQueue> =
    std::sync::LazyLock::new(|| crate::tv::plane::CompletionQueue::new(64));

/// The completion callback of `libAcbAPI`, forwarded by `acb_cb` in `src/video_plane.c` on the
/// LIBRARY'S thread: `id` is the handle `plane_create` returned for the core that issued the call
/// (the callback's first argument), `task` the task id that core's call returned (each core numbers
/// its own), `ev` the event type (`-2` timeout, `-1` error, `0` cancel, `1` complete). Takes no
/// `MainThread`, touches no pipeline state: it only records `(id, task, ev)` for the main thread and
/// keeps today's log line.
#[no_mangle]
pub extern "C" fn acb_on_event(id: c_long, task: c_long, ev: c_long, reply: *const c_char) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (code, binding_error) = if reply.is_null() { (None, false) } else {
            // SAFETY: the C seam passes the library's NUL-terminated reply for the call.
            let text = unsafe { CStr::from_ptr(reply) }.to_string_lossy();
            let code = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|value| value.get("errorCode").and_then(serde_json::Value::as_i64));
            (code, text.contains("ERROR_06"))
        };
        plx_base::eventlog::log(&format!("acb_cb task={task} ev={ev} errorCode={code:?} ERROR_06={binding_error} id={id}"));
        COMPLETIONS.push_from(i64::from(id), i64::from(task), i32::try_from(ev).unwrap_or(i32::MIN));
    }));
}

/// The television's video sink: each method is the one seam verb of the same name, through the
/// private `sys` declarations above.
pub struct StarfishSink;

#[cfg(feature = "feed-sink")]
impl crate::tv::sink::VideoSink for StarfishSink {
    /// **One of the two verbs of this seam that is NOT main-thread (the other is `unload`, on its
    /// `UnloadTicket`), and the missing token is how you can tell.** `Load` blocks for the pipeline construction and the library owns its own GMainContext
    /// behind it, so it runs on the media worker (`native_load::load_thread`) by design — putting it on
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
    unsafe fn set_volume(&self, _: &MainThread, volume: c_int) -> c_int {
        sys::sf_set_volume(volume)
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
    unsafe fn seek(&self, _: &MainThread, ms: i64) -> c_int {
        sys::sf_seek_ms(ms)
    }
    #[inline]
    fn feed_abi_is_cow(&self) -> bool {
        // SAFETY: the C accessor takes no arguments and resolves under pthread_once.
        unsafe { sys::sf_feed_abi_is_cow() != 0 }
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
    unsafe fn unload(&self, _: &crate::tv::sink::UnloadTicket) {
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
}

impl crate::tv::sink::DisplaySink for StarfishSink {
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
    unsafe fn plane_select(&self, _: &MainThread, handle: c_long) -> bool { sys::acb_select(handle) != 0 }
    #[inline]
    unsafe fn plane_initialized(&self, _: &MainThread) -> bool { sys::acb_initialized() != 0 }
    #[cfg(feature = "devtriggers")]
    unsafe fn plane_probe_dass_available(&self, _: &MainThread) -> bool { sys::acb_probe_dass_available() != 0 }
    unsafe fn plane_media_id(&self, _: &MainThread, media_id: *const c_char) -> c_int { sys::acb_media_id(media_id) }
    unsafe fn plane_loaded(&self, _: &MainThread) -> c_int { sys::acb_loaded() }
    #[cfg(feature = "devtriggers")]
    unsafe fn plane_probe_connect_dass(&self, _: &MainThread) -> c_int { sys::acb_probe_connect_dass() }
    unsafe fn plane_finish(&self, _: &MainThread) -> crate::tv::sink::PlaneFinish {
        let (mut display_task, mut playing_task): (c_long, c_long) = (0, 0);
        let rv = sys::acb_finish(&mut display_task, &mut playing_task);
        crate::tv::sink::PlaneFinish { rv, display_task, playing_task }
    }
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
    unsafe fn plane_unload(&self, _: &MainThread) -> crate::tv::sink::PlaneUnload {
        let mut task: c_long = 0;
        let rv = sys::acb_unload(&mut task);
        crate::tv::sink::PlaneUnload { rv, task }
    }
    #[inline]
    fn plane_completions(&self, _: &MainThread) -> Vec<crate::tv::plane::PlaneCompletion> {
        COMPLETIONS.drain()
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
