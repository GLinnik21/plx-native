//! The simulator's side of the playback seam: the host sink its port table holds, and the decoded
//! picture that sink is built with. The sink itself is the Feed backend's stand-in
//! (`plx_play_feed::ffi_host`); what lives here is what only the application may name, because it
//! draws: `sim_video` composites the picture under the finished UI frame with `plx_gfx`. The media
//! layer's host tests build their own sink beside it (`player::test_physical`, with no picture).
//!
//! `hostsim` builds only: the television's port holds the Starfish sink.

mod sim_video;

use plx_play_feed::ffi_host::{HostSink, Hooks};

/// The simulator's video sink, built with the simulator's decoded picture. The port table holds
/// it as `Port.sink`; a hostsim test binary of this crate reads it directly.
pub(crate) static HOST_SINK: HostSink = HostSink::new(Hooks::over(&sim_video::SimVideo));

/// The render thread's half of the simulator's picture, once per bound-plane frame, after the
/// straighten pass and before the capture and the swap: the current decoded frame, composited UNDER
/// what the UI drew through the television compositor's straight-alpha mix. `false` when there is no
/// picture to put under it (not armed, or nothing decoded yet), which is the caller's cue to
/// composite over black instead.
pub(crate) fn composite_under() -> bool {
    sim_video::composite_under()
}

/// The simulator's clock-sink stop; the television's pipeline has no such control.
pub(crate) fn stop_sim_clock_at(media_ns: Option<i64>) -> bool {
    HOST_SINK.stop_clock_at(media_ns)
}
