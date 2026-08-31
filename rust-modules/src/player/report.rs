//! **Playback lifecycle events joined by one attempt.**
//!
//! `requested -> started -> failed|ended|abandoned`, or
//! `requested -> failed|cancelled|abandoned`. The interesting number is the gap between requested
//! and a terminal outcome: `started / requested` is the startup success rate without leaving a
//! silent unresolved bucket for "it just sat there".
//!
//! # These are TRANSITIONS, not log lines
//!
//! The first design put `playback.started` on the engine's `load:` line. That line is the wrong
//! seam and the name would have been a lie: it is emitted BEFORE the source is opened and before
//! anything plays, so a television that never produced a frame would report a start. It is
//! `requested` now, and `started` fires on the first transition into `Playing` — the same value the
//! HUD renders, so the event says what the viewer saw.
//!
//! # Observed at the DERIVED state, not at `pump::set_state`
//!
//! `set_state` looks like the choke point and is not: [`super::state`] derives two of its answers
//! outside `pb_state` entirely — `Resolving` while a plan is in flight, and `Error` for a
//! `/decision` refusal, which happens before an engine exists and so before the pump has ever run.
//! Hooking the setter would have silently missed the earliest and most certain failure there is.
//! So this observes the value the HUD reads, once a frame.
//!
//! # Once each, and only for a REAL end
//!
//! `Playing` is re-entered after every seek and every reload, and `Error` can be republished on
//! consecutive frames; the latch is what makes each event mean "this attempt", not "this frame".
//! And `ended` fires on a genuine teardown only — a seek, an ABR rung change and an app-switch
//! suspend all end an ENGINE without ending a playback, and counting those as endings would make
//! the completion rate a measure of how often people scrub.

use crate::diag::schema::DiagEvent;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering::Relaxed};

/// The current attempt's opaque id — random, per attempt, never stored. See `DiagEvent`'s playback
/// block: it joins one attempt's lifecycle events and cannot link two playbacks, let alone two sets.
static ATTEMPT: AtomicI64 = AtomicI64::new(0);
/// The last state this module reported a transition FROM.
static LAST: AtomicU8 = AtomicU8::new(0);
/// One `started`/`failed`/`ended` per attempt.
static SAW_START: AtomicBool = AtomicBool::new(false);
static SAW_FAIL: AtomicBool = AtomicBool::new(false);
static SAW_END: AtomicBool = AtomicBool::new(false);
/// At most one bounded rebuffer summary, emitted when a started attempt reaches an observable
/// terminal or replacement path. The app deliberately does not report dropped frames: LG's
/// position callback is a fixed 5 Hz clock and looks identical on smooth and visibly stuttering
/// playback, so treating it as frame cadence would fabricate data.
static SAW_QUALITY: AtomicBool = AtomicBool::new(false);
static REBUFFER_COUNT: AtomicU8 = AtomicU8::new(0);
static REBUFFER_AT_MS: AtomicI64 = AtomicI64::new(0);
static REBUFFER_TOTAL_MS: AtomicI64 = AtomicI64::new(0);
/// When this attempt was requested, in `SDL_GetTicks` milliseconds — the same monotonic clock every
/// other timestamp in this app uses, because pmlog's wall clock on this television runs ~3h off.
static REQUESTED_MS: AtomicI64 = AtomicI64::new(0);

/// **A new attempt.** Called where the app commits to a plan, before anything opens a socket.
///
/// Mints the id and clears every latch, so a second Play on the same item is a second attempt with
/// its own funnel rather than a silent no-op against the first one's latches.
pub(crate) fn requested() {
    resolve_replaced_attempt();
    let id = new_attempt_id();
    ATTEMPT.store(id, Relaxed);
    SAW_START.store(false, Relaxed);
    SAW_FAIL.store(false, Relaxed);
    SAW_END.store(false, Relaxed);
    SAW_QUALITY.store(false, Relaxed);
    REBUFFER_COUNT.store(0, Relaxed);
    REBUFFER_AT_MS.store(0, Relaxed);
    REBUFFER_TOTAL_MS.store(0, Relaxed);
    REQUESTED_MS.store(now_ms(), Relaxed);
    LAST.store(super::shared::PlaybackState::Resolving as u8, Relaxed);
    crate::diag::event(DiagEvent::PlaybackRequested { playback_id: id });
}

