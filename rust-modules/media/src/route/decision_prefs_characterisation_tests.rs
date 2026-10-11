//! **Characterisation of the live player settings' write styles, as `main` has them.**
//!
//! Nine settings are published through process-global cells here and persisted through the session
//! store, and they do it in four different ways. This file pins what each way does TODAY, so the
//! refactor that folds them into one door (`track-memory-plan.md` §3.5, PR A2) can be held to it:
//! every assertion was read off `main` by running it, not derived from the plan. A deliberate
//! change to one of these rows (the plan's §3.7 deltas) edits the named test in the same commit.
//!
//! * **Confirm** (`set_direct_play_mode`, `set_next_episode_mode`, `set_skip_interval`,
//!   `set_deck_press`, `set_default_quality`): blocking, on the storage worker; the cell moves only
//!   when the write is `Durable`.
//! * **Optimistic** (`select_subtitle_size`, `select_subtitle_position`, `set_subtitle_tone`,
//!   `set_audio_enhancements`, the player's quality pick `persist_quality_choice`): the cell moves
//!   first, a persist-only closure follows on the worker and never republishes.
//! * **Unchanged-skip**: tone, enhancements and the player's quality pick write nothing when the
//!   stored value already equals the pick; the Confirm rows and size/position always write.
//!
//! **A non-Durable outcome without a new seam.** `redirect_for_test(blocker/auth.json)` (what the
//! older "failed write" tests use) makes the session UNREADABLE, so `update_with_outcome` returns
//! `None` and no write is ever attempted. The fixture used here, [`Unwritable`], leaves the session
//! readable and makes the write itself fail: its directory is read-only, so `write_atomic` cannot
//! create the temp sibling it writes first. That is a genuine `LiveWrite` whose `classify()` is
//! `Failed` (for an unprivileged uid). The first test pins that the fixture really is that.
use super::*;
use plx_plex::plex::session::{
    self as session, DeckPress, NextEpisodeMode, SkipInterval, SubtitlePosition, SubtitleSize, SubtitleTone,
};
use plx_plex::plex::AudioEnhancements;
use plx_telemetry::diag::schema::{DiagEvent, Feature};
use std::sync::mpsc;

/// Put every cell this file moves back, whatever the outcome. Declared AFTER the `serial()` guard so
/// it drops first (`restore_*` assert the lock is held).
struct Restore {
    direct_play: DirectPlayMode,
    next_episode: NextEpisodeMode,
    skip: SkipInterval,
    deck: DeckPress,
    quality: Quality,
    size: SubtitleSize,
    position: SubtitlePosition,
    tone: SubtitleTone,
    enhancements: AudioEnhancements,
}
impl Restore {
    fn take() -> Self {
        Restore {
            direct_play: direct_play_mode(),
            next_episode: next_episode_mode(),
            skip: skip_interval(),
            deck: deck_press(),
            quality: quality(),
            size: subtitle_size(),
            position: subtitle_position(),
            tone: crate::player::subtitle_tone(),
            enhancements: crate::player::audio_enhancements(),
        }
    }
}
impl Drop for Restore {
    fn drop(&mut self) {
        restore_direct_play_mode(self.direct_play);
        restore_next_episode_mode(self.next_episode);
        restore_skip_interval(self.skip);
        restore_deck_press(self.deck);
        restore_quality(self.quality);
        restore_subtitle_size(self.size);
        restore_subtitle_position(self.position);
        crate::player::restore_subtitle_tone(self.tone);
        crate::player::restore_audio_enhancements(self.enhancements);
    }
}

/// The nine cells at the shipped defaults, so a test starts from a known baseline.
fn defaults() {
    restore_direct_play_mode(DirectPlayMode::Auto);
    restore_next_episode_mode(NextEpisodeMode::Countdown);
    restore_skip_interval(SkipInterval::Seconds10);
    restore_deck_press(DeckPress::Details);
    restore_quality(Quality::Original);
    restore_subtitle_size(SubtitleSize::Medium);
    restore_subtitle_position(SubtitlePosition::Low);
    crate::player::restore_subtitle_tone(SubtitleTone::White);
    crate::player::restore_audio_enhancements(AudioEnhancements::NONE);
}

