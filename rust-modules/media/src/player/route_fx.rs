//! The physical effects route policy asks of the Feed executor, and the one place that knows whether
//! Feed was composed. Route decides (it names no backend and carries no backend `cfg`); this module
//! executes, and every function is a no-op when the build composes no Feed driver. Each one writes
//! the Feed executor's shared state or its side reader, which only a composed Feed reads.

use crate::route::{PlaybackSession, SideReaderTarget};

/// Drop the claim hold: the playback it belonged to is gone.
pub fn clear_claim_hold() {
    #[cfg(feature = "play-feed")]
    super::claim_hold::clear();
}

/// Forget the typed failure of the Original probe a replacement source is leaving.
pub fn clear_original_failure() {
    #[cfg(feature = "play-feed")]
    super::clear_original_failure();
}

/// Select the audio stream index the demuxer feeds at the first Load (`-1`: the default).
pub fn set_audio_track(idx: i32) {
    #[cfg(feature = "play-feed")]
    super::set_audio_track(idx);
    #[cfg(not(feature = "play-feed"))]
    let _ = idx;
}

/// Back to the default audio stream, for a new item.
pub fn reset_audio_track() {
    #[cfg(feature = "play-feed")]
    super::reset_audio_track();
}

/// Back to 1x, for a new item.
pub fn reset_play_rate() {
    #[cfg(feature = "play-feed")]
    super::reset_play_rate();
}

/// Stage a native audio switch (the Load codec and the demuxer's stream index) without queueing
/// its reload.
pub fn stage_native_audio(ps: &mut PlaybackSession, audio_idx: i32, codec: &str) {
    #[cfg(feature = "play-feed")]
    super::stage_native_audio(ps, audio_idx, codec);
    #[cfg(not(feature = "play-feed"))]
    let _ = (ps, audio_idx, codec);
}

/// Request a native audio-track switch: stage it and queue the direct-play reload.
pub fn request_audio_track(ps: &mut PlaybackSession, audio_idx: i32, codec: &str) {
    #[cfg(feature = "play-feed")]
    super::request_audio_track(ps, audio_idx, codec);
    #[cfg(not(feature = "play-feed"))]
    let _ = (ps, audio_idx, codec);
}

/// Is the side subtitle reader running?
pub fn side_reader_active() -> bool {
    #[cfg(feature = "play-feed")]
    { super::subside::active() }
    #[cfg(not(feature = "play-feed"))]
    { false }
}

/// The track the dev trigger arms the side reader for, if any.
pub fn side_reader_dev_armed_ordinal() -> Option<i32> {
    #[cfg(feature = "play-feed")]
    { super::subside::dev_armed_ordinal() }
    #[cfg(not(feature = "play-feed"))]
    { None }
}

/// Make the side reader match the route's target: start, switch, run clock-only, or stop for `None`.
pub fn sync_side_reader(target: Option<SideReaderTarget>) {
    #[cfg(feature = "play-feed")]
    super::subside::sync(target.and_then(|t| super::subside::spec_for(&t)));
    #[cfg(not(feature = "play-feed"))]
    let _ = target;
}
