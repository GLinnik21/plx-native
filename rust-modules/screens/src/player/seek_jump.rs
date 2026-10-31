//! Immediate FF/REW taps and optional hidden-HUD D-pad jumps, sharing one seek path.
use super::*;
use plx_plex::plex::session::InstantDpadSeeking;

/// Dismissed immediate seeks stay bare through the first new picture. Ordinary buffering and
/// failures retain the shared read-out policy, independently of the D-pad preference.
pub(super) fn seek_stays_hidden(ps: &plx_media::route::PlaybackSession, dismissed: bool) -> bool {
    use plx_media::player::{self, PlaybackState};
    if !dismissed { return false; }
    let state = player::state(ps);
    state == PlaybackState::Seeking
        || (state == PlaybackState::Buffering && !player::rebuffering() && player::seek_display_ns() >= 0)
}

/// One scrub step toward `forward`'s end of the bar.
fn signed_step(forward: bool) -> i64 {
    input::scrub_step_ns() * if forward { 1 } else { -1 }
}

impl PlayerScreen {
    /// A gesture other than a transport tap has begun: the pending tap is no longer one, and the
    /// relative jump burst restarts from the playhead.
    pub(super) fn end_jump_gesture(&mut self) {
        self.transport_tap = false;
        self.seek_burst.reset();
    }

    /// Another key has interrupted an FF/REW tap that has not been released: its armed scrub goes
    /// with it, uncommitted.
    pub(super) fn drop_transport_tap(&mut self) {
        if self.transport_tap {
            self.transport_tap = false;
            self.scrub.cancel();
        }
    }

    fn commit_jump<H: AppLike>(&mut self, ps: &plx_media::route::PlaybackSession,
        forward: bool, now: u32, fx: &mut Effects<'_, H>) {
        if let Some(target) = self.jump_by(ps, signed_step(forward), false, now) {
            Self::ask(fx, PlayerReq::CommitSeek(target));
        }
    }

    /// **One relative jump of `step_ns` from where the viewer is**, the target to commit
    /// ([`PlayerReq::CommitSeek`], or `commit_seek` in the app's performer). `None` when the
    /// duration is not known: a jump has nothing to clamp against.
    ///
    /// `keep_hud` is the one difference between the two callers. A jump KEY on a hidden transport
    /// is a tap that dismisses it again (`hide`), and a seek CONTROL pressed on a visible one keeps
    /// it up and extends its linger. Both share the burst: presses inside
    /// [`BURST_GAP_MS`](plx_appkit::player_hud::seek_jump::BURST_GAP_MS) accumulate into one target, so keyframe rounding never eats the requested travel.
    pub fn jump_by(&mut self, ps: &plx_media::route::PlaybackSession, step_ns: i64, keep_hud: bool, now: u32) -> Option<i64> {
        let duration = plx_media::player::duration_ns();
        if duration <= 0 { return None; }
        let target = self.seek_burst.jump(now, plx_media::player::intended_pos_ns(ps), step_ns, duration);
        self.scrub.cancel();
        if keep_hud {
            self.hud.extend(now, input::HUD_LINGER_MS);
        } else {
            self.hud.hide(false);
        }
        self.publish();
        Some(target)
    }