/// The scratch session, readable, whose every write is refused: its directory loses the write
/// bit, so the atomic writer cannot create its temp sibling (`WriteFailure::CreateFailed`) while
/// the file itself still reads. The same technique `plex`'s `session_persistence_tests` use; it
/// binds only an unprivileged uid, so the fixture refuses to run as root rather than quietly
/// producing a Durable write.
struct Unwritable {
    dir: std::path::PathBuf,
}
impl Unwritable {
    fn new(session: &session::TempSession) -> Self {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = session.path().parent().unwrap().to_path_buf();
        assert_ne!(
            std::fs::metadata(&dir).unwrap().uid(),
            0,
            "a read-only directory does not stop root: run the host suite as an ordinary user"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        session::invalidate_for_test();
        Unwritable { dir }
    }
}
impl Drop for Unwritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        session::invalidate_for_test();
    }
}

/// The record on disk, read without letting the read count as a write ([`session::load`] re-persists
/// a plaintext session, which would set the write witness).
fn stored() -> session::Session {
    let s = session::load();
    session::reset_last_write_authority_for_test();
    s
}

/// Did running `f` (and every storage-worker job it queued) write the session?
fn writes(f: impl FnOnce()) -> bool {
    session::reset_last_write_authority_for_test();
    f();
    plx_base::storage_worker::drain_for_test();
    session::last_write_authority_for_test().is_some()
}

fn recv(rx: &mpsc::Receiver<bool>) -> bool {
    rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the persist-only write answered")
}

/// A mark in the shared event log, to read back what a STORAGE-WORKER thread logged since (the
/// private per-thread log cannot see a worker). Other processes only ever add lines, so a test
/// asserts presence ("at least once") of its own distinctive text, or absence of text only this
/// code path writes.
struct LogMark(u64);
impl LogMark {
    fn now() -> Self {
        LogMark(std::fs::metadata(plx_base::eventlog::events_log()).map_or(0, |m| m.len()))
    }
    fn since(&self) -> String {
        let all = std::fs::read(plx_base::eventlog::events_log()).unwrap_or_default();
        String::from_utf8_lossy(all.get(self.0 as usize..).unwrap_or_default()).into_owned()
    }
}

const WRITE_FAILED: &str = "durable write failed (live value kept for this session)";

// =====================================================================================
// The fixture
// =====================================================================================

/// **The fixture is a real failed write, not an unreadable session.** `redirect_for_test` onto an
/// unwritable parent gives `update_with_outcome == None` (nothing to modify); [`Unwritable`] gives
/// `Some(write)` whose classification is not `Durable`. A fixture that quietly degraded to the first
/// would let every "non-Durable" test below pass for the wrong reason.
#[test]
fn the_unwritable_fixture_is_a_readable_session_whose_write_is_not_durable() {
    use session::async_persistence::CompletionOutcome;
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-fixture");

    let durable = session::update_with_outcome(|s| Some(s.with_skip_interval(SkipInterval::Seconds30)))
        .expect("a readable session is written");
    assert!(matches!(durable.classify(), CompletionOutcome::Durable(_)));

    let fixture = Unwritable::new(&session);
    assert_eq!(stored().skip_interval(), SkipInterval::Seconds30, "the session is still READABLE");
    let refused = session::update_with_outcome(|s| Some(s.with_skip_interval(SkipInterval::Seconds60)))
        .expect("a readable session reaches the write");
    assert!(!matches!(refused.classify(), CompletionOutcome::Durable(_)), "and the write is not durable");
    drop(fixture);

    // the OTHER way to fail, for contrast: an unreadable session never reaches the write
    let dir = std::env::temp_dir().join(format!("plxnative-prefs-char-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blocker"), b"x").unwrap();
    session::redirect_for_test(Some(dir.join("blocker").join("auth.json")));
    assert!(session::update_with_outcome(|s| Some(s.with_skip_interval(SkipInterval::Seconds60))).is_none());
    session::redirect_for_test(None);
    let _ = std::fs::remove_dir_all(&dir);
    drop(session);
}

// =====================================================================================
// 6. Confirm: the cell moves only on a Durable write
// =====================================================================================

