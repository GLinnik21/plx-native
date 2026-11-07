//! The codecs' FFmpeg bindings media's own code reaches (`plx_media_codecs`), and the integration
//! fixtures that drive the physical Feed (`plx_play_feed`) through the player session.
pub use plx_media_codecs::ffi::*;
pub use plx_media_codecs::{SpeedDecoder, Venc};
#[cfg(test)]
use plx_play_feed::ff_test_support::*;
#[cfg(test)]
use plx_play_feed::{demux, DemuxTransport};
#[cfg(test)]
use plx_media_codecs::subtitle_extract::pts_ns;
#[cfg(test)]
pub(crate) use crate::subtitle_job as subside;

// The integration fixtures remain in media because they exercise its player
// session and route adapter. Their physical vocabulary is imported from the
// lower contracts and the same standard types the former local module used.
#[cfg(test)]
use plx_play_feed::aq::AuQueue;
#[cfg(test)]
use plx_playback_contract::adaptive::TrialClock;
#[cfg(test)]
use plx_playback_contract::demux::{DemuxCallbackEffect, DemuxObserver, DemuxSession};
#[cfg(test)]
use plx_play_feed::SendPtr;
#[cfg(test)]
use plx_net::stream::HttpStream;
#[cfg(test)]
use plx_net::net::origin::Origin;
#[cfg(test)]
use std::ffi::CString;
#[cfg(test)]
use std::os::raw::{c_int, c_void};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Arc;

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "ff_test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "ff_abr_reject_tests.rs"]
mod abr_reject_tests;

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
