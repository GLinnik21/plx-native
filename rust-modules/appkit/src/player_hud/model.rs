//! **`HudModel`: the per-frame, read-only snapshot of every value the HUD's presentation reads.**
//!
//! Before the split the draw path read process globals in the middle of drawing: `FEED_TX.paused`, the
//! scrub target, `playpos_ns()`, `seek_display_ns()`, `duration_ns()`, `loading(ps)` and the
//! subtitle picks and bitmap cue. The player screen is a machine and reads no global
//! (`docs/sans-io.md`), so those reads become fields of this value. `src/app/run.rs` publishes it
//! before input/Tick and refreshes it before drawing, alongside `busy` and `transport`. The renderer
//! reads it through its `Frame`, and the scrub preview takes it directly; a draw reads no player global.
//!
//! Pure data: every field is `Copy`, there is no borrowed string and no allocation, so building one is
//! a dozen atomic loads and the snapshot costs nothing to hand down the draw. It carries no
//! `PartialEq` because of [`HudModel::bitmap_by_key`], a function pointer.
//!
//! The model also carries the marker entitlement and transport facts the automatic-skip timer
//! consumes at Tick time. The app publishes those before dispatch so the timer sees the current
//! profile and playback. Other live HUD state resolution stays in `state.rs` and `motion.rs`.

use plx_plex::plex::session::{SubtitlePosition, SubtitleSize, SubtitleTone};

/// The image-subtitle cue lookup [`HudModel::bitmap_by_key`] injects: the canvas size and the
/// rects of the selected track's display set with this `start_ns` key.
pub type BitmapLookup = fn(i64) -> Option<(i32, i32, Vec<plx_media::player::SubRect>)>;

/// The HUD's presentation and automatic-skip inputs, sampled once for the frame from the owned
/// attempt readout and viewer controls. The doc comment of each field names its source in
/// `src/app/run.rs`.
#[derive(Clone, Copy, Debug)]
pub struct HudModel {
    /// `plx_play_feed::shared::FEED_TX.paused.load(Relaxed)`: the viewer is logically paused. Drives the
    /// title's reveal and the playbar's state mark.
    pub paused: bool,
    pub marker_prefs: crate::skip_policy::MarkerPrefsView,
    pub playback_id: u32,
    pub advancing: bool,
    pub seeking: bool,
    pub covered: bool,
    pub skip_progress: Option<f32>,
    /// The playing metadata is a genuine episode, independent of queue neighbours.
    pub is_episode: bool,
    /// The projected local wall-clock finish time, supplied by the app for this frame.
    pub end_time: Option<[i32; 2]>,
    /// `plx_media::player::TX.scrub_ns.load(Relaxed)`: the live scrub preview's target, -1 when no
    /// scrub is in progress.
    pub scrub_ns: i64,
    /// `plx_media::player::playpos_ns(ps)`: the published playhead, with unknown mapped at the widget boundary.
    pub playpos_ns: i64,
    /// `plx_media::player::seek_display_ns(ps)`: the target a loading seek freezes the playhead at,
    /// -1 when none.
    pub seek_display_ns: i64,
    /// `plx_media::player::duration_ns(ps)`, with unknown mapped at the widget boundary.
    pub duration_ns: i64,
    /// `plx_media::player::loading(ps)`: a seek is resolving (the playbar shows `seek_display_ns`).
    pub loading: bool,
    /// `plx_media::route::subtitle_size()`: the viewer's caption size pick.
    pub sub_size: SubtitleSize,
    /// `plx_media::route::subtitle_position()`: the viewer's caption position pick.
    pub sub_position: SubtitlePosition,
    /// `plx_media::player::subtitle_tone()`: the caption ink's tone.
    pub sub_tone: SubtitleTone,
    /// `plx_media::player::desired_sub_idx()`: the selected subtitle track, -1 when off.
    pub sub_sel: i32,
    /// `plx_media::player::active_bitmap_key(plx_media::player::playpos_ns(ps))`: the `start_ns` key
    /// of the selected track's image cue active now, `None` between cues.
    pub bitmap_key: Option<i64>,
    /// `Controls::now(ps)`: which of the Previous/Next pair the playing episode has, and whether
    /// Previous restarts. The enabled predicates of `Previous` and `Next` read it
    /// ([`super::command::HudCommand::enabled`]).
    pub controls: super::state::Controls,
    /// `plx_appkit::chapters_panel::has_chapters(meta)`: the playing item has chapters.
    pub chapters: bool,
    /// `plx_appkit::info_panel::detail_rk(meta).is_some()`: the playing item has a ratingKey to
    /// open the page of (`go_to_detail`).
    pub detail: bool,
    /// The viewer's Skip interval in whole seconds (`plx_media::route::skip_interval().ns() / 1e9`): the
    /// rung a `seek by: setting` control's `by_interval` face is picked at.
    pub skip_secs: u32,
    /// Stats for nerds is up (`app::diagnostics::enabled()`): the `on` of a `toggle_stats` face.
    pub stats: bool,
    /// `plx_media::player::bitmap_by_key`: the lookup `draw_subtitle_bitmap` calls when the key
    /// changes. Injected so this module never names the player.
    pub bitmap_by_key: BitmapLookup,
}

