//! Live player presentation IO boundary. Machines receive the complete immutable answer.
use plx_appkit::player_hud::{self, Busy, FailureReadout, Presentation};
use plx_media::player;

pub(super) fn capture(ps: &plx_media::route::PlaybackSession) -> Presentation {
    plx_base::step::assert_io_allowed("player presentation capture");
    let diagnostic = plx_base::devtrig::read("failtest");
    // Preserve the old exists semantics for an unreadable trigger as well as an empty file.
    let forced = diagnostic.is_some() || plx_base::devtrig::flag("failtest");
    let state = player::state(ps);
    if forced || state == player::PlaybackState::Error {
        let error = player::error_captured(ps, diagnostic.as_deref());
        let caption = error.caption;
        let context = player::failure_context_captured(ps, diagnostic.as_deref().is_some_and(|arm| arm.trim() == "policy"));
        let actions = player::failure_actions(error.kind, context);
        let support = player::support_line(error.kind);
        let readout = FailureReadout::captured(error, actions, support, ps.repair_status);
        Presentation { busy: Busy::Readout(plx_ui::widgets::StatusKind::Failed, caption),
            failure: Some(std::sync::Arc::new(readout)) }
    } else {
        Presentation { busy: player_hud::busy_surface_with(c"", state,
            player::reported_frame(ps).unwrap_or(false), player::audio_switching()), failure: None }
    }
}

#[cfg(all(test, feature = "devtriggers"))]
mod tests {
    use super::*;
    #[test]
    fn adjacent_live_diagnostic_arms_clears_and_pure_readout_is_coherent() {
        let _serial = plx_base::testlock::serial();
        plx_base::devtrig::with_private_triggers(|| {
            let ps = plx_media::route::PlaybackSession::IDLE;
            let neutral = capture(&ps);
            let path = plx_base::devtrig::path("failtest");
            std::fs::write(&path, "policy").unwrap();
            let failed = capture(&ps);
            assert!(player_hud::transport_hidden(&failed));
            assert!(failed.failure.is_some());
            std::fs::remove_file(&path).unwrap();
            assert_eq!(capture(&ps), neutral, "clearing the LIVE flag must retire the failure");
            let _step = plx_base::step::StepScope::enter();
            assert!(!player_hud::transport_hidden(&neutral));
            assert!(player_hud::transport_hidden(&failed), "a captured owner does not reread the cleared flag");
            let readout = failed.failure.as_ref().unwrap();
            assert!(!readout.actions().is_empty());
            assert!(readout.frames(&plx_ui::fixture::FixtureMeasure).iter().any(Option::is_some));
            let drawn = plx_ui::record_walk(|| plx_ui::draw_census::capture(||
                player_hud::classic::draw_readout(failed.failure.as_deref(), failed.busy, 0, 0, &plx_ui::fixture::FixtureMeasure)));
            assert!(!drawn.is_empty());
            assert!(std::panic::catch_unwind(|| capture(&ps)).is_err(), "the producer is explicitly outside StepScope");
        });
    }
}
