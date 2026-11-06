//! **The Stats-for-nerds rows the Feed writes are the session's last sample of it.** `diag()` reads
//! the feed-written half (`FeedRaw::diag`, [`FeedDiag`]) from the readout, which holds what the pump
//! last ingested, instead of loading the feed's mailboxes itself. Each test delivers a scripted sample
//! through the session's real ingestion while the mailboxes hold something else, so a read that still
//! looked at the mailboxes gives the mailbox's value, and a read that looked at another attempt's
//! sample gives that one.

use plx_play_feed::shared::FEED_SHARED;
use plx_playback_contract::feed::{FeedDiag, FeedFacts};
use std::sync::atomic::Ordering::Relaxed;

use super::attempt_view::Backend;
use super::test_physical::diag;
use super::Diag;
use crate::route::{PlaybackSession, RouteStartAttempt};

/// Every field a different, non-idle value, so a swapped pair or a field read from the wrong place
/// fails.
pub(super) fn scripted() -> FeedDiag {
    FeedDiag {
        place_rv: -3, placed_w: 3840, placed_h: 2160, load_completed: true, cb_count: 41,
        aq_video: 9_000_001, aq_audio: 9_000_002, fed_v_pts: 10_400_000_000,
        fed_a_pts: 10_300_000_000, load_v: 2, load_a: 5, feed_state: 4, cb_err: 18, cb_err_at: 7,
        net_rx: 13_000_000, load_at: 4_200, frame_at: 5_300, video_fps_milli: 23_976,
        fed_v: 5_000, fed_a: 4_800, pushed_any: true,
    }
}

/// The feed's mailboxes as a live session leaves them: each holds `diag`'s value.
pub(super) fn store_mailboxes(diag: &FeedDiag) {
    FEED_SHARED.dg_place_rv.store(diag.place_rv, Relaxed);
    FEED_SHARED.dg_placed_w.store(diag.placed_w, Relaxed);
    FEED_SHARED.dg_placed_h.store(diag.placed_h, Relaxed);
    FEED_SHARED.load_completed.store(diag.load_completed, Relaxed);
    FEED_SHARED.dg_cb_count.store(diag.cb_count, Relaxed);
    FEED_SHARED.dg_aq_video.store(diag.aq_video, Relaxed);
    FEED_SHARED.dg_aq_audio.store(diag.aq_audio, Relaxed);
    FEED_SHARED.dg_fed_v_pts.store(diag.fed_v_pts, Relaxed);
    FEED_SHARED.dg_fed_a_pts.store(diag.fed_a_pts, Relaxed);
    FEED_SHARED.dg_load_v.store(diag.load_v, Relaxed);
    FEED_SHARED.dg_load_a.store(diag.load_a, Relaxed);
    FEED_SHARED.dg_feed_state.store(diag.feed_state, Relaxed);
    FEED_SHARED.dg_cb_err.store(diag.cb_err, Relaxed);
    FEED_SHARED.dg_cb_err_at.store(diag.cb_err_at, Relaxed);
    FEED_SHARED.dg_net_rx.store(diag.net_rx, Relaxed);
    FEED_SHARED.dg_load_at.store(diag.load_at, Relaxed);
    FEED_SHARED.dg_frame_at.store(diag.frame_at, Relaxed);
    FEED_SHARED.video_fps_milli.store(diag.video_fps_milli, Relaxed);
    plx_play_feed::ff_test_support::store_diag_counters(diag.fed_v, diag.fed_a, diag.pushed_any);
}

/// The read-out's feed-written rows, as the panel and the lab snapshot read them.
pub(super) fn rows(d: &Diag) -> FeedDiag {
    FeedDiag {
        place_rv: d.place_rv, placed_w: d.placed_w, placed_h: d.placed_h,
        load_completed: d.load_completed, cb_count: d.cb_count, aq_video: d.aq_video,
        aq_audio: d.aq_audio, fed_v_pts: d.fed_v_pts, fed_a_pts: d.fed_a_pts, load_v: d.load_v,
        load_a: d.load_a, feed_state: d.feed_state, cb_err: d.cb_err, cb_err_at: d.cb_err_at,
        net_rx: d.net_rx, load_at: d.load_at, frame_at: d.frame_at,
        video_fps_milli: d.video_fps_milli, fed_v: d.fed_v, fed_a: d.fed_a, pushed_any: d.pushed_any,
    }
}

fn sample(diag: FeedDiag) -> FeedFacts {
    let mut facts = FeedFacts::default();
    facts.raw.diag = diag;
    facts
}

/// **The rows are the readout's sample, addressed by its attempt.** The mailboxes hold their reset
/// values throughout; the read-out shows what was ingested, a later sample replaces it, and a
/// mailbox that moves between two samples changes nothing until the next one (the one-frame lag the
/// pump's start and end samples bound).
#[test]
fn the_rows_are_the_ingested_samples_for_the_addressed_attempt() {
    let _serial = plx_base::testlock::serial();
    crate::player::reset_session_for_test();
    let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
    let mut ps = PlaybackSession::IDLE;
    ps.begin_attempt_view(a, Backend::Feed);
    assert_eq!(rows(&diag(&ps)), FeedDiag::IDLE, "nothing sampled yet: the idle rows");

    assert!(ps.ingest_feed_facts(a, sample(scripted())));
    assert_eq!(rows(&diag(&ps)), scripted(), "the rows are the sample's, not the (reset) mailboxes'");

    FEED_SHARED.dg_cb_count.store(999, Relaxed);
    assert_eq!(rows(&diag(&ps)).cb_count, 41, "a mailbox that moved after the sample waits for the next one");

    let mut later = scripted();
    later.cb_count = 42;
    assert!(ps.ingest_feed_facts(a, sample(later)));
    assert_eq!(rows(&diag(&ps)), later);

    ps.begin_attempt_view(b, Backend::Feed);
    assert!(!ps.ingest_feed_facts(a, sample(scripted())), "a replaced attempt's sample is refused");
    assert_eq!(rows(&diag(&ps)), later, "so it cannot move the rows");
    crate::player::reset_session_for_test();
}