/// The lookup an idle model carries: no cue.
fn no_bitmap(_key: i64) -> Option<(i32, i32, Vec<plx_media::player::SubRect>)> {
    None
}

impl Default for HudModel {
    /// What an idle process reads from those globals: their initialisers. Not paused, no scrub and no
    /// seek target (-1), the playhead and duration at 0, nothing loading, the default caption picks
    /// (Medium, Low, white), no subtitle track (-1) and no cue.
    fn default() -> Self {
        Self {
            paused: false,
            marker_prefs: Default::default(), playback_id: 0, advancing: false, seeking: false, covered: false, skip_progress: None,
            is_episode: false,
            end_time: None,
            scrub_ns: -1,
            playpos_ns: 0,
            seek_display_ns: -1,
            duration_ns: 0,
            loading: false,
            sub_size: SubtitleSize::default(),
            sub_position: SubtitlePosition::default(),
            sub_tone: SubtitleTone::default(),
            sub_sel: -1,
            controls: super::state::Controls::NONE,
            chapters: false,
            detail: false,
            skip_secs: 10,
            stats: false,
            bitmap_key: None,
            bitmap_by_key: no_bitmap,
        }
    }
}


/// The playback facts that must all be known before the HUD can show a projected finish.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndTimeEligibility {
    /// The seated profile has enabled the end-time display.
    pub enabled: bool,
    /// The playing metadata is a genuine movie or episode.
    pub is_movie_or_episode: bool,
    /// The player is showing a preview/trailer transport.
    pub is_preview: bool,
    /// The playback state reports that media time is advancing.
    pub advancing: bool,
    /// The viewer's pause goal is active.
    pub viewer_paused: bool,
    /// A seek is pending or resolving.
    pub seeking: bool,
    /// The player is loading or rebuilding the current item.
    pub loading: bool,
    /// The viewer is dragging the scrubber.
    pub scrubbing: bool,
}

/// Whether the projected finish is meaningful for the current playback frame.
/// Scrubbing, seeking, loading and pauses deliberately suppress the estimate rather than
/// presenting a prediction based on a position or transport state that is not advancing.
pub fn end_time_is_eligible(state: EndTimeEligibility) -> bool {
    state.enabled
        && state.is_movie_or_episode
        && !state.is_preview
        && state.advancing
        && !state.viewer_paused
        && !state.seeking
        && !state.loading
        && !state.scrubbing
}

/// Project an item's end timestamp from the current wall clock, content position and the
/// playback pipeline's APPLIED rate (per mille). Invalid or unrepresentable inputs stay unknown.
///
/// Keep the wall clock and remaining media time at subsecond precision until they have been added.
/// Apply timezone rules at the projected instant, including any DST transition before the end.
pub fn projected_end_unix_seconds(
    now_utc_ms: i64,
    position_ns: i64,
    duration_ns: i64,
    rate_permille: Option<i32>,
) -> Option<i64> {
    let rate_permille = rate_permille?;
    if now_utc_ms <= 0 || position_ns < 0 || duration_ns <= position_ns || rate_permille <= 0 {
        return None;
    }
    let remaining_ns = i128::from(duration_ns) - i128::from(position_ns);
    let wall_ns = remaining_ns * 1_000 / i128::from(rate_permille);
    let end_ns = i128::from(now_utc_ms) * 1_000_000 + wall_ns;
    let end_ms = i64::try_from(end_ns / 1_000_000).ok()?;
    Some(end_ms / 1_000)
}

