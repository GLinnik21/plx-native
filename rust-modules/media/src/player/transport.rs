//! The player's transport-pause contract: `paused`, `viewer_paused`, `set_transport_paused`,
//! `commit_viewer_resume_for_load`, `resume_if_paused` and the pure `transport_target` are the one
//! door every surface reaches the player's pause through. The HUD and key surfaces, the detail
//! page's trailer transport ([`super::preview::transport`]), the claim hold ([`super::claim_hold`])
//! and the app-switch actuator all name them. They read and write `FEED_TX` and call
//! `player::pause` / `player::resume`, so this is the transport's IO door and lives in the IO layer
//! on purpose (docs/sans-io.md, Scope): the foreground machine ([`super::lifecycle`]) reads none of
//! it and takes the pause as a value on its events (`ForegroundEvent::Park { viewer_paused, .. }`).
//! `app::lifecycle` re-exports the names, so app code spells them as it always did.
//!
//! Moved out of `lifecycle.rs` verbatim in the Sans-IO lifecycle PR B (a pure move).

// transport state — was the C playback globals; now the session's readout (the feed keeps its own
// copy to gate feeding, written by the matching commands)
#[inline]
pub fn paused(ps: &crate::route::PlaybackSession) -> bool {
    ps.readout().paused()
}
/// Publish the viewer's Play for a restore that is about to Load with no Engine. A park keeps the
/// transport's Pause across the reload (`TX::reset_for_reload`), and the Load arms its initial clock
/// hold from it (`engine::ClockStartGuard`): a paused save restored PLAYING would otherwise start
/// under the viewer's pause, whose hold fences the Load's first Play behind a prime and leaves the
/// real Play to the late `PlayClock`. There is nothing native to resume yet, so this is the commit
/// half of [`set_transport_paused`] alone; the new Load's own first Play is the native half. The
/// caller guarantees there is no Engine (`PlayerForegroundActuator::before_load` checks
/// `is_live`): over a live one this would leave its pause standing while the transport reads Playing.
pub fn commit_viewer_resume_for_load(ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter) {
    crate::player::claim_hold::note_user_transport();
    crate::player::commit_paused(ps, pa, false);
}
/// PURE: the paused value a transport request settles on. `Some(true)` is the remote's PLAY key,
/// `Some(false)` its PAUSE, and `None` the single PLAYPAUSE toggle — which is the only one of the
/// three that needs the current state at all.
///
/// One function rather than a `match` per caller: the player's `PlayerReq::Transport` and the
/// detail page's full-trailer transport (`player::preview::transport`) answer the same key ladder,
/// and two hand-spelled copies of a three-arm match is exactly how a PLAY key ends up toggling on
/// one surface and resuming on the other.
pub fn transport_target(play: Option<bool>, paused: bool) -> bool {
    match play {
        Some(true) => false,
        Some(false) => true,
        None => !paused,
    }
}

/// The transport state the VIEWER sees and their next press toggles.
///
/// A claim's presentation hold (`player::claim_hold`) pauses the stream too, but that pause is the
/// hold's loan, not the viewer's: while it stands the viewer sees a spinner over a playing intent,
/// so the toggle means Pause (their intent, which the hold's restore then honours) and a seek's
/// resume leaves the stream held until the landing.
pub fn viewer_paused(ps: &crate::route::PlaybackSession) -> bool {
    paused(ps) && !crate::player::claim_hold::owns_pause()
}

/// During common startup, the viewer's desired pause can differ from the native pause hold.
/// Read the addressed goal first so a toggle never mistakes that hold for a viewer Pause.
pub fn viewer_paused_for(ps: &crate::route::PlaybackSession,
    pa: &crate::player::adapter::PlayerAdapter) -> bool {
    if let Some(goal) = pa.backend_viewer_paused() { return goal; }
    ps.backends().feed && viewer_paused(ps)
}

/// Ask the synchronized player clock to commit a user Pause/Resume. The player publishes the feed
/// gate at the same accepted native boundary; keeping a second commit here used to leave a window
/// in which deadline accounting still treated an already-accepted Pause as active playback.
/// Resume during an internal HLS hold is accepted as a deferred transition: feeding reopens, while
/// measured re-prime owns both the eventual Starfish Play and the matching ACB Resume.
pub fn set_transport_paused(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter,
    value: bool,
) -> bool {
    // With no Feed composed the only transport is the addressed attempt's: none running, nothing to ask.
    let feed = ps.backends().feed;
    if !feed && !matches!(pa.backend_snapshot().phase,
        plx_playback_contract::Phase::Starting | plx_playback_contract::Phase::Active) {
        return false;
    }
    // PLAY while a hold owns the pause: the viewer sees a spinner over a playing intent, so Play is
    // already satisfied. Resuming here would run the OLD stream mid-flight and replay its seconds
    // after the landing, which still owes the play-back. Not a press that outranks the hold.
    if feed && !value && crate::player::claim_hold::owns_pause() {
        return true;
    }
    // A viewer press outranks a claim's presentation hold (`player::claim_hold`), including the
    // press that finds the transport already in the state it asks for.
    if feed {
        crate::player::claim_hold::note_user_transport();
    }
    if viewer_paused_for(ps, pa) == value {
        return true;
    }
    if let Some(accepted) = pa.backend_transport_control(ps, value) {
        return accepted;
    }
    if !feed { return false; }
    if value { crate::player::pause(ps, pa) } else { crate::player::resume(ps, pa) }
}

