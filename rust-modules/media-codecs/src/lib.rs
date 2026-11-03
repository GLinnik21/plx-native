//! Bundled FFmpeg codec ABI and jobs. Transport and player state stay in `plx_media`.

pub mod ffi;
pub mod pcm;
pub mod speed_audio;
pub mod subtitle;
mod speed_decoder;
mod venc;
pub use speed_decoder::SpeedDecoder;
pub use venc::Venc;
