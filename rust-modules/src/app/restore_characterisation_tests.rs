//! **A0c: what `main` did with the nine install-wide player cells at boot, at a profile switch
//! and across profiles, and what the per-profile move (P1a, `track-memory-plan.md` §3.8) made of it.**
//! Characterisation, not a specification: every expected value below was read off `main` by running
//! it, and the per-profile-settings work flipped three of these on purpose (item 11's managed case,
//! item 13, item 14), each in its own commit; the rest are the seat trigger's own tests.
//!
//! The nine cells are the live copies the player reads every frame: the playback quality ceiling,
//! the Direct Play mode, the subtitle size and position, the next-episode mode, the skip interval,
//! the deck press, the subtitle tone and the audio enhancements. On `main` three places wrote them
//! from the one session record, and none of them knew which profile was seated:
//!
//! * `boot::restore_player_preferences`, once, before any profile is published (the list that was
//!   inline in `construct`);
//! * `bridge::follow_auth_landing`, at every credentials handoff (`Ready`), which a profile switch
//!   ends in;
//! * the doors that pick a value (`route::select_subtitle_size`, `route::set_direct_play_mode`,
//!   `playback::play_automatically`), which wrote the record's top-level key.
//!
//! Now the cells hold the SEATED profile's values: boot restores `Unseated` (the fallback), the
//! seat trigger (`bridge::SeatWatch`) restores at every seat change and nowhere else, and the doors
//! save the seated profile's own record and never the top-level key.
//!
//! `hostsim` only, and as a correctness gate: `boot::restore_player_preferences` reads
//! `dev::playback_quality_override()`, which reads `/tmp/plxnative-quality`. `PLXNATIVE_RUNTIME_DIR`
//! is honoured only in a `hostsim` build, so only there can the run promise "no dev override"
//! instead of grading whatever file another process left in the shared `/tmp`.

use super::*;
use super::test_support::{frame, tick};
use plx_plex::plex::session::{
    self, put_own, AudioEnhancementsPref, DeckPress, DeckPressPref, DirectPlayMode, DirectPlayModePref,
    HomeUserRef, NextEpisodeMode, NextEpisodeModePref, PlaybackQuality, ProfileCreds, QualityPref, ServerRef,
    Session, SkipInterval, SkipIntervalPref, SourceRef, SubtitlePosition, SubtitlePositionPref, SubtitleSize,
    SubtitleSizePref, SubtitleTone, SubtitleTonePref, TrailerAutoplayPref, UserRef,
};
use plx_plex::plex::AudioEnhancements;
use plx_session::auth::owner::{SessionEnvelope, SessionEvent, SessionWork};
use plx_session::auth::{Phase, Picker, SessionCmd, SessionInit};
use plx_ui::screen::ScreenArg;

/// The nine cells, as the player reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cells {
    quality: PlaybackQuality,
    direct_play: DirectPlayMode,
    size: SubtitleSize,
    position: SubtitlePosition,
    next_episode: NextEpisodeMode,
    skip: SkipInterval,
    deck_press: DeckPress,
    tone: SubtitleTone,
    enhancements: AudioEnhancements,
}

fn cells() -> Cells {
    Cells {
        quality: plx_media::route::quality(),
        direct_play: plx_media::route::direct_play_mode(),
        size: plx_media::route::subtitle_size(),
        position: plx_media::route::subtitle_position(),
        next_episode: plx_media::route::next_episode_mode(),
        skip: plx_media::route::skip_interval(),
        deck_press: plx_media::route::deck_press(),
        tone: plx_media::player::subtitle_tone(),
        enhancements: plx_media::player::audio_enhancements(),
    }
}

/// What a session that never chose anything holds: every row's default.
const DEFAULTS: Cells = Cells {
    quality: PlaybackQuality::Original,
    direct_play: DirectPlayMode::Auto,
    size: SubtitleSize::Medium,
    position: SubtitlePosition::Low,
    next_episode: NextEpisodeMode::Countdown,
    skip: SkipInterval::Seconds10,
    deck_press: DeckPress::Details,
    tone: SubtitleTone::White,
    enhancements: AudioEnhancements::NONE,
};

/// The record's nine install keys, each away from its default.
const STORED: Cells = Cells {
    quality: PlaybackQuality::P1080High,
    direct_play: DirectPlayMode::Forced,
    size: SubtitleSize::Large,
    position: SubtitlePosition::High,
    next_episode: NextEpisodeMode::AfterCredits,
    skip: SkipInterval::Seconds30,
    deck_press: DeckPress::Play,
    tone: SubtitleTone::Silver,
    enhancements: AudioEnhancements { boost_dialog: true, normalize_loudness: true },
};

/// Nine values that are neither the defaults nor [`STORED`]: the live cells standing where a
/// restore has not yet run, or where something other than the record last wrote them. The deck
/// press has two values only, so it cannot differ from both: it differs from [`STORED`], and a
/// restore dropped for it is invisible only against a record that chose nothing.
const SCRAMBLED: Cells = Cells {
    quality: PlaybackQuality::P720Low,
    direct_play: DirectPlayMode::Disabled,
    size: SubtitleSize::ExtraLarge,
    position: SubtitlePosition::Middle,
    next_episode: NextEpisodeMode::Off,
    skip: SkipInterval::Seconds60,
    deck_press: DeckPress::Details,
    tone: SubtitleTone::Grey,
    enhancements: AudioEnhancements { boost_dialog: false, normalize_loudness: true },
};

/// A second profile's own values: not the defaults, not [`STORED`] (profile A's), not [`SCRAMBLED`] in
/// the settings that have room for a fourth value.
const OTHER: Cells = Cells {
    quality: PlaybackQuality::P1080,
    direct_play: DirectPlayMode::Disabled,
    size: SubtitleSize::Small,
    position: SubtitlePosition::Middle,
    next_episode: NextEpisodeMode::Off,
    skip: SkipInterval::Seconds5,
    deck_press: DeckPress::Play,
    tone: SubtitleTone::LightGrey,
    enhancements: AudioEnhancements { boost_dialog: true, normalize_loudness: false },
};

/// Put the nine cells where a test says, by the same `restore_*` doors the restore lists use.
fn put(c: Cells) {
    plx_media::route::restore_quality(c.quality);
    plx_media::route::restore_direct_play_mode(c.direct_play);
    plx_media::route::restore_subtitle_size(c.size);
    plx_media::route::restore_subtitle_position(c.position);
    plx_media::route::restore_next_episode_mode(c.next_episode);
    plx_media::route::restore_skip_interval(c.skip);
    plx_media::route::restore_deck_press(c.deck_press);
    plx_media::player::restore_subtitle_tone(c.tone);
    plx_media::player::restore_audio_enhancements(c.enhancements);
}

/// A record carrying [`STORED`] in its nine install keys.
fn with_stored_installs(s: Session) -> Session {
    s.with_pref::<plx_plex::plex::session::QualityPref>(STORED.quality)
        .with_pref::<plx_plex::plex::session::DirectPlayModePref>(STORED.direct_play)
        .with_pref::<plx_plex::plex::session::SubtitleSizePref>(STORED.size)
        .with_pref::<plx_plex::plex::session::SubtitlePositionPref>(STORED.position)
        .with_pref::<plx_plex::plex::session::NextEpisodeModePref>(STORED.next_episode)
        .with_pref::<plx_plex::plex::session::SkipIntervalPref>(STORED.skip)
        .with_pref::<plx_plex::plex::session::DeckPressPref>(STORED.deck_press)
        .with_pref::<plx_plex::plex::session::SubtitleTonePref>(STORED.tone)
        .with_pref::<plx_plex::plex::session::AudioEnhancementsPref>(STORED.enhancements)
}

