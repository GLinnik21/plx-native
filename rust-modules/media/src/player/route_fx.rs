//! What route policy asks of the player. Route decides (it names no backend and carries no backend
//! `cfg`); this module executes. The audio and speed picks step the session's readout, which is
//! backend-neutral, so those run in every build. The rest still write the Feed executor's process
//! state (the claim hold, the Original failure mark, the side reader), which only a composed Feed
//! reads: each is a no-op when the build composes no Feed driver, and each waits for its route
//! caller to hold the adapter (route effects as typed effects a runner executes, L4). Which
//! backends a session may offer is not decided here: the application's boot installs the set its
//! drivers form (`PlayerAdapter::composed_backends`).

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
pub fn set_audio_track(ps: &mut PlaybackSession, idx: i32) {
    super::set_audio_track(ps, idx);
}

/// Back to the default audio stream, for a new item.
pub fn reset_audio_track(ps: &mut PlaybackSession) {
    super::reset_audio_track(ps);
}

/// Back to 1x, for a new item: the session's readout drops the offered rate and a pending reload.
pub fn reset_play_rate(ps: &mut PlaybackSession) {
    super::reset_play_rate(ps);
}

/// Stage a native audio switch (the Load codec and the demuxer's stream index) without queueing
/// its reload.
pub fn stage_native_audio(ps: &mut PlaybackSession, audio_idx: i32, codec: &str) {
    super::stage_native_audio(ps, audio_idx, codec);
}

/// Request a native audio-track switch: stage it and queue the direct-play reload.
pub fn request_audio_track(ps: &mut PlaybackSession, audio_idx: i32, codec: &str) {
    super::request_audio_track(ps, audio_idx, codec);
}

/// Is the side subtitle reader running?
pub fn side_reader_active() -> bool {
    #[cfg(feature = "play-feed")]
    { plx_play_feed::side_reader::active() }
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
///
/// A reader begun mid-play starts at the display base the live sample carries.
pub fn sync_side_reader(ps: &PlaybackSession, target: Option<SideReaderTarget>) {
    #[cfg(feature = "play-feed")]
    super::subside::sync(target.and_then(|t| super::subside::spec_for(&t, ps.readout().raw().disp_base)));
    #[cfg(not(feature = "play-feed"))]
    let _ = (ps, target);
}
