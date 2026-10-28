//! The tests of `plex::session` that need the application around it: the `Bridge` that lands the
//! session cache on the frame thread, the dispatcher and the Login screen that read it every frame,
//! and the player's audio-enhancement door onto the persisted record.
//!
//! They lived in `plex::session`'s own `cache_timing_tests` while that module could name the whole
//! app; `plex` sits beneath `app`, `screens`, `player` and `ui` now, so each moved to the layer that
//! owns all of its parts. What `plex` can grade alone (the refresh job's outcomes, the visible
//! generation, a worker publishing no damage) stays there, and the single-flight refresh is the
//! session owner's, graded in `owner::tests::cache_refresh`; this file keeps the halves that are an
//! application's (the frame step's refill, the adapter slot, the bridge landing).

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
    let mut pages = plx_ui::dispatch::Dispatcher::new();
    session::save(&signed_in());
    plx_machine::idle::reset_for_test();
    let _frame = plx_base::task::FrameScope::enter();
    bridge.land_session_cache(&mut pages);
    assert_eq!(plx_machine::idle::take_local_damage(), 1, "a changed session lands on the frame step");
    bridge.land_session_cache(&mut pages);
    assert_eq!(plx_machine::idle::take_local_damage(), 0, "one invalidation per landing");
    drop(_frame);
    session::save(&signed_in());
    let _frame = plx_base::task::FrameScope::enter();
    bridge.land_session_cache(&mut pages);
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
    // finds, so the owner asks for a refresh while the lock is held (the fixture adapter parks it:
    // the frames must neither take the lock nor read, whoever would have run it; the live
    // adapter's submit under the held lock is graded by
    // `a_live_adapter_begins_a_cache_refresh_without_taking_the_session_io_lock`).
    session::invalidate_for_test();
    session::reset_reads_for_test();
    session::with_io_for_test(|| {
        let _frame = plx_base::task::FrameScope::enter();
        let mut bridge = Bridge::for_test(|| 0);
        let mut pages = plx_ui::dispatch::Dispatcher::new();
        crate::app::bridge::nav_root(&mut pages, plx_screens::registry::AppArg::Login);
        for ms in 0..30 {
            crate::app::bridge::frame(&mut pages, &mut bridge,
                plx_machine::machine::Tick { ms: ms * 16, dt_us: 16_000 }, Vec::new());
        }
        assert!(matches!(pages.top_arg(), Some(plx_screens::registry::AppArg::Login)));
        assert_eq!(session::reads_for_test(), 0, "login's Browse/Search captures may only peek");
    });
    plx_base::storage_worker::drain_for_test();
}

/// Issue #266: the audio-DSP preference round-trips through the player's one write door
/// (`player::set_audio_enhancements` -> `route::choose` -> retained worker job ->
/// `session::persist_profile_pref`) as the SEATED profile's own value (the per-profile work), and
/// comes back through the seat's restore, merged into the record rather than replacing it. The TV's
/// own top-level key is never written, and a record saved before the field existed loads as NONE.
#[test]
fn audio_enhancements_persist_and_restore() {
    let _serial = plx_base::testlock::serial();
    let temp = session::TempSession::new("audio-enhancements");
    session::save(&signed_in());
    let stamp = temp.seated("u-audio", &signed_in().account_token);
    let seat = session::Seat::Seated { user: "u-audio".into(), stamp };
    let stored = |s: &Session| session::stored_effective::<session::AudioEnhancementsPref>(s, &seat);
    assert_eq!(stored(&session::load()), plx_plex::plex::AudioEnhancements::NONE, "absent field = NONE");

    let enh = plx_plex::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false };
    plx_media::player::set_audio_enhancements(enh);
    plx_base::storage_worker::drain_for_test();
    let saved = session::load();
    assert_eq!(stored(&saved), enh);
    assert_eq!(saved.audio_enhancements(), plx_plex::plex::AudioEnhancements::NONE, "the TV's own key is not written");
    assert_eq!(saved.client_id, signed_in().client_id, "merged, not replaced");

    plx_media::player::restore_audio_enhancements(plx_plex::plex::AudioEnhancements::NONE);
    plx_media::route::seed_profile_prefs_for_test(&saved);
    plx_media::route::restore_profile_rows(&saved, &seat, plx_media::route::Overrides::default());
    assert_eq!(plx_media::player::audio_enhancements(), enh, "the seat restores what the profile saved");
    session::reset_last_write_authority_for_test();
    let write = session::ProfileWrite::capture().expect("the profile is seated");
    assert!(session::persist_profile_pref::<session::AudioEnhancementsPref>(&write, enh), "an unchanged preference is saved...");
    assert!(session::last_write_authority_for_test().is_none(), "...without being rewritten");
    plx_media::player::restore_audio_enhancements(plx_plex::plex::AudioEnhancements::NONE);
    plx_media::route::unseed_profile_prefs_for_test();
}