/// `s` with the profile `uuid`'s OWN value of all nine settings set to `c`, under the stamp of `s`'s
/// account: what a profile that picked every one of them has in its record (P1a).
fn with_own_values(mut s: Session, uuid: &str, c: Cells) -> Session {
    let stamp = plx_plex::plex::grant::account_key(&s.account_token);
    let rows = &mut s.prefs.profile_prefs;
    put_own::<QualityPref>(rows, uuid, &stamp, c.quality);
    put_own::<DirectPlayModePref>(rows, uuid, &stamp, c.direct_play);
    put_own::<SubtitleSizePref>(rows, uuid, &stamp, c.size);
    put_own::<SubtitlePositionPref>(rows, uuid, &stamp, c.position);
    put_own::<NextEpisodeModePref>(rows, uuid, &stamp, c.next_episode);
    put_own::<SkipIntervalPref>(rows, uuid, &stamp, c.skip);
    put_own::<DeckPressPref>(rows, uuid, &stamp, c.deck_press);
    put_own::<SubtitleTonePref>(rows, uuid, &stamp, c.tone);
    put_own::<AudioEnhancementsPref>(rows, uuid, &stamp, c.enhancements);
    s
}

const MACHINE: &str = "a0c-machine";

fn source(token: &str) -> SourceRef {
    SourceRef { machine_id: MACHINE.into(), name: "A0c".into(), address: "127.0.0.1".into(), port: 32400,
        origin_url: "http://127.0.0.1:32400".into(), token: token.into(), owned: true,
        tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default() }
}

fn server(token: &str) -> ServerRef {
    ServerRef { machine_id: MACHINE.into(), name: "A0c".into(), address: "127.0.0.1".into(), port: 32400,
        origin_url: "http://127.0.0.1:32400".into(), token: token.into(), ..Default::default() }
}

fn user(uuid: &str) -> UserRef {
    UserRef { uuid: uuid.into(), title: uuid.into(), token: format!("{uuid}-token"), ..Default::default() }
}

/// A signed-in record with the profile `seated` standing and the others cached for an offline
/// switch. `""` is the owner, with no Home selection.
fn record(seated: &str, others: &[&str]) -> Session {
    let all: Vec<&str> = std::iter::once(seated).chain(others.iter().copied()).collect();
    let token = format!("{seated}-token");
    Session {
        client_id: "a0c-client".into(),
        account_token: "a0c-account".into(),
        user: user(seated),
        server: server(&token),
        sources: vec![source(&token)],
        home_users: if seated.is_empty() { Vec::new() } else {
            all.iter().map(|u| HomeUserRef { uuid: (*u).into(), title: (*u).into(), ..Default::default() }).collect()
        },
        profiles: all.iter().filter(|u| !u.is_empty()).map(|u| ProfileCreds { uuid: (*u).into(), user: user(u),
            server: server(&format!("{u}-token")), sources: vec![source(&format!("{u}-token"))], pin: None,
            extensions: Default::default() }).collect(),
        ..Default::default()
    }
}

/// The scratch session store, the server registry, and the nine cells all put back on drop. Declared
/// after `testlock::serial()` so it drops first, while the lock that `restore_*` asserts is held.
struct Fixture {
    _session: session::TempSession,
}

impl Fixture {
    fn new(tag: &str, record: &Session) -> Self {
        plx_plex::plex::reset_servers_for_test();
        plx_media::route::unseed_profile_prefs_for_test();
        let session = session::TempSession::new(tag);
        session::save(record);
        put(SCRAMBLED);
        Self { _session: session }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        put(DEFAULTS);
        plx_media::route::unseed_profile_prefs_for_test();
        plx_plex::plex::reset_servers_for_test();
    }
}

/// The value profile `uuid` has persisted for `S` in the session file, if it picked one.
fn stored_value<S: session::ProfileSetting>(uuid: &str) -> Option<S::Value> {
    let record = session::load();
    let stamp = plx_plex::plex::grant::account_key(&record.account_token);
    session::own_prefs(&record.prefs.profile_prefs, uuid, &stamp).and_then(S::own)
}

/// Seat `uuid` in the process global, as the live publisher does at a profile's commit. The fixture
/// adapter keeps a publication to itself and never touches the global, so a test that picks through the
/// doors (which capture the global seat) seats explicitly. The scratch session's drop unseats.
fn seat_globally(uuid: &str) {
    let stamp = plx_plex::plex::grant::account_key(&session::peek().account_token);
    session::publish_seat_for_test(Some(user(uuid)), session::current_gen().wrapping_add(1), stamp);
}

fn drain(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, records: Vec<SessionEnvelope>, at: u32) {
    let results = records.into_iter().map(|r| (r.addr, AppMsg::Session(SessionEvent::Result(r)))).collect();
    d.frame_with(rig, tick(at), Vec::new(), results, &mut NoTap, false);
    rig.settle_session_io_for_test(d);
}

/// **The stored-session boot, up to the handoff `follow_auth_landing` consumes.** `ResumeStored`
/// (boot.rs) seats the stored profile and emits `Ready`.
fn resume(stored: &Session) -> (Bridge, Dispatcher<AppHost>) {
    let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, SessionCmd::ResumeStored);
    drain(&mut rig, &mut d, Vec::new(), 1);
    assert_eq!(rig.auth_read().0.phase, Phase::Ready);
    (rig, d)
}

/// The offline profile switch: the real worker policy with a plex.tv that cannot be reached, so the
/// cached credentials of the chosen profile seat (the same rig as
/// `session_profile_regression_tests::real_profile_worker_injection_reaches_owner_and_handoff_without_global_state`).
struct OfflineIo;
impl plx_session::auth::ProfileWorkIo for OfflineIo {
    fn switch(&mut self, _: &plx_plex::plex::account::AccountClient, _: &str, _: Option<&str>)
        -> plx_plex::plex::account::SwitchOutcome {
        plx_plex::plex::account::SwitchOutcome::Unreachable
    }
    fn resources(&mut self, _: &plx_plex::plex::account::AccountClient)
        -> Result<Vec<plx_plex::plex::account::Resource>, plx_plex::plex::account::CallEvidence> {
        panic!("offline seating must not fetch resources")
    }
    fn probe(&mut self, _: &plx_plex::plex::account::Resource, _: &[i64])
        -> (Option<SourceRef>, plx_session::auth::SettledProbe) {
        panic!("offline seating must not probe")
    }
    fn gap(&mut self) { panic!("offline seating has no probe gap") }
}

/// What the loop does on the Account menu's "Change profile", up to the picker standing: the owner
/// starts a switch and the container drops every page of the outgoing profile.
fn open_picker(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, at: u32) {
    execute_session_command(d, SessionCmd::StartSwitch(Picker::ChangeProfile));
    switch_profile(d);
    for i in 0..24 { super::frame(d, rig, tick(at + i), vec![]); }
    assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
    assert!(d.nav.top_page().is_some_and(|e| e.arg == AppArg::Profiles));
}