/// Direct Play mode: a non-Durable write replies `false` and publishes nothing; a Durable one
/// publishes and persists. No telemetry on either path.
#[test]
fn confirm_direct_play_mode_publishes_only_a_durable_write() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-confirm-dpm");
    let _restore = Restore::take();
    defaults();

    let fixture = Unwritable::new(&session);
    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_direct_play_mode(DirectPlayMode::Forced));
    assert!(!saved);
    assert_eq!(direct_play_mode(), DirectPlayMode::Auto, "a refused write publishes nothing");
    assert!(events.is_empty(), "{events:?}");
    drop(fixture);
    assert_eq!(stored().direct_play_mode, DirectPlayMode::Auto);

    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_direct_play_mode(DirectPlayMode::Forced));
    assert!(saved);
    assert_eq!(direct_play_mode(), DirectPlayMode::Forced);
    assert_eq!(stored().direct_play_mode, DirectPlayMode::Forced);
    assert!(events.is_empty(), "this row reports no feature.used: {events:?}");
}

/// Next episode: the same gate, and `after_saved` (one `feature.used`) fires exactly once on a
/// Durable pick and never on a refused one.
#[test]
fn confirm_next_episode_publishes_and_reports_once_only_on_a_durable_write() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-confirm-ne");
    let _restore = Restore::take();
    defaults();

    let fixture = Unwritable::new(&session);
    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_next_episode_mode(NextEpisodeMode::AfterCredits));
    assert!(!saved);
    assert_eq!(next_episode_mode(), NextEpisodeMode::Countdown);
    assert!(events.is_empty(), "{events:?}");
    drop(fixture);
    assert_eq!(stored().next_episode_mode(), NextEpisodeMode::Countdown);

    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_next_episode_mode(NextEpisodeMode::AfterCredits));
    assert!(saved);
    assert_eq!(next_episode_mode(), NextEpisodeMode::AfterCredits);
    assert_eq!(stored().next_episode_mode(), NextEpisodeMode::AfterCredits);
    assert_eq!(
        events,
        [DiagEvent::FeatureUsed { feature: Feature::NextEpisode(NextEpisodeMode::AfterCredits) }],
        "exactly one"
    );
}

/// Skip interval: as next episode.
#[test]
fn confirm_skip_interval_publishes_and_reports_once_only_on_a_durable_write() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-confirm-skip");
    let _restore = Restore::take();
    defaults();

    let fixture = Unwritable::new(&session);
    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_skip_interval(SkipInterval::Seconds30));
    assert!(!saved);
    assert_eq!(skip_interval(), SkipInterval::Seconds10);
    assert!(events.is_empty(), "{events:?}");
    drop(fixture);
    assert_eq!(stored().skip_interval(), SkipInterval::Seconds10);

    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_skip_interval(SkipInterval::Seconds30));
    assert!(saved);
    assert_eq!(skip_interval(), SkipInterval::Seconds30);
    assert_eq!(stored().skip_interval(), SkipInterval::Seconds30);
    assert_eq!(
        events,
        [DiagEvent::FeatureUsed { feature: Feature::SkipInterval(SkipInterval::Seconds30) }],
        "exactly one"
    );
}

/// Continue Watching press: the same gate, no telemetry.
#[test]
fn confirm_deck_press_publishes_only_a_durable_write() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-confirm-deck");
    let _restore = Restore::take();
    defaults();

    let fixture = Unwritable::new(&session);
    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_deck_press(DeckPress::Play));
    assert!(!saved);
    assert_eq!(deck_press(), DeckPress::Details);
    assert!(events.is_empty(), "{events:?}");
    drop(fixture);
    assert_eq!(stored().deck_press(), DeckPress::Details);

    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_deck_press(DeckPress::Play));
    assert!(saved);
    assert_eq!(deck_press(), DeckPress::Play);
    assert_eq!(stored().deck_press(), DeckPress::Play);
    assert!(events.is_empty(), "{events:?}");
}

/// Settings' default quality (`set_default_quality`): the same gate. The published value is the
/// SUPPORTED rung, and the stored one is `Some(q)`.
#[test]
fn confirm_default_quality_publishes_only_a_durable_write() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-confirm-quality");
    let _restore = Restore::take();
    defaults();

    let fixture = Unwritable::new(&session);
    assert!(!set_default_quality(Quality::P720));
    assert_eq!(quality(), Quality::Original, "a refused write publishes nothing");
    drop(fixture);
    assert_eq!(stored().playback_quality, None, "and the legacy absent field stays absent");

    assert!(set_default_quality(Quality::P720));
    assert_eq!(quality(), Quality::P720);
    assert_eq!(stored().playback_quality, Some(Quality::P720));
}