/// Frames of 16 ms from `*ms`, running the refresh the fixture adapter parked and letting the
/// storage worker finish between them, so a refresh's answer is collected by the very next frame.
/// Every refresh the frames start is awaited before returning.
fn frames(pages: &mut plx_ui::dispatch::Dispatcher<crate::app::bridge::AppHost>, bridge: &mut Bridge,
    ms: &mut u32, count: u32) {
    for _ in 0..count {
        crate::app::bridge::frame(pages, bridge, plx_machine::machine::Tick { ms: *ms, dt_us: 16_000 }, Vec::new());
        bridge.run_held_cache_refresh_for_test();
        plx_base::storage_worker::drain_for_test();
        *ms += 16;
    }
}

/// `peek` never reads, so an `Unloaded` cache in a live frame is refilled by the session owner's
/// `RefreshCache` effect: observed by the frame step, run by the adapter (the storage worker live;
/// the test thread under a fixture adapter), answered to the owner. Within a few frames the cache is settled (what Settings' pending preference edits and
/// the account menu wait for), the visible generation moved ONCE, and nothing is left in flight.
#[test]
fn an_unloaded_cache_is_refilled_by_the_owner_within_a_few_frames() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("bridge-cache-refill");
    struct ResetIdle;
    impl Drop for ResetIdle {
        fn drop(&mut self) { plx_machine::idle::reset_for_test(); }
    }
    let _idle = ResetIdle;
    session::save(&signed_in());
    session::invalidate_for_test();
    let mut bridge = Bridge::for_test(|| 0);
    let mut pages = plx_ui::dispatch::Dispatcher::new();
    crate::app::bridge::nav_root(&mut pages, plx_screens::registry::AppArg::Login);
    assert!(session::peek_settled().is_none(), "the cache starts unloaded");
    assert!(session::peek().client_id.is_empty(), "and peek alone fills nothing");
    let generation = session::visible_generation();
    let mut ms = 0;
    frames(&mut pages, &mut bridge, &mut ms, 4);
    assert!(session::peek_settled().is_some(), "the owner's refresh filled the cache");
    assert_eq!(session::peek().client_id, "cid-1");
    assert_eq!(session::visible_generation(), generation + 1, "one visible landing");
    assert_eq!(bridge.session.cache_refresh_in_flight(), None, "the answer reached the owner");
    assert_eq!(session::refresh_need(), None);
    frames(&mut pages, &mut bridge, &mut ms, 4);
    assert_eq!(session::visible_generation(), generation + 1, "a settled cache raises no more landings");
}

/// An adapter that is replaced (or dropped) loses its job (ticket or parked refresh), so nothing would ever answer
/// the owner: the frame step reconciles it with `Refused`, the owner backs off about a second, and
/// then asks again.
#[test]
fn a_replaced_adapter_cannot_strand_an_in_flight_refresh() {
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("bridge-cache-adapter-replaced");
    session::save(&signed_in());
    session::invalidate_for_test();
    let mut bridge = Bridge::for_test(|| 0);
    let mut pages = plx_ui::dispatch::Dispatcher::new();
    crate::app::bridge::nav_root(&mut pages, plx_screens::registry::AppArg::Login);
    let mut ms = 0;
    crate::app::bridge::frame(&mut pages, &mut bridge, plx_machine::machine::Tick { ms, dt_us: 16_000 }, Vec::new());
    ms += 16;
    assert!(bridge.session.cache_refresh_in_flight().is_some(), "the first frame asked for a refresh");
    let mt = unsafe { plx_base::task::MainThread::assume() };
    bridge.session_adapter = crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
    assert!(session::refresh_need().is_some(), "the dropped adapter's parked job never ran: the need still stands");
    frames(&mut pages, &mut bridge, &mut ms, 1);
    assert_eq!(bridge.session.cache_refresh_in_flight(), None, "the reconcile answered the lost job");
    assert!(session::peek_settled().is_none(), "and the owner is now backing off");
    ms += 500;
    frames(&mut pages, &mut bridge, &mut ms, 2);
    assert!(session::peek_settled().is_none(), "the refusal holds back every generation for a second");
    ms += 600;
    frames(&mut pages, &mut bridge, &mut ms, 3);
    assert!(session::peek_settled().is_some(), "then the owner asks again and the cache fills");
    assert_eq!(bridge.session.cache_refresh_in_flight(), None);
}

