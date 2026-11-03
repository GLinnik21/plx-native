//! Compatibility facade for the bundled FFmpeg physical feed owned by `plx_play_feed`.
pub use plx_play_feed::ff::*;
pub(crate) use crate::subtitle_job as subside;

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "ff_test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "ff_abr_reject_tests.rs"]
mod abr_reject_tests;

#[cfg(test)]
#[path = "ff_packet_framing_tests.rs"]
mod packet_framing_tests;

#[cfg(test)]
#[path = "ff_aac_timestamp_tests.rs"]
mod aac_timestamp_tests;

#[cfg(test)]
#[path = "ff_source_teardown_tests.rs"]
mod source_teardown_tests;

#[cfg(test)]
#[path = "ff_hls_reserve_tests.rs"]
mod hls_reserve_tests;

#[cfg(test)]
#[path = "ff_stall_guard_tests.rs"]
mod stall_guard_tests;

#[cfg(test)]
#[path = "ff_image_subtitle_tests.rs"]
pub(crate) mod image_subtitle_tests;

#[cfg(test)]
#[path = "ff_subtitle_charset_tests.rs"]
mod subtitle_charset_tests;

#[cfg(test)]
#[path = "ff_redirect_tests.rs"]
mod redirect_tests;

#[cfg(test)]
#[path = "ff_pcm_decode_tests.rs"]
mod pcm_decode_tests;

#[cfg(test)]
#[path = "ff_late_audio_tests.rs"]
mod late_audio_tests;

#[cfg(test)]
#[path = "ff_eof_seek_tests.rs"]
mod eof_seek_tests;
#[cfg(test)]
#[path = "ff_body_heal_tests.rs"]
mod body_heal_tests;

#[cfg(test)]
#[path = "ff_subs_tests.rs"]
mod subs_tests;