/// Pick tile `index` and run the worker to the `Ready` it ends in; the handoff is left for
/// `follow_auth_landing` to consume.
fn pick(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, index: usize, at: u32) {
    let req = rig.session.snapshot_init().next_req + 1;
    let epoch = rig.auth_read().0.flow_epoch + 1;
    rig.session_adapter.inject_fixture_work(req, move |output, input| {
        let SessionWork::ProfileSwitch { session, tile, pin, recently_unreachable, .. } = input
            else { panic!("a pick launched another worker family") };
        let expected = plx_session::auth::SessionIdentity::of(&session);
        plx_session::auth::profile_switch_worker_with_io(epoch, expected, session, tile, pin,
            recently_unreachable, &output, &mut OfflineIo);
    });
    execute_session_command(d, SessionCmd::SelectProfile { index, pin: None });
    d.frame_with(rig, tick(at), Vec::new(), Vec::new(), &mut NoTap, false);
    assert_eq!(rig.auth_read().0.phase, Phase::Switching);
    let records = rig.session_adapter.take_results();
    assert_eq!(records.len(), 1);
    drain(rig, d, records, at + 1);
    assert_eq!(rig.auth_read().0.phase, Phase::Ready);
    execute_session_command(d, SessionCmd::TakeReady);
    d.frame_with(rig, tick(at + 2), Vec::new(), Vec::new(), &mut NoTap, false);
}

/// **A0 item 11.** Boot and `follow_auth_landing` set identical values in all nine cells, for a
/// stored-resume boot with a managed Home profile seated and for one with the owner, with the dev
/// overrides held fixed (no `/tmp/plxnative-quality`, `controlled = false`).
///
/// On `main` the values are the record's nine install keys whoever is seated; the seat does not
/// enter either list. That still holds for a profile with no values of its own (it reads the same
/// legacy keys boot does). The per-profile work flips the case where the seated profile HAS values of
/// its own (plan P-d8): [`boot_sets_the_fallback_and_the_landing_sets_the_seated_profiles_own_values`].
#[test]
fn boot_and_the_auth_landing_set_the_same_nine_cells_for_a_managed_profile_and_for_the_owner() {
    let _serial = plx_base::testlock::serial();
    assert!(crate::dev::playback_quality_override().is_none(), "the dev quality override is held fixed: none");
    for (case, seated, others) in [("managed", "managed-kid", &["managed-other"][..]), ("owner", "", &[][..])] {
        for (label, stored, expected) in [
            ("installs set", with_stored_installs(record(seated, others)), STORED),
            ("nothing ever chosen", record(seated, others), DEFAULTS),
        ] {
            let fixture = Fixture::new("a0c-boot-vs-landing", &stored);

            // Boot: the list `construct` runs before any profile is published, over the record it
            // loaded, with `controlled = false`.
            put(SCRAMBLED);
            crate::app::boot::restore_player_preferences(&session::load(), false);
            let at_boot = cells();

            // The landing: the stored-resume `Ready`, then the restore `follow_auth_landing` runs.
            put(SCRAMBLED);
            let (mut rig, mut d) = resume(&stored);
            follow_auth_landing(&mut d, &mut rig);
            let at_landing = cells();

            assert_eq!(at_boot, at_landing, "{case}/{label}: boot and the landing diverged");
            assert_eq!(at_boot, expected, "{case}/{label}: the cells are the record's install keys");
            drop(rig);
            drop(fixture);
        }
    }
}

/// **B1 reproduced (critique r4), and its fix.** A stored-session boot with a managed Home profile
/// seated, whose own values differ from the legacy install-wide keys. The process starts `Unseated`
/// (`restore_player_preferences`: every cell holds the fallback), then `ResumeStored` seats the
/// stored profile and emits `Ready` in the same drain, and the stored boot never reaches
/// `follow_auth_landing` (that runs only for a login or a picker), so the profile's own values reach
/// the cells only if SEATING restores them. Draft 7 restored at the landing alone and left them on
/// the legacy values until the first switch.
#[test]
fn stored_boot_restores_the_seated_profiles_own_values() {
    let _serial = plx_base::testlock::serial();
    let stored = with_own_values(record("managed-kid", &["managed-other"]), "managed-kid", STORED);
    let _fixture = Fixture::new("a0c-stored-boot-own", &stored);
    assert_eq!(session::peek().prefs.subtitle_size, SubtitleSize::Medium, "the legacy key chose nothing");

    crate::app::boot::restore_player_preferences(&session::load(), false);
    assert_eq!(cells(), DEFAULTS, "process start is unseated: the fallback, here the defaults");

    let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
    let mut d = Dispatcher::<AppHost>::new();
    let ready = crate::app::boot::resume_stored(&mut d, &mut rig, &mut NoTap);
    assert!(ready.is_some(), "ResumeStored ended in the Ready handoff");
    assert_eq!(rig.auth_read().0.phase, Phase::Ready);

    assert_eq!(cells(), STORED, "the seated profile's own values, not the legacy ones");
}

/// **A0 item 11, flipped for a seated profile by P1a (P-d8).** A stored-resume boot with a managed Home
/// profile seated, and one with the owner, each profile holding its own value of all nine settings
/// over legacy install-wide keys that say something else. Boot sets the FALLBACK (nobody is published
/// yet: the legacy keys), and the seat the landing's `ResumeStored` publishes sets the profile's own
/// values. On `main` the two were identical because the seat entered neither list.
#[test]
fn boot_sets_the_fallback_and_the_landing_sets_the_seated_profiles_own_values() {
    let _serial = plx_base::testlock::serial();
    assert!(crate::dev::playback_quality_override().is_none(), "the dev quality override is held fixed: none");
    for (case, seated, others) in [("managed", "managed-kid", &["managed-other"][..]), ("owner", "", &[][..])] {
        let stored = with_own_values(with_stored_installs(record(seated, others)), seated, OTHER);
        let fixture = Fixture::new("a0c-boot-vs-landing-own", &stored);

        put(SCRAMBLED);
        crate::app::boot::restore_player_preferences(&session::load(), false);
        let at_boot = cells();

        put(SCRAMBLED);
        let (mut rig, mut d) = resume(&stored);
        follow_auth_landing(&mut d, &mut rig);
        let at_landing = cells();

        assert_eq!(at_boot, STORED, "{case}: boot holds the legacy install-wide keys: nobody is seated yet");
        assert_eq!(at_landing, OTHER, "{case}: the landing holds the seated profile's own values");
        drop(rig);
        drop(fixture);
    }
}

fn settle(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, from: u32) {
    for i in 0..24 { super::frame(d, rig, tick(from + i * 16), vec![]); }
}

fn stack_has_player(d: &Dispatcher<AppHost>) -> bool {
    d.nav.tabs.stack.entries.iter().any(|e| matches!(e.arg, AppArg::Player))
}