/// **Quality Confirm writes `Some(q)` even when nothing changed**: from an absent field a pick of
/// Original stores `Some(Original)`, and picking the stored value again still rewrites the record.
#[test]
fn confirm_quality_writes_some_q_even_when_unchanged() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-confirm-quality-unchanged");
    let _restore = Restore::take();
    defaults();
    assert_eq!(stored().playback_quality, None, "a legacy session has no quality field");

    assert!(writes(|| assert!(set_default_quality(Quality::Original))));
    assert_eq!(stored().playback_quality, Some(Quality::Original), "an explicit pick of the default is recorded");

    assert!(writes(|| assert!(set_default_quality(Quality::Original))), "the same pick again still writes");
    assert_eq!(stored().playback_quality, Some(Quality::Original));
}

/// **Delta 6's "before": an unchanged Confirm pick is written again.** (Size and position, the two
/// Optimistic rows that share the habit, are pinned beside them.)
#[test]
fn an_unchanged_confirm_or_size_position_pick_still_writes() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-unchanged-writes");
    let _restore = Restore::take();
    defaults();

    assert!(writes(|| assert!(set_direct_play_mode(DirectPlayMode::Auto))), "direct play");
    assert!(writes(|| assert!(set_next_episode_mode(NextEpisodeMode::Countdown))), "next episode");
    assert!(writes(|| assert!(set_skip_interval(SkipInterval::Seconds10))), "skip interval");
    assert!(writes(|| assert!(set_deck_press(DeckPress::Details))), "deck press");

    let (tx, rx) = mpsc::channel();
    assert!(writes(|| select_subtitle_size(SubtitleSize::Medium, Some(tx))), "size");
    assert!(recv(&rx));
    let (tx, rx) = mpsc::channel();
    assert!(writes(|| select_subtitle_position(SubtitlePosition::Low, Some(tx))), "position");
    assert!(recv(&rx));
}

// =====================================================================================
// 7. Optimistic: publish first, persist second, never republish
// =====================================================================================

/// **The live value is the pick before the write has run.** The storage worker is held at the
/// session lock (`with_io_for_test`), so nothing can have been written when the call returns; the
/// cell already reads the pick and the reply has not come. Released, the write lands.
#[test]
fn optimistic_size_and_position_publish_before_the_write() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-opt-before");
    let _restore = Restore::take();
    defaults();

    let (size_tx, size_rx) = mpsc::channel();
    let (pos_tx, pos_rx) = mpsc::channel();
    session::with_io_for_test(|| {
        select_subtitle_size(SubtitleSize::Large, Some(size_tx));
        select_subtitle_position(SubtitlePosition::High, Some(pos_tx));
        assert_eq!(subtitle_size(), SubtitleSize::Large);
        assert_eq!(subtitle_position(), SubtitlePosition::High);
        assert_eq!(size_rx.try_recv(), Err(mpsc::TryRecvError::Empty), "the write has not run");
        assert_eq!(pos_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    });
    assert!(recv(&size_rx));
    assert!(recv(&pos_rx));
    assert_eq!(stored().subtitle_size, SubtitleSize::Large);
    assert_eq!(stored().subtitle_position, SubtitlePosition::High);
}

/// The same for the three Optimistic rows that take no reply: tone, enhancements, and the player's
/// quality pick (which also leaves the record untouched until the worker runs).
#[test]
fn optimistic_tone_enhancements_and_quality_publish_before_the_write() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-opt-before-silent");
    let _restore = Restore::take();
    defaults();
    let boost = AudioEnhancements { boost_dialog: true, normalize_loudness: false };

    session::with_io_for_test(|| {
        crate::player::set_subtitle_tone(SubtitleTone::Grey);
        crate::player::set_audio_enhancements(boost);
        set_quality_for_retry(Quality::P720);
        assert_eq!(crate::player::subtitle_tone(), SubtitleTone::Grey);
        assert_eq!(crate::player::audio_enhancements(), boost);
        assert_eq!(quality(), Quality::P720);
    });
    plx_base::storage_worker::drain_for_test();
    let after = stored();
    assert_eq!(after.subtitle_tone, SubtitleTone::Grey);
    assert_eq!(after.audio_enhancements, boost);
    assert_eq!(after.playback_quality, Some(Quality::P720));
}

