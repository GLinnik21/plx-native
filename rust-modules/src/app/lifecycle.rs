//! The app's side of the foreground/background lifecycle: the loop's adapter for the player's
//! resume machine ([`PlayerForegroundActuator`]) and the names the loop spells for it.
//!
//! The machine itself (`ForegroundLifecycle`, already a tested `State + Event -> Effects`
//! machine), the foreground driver and the transport-pause contract (`paused`,
//! `viewer_paused`, `set_transport_paused`, ...) describe the PLAYER's transport, so they live in
//! [`crate::player::lifecycle`] (module-layer step L12: `player` may not name `app`) and are
//! re-exported here, which keeps every `app` caller spelling the names it always did. Moved out of
//! `app.rs` verbatim in phase 1a (a pure move; `pub(crate)` widening only).

use super::*;

pub(crate) use crate::player::lifecycle::{
    clock_for_suspend_now, drive_foreground, paused, poll_foreground_load, resume_if_paused,
    set_paused, set_transport_paused, transport_target, viewer_paused, ForegroundActivation,
    ForegroundActuator, ForegroundClock, ForegroundInput, ForegroundLifecycle, ForegroundLoadStart,
    ForegroundLoadStatus,
};
// Only `app::run`'s hostsim lifecycle tests name the machine's state, so a re-export outside
// them would be an unused import.
#[cfg(all(test, feature = "hostsim"))]
pub(crate) use crate::player::lifecycle::ForegroundState;

pub(crate) struct PlayerForegroundActuator<'a> {
    /// The native session slot — phase 9's replacement for the `&MainThread` this held, and the
    /// same value every other player call in the loop is threaded (`player::adapter`).
    pub(crate) pa: &'a mut crate::player::adapter::PlayerAdapter,
    pub(crate) repause_at: &'a mut i64,
}

impl ForegroundActuator for PlayerForegroundActuator<'_> {
    type Attempt = crate::route::RouteStartAttempt;

    fn prepare_resume(&mut self, ps: &mut crate::route::PlaybackSession, resume_ns: i64) -> crate::player::ResumeOutcome {
        crate::player::resume_at(ps, resume_ns)
    }

    fn before_load(&mut self, resume_ns: i64, clock: ForegroundClock) {
        if matches!(clock, ForegroundClock::Paused) {
            *self.repause_at = resume_ns;
            set_resume_pend(true);
            crate::player::TX.begin_paused_seek();
        }
    }

    fn start_load(&mut self, ps: &mut crate::route::PlaybackSession) -> ForegroundLoadStart<Self::Attempt> {
        let first = crate::player::start_bufferfeed_tracked(ps, self.pa);
        if matches!(first, crate::player::BufferfeedStartOutcome::Failed) {
            // A synchronous failure inside an Original trial has no Engine for player::pump to
            // recover. Take the same explicit rollback edge here and follow its exact HLS Load.
            return match crate::player::recover_failed_foreground_original(ps, self.pa) {
                crate::player::ForegroundOriginalRecovery::NotOriginal
                | crate::player::ForegroundOriginalRecovery::RetryPrepared => {
                    ForegroundLoadStart::Failed
                }
                crate::player::ForegroundOriginalRecovery::Tracking(attempt) => {
                    ForegroundLoadStart::Launched(attempt)
                }
                crate::player::ForegroundOriginalRecovery::Terminal => {
                    ForegroundLoadStart::Terminal
                }
            };
        }
        match first {
            crate::player::BufferfeedStartOutcome::AlreadyRunning => {
                ForegroundLoadStart::AlreadyRunning
            }
            crate::player::BufferfeedStartOutcome::Launched(attempt) => {
                ForegroundLoadStart::Launched(attempt)
            }
            crate::player::BufferfeedStartOutcome::Failed => unreachable!("handled above"),
        }
    }

    fn load_status(&mut self, attempt: Self::Attempt) -> ForegroundLoadStatus<Self::Attempt> {
        match crate::route::route_start_status(attempt) {
            crate::route::RouteStartStatus::Pending => ForegroundLoadStatus::Pending,
            crate::route::RouteStartStatus::Started => ForegroundLoadStatus::Started,
            crate::route::RouteStartStatus::Failed => ForegroundLoadStatus::Failed,
            crate::route::RouteStartStatus::Superseded(replacement) => {
                ForegroundLoadStatus::Superseded(replacement)
            }
            crate::route::RouteStartStatus::Stale => ForegroundLoadStatus::Stale,
        }
    }

    fn after_load(
        &mut self,
        ps: &mut crate::route::PlaybackSession,
        attempt: Option<Self::Attempt>,
        clock: ForegroundClock,
        started: bool,
    ) {
        if matches!(clock, ForegroundClock::Paused) {
            if started {
                set_resume_pend(true);
            } else {
                crate::player::TX.finish_seek_preroll();
            }
        }
        if !started && attempt.is_some() {
            // `sf_load == 0` publishes Error but intentionally leaves the Engine available for
            // diagnostics. Retire it only if it still owns the observed token: player::pump may
            // already have launched a healthy replacement before foreground polls this result.
            let _ = crate::player::suspend_bufferfeed_if_attempt(ps, self.pa, attempt.unwrap());
        }
    }

    fn play_clock(&mut self) -> bool {
        // start_bufferfeed has installed the Initial native-clock hold from TX.paused. Publish
        // Play through the synchronized reducer so that hold and the feed gate reopen together.
        !paused() || set_transport_paused(self.pa, false)
    }
}

