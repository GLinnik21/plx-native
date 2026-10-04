//! The tests of `plex::session` that need the application around it: the `Bridge` that lands the
//! session cache on the frame thread, the dispatcher and the Login screen that read it every frame,
//! and the player's audio-enhancement door onto the persisted record.
//!
//! They lived in `plex::session`'s own `cache_timing_tests` while that module could name the whole
//! app; `plex` sits beneath `app`, `screens`, `player` and `ui` now, so each moved to the layer that
//! owns all of its parts. What `plex` can grade alone (the single-flight refresh, the visible
//! generation, a worker publishing no damage) stays there; this file keeps the halves that are an
//! application's.

use crate::app::bridge::Bridge;
use plx_plex::plex::session::{self, Session};

fn signed_in() -> Session {
    Session {
        client_id: "cid-1".into(),
        account_token: "acct".into(),
        ..Default::default()
    }
}

/// The frame thread's landing step is `Bridge::land_session_cache`: it compares the visible
/// generation `plex::session` publishes and invalidates the frame ONCE per move — and not at all
/// for a save that serves the same content.
#[test]
fn a_visible_session_change_invalidates_the_frame_once_per_landing() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("bridge-session-landing");
    struct ResetIdle;
    impl Drop for ResetIdle {
        fn drop(&mut self) { plx_machine::idle::reset_for_test(); }
    }
    let _idle = ResetIdle;
    let mut bridge = Bridge::for_test(|| 0);
    session::save(&signed_in());
    plx_machine::idle::reset_for_test();
    let _frame = plx_base::task::FrameScope::enter();
    bridge.land_session_cache();
    assert_eq!(plx_machine::idle::take_local_damage(), 1, "a changed session lands on the frame step");
    bridge.land_session_cache();
    assert_eq!(plx_machine::idle::take_local_damage(), 0, "one invalidation per landing");
    drop(_frame);
    session::save(&signed_in());
    let _frame = plx_base::task::FrameScope::enter();
    bridge.land_session_cache();
    assert_eq!(plx_machine::idle::take_local_damage(), 0, "a save that serves the same content lands nothing");
}

/// This thread holds the session's IO lock for the whole run, so a frame that took it — or made any
/// other blocking call — panics on the guard `FrameScope` arms (`task::assert_may_block`) instead of
/// merely running slowly. `plex::session` timed the same run against 200 ms; `app/` keeps no
/// wall-clock read (`ci/check-deps.sh`'s `wall` gate), so the guard and the per-thread read count
/// are the assertion here.
#[test]
fn login_frames_never_wait_for_the_session_io_lock() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("login-frame-storage-blocked");
    // The scratch session's own save primed the cache; start from the unloaded one a first frame
    // finds, so its `peek` has a refresh to schedule behind the held lock.
    session::invalidate_for_test();
    session::reset_reads_for_test();
    session::with_io_for_test(|| {
        let _frame = plx_base::task::FrameScope::enter();
        let mut bridge = Bridge::for_test(|| 0);
        let mut pages = plx_ui::dispatch::Dispatcher::new();
        crate::app::bridge::nav_root(&mut pages, crate::screens::registry::AppArg::Login);
        for ms in 0..30 {
            crate::app::bridge::frame(&mut pages, &mut bridge,
                plx_machine::machine::Tick { ms: ms * 16, dt_us: 16_000 }, Vec::new());
        }
        assert!(matches!(pages.top_arg(), Some(crate::screens::registry::AppArg::Login)));
        assert_eq!(session::reads_for_test(), 0, "login's Browse/Search captures may only peek");
    });
    session::drain_refresh_for_test();
}

/// Issue #266: the audio-DSP preference round-trips through the player's one write door
/// (`player::set_audio_enhancements` -> retained worker job -> `session::set_audio_enhancements`)
/// and comes back through the boot-time restore, merged into the record rather than replacing
/// it. A record saved before the field existed loads as NONE.
#[test]
fn audio_enhancements_persist_and_restore() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("audio-enhancements");
    session::save(&signed_in());
    assert_eq!(session::load().audio_enhancements(), plx_plex::plex::AudioEnhancements::NONE, "absent field = NONE");

    let enh = plx_plex::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false };
    plx_media::player::set_audio_enhancements(enh);
    plx_base::storage_worker::drain_for_test();
    let saved = session::load();
    assert_eq!(saved.audio_enhancements(), enh);
    assert_eq!(saved.client_id, signed_in().client_id, "merged, not replaced");

    plx_media::player::restore_audio_enhancements(plx_plex::plex::AudioEnhancements::NONE);
    plx_media::player::restore_audio_enhancements(saved.audio_enhancements());
    assert_eq!(plx_media::player::audio_enhancements(), enh, "boot restores what was saved");
    assert!(!session::set_audio_enhancements(enh), "an unchanged preference is not rewritten");
    plx_media::player::restore_audio_enhancements(plx_plex::plex::AudioEnhancements::NONE);
}
