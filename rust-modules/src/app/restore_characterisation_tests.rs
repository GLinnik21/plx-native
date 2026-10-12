//! **A0c: what `main` does with the nine install-wide player cells at boot, at a profile switch
//! and across profiles.** Characterisation, not a specification: every expected value below was
//! read off `main` by running it, and the per-profile-settings work (`track-memory-plan.md`, §3.8)
//! flips three of these on purpose, each in its own commit.
//!
//! The nine cells are the live copies the player reads every frame: the playback quality ceiling,
//! the Direct Play mode, the subtitle size and position, the next-episode mode, the skip interval,
//! the deck press, the subtitle tone and the audio enhancements. Today three places write them from
//! the one session record, and none of them knows which profile is seated:
//!
//! * `boot::restore_player_preferences`, once, before any profile is published (the list that was
//!   inline in `construct`);
//! * `bridge::follow_auth_landing`, at every credentials handoff (`Ready`), which a profile switch
//!   ends in;
//! * the doors that pick a value (`route::select_subtitle_size`, `route::set_direct_play_mode`,
//!   `playback::play_automatically`), which write the record's top-level key.
//!
//! `hostsim` only, and as a correctness gate: `boot::restore_player_preferences` reads
//! `dev::playback_quality_override()`, which reads `/tmp/plxnative-quality`. `PLXNATIVE_RUNTIME_DIR`
//! is honoured only in a `hostsim` build, so only there can the run promise "no dev override"
//! instead of grading whatever file another process left in the shared `/tmp`.

use super::*;
use super::test_support::{frame, tick};
use plx_plex::plex::session::{
    self, DeckPress, DirectPlayMode, HomeUserRef, NextEpisodeMode, PlaybackQuality, ProfileCreds, ServerRef,
    Session, SkipInterval, SourceRef, SubtitlePosition, SubtitleSize, SubtitleTone, UserRef,
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
        let session = session::TempSession::new(tag);
        session::save(record);
        put(SCRAMBLED);
        Self { _session: session }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        put(DEFAULTS);
        plx_plex::plex::reset_servers_for_test();
    }
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
/// enter either list. The managed case is the one the per-profile work flips (plan P-d8), because
/// there the seated profile's own value replaces the record's.
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

fn settle(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, from: u32) {
    for i in 0..24 { super::frame(d, rig, tick(from + i * 16), vec![]); }
}

fn stack_has_player(d: &Dispatcher<AppHost>) -> bool {
    d.nav.tabs.stack.entries.iter().any(|e| matches!(e.arg, AppArg::Player))
}

/// **A0 item 13.** A profile switch runs the restore: profile A seated and landed, A's player up,
/// then `switch_profile`, B's `Ready`, `follow_auth_landing`. Today the landing re-publishes the
/// record's install values, identical to the ones A had; and the player page is gone before `Ready`.
///
/// The cells are scrambled while the picker stands, so "identical" cannot be a restore that never
/// ran: only the landing's restore can put them back. P1a flips this (the landing then publishes
/// B's own values).
#[test]
fn a_profile_switch_republishes_the_install_values_and_retires_the_player_before_ready() {
    let _serial = plx_base::testlock::serial();
    let stored = with_stored_installs(record("profile-a", &["profile-b"]));
    let _fixture = Fixture::new("a0c-switch", &stored);

    let (mut rig, mut d) = resume(&stored);
    follow_auth_landing(&mut d, &mut rig);
    settle(&mut rig, &mut d, 100);
    assert_eq!(cells(), STORED, "A seated: the landing published the record's install values");
    let top = d.nav.top_page().map(|e| e.arg.id());
    assert!(top.is_some(), "A's landing mounted a page: {top:?}");

    // A is playing something.
    frame(&mut d, &mut rig, AppArg::Player, tick(1000), vec![]);
    settle(&mut rig, &mut d, 1100);
    assert!(stack_has_player(&d), "A's player is on the stack");

    // Something other than the record last wrote the cells (a pick that never persisted, a harness):
    // the switch's restore is the only thing that can undo it.
    put(SCRAMBLED);
    open_picker(&mut rig, &mut d, 2000);
    assert!(!stack_has_player(&d), "the player page is retired before B's Ready");
    assert_eq!(d.nav.tabs.stack.entries.len(), 1, "nothing of A's tree stands but the picker");
    assert_ne!(rig.auth_read().0.phase, Phase::Ready, "...and Ready has not arrived yet");
    assert_eq!(cells(), SCRAMBLED, "opening the picker restores nothing");

    pick(&mut rig, &mut d, 1, 3000);
    assert_eq!(cells(), SCRAMBLED, "B's Ready alone restores nothing: the landing does");
    follow_auth_landing(&mut d, &mut rig);
    assert_eq!(cells(), STORED, "B's landing re-published the identical install values");
}

