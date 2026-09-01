//! player::ffi — the starfish.c seam (sf_* / acb_* verbs). These stay C (the mangled-C++ + ACB
//! ABI). The library thread calls back into our sf_on_event / acb_on_event (defined in mod.rs).
//! Signatures mirror `src/starfish.h` exactly; `long` is 32-bit on the arm target -> c_long.
//!
//! **Everything here but `sf_load` takes a [`MainThread`]**, because the seam has no locking of
//! its own: the ACB bind order is a bare call sequence, and `sf_feed` races the pump's own
//! bookkeeping. The raw declarations live in a private `sys` module so the token is the only way
//! to reach them — a `use super::ffi::sys` from elsewhere in `player/` does not compile, which is
//! what makes this a guarantee rather than a note.
//!
//! The wrappers stay `unsafe fn`, one-for-one with the declarations. Several take raw pointers,
//! and the pointer-free ones still carry ordering preconditions the C side does not check
//! (`acb_start` before the bind completes, `sf_play` after `sf_destroy`). Calling any of them
//! remains something to think about; the token only says *where* from.
use crate::task::MainThread;
use std::os::raw::{c_char, c_int, c_long, c_uint};

/// The `hostsim` stand-in: the same 25 verbs, every one reporting the seam's own "no video path"
/// failure. See `ffi_host.rs` for why it fails rather than no-ops successfully.
///
/// This `#[cfg]` and its partner below are the ONLY platform gate in the playback stack — the
/// wrappers underneath are shared verbatim, so nothing in `engine`/`pump`/`threads` branches on
/// which platform it is running on.
#[cfg(feature = "hostsim")]
#[path = "ffi_host.rs"]
mod sys;

/// The declarations themselves — private ON PURPOSE. See the module doc.
#[cfg(not(feature = "hostsim"))]
mod sys {
    use std::os::raw::{c_char, c_int, c_long, c_uint};

    extern "C" {
        pub(super) fn sf_load(payload: *const c_char) -> c_int;
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
        pub(super) fn sf_destroy();

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

/// **The one verb of this seam that is NOT main-thread, and the missing token is how you can
/// tell.** `Load` blocks for the pipeline construction and the library owns its own GMainContext
/// behind it, so it runs on the media worker (`threads::load_thread`) by design — putting it on
/// the main thread would stall the frame loop for the whole load. Everything the main thread does
/// next is gated on `sf_ready()` / `loadCompleted`, which is what keeps that safe.
#[inline]
pub(crate) unsafe fn sf_load(payload: *const c_char) -> c_int {
    sys::sf_load(payload)
}

#[inline]
pub(crate) unsafe fn sf_ready(_: &MainThread) -> c_int {
    sys::sf_ready()
}
#[inline]
pub(crate) unsafe fn sf_is_load_completed(_: &MainThread) -> c_int {
    sys::sf_is_load_completed()
}
#[inline]
pub(crate) unsafe fn sf_play(_: &MainThread) -> c_int {
    sys::sf_play()
}
#[inline]
pub(crate) unsafe fn sf_pause(_: &MainThread) -> c_int {
    sys::sf_pause()
}
#[inline]
pub(crate) unsafe fn sf_flush(_: &MainThread) -> c_int {
    sys::sf_flush()
}
#[inline]
pub(crate) unsafe fn sf_push_eos(_: &MainThread) -> c_int {
    sys::sf_push_eos()
}
#[inline]
pub(crate) unsafe fn sf_set_time_to_decode(_: &MainThread, position_ns: i64) -> c_int {
    sys::sf_set_time_to_decode(position_ns)
}
#[inline]
pub(crate) unsafe fn sf_set_content_info(_: &MainThread, position_ns: i64) -> c_int {
    sys::sf_set_content_info(position_ns)
}
#[inline]
pub(crate) unsafe fn sf_send_segment(_: &MainThread) -> c_int {
    sys::sf_send_segment()
}
#[inline]
pub(crate) unsafe fn sf_feed(
    _: &MainThread,
    p: *const u8,
    size: c_uint,
    pts: i64,
    es_data: c_int,
) -> c_char {
    sys::sf_feed(p, size, pts, es_data)
}
#[inline]
pub(crate) unsafe fn sf_unload(_: &MainThread) {
    sys::sf_unload()
}
#[inline]
pub(crate) unsafe fn sf_destroy(_: &MainThread) {
    sys::sf_destroy()
}

/// Which video-plane binding this television has. See `src/starfish.h`'s `VP_*` and the long
/// comment at the top of `src/starfish.c`.
pub(crate) const VP_NONE: c_int = 0;
/// Not referenced in Rust: the ACB path is selected by the SEAM (every `acb_*` verb no-ops in the
/// other modes) rather than by a branch here, and `ACB_OK` carries the fact the pump needs. Kept
/// so the three values are readable together, matching `starfish.h`.
#[allow(dead_code)]
pub(crate) const VP_ACB: c_int = 1;
pub(crate) const VP_EXPORTED: c_int = 2;

/// Resolved once inside the seam and cached there, so this is cheap to call repeatedly. Takes no
/// token: it only reads a memoized int and touches no pipeline state.
#[inline]
pub(crate) fn vp_mode() -> c_int {
    unsafe { sys::vp_mode() }
}

/// The exported windowId the seam holds, or an empty string when none was created. Diagnostics
/// only (`ui::stats`): it answers "did the window this firmware needs ever exist?", which is the
/// first thing to check when webOS 5+ plays sound over a black screen. Points at the seam's own
/// long-lived buffer, so it is never NULL and never owned here. No token — it reads a static char[].
#[inline]
pub(crate) fn vp_window_id() -> *const c_char {
    unsafe { sys::vp_window_id() }
}

/// `VP_EXPORTED` only. Create the exported window; the returned id must go into the Load payload
/// as `option.windowId`. Ordering matters — see the `MainThread` note in the module doc, and note
/// this must happen BEFORE `sf_load`, which runs on the media worker.
#[inline]
pub(crate) unsafe fn vp_create_window(_: &MainThread) -> *const c_char {
    sys::vp_create_window()
}
#[inline]
pub(crate) unsafe fn vp_place(
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
pub(crate) unsafe fn vp_destroy_window(_: &MainThread) {
    sys::vp_destroy_window()
}

#[inline]
pub(crate) unsafe fn acb_create(
    _: &MainThread,
    app_id: *const c_char,
    player_type: c_int,
) -> c_long {
    sys::acb_create(app_id, player_type)
}
#[inline]
pub(crate) unsafe fn acb_bind(_: &MainThread, media_id: *const c_char) {
    sys::acb_bind(media_id)
}
#[inline]
pub(crate) unsafe fn acb_send_video_data(_: &MainThread, source_info: *const c_char) -> c_int {
    sys::acb_send_video_data(source_info)
}
#[inline]
pub(crate) unsafe fn acb_send_atmos(_: &MainThread, media_id: *const c_char) -> c_int {
    sys::acb_send_atmos(media_id)
}
#[inline]
pub(crate) unsafe fn acb_start(_: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long) {
    sys::acb_start(x, y, w, h)
}
#[inline]
pub(crate) unsafe fn acb_unload(_: &MainThread) {
    sys::acb_unload()
}
#[inline]
pub(crate) unsafe fn acb_pause(_: &MainThread) {
    sys::acb_pause()
}
#[inline]
pub(crate) unsafe fn acb_resume(_: &MainThread) {
    sys::acb_resume()
}
