//! Bundled FFmpeg codec ABI and jobs. The physical feed uses `plx_play_feed` and
//! `plx_net`; player policy stays in `plx_media`.

pub mod ffi;
#[cfg(any(test, feature = "test-support"))]
pub mod host_test_support;
pub mod pcm;
pub mod speed_audio;
pub mod subtitle;
pub mod subtitle_extract;
mod speed_decoder;
mod venc;
pub use speed_decoder::SpeedDecoder;
pub use venc::Venc;