/// **A0 item 13, flipped by P1a (P-d6).** A profile switch restores the NEW seat's values, and the
/// seat does it, not the landing. Profile A seated and landed, A's player up, then `switch_profile`,
/// B's `Ready`, `follow_auth_landing`.
///
/// On `main` the landing re-published the record's install values, identical to the ones A had, so
/// the cells never moved. Now each seat change restores: ResumeStored seats A (A's own values, before
/// the `Ready` the landing consumes), "Change profile" unseats (the cells hold the fallback while the
/// picker stands), and B's commit seats B (B's own values, again before `Ready`). The landing restores
/// nothing: the cells are scrambled after B's `Ready`, and the landing leaves them where they are.
/// The player page is retired before `Ready` either way.
#[test]
fn profile_switch_restores_the_new_profiles_values() {
    let _serial = plx_base::testlock::serial();
    let stored = with_own_values(with_own_values(record("profile-a", &["profile-b"]), "profile-a", STORED),
        "profile-b", OTHER);
    let _fixture = Fixture::new("a0c-switch", &stored);

    let (mut rig, mut d) = resume(&stored);
    assert_eq!(cells(), STORED, "A seated: the seat change restored A's own values, before any landing");
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    assert_eq!(cells(), STORED, "...and the landing left them");
    let top = d.nav.top_page().map(|e| e.arg.id());
    assert!(top.is_some(), "A's landing mounted a page: {top:?}");

    // A is playing something.
    frame(&mut d, &mut rig, AppArg::Player, tick(1000), vec![]);
    settle(&mut rig, &mut d, 1100);
    assert!(stack_has_player(&d), "A's player is on the stack");

    open_picker(&mut rig, &mut d, 2000);
    assert!(!stack_has_player(&d), "the player page is retired before B's Ready");
    assert_eq!(d.nav.tabs.stack.entries.len(), 1, "nothing of A's tree stands but the picker");
    assert_ne!(rig.auth_read().0.phase, Phase::Ready, "...and Ready has not arrived yet");
    assert_eq!(cells(), DEFAULTS, "the picker unseats: nobody owns a value, the cells hold the fallback");

    pick(&mut rig, &mut d, 1, 3000);
    assert_eq!(cells(), OTHER, "B's commit seated B: B's own values, restored before Ready is consumed");
    put(SCRAMBLED);
    follow_auth_landing(&mut d, &mut rig);
    assert_eq!(cells(), SCRAMBLED, "the landing restores nothing: the seat did");
}

/// **A0 item 14, flipped by P1a (P-d1, P-d2).** The nine settings are each profile's own. Profile A
/// picks subtitle size Large and Direct Play Force through the doors Settings uses; profile B is
/// seated and reads neither: its record has no value of its own, so it reads the legacy install-wide
/// key, which the picks never touched. B picks Force for itself, its playback fails, and B's failure
/// read-out "Play Automatically" moves B's Direct Play to Auto. A, seated again, still reads Large and
/// Force: B reset only B. On `main` the one shared key made B read A's Large and Force, and B's reset
/// reset A too.
///
/// The fixture adapter does not write the session file at a switch's commit, so each switch below
/// writes the new `user` into the record itself, the one thing a commit changes there, and seats the
/// process global that the doors capture (the fixture adapter keeps its publication to itself).
#[test]
fn profile_picks_are_each_profiles_own_and_play_automatically_on_b_leaves_a_alone() {
    let _serial = plx_base::testlock::serial();
    let stored = record("profile-a", &["profile-b"]);
    let _fixture = Fixture::new("a0c-shared", &stored);
    let seat = |uuid: &str| {
        let uuid = uuid.to_owned();
        assert!(session::update(move |s| Some(Session { user: user(&uuid), ..s.clone() })));
    };

    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    seat_globally("profile-a");
    assert_eq!(cells(), DEFAULTS, "A seated on a record that chose nothing");

    // Profile A picks Large and Force.
    plx_media::route::select_subtitle_size(SubtitleSize::Large, None);
    plx_base::storage_worker::drain_for_test();
    assert!(plx_media::route::set_direct_play_mode(DirectPlayMode::Forced));
    assert_eq!(stored_value::<SubtitleSizePref>("profile-a"), Some(SubtitleSize::Large));
    assert_eq!(stored_value::<DirectPlayModePref>("profile-a"), Some(DirectPlayMode::Forced));
    let a = session::load();
    assert_eq!((a.subtitle_size(), a.direct_play_mode()), (SubtitleSize::Medium, DirectPlayMode::Auto),
        "A's picks are A's own: the legacy install-wide keys are untouched");

    // Profile B is seated and reads the fallback, not A's picks.
    open_picker(&mut rig, &mut d, 2000);
    pick(&mut rig, &mut d, 1, 3000);
    seat("profile-b");
    follow_auth_landing(&mut d, &mut rig);
    seat_globally("profile-b");
    assert_eq!(session::peek().user.uuid, "profile-b");
    assert_eq!(stored_value::<SubtitleSizePref>("profile-b"), None, "B has picked nothing");
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Medium, "B reads Medium, not A's Large");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto, "B reads Auto, not A's Force");

    // B forces Direct Play itself, its playback fails under Force, and B presses Play Automatically.
    assert!(plx_media::route::set_direct_play_mode(DirectPlayMode::Forced));
    let mut player = plx_media::player::machine::Player::new();
    let mut adapter = plx_media::player::adapter::PlayerAdapter::new(unsafe {
        plx_base::task::MainThread::assume()
    });
    assert!(!crate::app::playback::play_automatically(&mut player.session, &mut adapter, rig.metadata_mut()),
        "there is no reusable Plex request to retry, so only the preference moves");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto, "B's live cell moved at once");
    plx_base::storage_worker::drain_for_test();
    assert_eq!(stored_value::<DirectPlayModePref>("profile-b"), Some(DirectPlayMode::Auto), "...and B's record");
    assert_eq!(stored_value::<DirectPlayModePref>("profile-a"), Some(DirectPlayMode::Forced), "A's was not reset");

    // A is seated again and finds its own picks.
    open_picker(&mut rig, &mut d, 4000);
    pick(&mut rig, &mut d, 0, 5000);
    seat("profile-a");
    follow_auth_landing(&mut d, &mut rig);
    seat_globally("profile-a");
    assert_eq!(session::peek().user.uuid, "profile-a");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Forced, "B's Play Automatically left A alone");
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Large, "A's size survived");
}

/// **Declared delta 3: Play Automatically publishes Auto once.** It used to publish twice, once on
/// the caller's thread and again from the storage worker when the queued Confirm write came back
/// Durable (which could overwrite a newer pick). The one door publishes on the caller's thread and
/// persists behind it, never republishing; the publish counter on the cell proves it.
#[test]
fn play_automatically_publishes_direct_play_auto_once() {
    use plx_media::route::publishes_for_test;
    use plx_plex::plex::session::DirectPlayModePref;
    let _serial = plx_base::testlock::serial();
    let stored = record("profile-a", &[]);
    let _fixture = Fixture::new("a0c-play-auto-once", &stored);

    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    seat_globally("profile-a");
    assert!(plx_media::route::set_direct_play_mode(DirectPlayMode::Forced));
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Forced);

    let mut player = plx_media::player::machine::Player::new();
    let mut adapter = plx_media::player::adapter::PlayerAdapter::new(unsafe {
        plx_base::task::MainThread::assume()
    });
    let before = publishes_for_test::<DirectPlayModePref>();
    crate::app::playback::play_automatically(&mut player.session, &mut adapter, rig.metadata_mut());
    plx_base::storage_worker::drain_for_test();
    assert_eq!(publishes_for_test::<DirectPlayModePref>() - before, 1, "published exactly once");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto);
    assert_eq!(stored_value::<DirectPlayModePref>("profile-a"), Some(DirectPlayMode::Auto), "and persisted behind it");
}

