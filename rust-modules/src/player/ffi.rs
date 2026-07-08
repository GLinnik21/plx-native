//! player::ffi — extern "C" declarations for the starfish.c seam (sf_* / acb_*
//! verbs). These stay C (the mangled-C++ + ACB ABI). The library thread calls back
//! into our sf_on_event / acb_on_event (defined in mod.rs). Signatures mirror
//! src/starfish.h exactly; `long` is 32-bit on the arm target -> c_long.
use std::os::raw::{c_char, c_int, c_long, c_uint};

extern "C" {
    pub(crate) fn sf_load(payload: *const c_char) -> c_int;
    pub(crate) fn sf_ready() -> c_int;
    pub(crate) fn sf_is_load_completed() -> c_int;
    pub(crate) fn sf_play() -> c_int;
    pub(crate) fn sf_pause() -> c_int;
    pub(crate) fn sf_flush() -> c_int;
    pub(crate) fn sf_set_playtime(t: i64);
    pub(crate) fn sf_set_time_to_decode(position_ns: i64) -> c_int;
    pub(crate) fn sf_set_content_info(position_ns: i64) -> c_int;
    pub(crate) fn sf_send_segment() -> c_int;
    pub(crate) fn sf_feed(p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char;
    pub(crate) fn sf_unload();
    pub(crate) fn sf_destroy();

    pub(crate) fn acb_create(app_id: *const c_char, player_type: c_int) -> c_long;
    pub(crate) fn acb_bind(media_id: *const c_char);
    pub(crate) fn acb_send_video_data(source_info: *const c_char) -> c_int;
    pub(crate) fn acb_start(x: c_long, y: c_long, w: c_long, h: c_long);
    pub(crate) fn acb_unload();
}