/// Resume if a seek landed while paused. Its stay-paused counterpart is the pump's
/// `settle_paused_seek`, which re-pauses a paused-seek preroll the pump itself opened;
/// `app::playback::commit_seek` is a bare seek and holds nothing. Written out four separate times
/// in `app::playback` before it had a name; it came from `app/playback.rs` (module-layer step L12
/// moved it into `lifecycle.rs`, and the Sans-IO lifecycle PR B moved it on, here).
pub fn resume_if_paused(ps: &mut crate::route::PlaybackSession,
    pa: &mut crate::player::adapter::PlayerAdapter) {
    if viewer_paused_for(ps, pa) {
        set_transport_paused(ps, pa, false);
    }
}

#[cfg(test)]
mod transport_target_tests {
    /// The three keys, both ways round. A PLAY key on an already-playing transport is a no-op
    /// rather than a pause, and a PAUSE key on an already-paused one likewise — the property a
    /// per-caller `match` kept getting wrong, and the reason the player's transport and the detail
    /// page's trailer transport share this one function.
    #[test]
    fn only_the_toggle_depends_on_the_current_state() {
        for paused in [false, true] {
            assert!(!super::transport_target(Some(true), paused), "PLAY plays");
            assert!(super::transport_target(Some(false), paused), "PAUSE pauses");
        }
        assert!(super::transport_target(None, false), "PLAYPAUSE pauses a playing transport");
        assert!(!super::transport_target(None, true), "…and resumes a paused one");
    }
}

#[cfg(test)]
mod viewer_resume_for_load_tests {
    use plx_play_feed::shared::FEED_TX;
    use std::sync::atomic::Ordering;