// D8 (UI restructure phase 12): relocated verbatim from `app/mod.rs`'s `root_back_tests`. The
// functions under test (`input::back_at_root`/`input::after_cancel`/`input::AfterCancel`) stay in
// `app/input.rs` — only the TEST is moved, which D8 asks for by name; grouping it beside the
// foreground/background lifecycle adapter above is the judgment call `app::App` reaching "the
// television took the screen back" (`back_at_root`) and "the app keeps running through this" are
// both properties of the app's OWN outer lifecycle, in the sense this module's other half already
// covers (its machine and their tests now live in `player::lifecycle`), even though today's
// `ForegroundLifecycle` type itself does not cover them.
#[cfg(test)]
mod root_back_tests {
    //! **BACK at a ROOT hands the screen back to the television, and the app keeps running.**
    //!
    //! [`back_at_root`] is driven for real — it is the app's whole answer to "there is nowhere
    //! further back to go", and the regression to catch is a future edit putting `running = false`,
    //! or a modal question, back where the platform call now goes. [`after_cancel`] is pure,
    //! because its callers reach `auth`/`webos`, neither of which a unit test wants to drive.
    //!
    //! **Phase 6 retired the other half this module doc used to describe** — `onboarding_back`,
    //! `OnboardBack` and their five tests, which pinned issues #16-#18's rule as it was reached from
    //! the legacy `key_onboarding` key ladder. The RULE did not change (the Session adapter
    //! performs the `after_cancel` policy those tests exercised); what moved is WHO decides "is this press the screen's own
    //! modal or the app's root" — since phase 6 that is `screens::login`/`screens::profiles`'s own
    //! job, reading their own focus-engine state (a PIN pad's open flag, on the owned
    //! `ProfilesScreen`) that this module cannot see and must not reach into (`app/` never names a
    //! sibling `screens/` module's internals). A host test of that half now belongs beside the
    //! screens that make the decision, not here.
    //!
    //! What NO host test can say is that the television actually shows its launcher and that the
    //! process survives it. That is `webos::go_home`'s device half — `gohome: SAM accepted`, a
    //! capture of the launcher (on webOS 4 a RIBBON over the still-running app, so no lifecycle
    //! event at all) and `fuser` reporting one pid throughout — and it is why this file's
    //! `home_requests` counter grades the DECISION and never the outcome.
    use super::*;

