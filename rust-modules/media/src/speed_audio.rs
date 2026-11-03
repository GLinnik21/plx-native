//! Pure audio DSP lives with the independent codec consumers.
pub use plx_media_codecs::speed_audio::*;

// The decoder-availability fixture belongs to the codec consumer, independently of feed.
#[cfg(any(test, feature = "test-support"))]
pub use plx_media_codecs::ffi::force_speed_decoder_for_test;