// ---- the seat trigger: what each way of arriving at a seat restores -----------------------------

/// The stamp of the A0c account.
fn stamp_of(token: &str) -> String {
    plx_plex::plex::grant::account_key(token)
}

/// The seat of `uuid` on the A0c account.
fn seat_of(uuid: &str) -> plx_plex::plex::session::Seat {
    plx_plex::plex::session::Seat::Seated { user: uuid.into(), stamp: stamp_of("a0c-account") }
}

fn size_publishes() -> u32 {
    plx_media::route::publishes_for_test::<SubtitleSizePref>()
}

/// **Case 1 of the boot sequence (plan 3.8.5a), with auto sign-in on.** The Home has two profiles,
/// auto sign-in is on and the stored profile is in the roster, so there is NO picker: `ResumeStored`
/// seats the stored profile in the same drain and the profile's own values are in the cells before the
/// handoff is consumed. (`stored_boot_restores_the_seated_profiles_own_values` is the same boot with
/// the roster and the switch left implicit; this one states the precondition.)
#[test]
fn boot_auto_sign_in_restores_the_seated_profile() {
    let _serial = plx_base::testlock::serial();
    let mut stored = with_own_values(record("profile-b", &["profile-a"]), "profile-b", OTHER);
    stored.prefs.auto_sign_in = true;
    assert!(!stored.boot_shows_picker(false, false), "auto sign-in on and seated in the roster: no picker");
    let _fixture = Fixture::new("a0c-boot-auto", &stored);

    crate::app::boot::restore_player_preferences(&session::load(), false);
    assert_eq!(cells(), DEFAULTS, "process start: nobody is seated");
    let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
    let mut d = Dispatcher::<AppHost>::new();
    assert!(crate::app::boot::resume_stored(&mut d, &mut rig, &mut NoTap).is_some());
    assert_eq!(rig.seat_watch.restored, seat_of("profile-b"));
    assert_eq!(cells(), OTHER, "the stored profile's own values, before Home mounts");
}

/// **Case 2: the boot picker (plan 3.8.5a).** Auto sign-in off on a Home with two profiles:
/// "Who's watching?" is up and Home has not mounted, so no handoff has been released, and the pick's
/// `release_held_handoff` seats the CHOSEN profile, whose own values are restored before the `Ready` the
/// landing consumes. Nothing that reads a Profile value runs before the pick.
///
/// **What the owner does at `StartSwitch(Picker::Boot)`, which the plan's table gets wrong.** The plan
/// says nothing is published until a viewer is chosen. The owner's `initial_profile` commit
/// (`start_switch`: a boot picker with no active profile and a stored session that can go local) does
/// publish the STORED profile when its registry commit lands, with `activate_profile` and no `Ready`,
/// because the roster's own avatar requests need a primary and a profile behind it. So the seat is the
/// stored profile's while the picker stands, and its values are in the cells; the pick then replaces
/// that seat (or keeps it, when the viewer picks the stored profile, whose cells already hold its
/// values). The landing sees the chosen profile's values either way, which is the property that
/// matters; the stored profile's values are never read by anything while the picker is up.
#[test]
fn boot_picker_restores_only_after_the_pick() {
    let _serial = plx_base::testlock::serial();
    let stored = with_own_values(with_own_values(record("profile-a", &["profile-b"]), "profile-a", STORED),
        "profile-b", OTHER);
    assert!(stored.boot_shows_picker(false, false), "auto sign-in off on a multi-user Home: the picker");
    let _fixture = Fixture::new("a0c-boot-picker", &stored);

    crate::app::boot::restore_player_preferences(&session::load(), false);
    assert_eq!(cells(), DEFAULTS, "process start: nobody is seated");
    let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
    let mut d = Dispatcher::<AppHost>::new();
    execute_session_command(&mut d, SessionCmd::StartSwitch(Picker::Boot));
    drain(&mut rig, &mut d, Vec::new(), 1);
    assert_eq!(rig.auth_read().0.phase, Phase::Profiles, "Who's watching? is up");
    assert!(rig.session_ready.is_none(), "and no handoff was released: Home has not mounted");
    let published = rig.session_adapter.fixture_resources().profile.as_ref()
        .and_then(|p| p.profile.as_ref().map(|u| u.uuid.clone()));
    assert_eq!(published.as_deref(), Some("profile-a"), "the owner seats the stored profile behind the picker");
    assert_eq!(rig.seat_watch.restored, seat_of("profile-a"));
    assert_eq!(cells(), STORED, "...and its own values are in the cells, which nothing reads yet");

    pick(&mut rig, &mut d, 1, 3000);
    assert_eq!(rig.seat_watch.restored, seat_of("profile-b"));
    assert_eq!(cells(), OTHER, "the pick seated profile-b: its own values, restored before Ready is consumed");
}

/// **Case 3: a fresh sign-in as the account owner (plan 3.8.5a).** The owner with no Home selection
/// (uuid `""`) is published as a user, not as nobody, so it is seated and reads ITS values: the
/// fallback on a new install. Driven through `ResumeStored`, which releases the handoff through the
/// same `release_held_handoff` a fresh single-user sign-in does; the QR and discovery front half is
/// the session crate's.
#[test]
fn first_login_single_user_seats_the_owner() {
    let _serial = plx_base::testlock::serial();
    let stored = record("", &[]);
    let _fixture = Fixture::new("a0c-first-login-owner", &stored);

    crate::app::boot::restore_player_preferences(&session::load(), false);
    put(SCRAMBLED); // so only the seat can put the owner's fallback in the cells
    let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
    let mut d = Dispatcher::<AppHost>::new();
    assert!(crate::app::boot::resume_stored(&mut d, &mut rig, &mut NoTap).is_some());
    let publication = rig.session_adapter.fixture_resources().profile.clone().expect("the owner was published");
    assert_eq!(publication.profile.as_ref().map(|u| u.uuid.as_str()), Some(""), "as a user, not as nobody");
    assert_eq!(publication.stamp, stamp_of("a0c-account"), "under the stamp of the account token");
    assert_eq!(rig.seat_watch.restored, seat_of(""), "so the owner is seated");
    assert_eq!(cells(), DEFAULTS, "a new install's owner reads the fallback");

    // Its own pick lands in ITS record, under that stamp.
    seat_globally("");
    plx_media::route::select_subtitle_size(SubtitleSize::Large, None);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(stored_value::<SubtitleSizePref>(""), Some(SubtitleSize::Large));
}