/// Resolve an attempt before a newer Play overwrites its join key. Before first frame this is an
/// explicit cancellation; after first frame it is an abandoned viewing session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replacement {
    None,
    Cancelled,
    Abandoned,
}

fn replacement(saw_start: bool, saw_fail: bool, saw_end: bool) -> Replacement {
    if saw_fail || saw_end {
        Replacement::None
    } else if saw_start {
        Replacement::Abandoned
    } else {
        Replacement::Cancelled
    }
}

fn resolve_replaced_attempt() {
    let id = ATTEMPT.swap(0, Relaxed);
    if id == 0 {
        return;
    }
    match replacement(SAW_START.load(Relaxed), SAW_FAIL.load(Relaxed), SAW_END.load(Relaxed)) {
        Replacement::None => {}
        Replacement::Cancelled => {
            crate::diag::event(DiagEvent::PlaybackCancelled { playback_id: id, mode: mode() });
        }
        Replacement::Abandoned => {
            report_quality(id);
            crate::diag::event(DiagEvent::PlaybackAbandoned { playback_id: id, mode: mode() });
        }
    }
}

/// The process is leaving an unresolved attempt. Unlike a newer Play, this was not a replacement
/// choice, so classify it as abandonment; if playback had started, close its quality summary too.
pub(crate) fn abandon_pending() {
    let id = ATTEMPT.load(Relaxed);
    if id == 0 || SAW_FAIL.load(Relaxed) || SAW_END.swap(true, Relaxed) {
        return;
    }
    report_quality(id);
    crate::diag::event(DiagEvent::PlaybackAbandoned { playback_id: id, mode: mode() });
}

/// What a frame's state change is worth reporting, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Started,
    Failed,
}

/// **The rule, pure — because this is the part that decides whether a number is right or double.**
///
/// Everything a dashboard says about playback rests on each of these firing exactly once per
/// attempt, and the two ways to get it wrong are invisible from the dashboard itself: a `started`
/// that re-fires makes the success rate exceed 100% quietly, and one that fires on a rebuffer makes
/// heavy scrubbers look like heavy watchers. Neither is observable without a test, so the decision
/// is separated from the globals and graded on the host.
fn transition(
    prev: super::shared::PlaybackState,
    now: super::shared::PlaybackState,
    saw_start: bool,
    saw_fail: bool,
) -> Option<What> {
    use super::shared::PlaybackState as S;
    if prev == now {
        return None;
    }
    match now {
        // Re-entered after every seek, every ABR rung change and every rebuffer — the latch is what
        // makes this event mean "this attempt", not "this frame".
        S::Playing if !saw_start => Some(What::Started),
        // Republished on consecutive frames while the read-out is up, and reachable twice if a
        // transient state passes through between. Same latch, same reason.
        S::Error if !saw_fail => Some(What::Failed),
        _ => None,
    }
}

/// **Observe the state the HUD is rendering.** Called once a frame from the main loop.
pub(crate) fn tick() {
    let now = super::state();
    let prev = super::shared::PlaybackState::from_u8(LAST.swap(now as u8, Relaxed));
    note_rebuffer(prev, now, SAW_START.load(Relaxed));
    match transition(prev, now, SAW_START.load(Relaxed), SAW_FAIL.load(Relaxed)) {
        Some(What::Started) => {
            SAW_START.store(true, Relaxed);
            crate::diag::event(DiagEvent::PlaybackStarted {
                playback_id: ATTEMPT.load(Relaxed),
                mode: mode(),
                raster: raster_class(super::SHARED.video_h.load(Relaxed)),
                fps: fps_rung(crate::route::stream_fps()),
                video: video_codec_class(&crate::route::stream_vcodec()),
                audio: audio_codec_class(&crate::route::stream_acodec()),
                startup: startup_class(now_ms() - REQUESTED_MS.load(Relaxed)),
            });
        }
        Some(What::Failed) => {
            SAW_FAIL.store(true, Relaxed);
            report_quality(ATTEMPT.load(Relaxed));
            crate::diag::event(DiagEvent::PlaybackFailed {
                playback_id: ATTEMPT.load(Relaxed),
                mode: mode(),
                kind: super::error_now().kind.code(),
            });
        }
        None => {}
    }
}