/// **A failed write keeps the viewer's pick, replies `false` and logs the line** (size and
/// position; the line is `"<label>: durable write failed (live value kept for this session)"`).
#[test]
fn optimistic_failure_keeps_the_value_replies_false_and_logs() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-opt-failed");
    let _restore = Restore::take();
    defaults();
    let _fixture = Unwritable::new(&session);
    let mark = LogMark::now();

    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::ExtraLarge, Some(tx));
    assert!(!recv(&rx));
    let (tx, rx) = mpsc::channel();
    select_subtitle_position(SubtitlePosition::High, Some(tx));
    assert!(!recv(&rx));

    assert_eq!(subtitle_size(), SubtitleSize::ExtraLarge, "the live pick stands");
    assert_eq!(subtitle_position(), SubtitlePosition::High);
    let log = mark.since();
    assert!(log.contains(&format!("subtitle size: {WRITE_FAILED}")), "{log}");
    assert!(log.contains(&format!("subtitle position: {WRITE_FAILED}")), "{log}");
}

/// A reply is optional: with `None` a failure still keeps the value and still logs.
#[test]
fn optimistic_failure_without_a_reply_still_keeps_and_logs() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-opt-failed-noreply");
    let _restore = Restore::take();
    defaults();
    let _fixture = Unwritable::new(&session);
    let mark = LogMark::now();

    select_subtitle_size(SubtitleSize::Small, None);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(subtitle_size(), SubtitleSize::Small);
    assert!(mark.since().contains(&format!("subtitle size: {WRITE_FAILED}")));
}

/// **Tone, enhancements and the quality pick fail silently**: the pick stays live, nothing is
/// logged, nothing is republished (plan delta 4's "before"; the record keeps the old value).
#[test]
fn tone_enhancements_and_quality_failures_keep_the_value_and_log_nothing() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-opt-failed-silent");
    let _restore = Restore::take();
    defaults();
    let boost = AudioEnhancements { boost_dialog: true, normalize_loudness: false };
    let fixture = Unwritable::new(&session);
    let mark = LogMark::now();

    crate::player::set_subtitle_tone(SubtitleTone::Grey);
    crate::player::set_audio_enhancements(boost);
    set_quality_for_retry(Quality::P720);
    plx_base::storage_worker::drain_for_test();

    assert_eq!(crate::player::subtitle_tone(), SubtitleTone::Grey);
    assert_eq!(crate::player::audio_enhancements(), boost);
    assert_eq!(quality(), Quality::P720);
    assert!(!mark.since().contains("durable write failed"), "no failure line for these rows: {}", mark.since());
    drop(fixture);
    let after = stored();
    assert_eq!(after.subtitle_tone, SubtitleTone::White, "nothing landed");
    assert_eq!(after.audio_enhancements, AudioEnhancements::NONE);
    assert_eq!(after.playback_quality, None);
}

/// **The write never republishes over a newer pick.** A second pick is made while the first write
/// is held at the session lock; when the first one completes — durably or not — the live value is
/// still the newer pick. Size and position, durable.
#[test]
fn optimistic_size_and_position_never_republish_over_a_newer_pick() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-opt-newer");
    let _restore = Restore::take();
    defaults();

    let (size_tx, size_rx) = mpsc::channel();
    let (pos_tx, pos_rx) = mpsc::channel();
    session::with_io_for_test(|| {
        select_subtitle_size(SubtitleSize::Large, Some(size_tx));
        select_subtitle_position(SubtitlePosition::High, Some(pos_tx));
        restore_subtitle_size(SubtitleSize::Small); // a newer pick, before the worker runs
        restore_subtitle_position(SubtitlePosition::Low);
    });
    assert!(recv(&size_rx));
    assert!(recv(&pos_rx));
    assert_eq!(subtitle_size(), SubtitleSize::Small);
    assert_eq!(subtitle_position(), SubtitlePosition::Low);
    assert_eq!(stored().subtitle_size, SubtitleSize::Large, "the older pick is what the record holds");
}

/// The same when the completing write FAILS: no revert, no republish.
#[test]
fn optimistic_size_never_republishes_over_a_newer_pick_when_the_write_fails() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-opt-newer-failed");
    let _restore = Restore::take();
    defaults();
    let _fixture = Unwritable::new(&session);

    let (tx, rx) = mpsc::channel();
    session::with_io_for_test(|| {
        select_subtitle_size(SubtitleSize::Large, Some(tx));
        restore_subtitle_size(SubtitleSize::Small);
    });
    assert!(!recv(&rx));
    assert_eq!(subtitle_size(), SubtitleSize::Small);
}