/// **A dev-token boot never seats (plan 3.8.5).** `ActivateDevBootstrap` publishes nobody, so the cells
/// keep the fallback (the legacy keys) and never the owner's record, and the dev audio-enhancement arm
/// sets the live cell and saves nothing.
#[test]
fn dev_token_boot_is_unseated_and_reads_the_fallback() {
    let _serial = plx_base::testlock::serial();
    assert!(plx_base::devtrig::ENABLED, "this test arms a dev trigger, which a build without devtriggers cannot");
    let stored = with_own_values(with_stored_installs(record("", &[])), "", OTHER);
    let _fixture = Fixture::new("a0c-dev-token", &stored);
    let loudness = AudioEnhancements { boost_dialog: false, normalize_loudness: true };

    plx_base::devtrig::with_private_triggers(|| {
        std::fs::write(plx_base::devtrig::path("audioenh"), "loudness").unwrap();
        crate::app::boot::restore_player_preferences(&session::load(), false);
        crate::dev::scenarios::arm_audio_enhancements();
        let mut rig = Bridge::for_session_test(crate::app::boot::captured_session_for_boot(
            stored.clone(), Some(server("dev-token")), Vec::new()));
        rig.session_adapter.fixture_resources().disk = stored.clone();
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, SessionCmd::ActivateDevBootstrap);
        drain(&mut rig, &mut d, Vec::new(), 1);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready, "the dev token reached Ready");

        assert_eq!(rig.seat_watch.restored, plx_plex::plex::session::Seat::Unseated, "and nobody was seated");
        assert_eq!(cells(), Cells { enhancements: loudness, ..STORED },
            "the legacy keys, not the owner's own values; only the armed enhancement differs");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(stored_value::<AudioEnhancementsPref>(""), Some(OTHER.enhancements),
            "nothing was saved for nobody");
        assert_eq!(session::load().audio_enhancements(), STORED.enhancements, "nor over the legacy key");
    });
}

/// **The dev audio-enhancement arm writes the seated profile (P-d8).** Boot runs the arm before anyone
/// is seated (the live cell only, nothing saved); the seat trigger runs it again right after the
/// restore of the first seating, and that write lands in the SEATED profile's record, not in the
/// legacy key and not in another profile's. It is one-shot: a later seat does not force the next
/// profile's record.
#[test]
fn audioenh_arm_writes_the_seated_profile() {
    let _serial = plx_base::testlock::serial();
    assert!(plx_base::devtrig::ENABLED, "this test arms a dev trigger, which a build without devtriggers cannot");
    let stored = with_own_values(with_own_values(record("guest", &["profile-b"]), "guest", OTHER),
        "profile-b", STORED);
    let _fixture = Fixture::new("a0c-audioenh-arm", &stored);
    let loudness = AudioEnhancements { boost_dialog: false, normalize_loudness: true };

    plx_base::devtrig::with_private_triggers(|| {
        std::fs::write(plx_base::devtrig::path("audioenh"), "loudness").unwrap();
        // Boot, with nobody seated.
        crate::app::boot::restore_player_preferences(&session::load(), false);
        crate::dev::scenarios::arm_audio_enhancements();
        assert_eq!(plx_media::player::audio_enhancements(), loudness, "unseated: the live cell is forced");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(stored_value::<AudioEnhancementsPref>("guest"), Some(OTHER.enhancements), "...and nothing is saved");

        // The stored boot seats the guest. The fixture adapter keeps its publication to itself, so the
        // process global the write door captures is seated as the live publisher would.
        seat_globally("guest");
        let (mut rig, mut d) = resume(&stored);
        assert_eq!(rig.seat_watch.restored, seat_of("guest"));
        assert_eq!(plx_media::player::audio_enhancements(), loudness);
        plx_base::storage_worker::drain_for_test();
        assert_eq!(stored_value::<AudioEnhancementsPref>("guest"), Some(loudness),
            "the seated profile's record is forced");
        assert_eq!(stored_value::<AudioEnhancementsPref>("profile-b"), Some(STORED.enhancements), "nobody else's");
        assert_eq!(session::load().audio_enhancements(), AudioEnhancements::NONE, "nor the legacy key");

        // One-shot: the next profile seated is not forced.
        follow_auth_landing(&mut d, &mut rig);
        settle(&mut rig, &mut d, 100);
        open_picker(&mut rig, &mut d, 2000);
        seat_globally("profile-b");
        pick(&mut rig, &mut d, 1, 3000);
        assert_eq!(rig.seat_watch.restored, seat_of("profile-b"));
        assert_eq!(plx_media::player::audio_enhancements(), STORED.enhancements, "profile-b reads its own");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(stored_value::<AudioEnhancementsPref>("profile-b"), Some(STORED.enhancements), "the arm ran once");
    });
}

/// **The dev track-memory clear runs at the first seat (track memory C4).** `plxnative-trackmemory=clear`
/// forgets what the SEATED profile remembered, so the harness's seated cases start from empty
/// memory. Boot runs nothing for it (nobody is seated, and the memory door refuses for nobody); the
/// seat trigger runs it beside the audio-enhancement arm, once, with the profile it seated. An
/// ADDED test: the cases above are as they were.
#[test]
fn trackmemory_arm_forgets_the_first_seated_profiles_shows() {
    use plx_plex::plex::session::prefs::{AudioMemo, TrackMemoryEntry};
    let _serial = plx_base::testlock::serial();
    assert!(plx_base::devtrig::ENABLED, "this test arms a dev trigger, which a build without devtriggers cannot");
    let stored = record("guest", &["profile-b"]);
    let _fixture = Fixture::new("a0c-trackmemory-arm", &stored);
    let held = |uuid: &str| plx_media::route::recall_all::<TrackMemoryEntry>(uuid).len();

    plx_base::devtrig::with_private_triggers(|| {
        std::fs::write(plx_base::devtrig::path("trackmemory"), "clear").unwrap();
        crate::app::boot::restore_player_preferences(&session::load(), false);
        // Something remembered before the boot's seat, as an earlier case's pick would have left it.
        seat_globally("guest");
        let japanese = AudioMemo { lang: "jpn".into(), codec: "eac3".into(), channels: 6, ..Default::default() };
        let entry = TrackMemoryEntry::new(MACHINE, "show-1", Some(japanese), None);
        assert!(plx_media::route::remember::<TrackMemoryEntry>("guest", (MACHINE.into(), "show-1".into()),
            move |_| Some(entry.clone())));
        assert_eq!(held("guest"), 1, "setup: the guest remembered a show");

        let (rig, _d) = resume(&stored);
        assert_eq!(rig.seat_watch.restored, seat_of("guest"), "the stored boot seated the guest");
        assert_eq!(held("guest"), 0, "and the seat trigger ran the clear for the profile it seated");
        plx_base::storage_worker::drain_for_test();
        assert!(session::load().prefs.track_memory.iter().all(|list| list.items.is_empty()),
            "the file forgot it too");
    });
    plx_media::route::unseed_memory_for_test::<TrackMemoryEntry>();
}