#[cfg(test)]
mod end_time_tests {
    use super::{end_time_is_eligible, projected_end_unix_seconds};

    #[test]
    fn end_time_visibility_requires_a_reliable_movie_or_episode_playback() {
        use super::EndTimeEligibility as E;
        let playing = E { enabled: true, is_movie_or_episode: true, advancing: true, ..E::default() };
        assert!(end_time_is_eligible(playing));
        assert!(!end_time_is_eligible(E { enabled: false, ..playing }), "preference off");
        assert!(!end_time_is_eligible(E { is_movie_or_episode: false, ..playing }), "other content");
        assert!(!end_time_is_eligible(E { is_preview: true, ..playing }), "preview/trailer");
        assert!(!end_time_is_eligible(E { advancing: false, ..playing }), "not advancing");
        assert!(!end_time_is_eligible(E { viewer_paused: true, ..playing }), "viewer pause");
        assert!(!end_time_is_eligible(E { seeking: true, ..playing }), "seek");
        assert!(!end_time_is_eligible(E { loading: true, ..playing }), "loading");
        assert!(!end_time_is_eligible(E { scrubbing: true, ..playing }), "scrub");
    }

    #[test]
    fn projected_finish_uses_remaining_content_time_and_applied_speed() {
        let now = 1_794_219_544_000;
        let thirty_five_minutes = 35 * 60 * 1_000_000_000i64;
        assert_eq!(projected_end_unix_seconds(now, 0, thirty_five_minutes, Some(1000)), Some(now / 1_000 + 2100));
        assert_eq!(projected_end_unix_seconds(now, 0, thirty_five_minutes, Some(2000)), Some(now / 1_000 + 1050));
        assert_eq!(projected_end_unix_seconds(now, 0, thirty_five_minutes, Some(500)), Some(now / 1_000 + 4200));
        assert_eq!(projected_end_unix_seconds(now, 0, thirty_five_minutes, Some(1250)), Some(now / 1_000 + 1680));
        assert_eq!(
            projected_end_unix_seconds(
                now,
                10 * 60 * 1_000_000_000,
                45 * 60 * 1_000_000_000,
                Some(1500),
            ),
            Some(now / 1_000 + 1400)
        );
        assert_eq!(
            projected_end_unix_seconds(86_390_000, 0, 20_000_000_000, Some(1000)),
            Some(86_410),
            "the projection crosses midnight as a Unix timestamp, not local clock arithmetic"
        );
    }

    #[test]
    fn projected_finish_rejects_unknown_invalid_and_overflowing_inputs() {
        let now = 1_794_219_544_000;
        assert_eq!(projected_end_unix_seconds(now, -1, 100, Some(1000)), None);
        assert_eq!(projected_end_unix_seconds(now, 0, 0, Some(1000)), None);
        assert_eq!(projected_end_unix_seconds(now, 10, 10, Some(1000)), None);
        assert_eq!(projected_end_unix_seconds(now, 0, 100, Some(0)), None);
        assert_eq!(
            projected_end_unix_seconds(now, 0, 100, None),
            None,
            "unknown applied rate is not assumed to be 1x"
        );
        assert_eq!(projected_end_unix_seconds(0, 0, 100, Some(1000)), None);
        assert_eq!(projected_end_unix_seconds(i64::MAX, 0, i64::MAX, Some(1)), None);
    }

    #[test]
    fn finish_time_is_stable_near_a_minute_boundary() {
        let duration = 78_300_000_000;
        for elapsed_ms in [0, 500, 1000, 1500] {
            assert_eq!(projected_end_unix_seconds(1_000_200 + elapsed_ms, elapsed_ms * 1_000_000, duration, Some(1000)), Some(1078));
        }
        assert_eq!(projected_end_unix_seconds(1_079_999, 0, 1_000_000, Some(1000)), Some(1080));
    }
}
