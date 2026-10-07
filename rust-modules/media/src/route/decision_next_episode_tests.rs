//! The Next episode persistence seam: durable-first, one `feature.used` per durable pick.
use super::*;
use plx_telemetry::diag::schema::{DiagEvent, Feature};
use plx_plex::plex::session::NextEpisodeMode;

struct Restore(NextEpisodeMode);
impl Drop for Restore {
    fn drop(&mut self) {
        restore_next_episode_mode(self.0);
    }
}

/// **A durable pick is live, read back by the next load, and reported once** (as the mode alone).
#[test]
fn a_durable_pick_is_live_persisted_and_reported_once() {
    let _g = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("next-episode-set");
    let _restore = Restore(next_episode_mode());
    restore_next_episode_mode(NextEpisodeMode::Countdown);

    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_next_episode_mode(NextEpisodeMode::AfterCredits));
    assert!(saved);
    assert_eq!(next_episode_mode(), NextEpisodeMode::AfterCredits);
    assert_eq!(plx_plex::plex::session::load().next_episode_mode(), NextEpisodeMode::AfterCredits);
    assert_eq!(
        events,
        [DiagEvent::FeatureUsed { feature: Feature::NextEpisode(NextEpisodeMode::AfterCredits) }]
    );

    // choosing the default again removes the key from the file, and is still a (reported) pick
    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_next_episode_mode(NextEpisodeMode::Countdown));
    assert!(saved);
    assert_eq!(plx_plex::plex::session::load().next_episode_mode(), NextEpisodeMode::Countdown);
    assert_eq!(events.len(), 1);
}

/// **A failed write claims nothing**: the live value stays and nothing is reported.
#[test]
fn a_failed_write_changes_and_reports_nothing() {
    let _g = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("next-episode-failed");
    let _restore = Restore(next_episode_mode());
    restore_next_episode_mode(NextEpisodeMode::Countdown);
    // the session file's parent is a regular file, so no write can land
    let dir = std::env::temp_dir().join(format!("plxnative-next-episode-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blocker"), b"x").unwrap();
    plx_plex::plex::session::redirect_for_test(Some(dir.join("blocker").join("auth.json")));

    let (saved, events) = plx_telemetry::diag::test_events::capture(|| set_next_episode_mode(NextEpisodeMode::Off));
    plx_plex::plex::session::redirect_for_test(None);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!saved);
    assert_eq!(next_episode_mode(), NextEpisodeMode::Countdown);
    assert!(events.is_empty(), "{events:?}");
}

/// The Up Next still is a card, and a card's placement history is keyed by the ADDRESS of its
/// thumb path (`plx_ui::widgets::Art::motion_identity`). The screen reads the per-frame
/// [`PlaybackSession::publication`], so a publication that copies the path hands the card a new
/// identity every frame: its placement stays unknown, a still that is not in the cache yet is
/// declined for ever, and each decline asks for another frame — a player screen that never comes
/// to rest (the `site-up-next` screenshot scene timed out on exactly this).
#[test]
fn every_publication_lends_the_same_up_next_thumb() {
    let mut ps = PlaybackSession::IDLE;
    ps.up_next = Some(UpNext { thumb: "/library/metadata/2011003/thumb/1".into(), ..Default::default() }.into());
    let (a, b) = (ps.publication(), ps.publication());
    let (a, b) = (up_next(&a).unwrap(), up_next(&b).unwrap());
    assert_eq!(a.thumb, "/library/metadata/2011003/thumb/1");
    assert_eq!(a.thumb.as_ptr(), b.thumb.as_ptr(), "two frames must see one card identity");
}
