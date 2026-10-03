//! The Skip interval persistence seam: durable-first, one `feature.used` per durable pick.
use super::*;
use crate::diag::schema::{DiagEvent, Feature};
use crate::plex::session::SkipInterval;

struct Restore(SkipInterval);
impl Drop for Restore {
    fn drop(&mut self) {
        restore_skip_interval(self.0);
    }
}

/// **A durable pick is live, read back by the next load, and reported once** (as the length alone).
#[test]
fn a_durable_pick_is_live_persisted_and_reported_once() {
    let _g = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("skip-interval-set");
    let _restore = Restore(skip_interval());
    restore_skip_interval(SkipInterval::Seconds10);

    let (saved, events) = crate::diag::test_events::capture(|| set_skip_interval(SkipInterval::Seconds30));
    assert!(saved);
    assert_eq!(skip_interval(), SkipInterval::Seconds30);
    assert_eq!(crate::plex::session::load().skip_interval(), SkipInterval::Seconds30);
    assert_eq!(
        events,
        [DiagEvent::FeatureUsed { feature: Feature::SkipInterval(SkipInterval::Seconds30) }]
    );

    // choosing the default again removes the key from the file, and is still a (reported) pick
    let (saved, events) = crate::diag::test_events::capture(|| set_skip_interval(SkipInterval::Seconds10));
    assert!(saved);
    assert_eq!(crate::plex::session::load().skip_interval(), SkipInterval::Seconds10);
    assert_eq!(events.len(), 1);
}

/// **A failed write claims nothing**: the live value stays and nothing is reported.
#[test]
fn a_failed_write_changes_and_reports_nothing() {
    let _g = plx_base::testlock::serial();
    let _session = crate::plex::session::TempSession::new("skip-interval-failed");
    let _restore = Restore(skip_interval());
    restore_skip_interval(SkipInterval::Seconds10);
    // the session file's parent is a regular file, so no write can land
    let dir = std::env::temp_dir().join(format!("plxnative-skip-interval-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("blocker"), b"x").unwrap();
    crate::plex::session::redirect_for_test(Some(dir.join("blocker").join("auth.json")));

    let (saved, events) = crate::diag::test_events::capture(|| set_skip_interval(SkipInterval::Seconds60));
    crate::plex::session::redirect_for_test(None);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!saved);
    assert_eq!(skip_interval(), SkipInterval::Seconds10);
    assert!(events.is_empty(), "{events:?}");
}