/// **A0 item 14.** Install keys are shared by every profile today. Profile A picks subtitle size
/// Large and Direct Play Force through the doors Settings uses; profile B is seated and reads Large
/// and Force, from the record and in the live cells. Then B's failure read-out "Play Automatically"
/// moves Direct Play to Auto, and A, seated again, reads Auto: B reset A too.
///
/// The fixture adapter does not write the session file at a switch's commit, so each switch below
/// writes the new `user` into the record itself, the one thing a commit changes there.
#[test]
fn install_keys_are_shared_by_every_profile_and_play_automatically_on_b_resets_a() {
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
    assert_eq!(cells(), DEFAULTS, "A seated on a record that chose nothing");

    // Profile A picks Large and Force.
    plx_media::route::select_subtitle_size(SubtitleSize::Large, None);
    plx_base::storage_worker::drain_for_test();
    assert!(plx_media::route::set_direct_play_mode(DirectPlayMode::Forced));
    let a = session::load();
    assert_eq!((a.user.uuid.as_str(), a.subtitle_size(), a.direct_play_mode()),
        ("profile-a", SubtitleSize::Large, DirectPlayMode::Forced));

    // Profile B is seated.
    put(SCRAMBLED);
    open_picker(&mut rig, &mut d, 2000);
    pick(&mut rig, &mut d, 1, 3000);
    seat("profile-b");
    follow_auth_landing(&mut d, &mut rig);
    let b = session::peek();
    assert_eq!((b.user.uuid.as_str(), b.subtitle_size(), b.direct_play_mode()),
        ("profile-b", SubtitleSize::Large, DirectPlayMode::Forced), "B's record carries A's keys");
    assert_eq!(plx_media::route::subtitle_size(), SubtitleSize::Large, "B reads Large");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Forced, "B reads Force");

    // B's playback failed under Force and B presses Play Automatically.
    let mut player = plx_media::player::machine::Player::new();
    let mut adapter = plx_media::player::adapter::PlayerAdapter::new(unsafe {
        plx_base::task::MainThread::assume()
    });
    assert!(!crate::app::playback::play_automatically(&mut player.session, &mut adapter, rig.metadata_mut()),
        "there is no reusable Plex request to retry, so only the preference moves");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto, "B's live cell moved at once");
    plx_base::storage_worker::drain_for_test();
    let after = session::load();
    assert_eq!((after.user.uuid.as_str(), after.direct_play_mode(), after.subtitle_size()),
        ("profile-b", DirectPlayMode::Auto, SubtitleSize::Large), "...and the shared record moved");

    // A is seated again and finds Auto.
    put(SCRAMBLED);
    open_picker(&mut rig, &mut d, 4000);
    pick(&mut rig, &mut d, 0, 5000);
    seat("profile-a");
    follow_auth_landing(&mut d, &mut rig);
    assert_eq!(session::peek().user.uuid, "profile-a");
    assert_eq!(plx_media::route::direct_play_mode(), DirectPlayMode::Auto, "B's Play Automatically reset A too");
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
    assert_eq!(session::load().direct_play_mode(), DirectPlayMode::Auto, "and persisted behind it");
}