/// **A real teardown.** Called from the one place playback actually ends — never from a seek, a
/// rung change or a suspend, each of which destroys an engine and keeps the playback.
pub(crate) fn ended(position_ns: i64, duration_ns: i64) {
    if SAW_END.swap(true, Relaxed) || SAW_FAIL.load(Relaxed) {
        return; // already terminal
    }
    let id = ATTEMPT.load(Relaxed);
    if id == 0 {
        return;
    }
    if !SAW_START.load(Relaxed) {
        crate::diag::event(DiagEvent::PlaybackAbandoned { playback_id: id, mode: mode() });
        return;
    }
    report_quality(id);
    crate::diag::event(DiagEvent::PlaybackEnded {
        playback_id: id,
        mode: mode(),
        watched: watched_class(position_ns, duration_ns),
    });
}

fn note_rebuffer(
    prev: super::shared::PlaybackState,
    now: super::shared::PlaybackState,
    saw_start: bool,
) {
    use super::shared::PlaybackState as S;
    if starts_rebuffer(prev, now, saw_start) {
        let _ = REBUFFER_COUNT.fetch_update(Relaxed, Relaxed, |n| Some(n.saturating_add(1)));
        REBUFFER_AT_MS.store(now_ms().max(1), Relaxed);
    } else if now != S::Buffering {
        finish_rebuffer_window();
    }
}

fn starts_rebuffer(
    prev: super::shared::PlaybackState,
    now: super::shared::PlaybackState,
    saw_start: bool,
) -> bool {
    use super::shared::PlaybackState as S;
    saw_start && prev == S::Playing && now == S::Buffering
}

fn finish_rebuffer_window() {
    let at = REBUFFER_AT_MS.swap(0, Relaxed);
    if at > 0 {
        REBUFFER_TOTAL_MS.fetch_add((now_ms() - at).max(0), Relaxed);
    }
}

fn report_quality(playback_id: i64) {
    if playback_id == 0 || !SAW_START.load(Relaxed) || SAW_QUALITY.swap(true, Relaxed) {
        return;
    }
    finish_rebuffer_window();
    crate::diag::event(DiagEvent::PlaybackQuality {
        playback_id,
        rebuffers: rebuffer_count_class(REBUFFER_COUNT.load(Relaxed)),
        buffering: rebuffer_time_class(REBUFFER_TOTAL_MS.load(Relaxed)),
    });
}

fn rebuffer_count_class(n: u8) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=3 => "2-3",
        _ => "4+",
    }
}

fn rebuffer_time_class(ms: i64) -> &'static str {
    match ms {
        ..=0 => "none",
        1..=1_999 => "<2s",
        2_000..=9_999 => "2-10s",
        _ => "10s+",
    }
}

fn mode() -> &'static str {
    if crate::route::is_transcoding() { "transcode" } else { "direct" }
}

/// Milliseconds since this process started, monotonic.
///
/// **`std::time::Instant`, not `SDL_GetTicks`**, and the reason is the host suite rather than
/// taste: `cargo test --lib` links no SDL, so a `SDL_GetTicks` here does not skip a test — it stops
/// the whole suite LINKING, which is the boundary `ui/CLAUDE.md` records for `TTF_SizeUTF8`. Only
/// the DIFFERENCE of two readings is ever used, so any monotonic origin will do.
///
/// Never a wall clock either way: pmlog's on this television runs about three hours off, which is
/// why `docs/agent-reference.md` says to correlate a crash by monotonic time and not by time of day.
fn now_ms() -> i64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(std::time::Instant::now).elapsed().as_millis() as i64
}

/// A random attempt id. `/dev/urandom` like every other random value in this crate — never a clock
/// or a counter, both of which would say something about the television across attempts.
fn new_attempt_id() -> i64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_err() {
        return 0; // no randomness: the funnel loses its join and nothing else
    }
    (i64::from_le_bytes(b) & i64::MAX) as i64
}