    /// **The one that matters (issue #16).** The root press asks the platform for its Home screen.
    ///
    /// Observed RED against the shipped `back_at_root`, which raised the "Exit PlxNative?" alert
    /// and asked webOS for nothing: `left: 0, right: 1`.
    #[test]
    fn back_at_home_root_shows_the_platform_home() {
        let _g = crate::testlock::serial();
        crate::webos::release_root_press();
        let before = crate::webos::home_requests();
        back_at_root();
        assert_eq!(
            crate::webos::home_requests(),
            before + 1,
            "BACK at Home's root must ask webOS for its Home screen"
        );
        crate::webos::release_root_press();
    }

    /// **A refused root BACK leaves the sign-in it refused to leave RUNNING, and asks for the
    /// television's Home.** This branch used to restart the flow first (`RestartAndHome`), because
    /// `auth::cancel` invalidated the worker before it decided; that ordering is gone (issue #30,
    /// `auth::a_refused_back_leaves_the_live_pin_poll_running`), and a restart on top of a live
    /// poll would mint a fresh code over one the user's phone may already have answered. Observed
    /// RED against the shipped `after_cancel`, which answered `RestartAndHome` for `Waiting`.
    #[test]
    fn a_root_back_out_of_a_running_sign_in_leaves_it_running() {
        assert_eq!(
            after_cancel(false),
            AfterCancel::Home,
            "nothing was disturbed, so there is nothing to restart — go to the television's Home"
        );
    }

    /// A cancel that SUCCEEDED went somewhere inside the app: nothing to ask the platform for, and
    /// the claim goes back so the real root BACK a moment later is not swallowed.
    #[test]
    fn a_cancel_that_backed_out_asks_the_platform_for_nothing() {
        assert_eq!(after_cancel(true), AfterCancel::BackedOut);
    }

    fn back_input(ms: u32) -> crate::ui::machine::InputEvent<u32> {
        crate::ui::machine::InputEvent {
            at: crate::ui::machine::Tick { ms, dt_us: 16_000 },
            source: crate::ui::machine::Source::Script,
            kind: crate::ui::machine::InputKind::Key {
                key: crate::ui::machine::Key::Back, sym: 0, wcode: 0,
                edge: crate::ui::machine::Edge::Down, at_edge: false,
            },
        }
    }

    /// A real Home instance owns its root decision and emits exactly the request whose production
    /// performer is `back_at_root`. EntryId/InstanceId prove the root was neither replaced nor
    /// torn down, and the container's root fallback is not accidentally entered a second time.
    #[test]
    fn owned_home_root_back_reaches_platform_home_without_moving_the_root() {
        let _guard = crate::testlock::serial();
        crate::webos::release_root_press();
        let before = crate::webos::home_requests();
        let mut d = crate::ui::dispatch::Dispatcher::<crate::app::bridge::AppHost>::new();
        let mut rig = crate::app::bridge::Bridge::for_test(|| 0);
        crate::app::bridge::nav_root(&mut d, crate::screens::registry::AppArg::Home);
        crate::app::bridge::frame(&mut d, &mut rig, crate::ui::machine::Tick::default(), vec![]);
        let entry = d.nav.top_page().expect("Home root").id;
        let instance = d.nav.instance_of(entry).expect("owned Home body");
        assert!(d.top_screen().unwrap().as_any().unwrap()
            .is::<crate::screens::home::HomeScreen>());

        let (_, report) = crate::app::bridge::frame(&mut d, &mut rig,
            crate::ui::machine::Tick { ms: 16, dt_us: 16_000 }, vec![back_input(16)]);
        assert!(!report.back_at_root, "Home answered its own root BACK exactly once");
        let requests = rig.take_reqs();
        assert!(matches!(requests.as_slice(),
            [crate::screens::registry::LoopReq::BackAtRoot]));
        for request in requests {
            assert!(crate::app::run::reduce_navigation_request(request, &mut d).is_ok());
        }
        assert_eq!(crate::webos::home_requests(), before + 1);
        assert_eq!(d.nav.top_page().map(|page| page.id), Some(entry));
        assert_eq!(d.nav.instance_of(entry), Some(instance));
        crate::webos::release_root_press();
    }

