//! Live executor for Settings preference effects. The bridge's controlled-IO guard must admit
//! the effect before this entrypoint: profile capture, thread admission and persistence all live
//! here, never in a screen constructor or step.
mod marker_access;
pub(crate) use marker_access::MarkerAccessRunner;
use plx_media::route::{choose, Choice, How};
use plx_plex::plex::account::{PreferenceError, PreferenceRequest};
use plx_plex::plex::session::{IntroSkipPref, CreditsSkipPref, DolbyVisionModePref, InstantDpadSeekingPref, UriEnginePref, DeckPressPref, DirectPlayModePref, HdrOverlayBrightnessPref, NextEpisodeModePref, QualityPref,
    SkipIntervalPref, SubtitlePositionPref, SubtitleSizePref, SubtitleTonePref, TrailerAutoplayPref, HudEndTimePref, UnderPostersPref};
use plx_screens::registry::{AccountPreferenceReply, PreferenceCmd};
use std::sync::mpsc::Sender;

/// The one place a Settings pick meets the write door: each row's own `SETTINGS` policy
/// (`How::settings`) decides whether the pick is confirmed first or published at once.
fn choose_setting(choice: Choice, reply: Sender<bool>) {
    match choice {
        Choice::IntroSkip(v) => choose::<IntroSkipPref>(v, How::settings::<IntroSkipPref>(reply)),
        Choice::CreditsSkip(v) => choose::<CreditsSkipPref>(v, How::settings::<CreditsSkipPref>(reply)),
        Choice::Quality(v) => choose::<QualityPref>(v, How::settings::<QualityPref>(reply)),
        Choice::DirectPlay(v) => choose::<DirectPlayModePref>(v, How::settings::<DirectPlayModePref>(reply)),
        Choice::NextEpisode(v) => choose::<NextEpisodeModePref>(v, How::settings::<NextEpisodeModePref>(reply)),
        Choice::InstantDpadSeeking(v) => choose::<InstantDpadSeekingPref>(v, How::settings::<InstantDpadSeekingPref>(reply)),
        Choice::UriEngine(v) => choose::<UriEnginePref>(v, How::settings::<UriEnginePref>(reply)),
        Choice::DolbyVision(v) => choose::<DolbyVisionModePref>(v, How::settings::<DolbyVisionModePref>(reply)),
        Choice::SkipInterval(v) => choose::<SkipIntervalPref>(v, How::settings::<SkipIntervalPref>(reply)),
        Choice::DeckPress(v) => choose::<DeckPressPref>(v, How::settings::<DeckPressPref>(reply)),
        Choice::UnderPosters(v) => choose::<UnderPostersPref>(v, How::settings::<UnderPostersPref>(reply)),
        Choice::TrailerAutoplay(v) => choose::<TrailerAutoplayPref>(v, How::settings::<TrailerAutoplayPref>(reply)),
        Choice::HudEndTime(v) => choose::<HudEndTimePref>(v, How::settings::<HudEndTimePref>(reply)),
        Choice::SubtitleSize(v) => choose::<SubtitleSizePref>(v, How::settings::<SubtitleSizePref>(reply)),
        Choice::SubtitlePosition(v) => choose::<SubtitlePositionPref>(v, How::settings::<SubtitlePositionPref>(reply)),
        Choice::SubtitleTone(v) => choose::<SubtitleTonePref>(v, How::settings::<SubtitleTonePref>(reply)),
        Choice::HdrOverlayBrightness(v) => choose::<HdrOverlayBrightnessPref>(v, How::settings::<HdrOverlayBrightnessPref>(reply)),
    }
}