/// The video codec, from a CLOSED table.
///
/// `route::stream_vcodec` hands back a `String` off the wire, and `diag::schema` has no arm that
/// could carry one — deliberately, that being the property that makes "no runtime string reaches
/// the wire" a fact about the type. So the mapping is here: a name the table does not know becomes
/// `other`, which is a real answer (it means the server sent something this app did not expect) and
/// cannot become a leak.
pub(crate) fn video_codec_class(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" => "h264",
        "hevc" | "h265" | "hvc1" => "hevc",
        "av1" => "av1",
        "vp9" => "vp9",
        "mpeg2video" | "mpeg2" => "mpeg2",
        "" => "unknown",
        _ => "other",
    }
}

/// The audio codec, from a closed table, for [`video_codec_class`]'s reason.
pub(crate) fn audio_codec_class(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "aac" => "aac",
        "ac3" => "ac3",
        "eac3" | "ac3 plus" | "ec-3" => "eac3",
        "truehd" => "truehd",
        "dts" | "dca" => "dts",
        "flac" => "flac",
        "mp3" => "mp3",
        "opus" => "opus",
        "" => "unknown",
        _ => "other",
    }
}

// ---- the buckets, which are the privacy decision --------------------------------------------
//
// Exact duration + exact raster + exact frame rate + codec identifies a specific file in a specific
// library. As classes they answer every question this channel exists to answer — does 4K HEVC fail
// more than 1080p h264, does startup get worse on big files — and identify nothing. All pure, so
// every boundary is graded on the host.

/// The four rungs the whole project already reasons in — the pipeline tier's resolution matrix, the
/// PMS decision's own classes, LG's checklist. Named rather than measured for the reason above.
pub(crate) fn raster_class(height: i32) -> &'static str {
    match height {
        h if h <= 0 => "unknown",
        h if h <= 576 => "sd",
        h if h <= 720 => "hd",
        h if h <= 1080 => "fhd",
        _ => "uhd",
    }
}

/// A fixed rung, so 23.976 and 24.000 are one bucket rather than two — the distinction is a
/// fingerprint of a particular encode and answers nothing. Anything off the ladder is `other`
/// rather than the nearest rung: a genuinely odd rate is a fact worth being able to see.
pub(crate) fn fps_rung(fps: f64) -> &'static str {
    const RUNGS: [(f64, &str); 6] =
        [(24.0, "24"), (25.0, "25"), (30.0, "30"), (50.0, "50"), (60.0, "60"), (100.0, "100")];
    if !(fps > 0.0) {
        return "unknown";
    }
    // 1.5% either side, which separates every rung above and still catches both spellings of each
    // (23.976/24, 29.97/30, 59.94/60 — the 1001-denominator forms this project's own fixtures use).
    RUNGS
        .iter()
        .find(|(r, _)| (fps - r).abs() / r <= 0.015)
        .map(|(_, n)| *n)
        .unwrap_or("other")
}

/// How long the viewer waited for a picture. The boundaries are where the EXPERIENCE changes, not
/// round numbers: under a second reads as instant, three seconds is where a person starts to wonder,
/// ten is where they press something.
pub(crate) fn startup_class(ms: i64) -> &'static str {
    match ms {
        m if m < 0 => "unknown",
        m if m < 1_000 => "<1s",
        m if m < 3_000 => "1-3s",
        m if m < 10_000 => "3-10s",
        _ => "10s+",
    }
}