/// And for tone, enhancements and the quality pick: a newer live value survives the older write.
#[test]
fn optimistic_tone_enhancements_and_quality_never_republish_over_a_newer_pick() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-opt-newer-silent");
    let _restore = Restore::take();
    defaults();
    let boost = AudioEnhancements { boost_dialog: true, normalize_loudness: false };

    session::with_io_for_test(|| {
        crate::player::set_subtitle_tone(SubtitleTone::Grey);
        crate::player::set_audio_enhancements(boost);
        set_quality_for_retry(Quality::P720);
        crate::player::restore_subtitle_tone(SubtitleTone::Silver); // newer picks
        crate::player::restore_audio_enhancements(AudioEnhancements::NONE);
        restore_quality(Quality::P480);
    });
    plx_base::storage_worker::drain_for_test();
    assert_eq!(crate::player::subtitle_tone(), SubtitleTone::Silver);
    assert_eq!(crate::player::audio_enhancements(), AudioEnhancements::NONE);
    assert_eq!(quality(), Quality::P480);
    let after = stored();
    assert_eq!(after.subtitle_tone, SubtitleTone::Grey, "while the record holds the older ones");
    assert_eq!(after.audio_enhancements, boost);
    assert_eq!(after.playback_quality, Some(Quality::P720));
}

// =====================================================================================
// 8. Unchanged-skip
// =====================================================================================

/// **Tone: a pick equal to the stored tone writes nothing; any other pick writes**, including a
/// return to the default (the key is written at default).
#[test]
fn an_unchanged_tone_pick_is_not_written() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-skip-tone");
    let _restore = Restore::take();
    defaults();

    assert!(!writes(|| crate::player::set_subtitle_tone(SubtitleTone::White)), "stored White, picked White");
    assert!(writes(|| crate::player::set_subtitle_tone(SubtitleTone::Grey)));
    assert_eq!(stored().subtitle_tone, SubtitleTone::Grey);
    assert!(!writes(|| crate::player::set_subtitle_tone(SubtitleTone::Grey)), "stored Grey, picked Grey");
    assert!(writes(|| crate::player::set_subtitle_tone(SubtitleTone::White)), "back to the default writes");
    assert_eq!(stored().subtitle_tone, SubtitleTone::White);
}

/// **Enhancements: the same skip**, compared as the pair of switches.
#[test]
fn an_unchanged_enhancements_pick_is_not_written() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-skip-enh");
    let _restore = Restore::take();
    defaults();
    let boost = AudioEnhancements { boost_dialog: true, normalize_loudness: false };

    assert!(!writes(|| crate::player::set_audio_enhancements(AudioEnhancements::NONE)));
    assert!(writes(|| crate::player::set_audio_enhancements(boost)));
    assert_eq!(stored().audio_enhancements, boost);
    assert!(!writes(|| crate::player::set_audio_enhancements(boost)));
    let both = AudioEnhancements { boost_dialog: true, normalize_loudness: true };
    assert!(writes(|| crate::player::set_audio_enhancements(both)), "a second switch is a change");
    assert_eq!(stored().audio_enhancements, both);
}

/// **The player's quality pick: `Some(q)` already stored means no write; an absent field plus a
/// pick of Original writes `Some(Original)`** (the legacy marker is replaced by an explicit choice).
#[test]
fn the_players_quality_pick_skips_only_a_stored_equal_rung() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-skip-quality");
    let _restore = Restore::take();
    defaults();
    assert_eq!(stored().playback_quality, None);

    assert!(writes(|| set_quality_for_retry(Quality::Original)), "None + Original is a change");
    assert_eq!(stored().playback_quality, Some(Quality::Original));
    assert!(!writes(|| set_quality_for_retry(Quality::Original)), "Some(Original) + Original is not");
    assert!(writes(|| set_quality_for_retry(Quality::P720)));
    assert_eq!(stored().playback_quality, Some(Quality::P720));
    assert!(!writes(|| set_quality_for_retry(Quality::P720)));
    assert_eq!(quality(), Quality::P720, "and the live value is the pick either way");
}