pub(super) fn execute(command: PreferenceCmd) {
    match command {
        // These commands need the Bridge's admitted, profile-scoped feature publication.
        PreferenceCmd::EnsureMarkerAccess => {}
        PreferenceCmd::ChooseMarker { reply, .. } => { let _ = reply.send(false); }
        PreferenceCmd::Choose(Choice::IntroSkip(_) | Choice::CreditsSkip(_), reply) => { let _ = reply.send(false); }
        PreferenceCmd::Load { reply } => {
            let Some(request) = PreferenceRequest::capture() else {
                let _ = reply.send(AccountPreferenceReply {
                    request: None, outcome: Err(PreferenceError::Unavailable),
                });

                return;
            };
            // A refused spawn drops the reply sender. The screen reports its disconnected
            // receipt as retryable without claiming that an account call ran.
            plx_base::task::spawn_small("acct-prefs", move || {
                let outcome = request.load();
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::present::wake_from_worker();
            });
        }
        PreferenceCmd::Save { request, base, update, reply } => {
            plx_base::task::spawn_small("acct-prefs", move || {
                let outcome = request.save(&base, update);
                let _ = reply.send(AccountPreferenceReply { request: Some(request), outcome });
                plx_machine::present::wake_from_worker();
            });
        }
        // Every live setting saves through the one write door, in `choose_setting`.
        PreferenceCmd::Choose(choice, reply) => choose_setting(choice, reply),
        PreferenceCmd::AutoSignIn { on, reply } => {
            let expected = plx_plex::plex::session::peek();
            let tenure = plx_plex::plex::session::tenure();
            let _ = plx_base::storage_worker::submit_retained(move || {
                let saved = plx_plex::plex::session::update_guarded_with_outcome(|current| {
                    if current.client_id != expected.client_id || current.account_token != expected.account_token { return None; }
                    (current.auto_sign_in() != on).then(|| current.with_auto_sign_in(on))
                }, || plx_plex::plex::session::tenure() == tenure).is_some_and(|write| write.outcome.persisted());
                let _ = reply.send(saved);
                plx_machine::present::wake_from_worker();
            });
        }
        PreferenceCmd::UiResolution { mode, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_ui_resolution(mode));
                plx_machine::present::wake_from_worker();
            });
        }
        PreferenceCmd::Language { language, reply } => {
            let _ = plx_base::storage_worker::submit_retained(move || {
                let _ = reply.send(plx_plex::plex::session::set_language(language));
                plx_machine::present::wake_from_worker();
            });
        }
    }
}

