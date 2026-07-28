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

/// The declarations themselves — private ON PURPOSE. See the module doc.
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

        pub(super) fn acb_create(app_id: *const c_char, player_type: c_int) -> c_long;
        pub(super) fn acb_bind(media_id: *const c_char);
        pub(super) fn acb_send_video_data(source_info: *const c_char) -> c_int;
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
pub(crate) unsafe fn sf_feed(_: &MainThread, p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char {
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

#[inline]
pub(crate) unsafe fn acb_create(_: &MainThread, app_id: *const c_char, player_type: c_int) -> c_long {
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