    pub(super) fn jump_key<H: AppLike>(&mut self, ps: &plx_media::route::PlaybackSession,
        key: consts::Key, edge: Edge, now: u32, fx: &mut Effects<'_, H>) -> bool {
        let (forward, transport) = match key {
            consts::Key::Left { alt } => (false, alt),
            consts::Key::Right { alt } => (true, alt),
            _ => return false,
        };
        if transport {
            if plx_media::player::duration_ns() <= 0 {
                self.transport_tap = false;
                self.scrub.cancel();
                return true;
            }
            match edge {
                Edge::Down => {
                    if self.scrub.key.bounce(now, forward) { return true; }
                    // Tap/hold is decided by the shared HeldKey's first hardware repeat.
                    // No seek is sent on Down: a hold must never begin with a committed jump.
                    let seed = if self.scrub.is_live() && !self.scrub.reveal && !self.scrub.drag {
                        self.scrub.ns
                    } else { plx_media::player::intended_pos_ns(ps) };
                    self.transport_tap = true;
                    self.scrub.cancel();
                    self.scrub.begin(now, forward);
                    self.scrub.ns = seed;
                    self.scrub.reveal = true;
                    self.hud.nav.focus = GROUP_SCRUB;
                    Self::repair_focus(fx, GROUP_SCRUB);
                    if !self.hud.visible_at_press { self.hud.hide(false); }
                }
                Edge::Repeat => {
                    if self.transport_tap && self.hud.visible_at_press {
                        self.scrub.ns = Scrub::clamp_target(self.scrub.ns.saturating_add(signed_step(forward)), plx_media::player::duration_ns());
                        self.scrub.reveal = false;
                    }
                    self.hud.dismissed = false;
                    self.end_jump_gesture();
                    self.key_scrub_repeat(now);
                    self.hud.extend(now, input::HUD_LINGER_MS);
                }
                Edge::Up => {
                    if self.transport_tap {
                        self.transport_tap = false;
                        // `commit_jump` publishes its own result
                        self.commit_jump(ps, forward, now, fx);
                        return true;
                    }
                    self.key_scrub_release(now);
                }
            }
            self.publish();
            return true;
        }
        if edge == Edge::Down { self.drop_transport_tap(); }
        if plx_media::route::instant_dpad_seeking() != InstantDpadSeeking::On || self.hud.visible_at_press {
            self.seek_burst.reset();
            return false;
        }
        if edge == Edge::Down { self.commit_jump(ps, forward, now, fx); }
        // The opt-in D-pad path is one immediate jump per fresh press, without a held scrub.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::travel_rig::Rig;
    use super::super::*;
    use crate::player::overlay_tests::TestHost;
    use plx_machine::machine::{InputEvent, InputOwner, MachineId, Source, Tick};
    use plx_plex::plex::session::InstantDpadSeeking;
    use plx_ui::fixture::FixtureMeasure;
    use std::sync::atomic::Ordering::Relaxed;

    struct PlaybackFixture {
        duration: i64,
        position: i64,
        dpad_seeking: InstantDpadSeeking,
        state: u8,
        rebuffering: bool,
        seek_display: i64,
    }
    impl PlaybackFixture {
        fn new() -> Self {
            let saved = Self {
                duration: plx_media::player::SHARED
                    .duration_ns
                    .swap(100_000_000_000, Relaxed),
                position: plx_media::player::SHARED
                    .playpos_ns
                    .swap(40_000_000_000, Relaxed),
                dpad_seeking: plx_media::route::instant_dpad_seeking(),
                state: plx_media::player::swap_state_for_test(
                    plx_media::player::PlaybackState::Playing,
                ),
                rebuffering: plx_media::player::SHARED
                    .hls_rebuffering
                    .swap(false, Relaxed),
                seek_display: plx_media::player::SHARED.seek_display_ns.swap(-1, Relaxed),
            };
            plx_media::route::restore_instant_dpad_seeking(InstantDpadSeeking::On);
            saved
        }
    }
    impl Drop for PlaybackFixture {
        fn drop(&mut self) {
            plx_media::player::SHARED
                .duration_ns
                .store(self.duration, Relaxed);
            plx_media::player::SHARED
                .playpos_ns
                .store(self.position, Relaxed);
            plx_media::route::restore_instant_dpad_seeking(self.dpad_seeking);
            plx_media::player::restore_state_for_test(self.state);
            plx_media::player::SHARED
                .hls_rebuffering
                .store(self.rebuffering, Relaxed);
            plx_media::player::SHARED
                .seek_display_ns
                .store(self.seek_display, Relaxed);
        }
    }

    fn key(page: &mut PlayerScreen, sym: u32, edge: Edge, now: u32) -> Vec<PlayerReq> {
        let cx = Cx {
            views: (),
            tick: Tick::default(),
            measure: &FixtureMeasure,
            focus: Default::default(),
            press: Default::default(),
            owner: InputOwner::Entry(EntryId(9)),
        };
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        page.step(
            &ScreenEvent::<TestHost>::Input(InputEvent {
                at: Tick { ms: now, dt_us: 0 },
                source: Source::Sdl,
                kind: InputKind::Key {
                    key: plx_machine::machine::Key::Other,
                    sym,
                    wcode: 0,
                    edge,
                    at_edge: false,
                },
            }),
            &cx,
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(3)), &mut present),
        );
        out.into_iter()
            .filter_map(|e| match e.fx {
                Fx::App(AppFx::Player(req)) => Some(req),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn transport_ff_tap_jumps_without_the_dpad_option_or_hud() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        plx_media::route::restore_instant_dpad_seeking(InstantDpadSeeking::Off);
        let _interval = player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        page.hud.dismissed = true;
        assert!(key(&mut page, consts::WCODE_FASTFORWARD, Edge::Down, 1_000).is_empty());
        assert!(page.hud.dismissed);
        assert_eq!(key(&mut page, consts::WCODE_FASTFORWARD, Edge::Up, 1_050),
            vec![PlayerReq::CommitSeek(50_000_000_000)]);
    }

    #[test]
    fn seek_jump_ff_burst_accumulates_and_rew_reverses_with_dpad_off() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        plx_media::route::restore_instant_dpad_seeking(InstantDpadSeeking::Off);
        let _interval = player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        for (sym, at, target) in [(consts::WCODE_FASTFORWARD, 1_000, 50_000_000_000),
            (consts::WCODE_FASTFORWARD, 1_300, 60_000_000_000),
            (consts::WCODE_REWIND, 1_600, 50_000_000_000)] {
            assert!(key(&mut page, sym, Edge::Down, at).is_empty());
            assert_eq!(key(&mut page, sym, Edge::Up, at + 40), vec![PlayerReq::CommitSeek(target)]);
            plx_media::player::SHARED.playpos_ns.store(40_000_000_000, Relaxed);
        }
    }

