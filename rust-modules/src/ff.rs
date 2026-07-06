//! FFmpeg FFI (libavformat / libavcodec / libavutil). The TV ships FFmpeg 3.4; we
//! link stub `.so`s carrying the real SONAMEs (libavformat.so.57 / libavcodec.so.57 /
//! libavutil.so.55) and the device loads the real libraries at runtime — the same
//! stub trick used for SDL/GLES/Starfish. This module owns the media demuxer that
//! replaces the hand-rolled mkv.rs: robust MKV/MP4/TS demux, HTTP input, and seeking.
#![allow(dead_code)]
use std::os::raw::c_uint;

extern "C" {
    fn avformat_version() -> c_uint;
    fn avcodec_version() -> c_uint;
    fn avutil_version() -> c_uint;
}

fn ver(v: c_uint) -> String {
    format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
}

/// Boot smoke test: confirms the stub-.so link resolves to the TV's real FFmpeg
/// (non-zero versions => the device libraries loaded). Logs to /tmp/poc-events.log.
pub(crate) fn smoke() {
    unsafe {
        crate::player::log(&format!(
            "ff: avformat={} avcodec={} avutil={}",
            ver(avformat_version()),
            ver(avcodec_version()),
            ver(avutil_version())
        ));
    }
}