/// **A republication of the same seat restores nothing (plan 3.8.5).** A roster refresh or a
/// rediscovery publishes the profile again while a pick is in flight; the pick is not clobbered. The
/// cell's publish counter is the oracle: the value alone could not tell a restore of the same record
/// from no restore.
#[test]
fn same_seat_republication_restores_nothing() {
    let _serial = plx_base::testlock::serial();
    let stored = with_own_values(with_own_values(record("profile-a", &["profile-b"]), "profile-a", STORED),
        "profile-b", OTHER);
    let _fixture = Fixture::new("a0c-same-seat", &stored);
    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    seat_globally("profile-a");
    assert_eq!(rig.seat_watch.restored, seat_of("profile-a"));

    // A picks a size; before the write comes back, the same seat is published again.
    plx_media::route::select_subtitle_size(SubtitleSize::Small, None);
    let before = size_publishes();
    assert!(!rig.seat_watch.arrive(seat_of("profile-a")), "the same seat again is not a change");
    assert_eq!(size_publishes(), before, "so nothing was published");
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Small, "and the pick stands");

    assert!(rig.seat_watch.arrive(seat_of("profile-b")), "another seat is a change");
    assert_eq!(size_publishes(), before + 1, "and restores once");
    assert_eq!(cells(), OTHER);
}

/// **A -> picker -> B -> picker -> A restores at each change, once (plan 3.8.5).** The picker is an
/// `Unseated` stop (the cells hold the fallback), each commit seats a profile, and the landing restores
/// nothing, so a cell is published exactly once per seat change.
#[test]
fn seat_change_on_switch_restores_once() {
    let _serial = plx_base::testlock::serial();
    let stored = with_own_values(with_own_values(record("profile-a", &["profile-b"]), "profile-a", STORED),
        "profile-b", OTHER);
    let _fixture = Fixture::new("a0c-seat-changes", &stored);
    let start = size_publishes();
    let since = || size_publishes() - start;

    let (mut rig, mut d) = resume(&stored);
    assert_eq!((since(), cells()), (1, STORED), "ResumeStored seats A");
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    assert_eq!(since(), 1, "the landing restores nothing");

    open_picker(&mut rig, &mut d, 2000);
    assert_eq!((since(), cells()), (2, DEFAULTS), "the picker unseats");
    pick(&mut rig, &mut d, 1, 3000);
    assert_eq!((since(), cells()), (3, OTHER), "B's commit seats B");
    follow_auth_landing(&mut d, &mut rig);
    assert_eq!(since(), 3, "the landing restores nothing");

    open_picker(&mut rig, &mut d, 4000);
    assert_eq!((since(), cells()), (4, DEFAULTS), "the picker unseats again");
    pick(&mut rig, &mut d, 0, 5000);
    assert_eq!((since(), cells()), (5, STORED), "A is seated again, with A's values");
    follow_auth_landing(&mut d, &mut rig);
    assert_eq!(since(), 5);
}

/// **P-d1: a size profile A picked is not seen by profile B (hostsim switch).** A picks Large through
/// the door Settings uses; the switch seats B, which has picked nothing, so B reads the fallback and
/// not A's Large; A, seated again, finds Large.
#[test]
fn size_picked_by_profile_a_is_not_seen_by_profile_b() {
    let _serial = plx_base::testlock::serial();
    let stored = record("profile-a", &["profile-b"]);
    let _fixture = Fixture::new("a0c-size-a-not-b", &stored);
    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    seat_globally("profile-a");

    plx_media::route::select_subtitle_size(SubtitleSize::Large, None);
    plx_base::storage_worker::drain_for_test();
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Large);

    open_picker(&mut rig, &mut d, 2000);
    seat_globally("profile-b");
    pick(&mut rig, &mut d, 1, 3000);
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Medium, "B reads the fallback, not A's Large");
    assert_eq!(stored_value::<SubtitleSizePref>("profile-b"), None, "and has picked nothing");

    open_picker(&mut rig, &mut d, 4000);
    seat_globally("profile-a");
    pick(&mut rig, &mut d, 0, 5000);
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Large, "A finds its own pick");
}

/// **Play Automatically resets the active profile's Direct Play, and only that (plan 3.8.1).** A is
/// forced and B has a Direct Play of its own: the reset on A leaves B's record alone, and the legacy key
/// (which says something else) is never written.
#[test]
fn play_automatically_resets_the_active_profiles_direct_play_only() {
    let _serial = plx_base::testlock::serial();
    let stamp = stamp_of("a0c-account");
    let mut stored = record("profile-a", &["profile-b"]).with_pref::<DirectPlayModePref>(DirectPlayMode::Disabled);
    put_own::<DirectPlayModePref>(&mut stored.prefs.profile_prefs, "profile-a", &stamp, DirectPlayMode::Forced);
    put_own::<DirectPlayModePref>(&mut stored.prefs.profile_prefs, "profile-b", &stamp, DirectPlayMode::Disabled);
    let _fixture = Fixture::new("a0c-play-auto-active", &stored);

    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    seat_globally("profile-a");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Forced, "A is forced");

    let mut player = plx_media::player::machine::Player::new();
    let mut adapter = plx_media::player::adapter::PlayerAdapter::new(unsafe {
        plx_base::task::MainThread::assume()
    });
    crate::app::playback::play_automatically(&mut player.session, &mut adapter, rig.metadata_mut());
    plx_base::storage_worker::drain_for_test();
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto);
    assert_eq!(stored_value::<DirectPlayModePref>("profile-a"), Some(DirectPlayMode::Auto), "A's record moved");
    assert_eq!(stored_value::<DirectPlayModePref>("profile-b"), Some(DirectPlayMode::Disabled), "B's did not");
    assert_eq!(session::load().direct_play_mode(), DirectPlayMode::Disabled, "and the legacy key was never written");
}

/// **A forced Direct Play does not follow a profile switch (plan 3.8.1).** Profile A is forced; B has
/// no value of its own. After the switch the cell holds B's fallback, Auto, so a failure under A's
/// Force cannot be offered to, or inflicted on, B.
#[test]
fn forced_mode_does_not_follow_a_profile_switch() {
    let _serial = plx_base::testlock::serial();
    let mut stored = record("profile-a", &["profile-b"]);
    put_own::<DirectPlayModePref>(&mut stored.prefs.profile_prefs, "profile-a", &stamp_of("a0c-account"),
        DirectPlayMode::Forced);
    let _fixture = Fixture::new("a0c-forced-no-follow", &stored);

    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Forced);
    open_picker(&mut rig, &mut d, 2000);
    pick(&mut rig, &mut d, 1, 3000);
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto, "B reads the fallback, not A's Force");
    follow_auth_landing(&mut d, &mut rig);
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto);
}

// ---- hero trailer autoplay (P1c) ----------------------------------------------------------------

/// `s` with the profile `uuid`'s OWN hero-trailer switch set to `on`, under the stamp of `s`'s account,
/// and the TV's older install-wide key set to `legacy`: what a household that turned trailers off
/// before profiles had their own (`legacy` false) or never touched the setting (`legacy` true) has.
fn with_own_trailer(mut s: Session, uuid: &str, on: bool, legacy: bool) -> Session {
    let stamp = plx_plex::plex::grant::account_key(&s.account_token);
    s.prefs.trailer_autoplay = legacy;
    assert!(put_own::<TrailerAutoplayPref>(&mut s.prefs.profile_prefs, uuid, &stamp, on));
    s
}

/// What the detail page's hero asks every frame (`preview::enabled`), which is the decision P1c moved onto the
/// seat: it reads the trailer cell, nothing else.
fn hero_may_autoplay() -> bool {
    plx_media::player::preview::enabled()
}

/// Put the trailer cell back to its seed (on) so the next test starts from a process that restored
/// nothing.
struct TrailerCell;
impl Drop for TrailerCell {
    fn drop(&mut self) {
        plx_media::player::preview::restore_trailer_autoplay(true);
    }
}