    #[test]
    fn seek_jump_ff_hold_scrubs_and_waits_for_ok_without_an_initial_seek() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        plx_media::route::restore_instant_dpad_seeking(InstantDpadSeeking::Off);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        page.hud.dismissed = true;
        assert!(key(&mut page, consts::WCODE_FASTFORWARD, Edge::Down, 1_000).is_empty());
        assert!(key(&mut page, consts::WCODE_FASTFORWARD, Edge::Repeat, 1_500).is_empty());
        page.step_scrub_hold(1_600);
        assert!(page.scrub.ns > 40_000_000_000);
        assert!(!page.hud.dismissed);
        assert!(key(&mut page, consts::WCODE_FASTFORWARD, Edge::Up, 1_610).is_empty());
        page.step_scrub_hold(1_800);
        let target = page.scrub.ns;
        assert_eq!(key(&mut page, consts::SDLK_RETURN, Edge::Down, 1_810), vec![PlayerReq::SeekTo(target)]);
    }

    #[test]
    fn episode_row_focus_uses_engine_and_skips_disabled_or_absent_controls() {
        use plx_ui::focus::{FocusEngine, Outcome};
        use plx_ui::screen::By;
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        let cx: Cx<'_, TestHost> = Cx { views: (), tick: Tick::default(), measure: &FixtureMeasure,
            focus: Default::default(), press: Default::default(), owner: InputOwner::Entry(EntryId(9)) };
        for (controls, primary, right, left) in [
            (player_hud::Controls::for_item(true, false, true, false), 1, 2, 1),
            (player_hud::Controls::for_item(true, true, false, false), 0, 2, 0),
            (player_hud::Controls::NONE, 2, 3, 2),
        ] {
            page.slot = ControlSlot::Discs(controls);
            page.hud.nav.focus = GROUP_SCRUB;
            page.hud.visible_at_press = true;
            // UP from the scrubber is the engine's: the declared link lands the row's own seat.
            let mut travel = Rig::on(&page, EntryId(9));
            travel.key(&mut page, consts::SDLK_UP, 0, Edge::Down, 1_000);
            assert_eq!(page.hud.nav.focus, GROUP_ROW);
            assert_eq!(page.hud.nav.btn, primary);
            let mut engine = FocusEngine::new();
            let start = <PlayerScreen as Focusable<TestHost>>::seat(&page, GROUP_ROW,
                Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None }, &cx);
            assert_eq!(start.elem, player_hud::ELEM_CONTROL_BASE + primary as u32);
            engine.set(cx.owner, start, Some(GROUP_ROW), By::Restore);
            for (dir, expected) in [(Dir::Right, right), (Dir::Left, left)] {
                let outcome = engine.move_dir(cx.owner, &page, &[], dir, &cx);
                let Outcome::Moved { from, to, by } = outcome else { panic!("{outcome:?}") };
                assert_eq!(to.elem, player_hud::ELEM_CONTROL_BASE + expected as u32);
                let mut out = Vec::new();
                let mut present = plx_machine::present::Present::new();
                page.step(&ScreenEvent::FocusMoved { from, to, by }, &cx,
                    &mut Effects::new(&mut out, MachineId::Instance(InstanceId(3)), &mut present));
                assert_eq!(page.hud.nav.btn, expected);
            }
            for i in 0..page.slot.items() {
                assert_eq!(<PlayerScreen as Focusable<TestHost>>::place(&page,
                    &(player_hud::ELEM_CONTROL_BASE + i as u32), &cx, At::Drawn).is_some(), controls.enabled(i));
            }
        }
    }

    #[test]
    fn seek_jump_rapid_taps_accumulate_past_an_earlier_keyframe_landing() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        page.hud.dismissed = true;
        // A +10s request lands on the preceding keyframe at 40s again. A second tap must
        // advance the REQUESTED position, even after the player reports Playing at that frame.
        for (sym, at, target) in [
            (consts::SDLK_RIGHT, 1_000, 50_000_000_000),
            (consts::SDLK_RIGHT, 1_300, 60_000_000_000),
            (consts::SDLK_LEFT, 1_600, 50_000_000_000),
        ] {
            assert_eq!(
                key(&mut page, sym, Edge::Down, at),
                vec![PlayerReq::CommitSeek(target)]
            );
            assert!(key(&mut page, sym, Edge::Up, at + 20).is_empty());
            plx_media::player::SHARED
                .seek_display_ns
                .store(target, Relaxed);
            plx_media::player::SHARED
                .playpos_ns
                .store(40_000_000_000, Relaxed);
        }
    }

    #[test]
    fn seek_jump_burst_expires_and_other_controls_clear_it() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        let right = consts::SDLK_RIGHT;
        assert_eq!(
            key(&mut page, right, Edge::Down, 1_000),
            vec![PlayerReq::CommitSeek(50_000_000_000)]
        );
        plx_media::player::SHARED
            .playpos_ns
            .store(43_000_000_000, Relaxed);
        assert_eq!(
            key(&mut page, right, Edge::Down, 2_501),
            vec![PlayerReq::CommitSeek(53_000_000_000)]
        );
        key(&mut page, consts::SDLK_UP, Edge::Down, 2_550);
        // UP raises or moves the HUD; simulate dismissing it again without moving the playhead.
        page.hud.visible_at_press = false;
        assert_eq!(
            key(&mut page, right, Edge::Down, 2_600),
            vec![PlayerReq::CommitSeek(53_000_000_000)]
        );
        page.hud.visible_at_press = true;
        key(&mut page, right, Edge::Down, 2_650);
        page.scrub.cancel();
        page.hud.visible_at_press = false;
        assert_eq!(
            key(&mut page, right, Edge::Down, 2_700),
            vec![PlayerReq::CommitSeek(53_000_000_000)]
        );
    }

    #[test]
    fn seek_jump_burst_survives_a_seek_reload_but_not_an_item_change() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        assert_eq!(
            key(&mut page, consts::SDLK_RIGHT, Edge::Down, 1_000),
            vec![PlayerReq::CommitSeek(50_000_000_000)]
        );
        page.transport_reset(true);
        assert_eq!(
            key(&mut page, consts::SDLK_RIGHT, Edge::Down, 1_100),
            vec![PlayerReq::CommitSeek(60_000_000_000)]
        );
        page.transport_reset(false);
        assert_eq!(
            key(&mut page, consts::SDLK_RIGHT, Edge::Down, 1_200),
            vec![PlayerReq::CommitSeek(50_000_000_000)]
        );
    }

    #[test]
    fn seek_jump_hidden_press_seeks_immediately_without_raising_the_hud() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        for (sym, target) in [
            (consts::SDLK_RIGHT, 50_000_000_000),
            (consts::SDLK_LEFT, 30_000_000_000),
        ] {
            let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
            page.hud.dismissed = true;
            assert_eq!(
                key(&mut page, sym, Edge::Down, 1_000),
                vec![PlayerReq::CommitSeek(target)]
            );
            assert_eq!(page.scrub.ns, -1, "no preview is waiting for confirmation");
            assert_eq!(
                page.hud.until, 0,
                "the hidden seek does not extend the HUD timer"
            );
            assert!(page.hud.dismissed);
            assert!(key(&mut page, sym, Edge::Up, 1_020).is_empty());
        }
    }

    #[test]
    fn seek_jump_visible_seek_still_waits_for_confirmation() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        page.hud.visible_at_press = true;
        assert!(key(&mut page, consts::SDLK_RIGHT, Edge::Down, 1_000).is_empty());
        assert_eq!(page.scrub.ns, 50_000_000_000);
        assert!(key(&mut page, consts::SDLK_RIGHT, Edge::Up, 1_020).is_empty());
        assert_eq!(
            key(&mut page, consts::SDLK_RETURN, Edge::Down, 1_040),
            vec![PlayerReq::SeekTo(50_000_000_000)]
        );
    }

    #[test]
    fn seek_jump_do_not_raise_the_hud_during_seeking_but_buffering_still_does() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let ps = plx_media::route::PlaybackSession::IDLE;
        plx_media::player::swap_state_for_test(plx_media::player::PlaybackState::Seeking);
        assert!(!input::hud_visible(&ps, 1_000, 0, false, true));
        assert!(input::hud_visible(&ps, 1_000, 2_000, false, false));
        plx_media::player::swap_state_for_test(plx_media::player::PlaybackState::Buffering);
        assert!(input::hud_visible(&ps, 1_000, 0, false, true));
        // Play has been issued after seeking, but its first new picture has not arrived yet.
        // The derived state briefly becomes Buffering; that must not flash the transport.
        plx_media::player::SHARED
            .seek_display_ns
            .store(50_000_000_000, Relaxed);
        assert!(!input::hud_visible(&ps, 1_000, 0, false, true));
        plx_media::player::SHARED
            .hls_rebuffering
            .store(true, Relaxed);
        assert!(
            input::hud_visible(&ps, 1_000, 0, false, true),
            "later rebuffering stays visible"
        );
    }

    #[test]
    fn seek_jump_off_keeps_the_reveal_first_behavior() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        plx_media::route::restore_instant_dpad_seeking(InstantDpadSeeking::Off);
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        assert!(key(&mut page, consts::SDLK_RIGHT, Edge::Down, 1_000).is_empty());
        assert!(page.scrub.reveal);
        assert!(page.hud.until > 1_000);
    }

    #[test]
    fn seek_jump_hidden_jumps_clamp_and_repeats_do_not_create_a_scrub() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds60);
        for (sym, target) in [(consts::SDLK_RIGHT, 97_000_000_000), (consts::SDLK_LEFT, 0)] {
            let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
            assert_eq!(
                key(&mut page, sym, Edge::Down, 1_000),
                vec![PlayerReq::CommitSeek(target)]
            );
            assert!(key(&mut page, sym, Edge::Repeat, 1_050).is_empty());
            assert!(key(&mut page, sym, Edge::Up, 1_080).is_empty());
            assert!(!page.scrub.is_live());
        }
    }

    #[test]
    fn a_seek_control_press_keeps_the_hud_up_where_a_hidden_tap_takes_it_down() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let ps = plx_media::route::PlaybackSession::IDLE;
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        assert_eq!(page.jump_by(&ps, 10_000_000_000, true, 1_000), Some(50_000_000_000));
        assert!(!page.hud.dismissed, "a control pressed on a visible transport does not hide it");
        assert!(page.hud.until > 1_000, "and extends its linger");
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        assert_eq!(page.jump_by(&ps, 10_000_000_000, false, 1_000), Some(50_000_000_000));
        assert!(page.hud.dismissed, "a jump key on a hidden transport dismisses it again");
        assert_eq!(page.hud.until, 0);
    }

    #[test]
    fn seek_control_presses_and_jump_keys_share_one_burst() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let ps = plx_media::route::PlaybackSession::IDLE;
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        let step = 10_000_000_000;
        assert_eq!(page.jump_by(&ps, step, true, 1_000), Some(50_000_000_000));
        assert_eq!(page.jump_by(&ps, step, true, 1_300), Some(60_000_000_000), "a second press inside the gap accumulates");
        assert_eq!(page.jump_by(&ps, -step, false, 1_500), Some(50_000_000_000), "a key joins the same burst");
        assert_eq!(page.jump_by(&ps, step, true, 9_000), Some(50_000_000_000), "a press after the gap restarts from the playhead");
    }

    #[test]
    fn a_jump_with_no_known_duration_has_no_target() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        plx_media::player::SHARED.duration_ns.store(0, Relaxed);
        let ps = plx_media::route::PlaybackSession::IDLE;
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        assert_eq!(page.jump_by(&ps, 10_000_000_000, true, 1_000), None);
        assert!(page.hud.until == 0, "nothing to clamp against: the HUD is left alone");
    }

    /// **OK on a seek control continues the burst through `handle_key`**: the guard that keeps the
    /// burst alive is in the key path, so two OK key-downs on a seek control inside the gap, each
    /// followed by the performer's `jump_by` (what `PressCommit` leads to), give ONE accumulated
    /// target, and an OK on any other control restarts it. No slot holds a seek control yet, so the
    /// control's command is handed to `handle_key` as the page resolves it.
    #[test]
    fn two_ok_presses_on_a_seek_control_inside_one_burst_give_one_target() {
        use player_hud::command::{HudCommand, SeekBy, SeekDir};
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let ps = plx_media::route::PlaybackSession::IDLE;
        let step = 10_000_000_000;
        let seek = Some(HudCommand::Seek(SeekBy::Setting(SeekDir::Forward)));
        let ok_down = |page: &mut PlayerScreen, command: Option<HudCommand>, now: u32| {
            let mut out = Vec::new();
            let mut present = plx_machine::present::Present::new();
            page.hud.visible_at_press = true;
            page.handle_key::<TestHost>(
                &ps,
                consts::Key::Ok,
                Edge::Down,
                false,
                now,
                Some(player_hud::control_elem(player_hud::BTN_MORE)),
                command,
                &mut Effects::new(&mut out, MachineId::Instance(InstanceId(3)), &mut present),
                plx_data::stores::metadata::MetadataStore::default().view(),
            )
        };
        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        ok_down(&mut page, seek, 1_000);
        assert_eq!(page.jump_by(&ps, step, true, 1_000), Some(50_000_000_000));
        ok_down(&mut page, seek, 1_300);
        assert_eq!(page.jump_by(&ps, step, true, 1_300), Some(60_000_000_000), "the second press accumulates");

        let mut page = PlayerScreen::new(EntryId(9), plx_appkit::player_hud::loaded::classic_for_test());
        ok_down(&mut page, seek, 1_000);
        assert_eq!(page.jump_by(&ps, step, true, 1_000), Some(50_000_000_000));
        ok_down(&mut page, Some(HudCommand::Next), 1_300);
        assert_eq!(
            page.jump_by(&ps, step, true, 1_300),
            Some(50_000_000_000),
            "an OK on another control ends the gesture, so the next jump starts from the playhead"
        );
    }
}