    /// A BACK on an actual non-root Detail page is the page's typed `ContentReq::Back`, never the
    /// platform-root request. This pins the boundary the deleted route-wide classifier guarded
    /// without recreating its alphabet.
    #[test]
    fn nonroot_owned_page_back_never_reaches_platform_home() {
        let _guard = crate::testlock::serial();
        crate::webos::release_root_press();
        let before = crate::webos::home_requests();
        let mut d = crate::ui::dispatch::Dispatcher::<crate::app::bridge::AppHost>::new();
        let mut rig = crate::app::bridge::Bridge::for_test(|| 0);
        crate::app::bridge::nav_root(&mut d, crate::screens::registry::AppArg::Home);
        crate::app::bridge::frame(&mut d, &mut rig, crate::ui::machine::Tick::default(), vec![]);
        crate::app::bridge::nav_push(&mut d, crate::screens::registry::AppArg::Content(
            crate::screens::registry::ContentArg::Detail {
                sid: crate::plex::ServerId::UNSET, rk: "nonroot-back".into(),
            }));
        crate::app::bridge::frame(&mut d, &mut rig,
            crate::ui::machine::Tick { ms: 16, dt_us: 16_000 }, vec![]);
        let detail_entry = d.nav.top_page().expect("Detail page").id;
        let detail_instance = d.nav.instance_of(detail_entry).expect("owned Detail body");

        let (_, report) = crate::app::bridge::frame(&mut d, &mut rig,
            crate::ui::machine::Tick { ms: 32, dt_us: 16_000 }, vec![back_input(32)]);
        assert!(!report.back_at_root);
        assert!(rig.take_reqs().iter().all(|req|
            !matches!(req, crate::screens::registry::LoopReq::BackAtRoot)));
        assert!(rig.take_content_reqs().iter().any(|(from, req, _)| matches!(
            (from, req),
            (crate::ui::machine::MachineId::Instance(instance),
                crate::screens::registry::ContentReq::Back) if *instance == detail_instance
        )));
        assert_eq!(crate::webos::home_requests(), before);
        assert_eq!(d.nav.instance_of(detail_entry), Some(detail_instance));
        crate::webos::release_root_press();
    }

    /// First-run BACK starts as a request from the real `OnboardScreen`, then runs the exact
    /// production request reducer called by `app/run.rs::loop_requests` and mounts real Profiles.
    /// The emitted enum stays live across the actual match arm; there is no copied route oracle.
    #[test]
    fn onboard_back_request_mounts_the_owned_profiles_screen() {
        let _guard = crate::testlock::serial();
        let mut d = crate::ui::dispatch::Dispatcher::<crate::app::bridge::AppHost>::new();
        let mut rig = crate::app::bridge::Bridge::for_test(|| 0);
        crate::app::bridge::nav_root(&mut d, crate::screens::registry::AppArg::Onboard);
        crate::app::bridge::frame(&mut d, &mut rig, crate::ui::machine::Tick::default(), vec![]);
        let onboard_entry = d.nav.top_page().expect("Onboard root").id;
        let onboard_instance = d.nav.instance_of(onboard_entry).expect("owned Onboard body");
        assert!(matches!(d.top_arg(), Some(crate::screens::registry::AppArg::Onboard)));
        assert_eq!(d.top_screen().unwrap().name(), "onboard");

        let (_, report) = crate::app::bridge::frame(&mut d, &mut rig,
            crate::ui::machine::Tick { ms: 16, dt_us: 16_000 }, vec![back_input(16)]);
        assert!(!report.back_at_root, "Onboard has an in-app destination");
        let requests = rig.take_reqs();
        assert!(matches!(requests.as_slice(),
            [crate::screens::registry::LoopReq::OnboardBack]));

        for request in requests {
            assert!(crate::app::run::reduce_navigation_request(request, &mut d).is_ok());
        }
        crate::app::bridge::frame(&mut d, &mut rig,
            crate::ui::machine::Tick { ms: 32, dt_us: 16_000 }, vec![]);
        assert!(matches!(d.top_arg(), Some(crate::screens::registry::AppArg::Profiles)));
        assert_eq!(d.top_screen().unwrap().name(), "profiles");
        assert_ne!(d.nav.instance_of(d.nav.top_page().unwrap().id), Some(onboard_instance));
    }
}
