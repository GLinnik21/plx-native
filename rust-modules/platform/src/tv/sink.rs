//! The video sink: the verbs the player speaks to the television's media pipeline. The webOS port
//! implements them over Starfish and ACB (`player::ffi`); the simulator implements them over its
//! own clock (`player::ffi_host`).
use plx_base::task::MainThread;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long, c_uint};

/// Which video-plane binding this television has. See `src/starfish.h`'s `VP_*` and the long
/// comment at the top of `src/starfish.c`.
pub const VP_NONE: c_int = 0;
/// Not referenced in Rust: the ACB path is selected by the SEAM (every `acb_*` verb no-ops in the
/// other modes) rather than by a branch here, and `ACB_OK` carries the fact the pump needs. Kept
/// so the three values are readable together, matching `starfish.h`.
#[allow(dead_code)]
pub const VP_ACB: c_int = 1;
pub const VP_EXPORTED: c_int = 2;

/// **Starfish-shaped.** One method per verb of the webOS seam (`src/starfish.h`): a Load payload,
/// `feed`, and the ACB bind verbs `pump` walks. Another TV OS cannot implement this as is; the
/// OS-neutral sink is step L15b (docs/module-layers.md).
///
/// Same arguments and C return values as the seam; `&MainThread` on every method but `load` (the
/// media worker calls it), `window_mode` and `window_id` (memoized reads).
///
/// # Safety
/// Every `unsafe fn` here forwards to a C verb that wants the arguments `src/starfish.h` documents:
/// a payload or id pointer must be a valid NUL-terminated string for the call, and `feed`'s
/// `p`/`size` a readable buffer. Apart from `load`, the verbs must be called on the main thread
/// (which `&MainThread` witnesses), in the order the player's pump drives them.
pub trait VideoSink: Sync {
    /// # Safety
    /// Unlike every other verb, `load` runs off the main thread (the media worker calls it, after
    /// `create_window` has put the windowId into `payload`), so an implementation must not touch
    /// main-thread state. `payload` must stay valid for the call.
    unsafe fn load(&self, payload: *const c_char, epoch: u32) -> c_int;
    unsafe fn ready(&self, mt: &MainThread) -> c_int;
    unsafe fn is_load_completed(&self, mt: &MainThread) -> c_int;
    unsafe fn play(&self, mt: &MainThread) -> c_int;
    unsafe fn pause(&self, mt: &MainThread) -> c_int;
    unsafe fn flush(&self, mt: &MainThread) -> c_int;
    unsafe fn push_eos(&self, mt: &MainThread) -> c_int;
    unsafe fn set_time_to_decode(&self, mt: &MainThread, position_ns: i64) -> c_int;
    unsafe fn set_content_info(&self, mt: &MainThread, position_ns: i64) -> c_int;
    unsafe fn send_segment(&self, mt: &MainThread) -> c_int;
    /// True when the library's `Feed` is the pre-C++11 copy-on-write-string build (webOS 3.x).
    /// A memoized read, so it takes no `MainThread`.
    fn feed_abi_is_cow(&self) -> bool;
    unsafe fn feed(&self, mt: &MainThread, p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char;
    unsafe fn unload(&self, mt: &MainThread);
    unsafe fn callback_gate_retire(&self, mt: &MainThread) -> c_int;
    unsafe fn callback_intercepts(&self, mt: &MainThread) -> u32;
    unsafe fn destroy(&self, mt: &MainThread) -> c_int;
    unsafe fn quarantine(&self, mt: &MainThread);
    fn window_mode(&self) -> c_int;
    /// The exported windowId the sink holds, or `""` when none was created. Never null. The
    /// storage is the sink's own long-lived buffer (the C seam's `g_window_id`), rewritten only by
    /// `create_window`/`destroy_window` on the main thread, so a caller copies it at once.
    fn window_id(&self) -> &'static CStr;
    /// # Safety
    /// Returns null when no window could be created (or the mode is not `VP_EXPORTED`); callers
    /// must check before reading the id. A non-null pointer is the sink's own buffer, valid until
    /// `destroy_window`.
    unsafe fn create_window(&self, mt: &MainThread) -> *const c_char;
    #[allow(clippy::too_many_arguments)]
    unsafe fn place_window(&self, mt: &MainThread, src_w: c_int, src_h: c_int, dst_x: c_int,
        dst_y: c_int, dst_w: c_int, dst_h: c_int) -> c_int;
    unsafe fn destroy_window(&self, mt: &MainThread);
    unsafe fn plane_create(&self, mt: &MainThread, app_id: *const c_char, player_type: c_int) -> c_long;
    unsafe fn plane_bind(&self, mt: &MainThread, media_id: *const c_char);
    unsafe fn plane_send_video_data(&self, mt: &MainThread, source_info: *const c_char) -> c_int;
    unsafe fn plane_send_atmos(&self, mt: &MainThread, media_id: *const c_char) -> c_int;
    unsafe fn plane_start(&self, mt: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long);
    unsafe fn plane_unload(&self, mt: &MainThread);
    unsafe fn plane_pause(&self, mt: &MainThread);
    unsafe fn plane_resume(&self, mt: &MainThread);
}

/// The installed sink: the only way out of `tv` for the Starfish verbs, and only `player/` may
/// call it (`ci/check-deps.sh`, rule `sink`).
#[cfg_attr(all(any(test, feature = "test-support"), feature = "hostsim"), allow(dead_code))] // player::sink() answers HostSink there
pub fn installed() -> &'static dyn VideoSink { super::port().sink }

/// No sink: exactly `ffi_host.rs`'s disabled answers.
pub struct NoSink;

impl VideoSink for NoSink {
    unsafe fn load(&self, _payload: *const c_char, _epoch: u32) -> c_int { 0 }
    unsafe fn ready(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn is_load_completed(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn play(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn pause(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn flush(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn push_eos(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn set_time_to_decode(&self, _mt: &MainThread, _position_ns: i64) -> c_int { 0 }
    unsafe fn set_content_info(&self, _mt: &MainThread, _position_ns: i64) -> c_int { 0 }
    unsafe fn send_segment(&self, _mt: &MainThread) -> c_int { 0 }
    fn feed_abi_is_cow(&self) -> bool { false }
    unsafe fn feed(&self, _mt: &MainThread, _p: *const u8, _size: c_uint, _pts: i64, _es_data: c_int) -> c_char {
        b'e' as c_char
    }
    unsafe fn unload(&self, _mt: &MainThread) {}
    unsafe fn callback_gate_retire(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn callback_intercepts(&self, _mt: &MainThread) -> u32 { 0 }
    unsafe fn destroy(&self, _mt: &MainThread) -> c_int { 0 }
    unsafe fn quarantine(&self, _mt: &MainThread) {}
    fn window_mode(&self) -> c_int { VP_NONE }
    fn window_id(&self) -> &'static CStr { c"" }
    unsafe fn create_window(&self, _mt: &MainThread) -> *const c_char { std::ptr::null() }
    #[allow(clippy::too_many_arguments)]
    unsafe fn place_window(&self, _mt: &MainThread, _src_w: c_int, _src_h: c_int, _dst_x: c_int,
        _dst_y: c_int, _dst_w: c_int, _dst_h: c_int) -> c_int { 0 }
    unsafe fn destroy_window(&self, _mt: &MainThread) {}
    unsafe fn plane_create(&self, _mt: &MainThread, _app_id: *const c_char, _player_type: c_int) -> c_long { 0 }
    unsafe fn plane_bind(&self, _mt: &MainThread, _media_id: *const c_char) {}
    unsafe fn plane_send_video_data(&self, _mt: &MainThread, _source_info: *const c_char) -> c_int { -1 }
    unsafe fn plane_send_atmos(&self, _mt: &MainThread, _media_id: *const c_char) -> c_int { 0 }
    unsafe fn plane_start(&self, _mt: &MainThread, _x: c_long, _y: c_long, _w: c_long, _h: c_long) {}
    unsafe fn plane_unload(&self, _mt: &MainThread) {}
    unsafe fn plane_pause(&self, _mt: &MainThread) {}
    unsafe fn plane_resume(&self, _mt: &MainThread) {}
}