/// **The sample is the mailboxes, field for field.** A live session's mailboxes, sampled by the
/// feed's real sampler and ingested, read back through `diag()` as the values stored: the equivalence
/// of the old direct loads and the sample, one golden per field.
#[test]
fn the_real_sample_reads_back_as_the_mailboxes_held() {
    let _serial = plx_base::testlock::serial();
    crate::player::reset_session_for_test();
    let a = RouteStartAttempt::fixture();
    let mut ps = PlaybackSession::IDLE;
    ps.begin_attempt_view(a, Backend::Feed);
    store_mailboxes(&scripted());
    assert!(ps.ingest_feed_facts(a, plx_play_feed::feed_pump::native_feed_facts()));
    let d = diag(&ps);
    let s = scripted();
    assert_eq!(d.place_rv, s.place_rv, "place_rv");
    assert_eq!((d.placed_w, d.placed_h), (s.placed_w, s.placed_h), "placed");
    assert_eq!(d.load_completed, s.load_completed, "load_completed");
    assert_eq!(d.cb_count, s.cb_count, "cb_count");
    assert_eq!((d.aq_video, d.aq_audio), (s.aq_video, s.aq_audio), "aq");
    assert_eq!((d.fed_v_pts, d.fed_a_pts), (s.fed_v_pts, s.fed_a_pts), "fed pts");
    assert_eq!((d.load_v, d.load_a), (s.load_v, s.load_a), "load codecs");
    assert_eq!(d.feed_state, s.feed_state, "feed_state");
    assert_eq!((d.cb_err, d.cb_err_at), (s.cb_err, s.cb_err_at), "cb_err");
    assert_eq!(d.net_rx, s.net_rx, "net_rx");
    assert_eq!((d.load_at, d.frame_at), (s.load_at, s.frame_at), "load_at / frame_at");
    assert_eq!(d.video_fps_milli, s.video_fps_milli, "video_fps_milli");
    assert_eq!((d.fed_v, d.fed_a, d.pushed_any), (s.fed_v, s.fed_a, s.pushed_any), "fed counters");
    // The text rows the panel words from the codes.
    assert_eq!(d.load_a_str(), "PCM (decoded in the app)");
    assert_eq!(d.feed_state_str(), "holding ~1.6 s ahead");
    crate::player::reset_session_for_test();
}

/// **A URI attempt never shows Feed rows**, whatever the readout still holds of an earlier Feed
/// session: the Engine that owned those values is gone.
#[test]
fn a_uri_attempt_shows_no_feed_rows() {
    let _serial = plx_base::testlock::serial();
    crate::player::reset_session_for_test();
    let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
    let mut ps = PlaybackSession::IDLE;
    ps.begin_attempt_view(a, Backend::Feed);
    assert!(ps.ingest_feed_facts(a, sample(scripted())));
    ps.begin_attempt_view(b, Backend::Uri);
    let d = diag(&ps);
    assert_eq!(d.backend, Some(Backend::Uri));
    assert_eq!(rows(&d), rows(&Diag::default()), "the early return reads nothing of the feed");
    crate::player::reset_session_for_test();
}

/// **The failure rows are the readout's pair.** What the Original probe's worker noted (its edit,
/// taken in through the session's addressed ingestion) and what the pump noted read back as the
/// kind and status the panel words, a session reset (the rollback reload) leaves them, and a clear
/// drops them: the same values the shared pair held.
#[test]
fn the_failure_rows_are_the_readouts_pair_the_workers_edit_included() {
    use plx_playback_contract::adaptive::FailureEdit;
    let _serial = plx_base::testlock::serial();
    crate::player::reset_session_for_test();
    let a = RouteStartAttempt::fixture();
    let mut ps = PlaybackSession::IDLE;
    ps.begin_attempt_view(a, Backend::Feed);
    let failure = |ps: &PlaybackSession| {
        let d = diag(ps);
        (d.abr_failure_kind, d.abr_failure_status)
    };
    assert_eq!(failure(&ps), (0, 0), "nothing failed");
    assert!(ps.ingest_original_failure(a, FailureEdit::Noted { kind: super::ABR_FAILURE_ORIGINAL_HTTP, status: 503 }));
    assert_eq!(failure(&ps), (super::ABR_FAILURE_ORIGINAL_HTTP, 503), "the worker's note");
    ps.step_readout(super::readout::ReadoutEvent::SessionReset);
    assert_eq!(failure(&ps), (super::ABR_FAILURE_ORIGINAL_HTTP, 503), "a reset leaves it");
    super::note_original_failure(&mut ps, super::ABR_FAILURE_ORIGINAL_OPEN, 0);
    assert_eq!(failure(&ps), (super::ABR_FAILURE_ORIGINAL_OPEN, 0), "the pump's note replaces it");
    assert!(ps.ingest_original_failure(a, FailureEdit::Cleared));
    assert_eq!(failure(&ps), (0, 0), "the worker's clear");
    crate::player::reset_session_for_test();
}