/// How much of it was watched, as the four answers anyone asks of a completion rate. Not a
/// percentage: a percentage plus a duration bucket is a duration, which is the fingerprint the
/// buckets exist to avoid.
pub(crate) fn watched_class(position_ns: i64, duration_ns: i64) -> &'static str {
    if duration_ns <= 0 || position_ns < 0 {
        return "unknown";
    }
    match (position_ns as f64) / (duration_ns as f64) {
        f if f < 0.05 => "abandoned",
        f if f < 0.5 => "some",
        f if f < 0.9 => "most",
        _ => "finished",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_an_attempt_always_gives_the_old_one_a_terminal_outcome() {
        assert_eq!(replacement(false, false, false), Replacement::Cancelled);
        assert_eq!(replacement(true, false, false), Replacement::Abandoned);
        assert_eq!(replacement(false, true, false), Replacement::None);
        assert_eq!(replacement(true, false, true), Replacement::None);
    }

    #[test]
    fn quality_is_bounded_and_seek_priming_is_not_a_rebuffer() {
        assert_eq!(rebuffer_count_class(0), "0");
        assert_eq!(rebuffer_count_class(1), "1");
        assert_eq!(rebuffer_count_class(3), "2-3");
        assert_eq!(rebuffer_count_class(u8::MAX), "4+");
        assert_eq!(rebuffer_time_class(0), "none");
        assert_eq!(rebuffer_time_class(1_999), "<2s");
        assert_eq!(rebuffer_time_class(2_000), "2-10s");
        assert_eq!(rebuffer_time_class(10_000), "10s+");
        assert!(starts_rebuffer(S::Playing, S::Buffering, true));
        assert!(!starts_rebuffer(S::Seeking, S::Buffering, true));
        assert!(!starts_rebuffer(S::Playing, S::Buffering, false));
    }

    use super::super::shared::PlaybackState as S;

    /// Drive a sequence of states through the rule the way a frame loop would, latches and all, and
    /// report what it emitted.
    fn drive(states: &[S]) -> Vec<What> {
        let (mut saw_start, mut saw_fail) = (false, false);
        let mut prev = S::Idle;
        let mut out = Vec::new();
        for &s in states {
            if let Some(w) = transition(prev, s, saw_start, saw_fail) {
                match w {
                    What::Started => saw_start = true,
                    What::Failed => saw_fail = true,
                }
                out.push(w);
            }
            prev = s;
        }
        out
    }

    /// The ordinary success: one `started`, whatever the pre-roll did on the way there.
    #[test]
    fn a_normal_start_reports_once() {
        assert_eq!(drive(&[S::Resolving, S::Connecting, S::Buffering, S::Playing]), [What::Started]);
    }

    /// **A seek is not a second start.** `Playing` is re-entered after every seek, every ABR rung
    /// change and every rebuffer; counting those would push the success rate over 100% and make a
    /// heavy scrubber look like several viewers — both silently, since neither is visible from the
    /// dashboard the number appears on.
    #[test]
    fn seeking_and_rebuffering_do_not_start_a_second_playback() {
        let scrubbed = drive(&[
            S::Playing,
            S::Seeking,
            S::Playing,
            S::Seeking,
            S::Playing,
            S::Buffering, // a rebuffer on a bad link
            S::Playing,
        ]);
        assert_eq!(scrubbed, [What::Started], "a seek or a rebuffer reported a second start");
    }

    /// A failure republished on consecutive frames — which is what the read-out being up looks
    /// like — is one failure.
    #[test]
    fn a_failure_held_on_screen_reports_once() {
        assert_eq!(drive(&[S::Resolving, S::Error, S::Error, S::Error]), [What::Failed]);
        // …and it stays one even if a transient state passes through and comes back.
        assert_eq!(drive(&[S::Error, S::Buffering, S::Error]), [What::Failed]);
    }

    /// A playback that started and then failed reports both, in that order: they are different
    /// questions ("did it ever play" and "did it break"), and a stream that dies mid-film answers
    /// yes to each.
    #[test]
    fn a_playback_that_starts_and_then_dies_reports_both() {
        assert_eq!(drive(&[S::Playing, S::Error]), [What::Started, What::Failed]);
    }

    /// The pre-flight refusal — `/decision` said no, so no engine ever existed and the pump never
    /// ran. It is the earliest and most certain failure there is, and it is why this observes the
    /// DERIVED state rather than `pump::set_state`.
    #[test]
    fn a_refusal_before_any_engine_still_reports_a_failure() {
        assert_eq!(drive(&[S::Resolving, S::Error]), [What::Failed]);
    }

    /// **A codec name the table does not know becomes `other`, never itself.** The wire vocabulary
    /// is closed by construction — `diag::schema` has no arm that can carry a runtime string — and
    /// this is the mapping that keeps it that way for a field whose source IS one.
    #[test]
    fn an_unknown_codec_name_cannot_travel_as_itself() {
        assert_eq!(video_codec_class("h264"), "h264");
        assert_eq!(video_codec_class("HEVC"), "hevc", "the server's casing varies");
        assert_eq!(audio_codec_class("AC3 PLUS"), "eac3", "the Load payload's own spelling");
        for odd in ["cinepak", "../../etc/passwd", "Dune.mkv", "h264 (Main)"] {
            assert_eq!(video_codec_class(odd), "other", "{odd} travelled as itself");
        }
        assert_eq!(video_codec_class(""), "unknown");
        assert_eq!(audio_codec_class(""), "unknown");
    }

    /// The rungs, and both spellings of each. `pipe_h264_1080p5994` is the project's own fixture
    /// that reaches `fps_rational`'s 1001-denominator branch and measures `60000/1001`; a bucket
    /// that put it beside 60 in one build and beside `other` in the next would make a year of
    /// comparisons meaningless.
    #[test]
    fn both_spellings_of_a_frame_rate_land_on_one_rung() {
        for (fps, want) in [
            (24.0, "24"), (24000.0 / 1001.0, "24"),
            (25.0, "25"),
            (30.0, "30"), (30000.0 / 1001.0, "30"),
            (50.0, "50"),
            (60.0, "60"), (60000.0 / 1001.0, "60"),
        ] {
            assert_eq!(fps_rung(fps), want, "{fps}");
        }
        // Off the ladder stays off it — a genuinely odd rate is worth being able to see.
        assert_eq!(fps_rung(48.0), "other");
        assert_eq!(fps_rung(23.0), "other");
        assert_eq!(fps_rung(0.0), "unknown");
        assert_eq!(fps_rung(f64::NAN), "unknown");
    }

    /// The raster classes are the ones the rest of the project already reasons in, and the
    /// boundaries are inclusive at the top of each: 1080 is FHD, 1081 is not.
    #[test]
    fn the_raster_classes_are_the_projects_own_rungs() {
        for (h, want) in [
            (480, "sd"), (576, "sd"), (720, "hd"), (1080, "fhd"), (1081, "uhd"), (2160, "uhd"),
        ] {
            assert_eq!(raster_class(h), want, "{h}");
        }
        assert_eq!(raster_class(0), "unknown");
        assert_eq!(raster_class(-1), "unknown");
    }

    /// **No bucket ever reports an exact value**, which is the whole reason they exist: raster plus
    /// frame rate plus duration plus codec identifies a specific file in a specific library.
    #[test]
    fn a_bucket_never_carries_the_number_it_was_built_from() {
        for h in [479, 481, 719, 1079, 2160, 4320] {
            assert!(!raster_class(h).contains(char::is_numeric), "{h} leaked its height");
        }
        assert!(!watched_class(3_600_000_000_000, 7_200_000_000_000).contains(char::is_numeric));
    }

    /// The completion classes, including the two ends that are the point of the measure.
    #[test]
    fn the_watched_classes_separate_a_bounce_from_a_finish() {
        let hour = 3_600_000_000_000i64;
        assert_eq!(watched_class(0, hour), "abandoned");
        assert_eq!(watched_class(hour / 100, hour), "abandoned");
        assert_eq!(watched_class(hour / 4, hour), "some");
        assert_eq!(watched_class(hour * 3 / 4, hour), "most");
        assert_eq!(watched_class(hour, hour), "finished");
        // A live stream, or metadata that never arrived — not a completion of anything.
        assert_eq!(watched_class(hour, 0), "unknown");
        assert_eq!(watched_class(-1, hour), "unknown");
    }

    /// The startup boundaries are where the EXPERIENCE changes rather than round numbers, and a
    /// negative interval — a clock that went backwards, or an `ended` with no `requested` — is
    /// `unknown` rather than the fastest bucket, which would silently improve the metric.
    #[test]
    fn a_backwards_clock_is_unknown_and_not_the_fastest_bucket() {
        assert_eq!(startup_class(0), "<1s");
        assert_eq!(startup_class(999), "<1s");
        assert_eq!(startup_class(1_000), "1-3s");
        assert_eq!(startup_class(9_999), "3-10s");
        assert_eq!(startup_class(10_000), "10s+");
        assert_eq!(startup_class(-1), "unknown");
    }
}