/// **The hero trailer decision follows the seat, in each boot path (plan 3.8.5, P1c).** Each
/// case is built so that a read at the wrong time is visibly wrong: the answer BEFORE the seat is the TV's
/// key, the answer AFTER it is the profile's own value, and the two differ. `preview::enabled()` is the
/// oracle at the point the handoff is consumed, which is where Home mounts and the first detail page
/// opened from it takes its hero decision.
///
/// 1. **Stored boot, auto sign-in on:** the TV's key says on, the stored profile turned trailers off.
///    Process start reads on (nobody seated); `ResumeStored` seats the profile in the same drain, so
///    the first hero decision after Home mounts reads off.
/// 2. **The boot picker:** the stored profile has no value of its own and the chosen one turned trailers
///    off. While the picker stands, the cell holds what the stored profile reads (on: nothing reads it);
///    the pick seats the chosen profile before the `Ready` the landing consumes, so Home reads off.
/// 3. **A fresh sign-in as the owner:** the TV's key says off and the owner has no value of its own. The
///    cell is forced ON after the boot restore (which had read off), so only the seat's restore can bring
///    it back: seating the owner reads the TV's off.
/// 4. **A dev-token boot never seats:** the owner's record says on over a TV key of off, and the cell
///    reads the TV's off, not the owner's on.
#[test]
fn trailer_decision_follows_the_seat() {
    let _serial = plx_base::testlock::serial();
    let _cell = TrailerCell;

    // 1. Stored boot with auto sign-in.
    {
        let mut stored = with_own_trailer(record("profile-b", &["profile-a"]), "profile-b", false, true);
        stored.prefs.auto_sign_in = true;
        assert!(!stored.boot_shows_picker(false, false), "auto sign-in on and seated in the roster: no picker");
        let _fixture = Fixture::new("p1c-boot-auto", &stored);
        crate::app::boot::restore_player_preferences(&session::load(), false);
        assert!(hero_may_autoplay(), "process start: nobody is seated, the TV's key says on");
        let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
        let mut d = Dispatcher::<AppHost>::new();
        assert!(crate::app::boot::resume_stored(&mut d, &mut rig, &mut NoTap).is_some());
        assert_eq!(rig.seat_watch.restored, seat_of("profile-b"));
        assert!(!hero_may_autoplay(), "case 1: the stored profile's own off, before Home mounts");
    }

    // 2. The boot picker.
    {
        let stored = with_own_trailer(record("profile-a", &["profile-b"]), "profile-b", false, true);
        assert!(stored.boot_shows_picker(false, false), "auto sign-in off on a multi-user Home: the picker");
        let _fixture = Fixture::new("p1c-boot-picker", &stored);
        crate::app::boot::restore_player_preferences(&session::load(), false);
        assert!(hero_may_autoplay(), "process start: the TV's on");
        let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, SessionCmd::StartSwitch(Picker::Boot));
        drain(&mut rig, &mut d, Vec::new(), 1);
        assert_eq!(rig.auth_read().0.phase, Phase::Profiles, "Who's watching? is up");
        assert!(rig.session_ready.is_none(), "and no handoff was released: Home has not mounted");
        assert!(hero_may_autoplay(), "the stored profile behind the picker has no value of its own: on");
        pick(&mut rig, &mut d, 1, 3000);
        assert_eq!(rig.seat_watch.restored, seat_of("profile-b"));
        assert!(!hero_may_autoplay(), "case 2: the chosen profile's own off, restored before Ready is consumed");
    }

    // 3. A fresh sign-in as the owner (no Home selection): nobody's own value, so the TV's key.
    {
        let mut stored = record("", &[]);
        stored.prefs.trailer_autoplay = false;
        let _fixture = Fixture::new("p1c-first-login-owner", &stored);
        crate::app::boot::restore_player_preferences(&session::load(), false);
        assert!(!hero_may_autoplay(), "process start: the TV's off");
        plx_media::player::preview::restore_trailer_autoplay(true); // so only the seat can put it back
        let mut rig = Bridge::for_session_test(SessionInit::captured(stored.clone()));
        let mut d = Dispatcher::<AppHost>::new();
        assert!(crate::app::boot::resume_stored(&mut d, &mut rig, &mut NoTap).is_some());
        assert_eq!(rig.seat_watch.restored, seat_of(""), "the owner is seated");
        assert!(!hero_may_autoplay(), "case 3: the owner has no value of its own: the TV's off, restored by the seat");
    }

    // 4. A dev-token boot never seats: the legacy key, never the owner's own value.
    {
        let stored = with_own_trailer(record("", &[]), "", true, false);
        let _fixture = Fixture::new("p1c-dev-token", &stored);
        crate::app::boot::restore_player_preferences(&session::load(), false);
        let mut rig = Bridge::for_session_test(crate::app::boot::captured_session_for_boot(
            stored.clone(), Some(server("dev-token")), Vec::new()));
        rig.session_adapter.fixture_resources().disk = stored.clone();
        let mut d = Dispatcher::<AppHost>::new();
        execute_session_command(&mut d, SessionCmd::ActivateDevBootstrap);
        drain(&mut rig, &mut d, Vec::new(), 1);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready, "the dev token reached Ready");
        assert_eq!(rig.seat_watch.restored, plx_plex::plex::session::Seat::Unseated, "and nobody was seated");
        assert!(!hero_may_autoplay(), "case 4: the TV's off, not the owner's own on");
    }
}

/// **An off pick on profile A is not seen by profile B (hostsim switch), and the TV's key is never
/// written.** A turns trailers off through the door Settings uses; the switch seats B, which has picked
/// nothing, so B reads the TV's on; A, seated again, finds its off. The legacy key stays what it was.
#[test]
fn trailer_off_on_profile_a_is_not_seen_by_b() {
    let _serial = plx_base::testlock::serial();
    let _cell = TrailerCell;
    let mut stored = record("profile-a", &["profile-b"]);
    stored.prefs.trailer_autoplay = true;
    let _fixture = Fixture::new("p1c-trailer-a-not-b", &stored);
    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    seat_globally("profile-a");
    assert!(hero_may_autoplay(), "setup: A reads the TV's on");

    assert!(plx_media::route::confirm::<TrailerAutoplayPref>(false), "A's pick is durable");
    assert!(!hero_may_autoplay(), "published for A");
    assert!(session::load().prefs.trailer_autoplay, "the TV's key was not written");
    assert_eq!(stored_value::<TrailerAutoplayPref>("profile-a"), Some(false));

    open_picker(&mut rig, &mut d, 2000);
    seat_globally("profile-b");
    pick(&mut rig, &mut d, 1, 3000);
    assert!(hero_may_autoplay(), "B reads the TV's on, not A's off");
    assert_eq!(stored_value::<TrailerAutoplayPref>("profile-b"), None, "and has picked nothing");

    open_picker(&mut rig, &mut d, 4000);
    seat_globally("profile-a");
    pick(&mut rig, &mut d, 0, 5000);
    assert!(!hero_may_autoplay(), "A finds its own off");
    assert!(session::load().prefs.trailer_autoplay, "and the TV's key is still what it was");
}