// =====================================================================================
// 9. play_automatically publishes twice
// =====================================================================================

/// **`app::playback::play_automatically` publishes Auto twice** (plan delta 3's "before"): once on
/// the caller's thread (`restore_direct_play_mode(Auto)`), then again from the storage worker when
/// the Confirm write it queues turns out Durable (`set_direct_play_mode(Auto)`).
///
/// The function itself is `pub(crate)` in the app crate and cannot be driven from here (it takes
/// the player adapter, and its worker closure calls `restore_direct_play_mode` off the test thread,
/// which the test lock refuses), so this pins the two ingredients it is made of. The second
/// publish is observable because it is unconditional: a newer cell value written between the two is
/// overwritten with Auto again.
#[test]
fn a_durable_direct_play_write_republishes_over_an_already_published_auto() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("prefs-char-play-automatically");
    let _restore = Restore::take();
    defaults();
    restore_direct_play_mode(DirectPlayMode::Forced);

    restore_direct_play_mode(DirectPlayMode::Auto); // publish 1, on the caller's thread
    assert_eq!(direct_play_mode(), DirectPlayMode::Auto);
    restore_direct_play_mode(DirectPlayMode::Disabled); // something else lands in between
    assert!(set_direct_play_mode(DirectPlayMode::Auto)); // publish 2, the worker's Confirm write
    assert_eq!(direct_play_mode(), DirectPlayMode::Auto, "the second publish overwrote it");
}

/// And when the Confirm half is refused there is no second publish: the first one stands alone.
#[test]
fn a_refused_direct_play_write_does_not_republish_auto() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("prefs-char-play-automatically-refused");
    let _restore = Restore::take();
    defaults();
    restore_direct_play_mode(DirectPlayMode::Forced);
    let _fixture = Unwritable::new(&session);

    restore_direct_play_mode(DirectPlayMode::Auto);
    restore_direct_play_mode(DirectPlayMode::Disabled);
    assert!(!set_direct_play_mode(DirectPlayMode::Auto));
    assert_eq!(direct_play_mode(), DirectPlayMode::Disabled);
}

// =====================================================================================
// 12. Indices and seeds
// =====================================================================================