    /// **A restore that Loads Playing starts with the viewer's pause gone.** The park leaves
    /// `FEED_TX.paused` standing and the Load arms its clock hold from it, so the helper must clear it
    /// with no Engine in hand, and must not open the paused-seek preroll on the way.
    #[test]
    fn commit_viewer_resume_for_load_clears_the_standing_pause() {
        let _guard = plx_base::testlock::serial();
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_TX.commit_paused(self.0);
            }
        }
        let _restore = Restore(FEED_TX.paused.load(Ordering::Acquire));
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::player::commit_paused(&mut ps, &mut pa, true);
        assert!(super::paused(&ps), "the premise: a Paused transport");

        super::commit_viewer_resume_for_load(&mut ps, &mut pa);
        assert!(!super::paused(&ps), "the viewer's Play is published");
        assert!(!super::viewer_paused(&ps));
        assert!(!FEED_TX.seek_preroll_active(), "no preroll opens for a Playing Load");
        crate::player::assert_transport_copies_agree(&ps);
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod transport_pause_contract_tests {
    use plx_play_feed::shared::{FEED_SHARED, FEED_TX};
    use super::{paused, set_transport_paused};
    use crate::player::foreground::{
        drive_foreground, poll_foreground_load, FlightLanding, ForegroundActivation,
        ForegroundActuator, ForegroundPollOutcome,
    };
    use crate::player::lifecycle::{
        ForegroundClock, ForegroundEvent, ForegroundInput, ForegroundLifecycle, ForegroundLoadStart,
        ForegroundLoadStatus, ForegroundState,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn a_refused_native_pause_or_play_cannot_diverge_the_feed_gate() {
        let _guard = plx_base::testlock::serial();
        let old_paused = FEED_TX.paused.load(Ordering::Acquire);
        FEED_TX.commit_paused(false);
        let old_rebuffering = FEED_SHARED
            .hls_rebuffering
            .swap(false, Ordering::AcqRel);
        struct Restore {
            paused: bool,
            rebuffering: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::player::force_pause_result_for_test(None);
                crate::player::force_play_result_for_test(None);
                FEED_TX.commit_paused(self.paused);
                FEED_SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Release);
            }
        }
        let _restore = Restore {
            paused: old_paused,
            rebuffering: old_rebuffering,
        };
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });

        crate::player::force_pause_result_for_test(Some(0));
        assert!(!set_transport_paused(&mut ps, &mut pa, true));
        assert!(
            !paused(&ps),
            "feed must continue when the native clock refused Pause"
        );

        crate::player::force_pause_result_for_test(Some(1));
        assert!(set_transport_paused(&mut ps, &mut pa, true));
        assert!(paused(&ps));
        crate::player::assert_transport_copies_agree(&ps);

        crate::player::force_play_result_for_test(Some(0));
        assert!(!set_transport_paused(&mut ps, &mut pa, false));
        assert!(
            paused(&ps),
            "feed must remain stopped when the native clock refused Play"
        );

        crate::player::force_play_result_for_test(Some(1));
        assert!(set_transport_paused(&mut ps, &mut pa, false));
        assert!(!paused(&ps));
        crate::player::assert_transport_copies_agree(&ps);
    }

    #[test]
    fn refused_foreground_play_retries_the_native_clock_without_a_second_load() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _guard = plx_base::testlock::serial();
        let old_paused = FEED_TX.paused.load(Ordering::Acquire);
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::player::force_pause_result_for_test(None);
                crate::player::force_play_result_for_test(None);
                FEED_SHARED.reset_hls_clock_for_test();
                FEED_TX.commit_paused(self.0);
            }
        }
        let _restore = Restore(old_paused);
        FEED_SHARED.reset_hls_clock_for_test();
        FEED_TX.commit_paused(false);
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::player::force_pause_result_for_test(Some(1));
        assert!(set_transport_paused(&mut ps, &mut pa, true));

        struct Actuator<'a> {
            pa: &'a mut crate::player::adapter::PlayerAdapter,
            prepares: usize,
            loads: usize,
        }
        impl ForegroundActuator for Actuator<'_> {
            type Attempt = u64;

            fn prepare_resume(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
                _resume_ns: i64,
            ) -> crate::player::ResumeStart {
                self.prepares += 1;
                crate::player::ResumeStart::Settled(crate::player::ResumeOutcome::Prepared)
            }

            fn land_prepare(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
            ) -> FlightLanding<crate::player::ResumeOutcome> {
                unreachable!("this actuator never goes pending")
            }

            fn land_recovery(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
            ) -> FlightLanding<ForegroundLoadStart<Self::Attempt>> {
                unreachable!("this actuator never goes pending")
            }

            fn before_load(&mut self, _ps: &mut crate::route::PlaybackSession, _resume_ns: i64,
                _clock: ForegroundClock) {}

            fn start_load(
            &mut self,
            _ps: &mut crate::route::PlaybackSession,
        ) -> ForegroundLoadStart<Self::Attempt> {
                self.loads += 1;
                ForegroundLoadStart::Launched(1)
            }

            fn load_status(
                &mut self,
                attempt: Self::Attempt,
            ) -> ForegroundLoadStatus<Self::Attempt> {
                assert_eq!(attempt, 1);
                ForegroundLoadStatus::Started
            }

            fn after_load(
                &mut self,
                _ps: &mut crate::route::PlaybackSession,
                _attempt: Option<Self::Attempt>,
                _clock: ForegroundClock,
                _started: bool,
            ) {
            }

            fn play_clock(&mut self, ps: &mut crate::route::PlaybackSession) -> bool {
                set_transport_paused(ps, self.pa, false)
            }
        }

        let mut lifecycle = ForegroundLifecycle::IDLE;
        lifecycle.step(
            &ForegroundEvent::Park { saved_ns: 42_000_000_000, viewer_paused: true },
            &mut Vec::new(),
        );
        let mut actuator = Actuator {
            pa: &mut pa,
            prepares: 0,
            loads: 0,
        };
        crate::player::force_play_result_for_test(Some(0));
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator,),
            ForegroundActivation::Launched,
        );
        assert!(matches!(
            lifecycle.state(),
            ForegroundState::LoadPending { .. }
        ));
        assert_eq!(
            poll_foreground_load(&mut lifecycle, &mut ps, &mut actuator),
            ForegroundPollOutcome::Started,
        );
        assert!(matches!(
            lifecycle.state(),
            ForegroundState::ClockPending { .. }
        ));
        assert_eq!((actuator.prepares, actuator.loads), (1, 1));
        assert!(paused(&ps), "a refused native Play keeps the feed held");
        crate::player::assert_transport_copies_agree(&ps);

        crate::player::force_play_result_for_test(Some(1));
        assert_eq!(
            drive_foreground(&mut lifecycle, &mut ps, ForegroundInput::PlayKey, &mut actuator,),
            ForegroundActivation::Handled,
        );
        assert_eq!(
            (actuator.prepares, actuator.loads),
            (1, 1),
            "the retry reached preparation or Load instead of the native clock",
        );
        assert_eq!(lifecycle.state(), ForegroundState::Idle);
        assert!(!paused(&ps));
        crate::player::assert_transport_copies_agree(&ps);
    }
}