pub(super) fn execute_marker(command: PreferenceCmd, view: plx_appkit::skip_policy::MarkerPrefsView) {
    if let PreferenceCmd::ChooseMarker { kind, mode, scope, reply } = command {
        if scope == 0 || scope != view.scope || !view.granted(kind) {
            let _ = reply.send(false); return;
        }
        choose_setting(match kind { plx_data::metadata::MarkerKind::Intro => Choice::IntroSkip(mode),
            plx_data::metadata::MarkerKind::Credits => Choice::CreditsSkip(mode) }, reply);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use plx_plex::plex::session::{self, Session};
    use std::sync::mpsc::{self, Sender, TryRecvError};

    struct FullStorageQueue(Option<Sender<()>>);
    impl FullStorageQueue {
        fn new() -> Self {
            plx_base::storage_worker::drain_for_test();
            let (release, held) = mpsc::channel();
            let (started, entered) = mpsc::channel();
            let guard = Self(Some(release));
            let _ = plx_base::storage_worker::submit(move || {
                started.send(()).unwrap();
                let _ = held.recv();
            }).unwrap();
            entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            for _ in 0..plx_base::storage_worker::CAPACITY {
                let _ = plx_base::storage_worker::submit(|| ()).unwrap();
            }
            assert!(matches!(plx_base::storage_worker::submit(|| ()),
                Err(plx_base::storage_worker::SubmitError::Full)));
            guard
        }
    }
    impl Drop for FullStorageQueue {
        fn drop(&mut self) {
            if let Some(release) = self.0.take() { let _ = release.send(()); }
            plx_base::storage_worker::drain_for_test();
        }
    }

    fn account() -> Session {
        Session { client_id: "preference-client".into(), account_token: "synthetic-account".into(),
            ..Default::default() }.with_auto_sign_in(false)
    }

    #[test]
    fn auto_sign_in_retains_a_full_worker_queue_edit() {
        let _serial = plx_base::testlock::serial();
        let _session = session::TempSession::new("app-auto-sign-in-full");
        session::save(&account());
        let queue = FullStorageQueue::new();
        let (reply, answer) = mpsc::channel();
        execute(PreferenceCmd::AutoSignIn { on: true, reply });
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty), "backpressure is not a terminal refusal");
        assert!(!session::peek().auto_sign_in());
        drop(queue);
        assert_eq!(answer.try_recv(), Ok(true));
        assert_eq!(answer.try_recv(), Err(TryRecvError::Disconnected), "exactly one terminal reply");
        session::invalidate_for_test();
        assert!(session::load().auto_sign_in(), "the actual preference command persisted its edit");
    }

    #[test]
    fn auto_sign_in_retained_edit_cannot_write_after_an_account_change() {
        let _serial = plx_base::testlock::serial();
        let _session = session::TempSession::new("app-auto-sign-in-stale");
        session::save(&account());
        let queue = FullStorageQueue::new();
        let (reply, answer) = mpsc::channel();
        execute(PreferenceCmd::AutoSignIn { on: true, reply });
        let mut replacement = account();
        replacement.account_token = "replacement-account".into();
        session::save(&replacement);
        let before = std::fs::read(_session.path()).unwrap();
        drop(queue);
        assert_eq!(answer.try_recv(), Ok(false));
        assert_eq!(answer.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(std::fs::read(_session.path()).unwrap(), before, "stale work writes nothing");
        assert!(!session::peek().auto_sign_in());
    }

    #[test]
    fn browse_preferences_retains_a_full_worker_queue_edit() {
        use plx_data::stores::browse::{BrowseCmd, BrowseStore, SecKind};
        use plx_data::stores::StoreJob;
        use plx_machine::job::Port;
        let _serial = plx_base::testlock::serial();
        let _session = session::TempSession::new("app-browse-preferences-full");
        session::save(&account());
        let mut store = BrowseStore::default();
        store.seed_pins_for_test(&[false]);
        assert!(store.run_for_test(BrowseCmd::ApplyPins(vec![(0, true)])));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1);
        let StoreJob::BrowsePreferences(job, port) = jobs.remove(0) else { panic!("preference job") };
        let addr = job.addr;
        let observer = port.clone();
        let queue = FullStorageQueue::new();
        crate::app::adapters::stores::StoreRunner::live().start(StoreJob::BrowsePreferences(job, port));
        assert!(session::peek().pins_for("").is_none());
        drop(queue);
        session::invalidate_for_test();
        let mut persisted = session::load();
        let pins = persisted.pins_for("").expect("the live runner persisted the retained edit");
        assert!(pins.asked);
        assert_eq!(pins.on.len(), 1);
        assert_eq!(pins.on[0].machine_id, "mach-test");
        assert_eq!(pins.on[0].key, 1);
        // A terminal receipt cannot be overwritten by a later addressed refusal.
        observer.refused(addr);
        observer.done(addr, plx_data::browse::preferences_job::WriteOutcome::Finished(false));
        store.observe_pin_write();
        let mut newer = pins.clone();
        newer.off = std::mem::take(&mut newer.on);
        persisted.set_pins_for("", newer);
        session::save(&persisted);
        store.append_section_for_test(0, 2, "Other", SecKind::Movie);
        assert!(!store.pinned_for_test(0), "one successful terminal releases the optimistic pin overlay");
        assert!(store.take_jobs().is_empty(), "no duplicate preference job");
    }

    #[test]
    fn browse_preferences_retained_edit_cannot_write_after_an_account_change() {
        use plx_data::stores::browse::{BrowseCmd, BrowseStore};
        let _serial = plx_base::testlock::serial();
        let _session = session::TempSession::new("app-browse-preferences-stale");
        session::save(&account());
        let mut store = BrowseStore::default();
        store.seed_pins_for_test(&[false]);
        assert!(store.run_for_test(BrowseCmd::ApplyPins(vec![(0, true)])));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1);
        let queue = FullStorageQueue::new();
        crate::app::adapters::stores::StoreRunner::live().start(jobs.remove(0));
        let mut replacement = account();
        replacement.account_token = "replacement-account".into();
        session::save(&replacement);
        let before = std::fs::read(_session.path()).unwrap();
        drop(queue);
        assert_eq!(std::fs::read(_session.path()).unwrap(), before, "stale work writes nothing");
        assert!(session::peek().pins_for("").is_none());
    }
}