/// **The in-memory index of every live cell**, read back from the raw atomics (the numbers the
/// future `Packed::to_u8` must reproduce). Out-of-range indices fall back to the documented
/// default of each type, never a neighbouring rung.
#[test]
fn live_cell_indices_are_pinned() {
    use std::sync::atomic::Ordering::Relaxed;
    let _g = plx_base::testlock::serial();
    let _restore = Restore::take();

    for (mode, index) in [(DirectPlayMode::Auto, 0), (DirectPlayMode::Forced, 1), (DirectPlayMode::Disabled, 2)] {
        restore_direct_play_mode(mode);
        assert_eq!(DIRECT_PLAY_MODE.load(Relaxed), index, "{mode:?}");
    }
    for (mode, index) in [(NextEpisodeMode::Countdown, 0), (NextEpisodeMode::AfterCredits, 1), (NextEpisodeMode::Off, 2)] {
        restore_next_episode_mode(mode);
        assert_eq!(NEXT_EPISODE_MODE.load(Relaxed), index, "{mode:?}");
    }
    assert_eq!(NextEpisodeMode::from_index(250), NextEpisodeMode::Countdown, "out of range is the default");
    for (interval, index) in [
        (SkipInterval::Seconds5, 0),
        (SkipInterval::Seconds10, 1),
        (SkipInterval::Seconds15, 2),
        (SkipInterval::Seconds30, 3),
        (SkipInterval::Seconds60, 4),
    ] {
        restore_skip_interval(interval);
        assert_eq!(SKIP_INTERVAL.load(Relaxed), index, "{interval:?}");
    }
    assert_eq!(SkipInterval::from_index(250), SkipInterval::Seconds10);
    for (press, index) in [(DeckPress::Details, 0), (DeckPress::Play, 1)] {
        restore_deck_press(press);
        assert_eq!(DECK_PRESS.load(Relaxed), index, "{press:?}");
    }
    assert_eq!(DeckPress::from_index(250), DeckPress::Details);
    for (size, index) in [
        (SubtitleSize::Small, 0),
        (SubtitleSize::Medium, 1),
        (SubtitleSize::Large, 2),
        (SubtitleSize::ExtraLarge, 3),
    ] {
        restore_subtitle_size(size);
        assert_eq!(SUBTITLE_SIZE.load(Relaxed), index, "{size:?}");
    }
    assert_eq!(SubtitleSize::from_index(250), SubtitleSize::Medium);
    for (position, index) in [(SubtitlePosition::Low, 0), (SubtitlePosition::Middle, 1), (SubtitlePosition::High, 2)] {
        restore_subtitle_position(position);
        assert_eq!(SUBTITLE_POSITION.load(Relaxed), index, "{position:?}");
    }
    assert_eq!(SubtitlePosition::from_index(250), SubtitlePosition::Low);

    // quality: the index is the rung's position in QUALITY_LADDER (Auto first, Original second)
    for (q, index) in [
        (Quality::Auto, 0),
        (Quality::Original, 1),
        (Quality::P1080High, 2),
        (Quality::P1080, 3),
        (Quality::P720, 4),
        (Quality::P720Low, 5),
        (Quality::P480, 6),
    ] {
        restore_quality(q);
        assert_eq!(QUALITY.load(Relaxed), index, "{q:?}");
        assert_eq!(quality(), q);
    }
    assert_eq!(quality_from_index(250), Quality::Original, "out of range is Original");

    // tone: the ladder position; enhancements: the two switches, observed through the getter (the
    // bit values themselves are private to `player`)
    for (tone, index) in [
        (SubtitleTone::White, 0),
        (SubtitleTone::Silver, 1),
        (SubtitleTone::LightGrey, 2),
        (SubtitleTone::Grey, 3),
        (SubtitleTone::DarkGrey, 4),
        (SubtitleTone::Charcoal, 5),
    ] {
        assert_eq!(tone.index(), index, "{tone:?}");
        crate::player::restore_subtitle_tone(tone);
        assert_eq!(crate::player::subtitle_tone(), tone);
    }
    assert_eq!(SubtitleTone::from_index(250), SubtitleTone::White);
    for (boost_dialog, normalize_loudness) in [(false, false), (true, false), (false, true), (true, true)] {
        let a = AudioEnhancements { boost_dialog, normalize_loudness };
        crate::player::restore_audio_enhancements(a);
        assert_eq!(crate::player::audio_enhancements(), a);
    }
}

/// **What a cell reads before anything has restored it** (the seeds): quality Original, skip 10 s,
/// size Medium, everything else the first variant, tone White, enhancements none. The statics are
/// shared by the whole test binary, so this runs in a CHILD process that executes only this test
/// (the pattern of `platform::storage::diagnostics`'s umask test); the parent asserts the child ran
/// and passed.
#[test]
fn unrestored_cells_hold_their_seeds() {
    use std::sync::atomic::Ordering::Relaxed;
    const CHILD_ENV: &str = "PLX_PREFS_CHAR_SEEDS_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        // libtest names a test without its crate: `route::decision::…`.
        let name = format!(
            "{}::unrestored_cells_hold_their_seeds",
            module_path!().split_once("::").unwrap().1
        );
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the child run failed or ran nothing:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    assert_eq!(DIRECT_PLAY_MODE.load(Relaxed), 0);
    assert_eq!(NEXT_EPISODE_MODE.load(Relaxed), 0);
    assert_eq!(SKIP_INTERVAL.load(Relaxed), 1);
    assert_eq!(DECK_PRESS.load(Relaxed), 0);
    assert_eq!(SUBTITLE_SIZE.load(Relaxed), 1);
    assert_eq!(SUBTITLE_POSITION.load(Relaxed), 0);
    assert_eq!(QUALITY.load(Relaxed), 1);
    assert_eq!(quality(), Quality::Original);
    assert_eq!(direct_play_mode(), DirectPlayMode::Auto);
    assert_eq!(next_episode_mode(), NextEpisodeMode::Countdown);
    assert_eq!(skip_interval(), SkipInterval::Seconds10);
    assert_eq!(deck_press(), DeckPress::Details);
    assert_eq!(subtitle_size(), SubtitleSize::Medium);
    assert_eq!(subtitle_position(), SubtitlePosition::Low);
    assert_eq!(crate::player::subtitle_tone(), SubtitleTone::White);
    assert_eq!(crate::player::audio_enhancements(), AudioEnhancements::NONE);
}