/// The reconcile refuses a refresh only after the adapter BEGAN it and lost it. An owner that is
/// `InFlight` while its `RefreshCache` effect is still queued (the drain budget carried the effect
/// to the next frame) has not been lost by anyone: answering `Refused` for it would back the owner
/// off, and the job the carried effect then starts would be answered into a stale `req`.
#[test]
fn a_refresh_effect_still_queued_is_not_reconciled_as_lost() {
    use plx_machine::machine::{Effects, MachineId};
    use plx_session::auth::owner::{scenarios::step_at, SessionEvent, SessionFx};
    let _serial = plx_base::testlock::serial();
    let _session = session::TempSession::new("bridge-cache-effect-queued");
    struct ResetIdle;
    impl Drop for ResetIdle {
        fn drop(&mut self) { plx_machine::idle::reset_for_test(); }
    }
    let _idle = ResetIdle;
    session::save(&signed_in());
    session::invalidate_for_test();
    let mut bridge = Bridge::for_test(|| 0);
    let mut pages = plx_ui::dispatch::Dispatcher::new();
    crate::app::bridge::nav_root(&mut pages, plx_screens::registry::AppArg::Login);
    let need = session::refresh_need().expect("an unloaded cache needs a read");
    // The owner asked on the previous frame; its effect is still waiting in the queue.
    let mut asked = step_at(&mut bridge.session, SessionEvent::CacheObserved(need), 0);
    let Some(SessionFx::RefreshCache { req, generation }) = asked.pop() else { panic!("the owner asked for a read") };
    assert_eq!(bridge.session.cache_refresh_in_flight(), Some(req));
    assert_eq!(bridge.session_adapter.cache_refresh_req(), None, "the adapter has begun nothing");
    // This frame's reconcile must leave it alone.
    let queued = pages.queued();
    {
        let _frame = plx_base::task::FrameScope::enter();
        bridge.land_session_cache(&mut pages);
    }
    assert_eq!(pages.queued(), queued, "no answer was made up for a job that has not begun");
    assert_eq!(bridge.session.cache_refresh_in_flight(), Some(req));
    // The carried effect reaches the adapter and the job's own answer is the one the owner takes.
    let mut present = plx_machine::present::Present::new();
    let mut effects = Vec::new();
    bridge.session_effect(SessionFx::RefreshCache { req, generation },
        &mut Effects::new(&mut effects, MachineId::Session, &mut present));
    assert!(effects.is_empty(), "the adapter took the job");
    assert_eq!(bridge.session_adapter.cache_refresh_req(), Some(req));
    bridge.run_held_cache_refresh_for_test();
    let mut ms = 16;
    frames(&mut pages, &mut bridge, &mut ms, 2);
    assert!(session::peek_settled().is_some(), "the job filled the cache");
    assert_eq!(bridge.session.cache_refresh_in_flight(), None, "and its answer reached the owner");
    // Had a made-up `Refused` taken the owner first, its backoff would still hold: a later need
    // goes unanswered for a second. The owner is Idle, so it asks again at once.
    session::invalidate_for_test();
    frames(&mut pages, &mut bridge, &mut ms, 3);
    assert!(session::peek_settled().is_some(), "no backoff was left behind by a made-up refusal");
}

/// A controlled run records its resources: a cache refresh is real storage IO it never recorded, so
/// the Bridge fails the run loudly instead of executing it (the same refusal as `Capture`/`Work`).
#[test]
fn a_controlled_run_refuses_a_cache_refresh_loudly() {
    use plx_machine::machine::{Effects, MachineId};
    let _serial = plx_base::testlock::serial();
    let mut bridge = Bridge::for_test(|| 0);
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let publisher = plx_plex::plex::session::ProfilePublisher::scoped(&mt);
    bridge.home_io = Some(crate::app::bootstrap::HomeIo { replay: false, preferences: Default::default(),
        requests: Vec::new(), admissions: Default::default(), failure: None, profile: publisher.snapshot() });
    assert_eq!(bridge.controlled_failure(), None);
    let mut present = plx_machine::present::Present::new();
    let mut effects = Vec::new();
    bridge.session_effect(plx_session::auth::owner::SessionFx::RefreshCache { req: 1, generation: 0 },
        &mut Effects::new(&mut effects, MachineId::Session, &mut present));
    assert_eq!(bridge.controlled_failure(), Some("unsupported controlled Home Session operation"));
    assert!(effects.is_empty(), "nothing was started or answered");
    assert_eq!(bridge.session_adapter.cache_refresh_req(), None);
}
