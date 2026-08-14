//! The `hostsim` stand-in for `src/starfish.c` — the television's media seam, absent.
//!
//! This file exists because the simulator has no StarfishMediaAPIs, no `libAcbAPI`, and no
//! hardware video plane. It is deliberately NOT a fake player: nothing here decodes, buffers or
//! pretends to. Every verb reports the same failure the seam itself reports when a television has
//! no usable video path (`vp_mode() == VP_NONE`, `sf_load` returning 0), which is a state the
//! engine, the pump and the HUD already handle — a real firmware can be in it, and issue #22 was
//! exactly that. So pressing Play in the simulator lands on the app's genuine full-screen failure
//! read-out rather than on a hang or a panic.
//!
//! **The alternative was worse.** Stubbing these as no-ops that return SUCCESS would have the pump
//! wait forever for frames that never arrive, and a simulator that silently hangs on Play teaches
//! an agent that playback is broken when it is merely absent. Failing honestly and immediately is
//! the whole contract of this file.
//!
//! Signatures mirror `super::sys`'s `extern` block one-for-one; `ffi.rs`'s wrappers are shared by
//! both and do not branch. Adding a verb to the seam means adding it in BOTH places or the host
//! build stops compiling — which is the intended way to find out.

use std::os::raw::{c_char, c_int, c_long, c_uint};

/// `sf_feed`'s rejection code. `starfish.h` documents the three replies as `'O'` (ok), `'B'`
/// (BufferFull) and `'e'` (error); the pump treats `'B'` as backpressure and retries forever, so
/// returning it here would spin. `'e'` is the one that terminates.
const FEED_ERROR: c_char = b'e' as c_char;

pub(super) unsafe fn sf_load(_payload: *const c_char) -> c_int {
    0 // "pipeline could not be constructed" — the engine's existing failure path
}
pub(super) unsafe fn sf_ready() -> c_int {
    0
}
pub(super) unsafe fn sf_is_load_completed() -> c_int {
    0
}
pub(super) unsafe fn sf_play() -> c_int {
    0
}
pub(super) unsafe fn sf_pause() -> c_int {
    0
}
pub(super) unsafe fn sf_flush() -> c_int {
    0
}
pub(super) unsafe fn sf_push_eos() -> c_int {
    0
}
pub(super) unsafe fn sf_set_time_to_decode(_position_ns: i64) -> c_int {
    0
}
pub(super) unsafe fn sf_set_content_info(_position_ns: i64) -> c_int {
    0
}
pub(super) unsafe fn sf_send_segment() -> c_int {
    0
}
pub(super) unsafe fn sf_feed(_p: *const u8, _size: c_uint, _pts: i64, _es_data: c_int) -> c_char {
    FEED_ERROR
}
pub(super) unsafe fn sf_unload() {}
pub(super) unsafe fn sf_destroy() {}

/// `VP_NONE` — "video cannot be displayed, but the app still runs", which is precisely the
/// simulator's situation and an existing, handled television state.
pub(super) unsafe fn vp_mode() -> c_int {
    super::VP_NONE
}
pub(super) unsafe fn vp_create_window() -> *const c_char {
    std::ptr::null()
}
/// Never NUL — contracted to return a valid string even when no window exists, and `ui::stats`
/// reads it unconditionally.
pub(super) unsafe fn vp_window_id() -> *const c_char {
    c"".as_ptr()
}
pub(super) unsafe fn vp_place(
    _src_w: c_int,
    _src_h: c_int,
    _dst_x: c_int,
    _dst_y: c_int,
    _dst_w: c_int,
    _dst_h: c_int,
) -> c_int {
    0
}
pub(super) unsafe fn vp_destroy_window() {}

pub(super) unsafe fn acb_create(_app_id: *const c_char, _player_type: c_int) -> c_long {
    0 // 0 = failed, per starfish.h
}
pub(super) unsafe fn acb_bind(_media_id: *const c_char) {}
pub(super) unsafe fn acb_send_video_data(_source_info: *const c_char) -> c_int {
    -1 // -1 = rejected, per starfish.h
}
pub(super) unsafe fn acb_start(_x: c_long, _y: c_long, _w: c_long, _h: c_long) {}
pub(super) unsafe fn acb_unload() {}
pub(super) unsafe fn acb_pause() {}
pub(super) unsafe fn acb_resume() {}
