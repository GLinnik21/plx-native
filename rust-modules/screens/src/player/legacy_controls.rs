//! The legacy remote policy lives beside, rather than inside, the modern scrub gesture.

use super::*;
use plx_plex::plex::session::PlaybackControls;

/// A seek requested over a dismissed legacy HUD stays invisible through the first new picture.
/// Ordinary rebuffering and failures retain the shared read-out policy.
pub(super) fn seek_stays_hidden(ps: &plx_media::route::PlaybackSession, dismissed: bool) -> bool {
    use plx_media::player::{self, PlaybackState};
    // Play clears Seeking before the first presented-frame callback. The pump then derives
    // Buffering until the first new picture. Its cached state can outlive frames=0 for a frame,
    // so use the existing seek target and rebuffer marker rather than re-sampling frame counts.
    let state = player::state(ps);
    let seeking = state == PlaybackState::Seeking
        || (state == PlaybackState::Buffering
            && !player::rebuffering()
            && player::seek_display_ns() >= 0);
    dismissed && plx_media::route::playback_controls() == PlaybackControls::Legacy && seeking
}

impl PlayerScreen {
    pub(super) fn legacy_hidden_seek<H: AppLike>(
        &mut self,
        ps: &plx_media::route::PlaybackSession,
        key: consts::Key,
        edge: Edge,
        fx: &mut Effects<'_, H>,
    ) -> bool {
        if plx_media::route::playback_controls() != PlaybackControls::Legacy
            || self.hud.visible_at_press
        {
            return false;
        }
        let forward = match key {
            consts::Key::Left { .. } => false,
            consts::Key::Right { .. } => true,
            _ => return false,
        };
        if edge == Edge::Down {
            let duration = plx_media::player::duration_ns();
            if duration > 0 {
                let step = input::scrub_step_ns() * if forward { 1 } else { -1 };
                let target = Scrub::clamp_target(
                    plx_media::player::intended_pos_ns(ps).saturating_add(step),
                    duration,
                );
                self.scrub.cancel();
                // note_global_press cleared dismissal after sampling visibility. Keep the picture
                // bare through the seek; the next non-seek press can reveal the HUD normally.
                self.hud.dismissed = true;
                Self::ask(fx, PlayerReq::CommitSeek(target));
                self.publish();
            }
        }
        // One immediate jump per press. Repeats and release must not create a pending preview.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::player::overlay_tests::TestHost;
    use plx_machine::machine::{InputEvent, InputOwner, MachineId, Source, Tick};
    use plx_plex::plex::session::PlaybackControls;
    use plx_ui::fixture::FixtureMeasure;
    use std::sync::atomic::Ordering::Relaxed;

    struct PlaybackFixture {
        duration: i64,
        position: i64,
        controls: PlaybackControls,
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
                controls: plx_media::route::playback_controls(),
                state: plx_media::player::swap_state_for_test(
                    plx_media::player::PlaybackState::Playing,
                ),
                rebuffering: plx_media::player::SHARED
                    .hls_rebuffering
                    .swap(false, Relaxed),
                seek_display: plx_media::player::SHARED.seek_display_ns.swap(-1, Relaxed),
            };
            plx_media::route::restore_playback_controls(PlaybackControls::Legacy);
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
            plx_media::route::restore_playback_controls(self.controls);
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
    fn legacy_controls_hidden_press_seeks_immediately_without_raising_the_hud() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        for (sym, target) in [
            (consts::SDLK_RIGHT, 50_000_000_000),
            (consts::SDLK_LEFT, 30_000_000_000),
        ] {
            let mut page = PlayerScreen::new(EntryId(9));
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
    fn legacy_controls_visible_seek_still_waits_for_confirmation() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds10);
        let mut page = PlayerScreen::new(EntryId(9));
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
    fn legacy_controls_do_not_raise_the_hud_during_seeking_but_buffering_still_does() {
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
    fn legacy_controls_off_keeps_the_modern_reveal_first_behavior() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        plx_media::route::restore_playback_controls(PlaybackControls::Modern);
        let mut page = PlayerScreen::new(EntryId(9));
        assert!(key(&mut page, consts::SDLK_RIGHT, Edge::Down, 1_000).is_empty());
        assert!(page.scrub.reveal);
        assert!(page.hud.until > 1_000);
    }

    #[test]
    fn legacy_controls_hidden_jumps_clamp_and_repeats_do_not_create_a_scrub() {
        let _g = plx_base::testlock::serial();
        let _playback = PlaybackFixture::new();
        let _interval =
            player_hud::SkipIntervalGuard::set(plx_media::route::SkipInterval::Seconds60);
        for (sym, target) in [(consts::SDLK_RIGHT, 97_000_000_000), (consts::SDLK_LEFT, 0)] {
            let mut page = PlayerScreen::new(EntryId(9));
            assert_eq!(
                key(&mut page, sym, Edge::Down, 1_000),
                vec![PlayerReq::CommitSeek(target)]
            );
            assert!(key(&mut page, sym, Edge::Repeat, 1_050).is_empty());
            assert!(key(&mut page, sym, Edge::Up, 1_080).is_empty());
            assert!(!page.scrub.is_live());
        }
    }
}
