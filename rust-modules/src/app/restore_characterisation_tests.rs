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
/// restore has not yet run, or where something other than the record last wrote them.
const SCRAMBLED: Cells = Cells {
    quality: PlaybackQuality::P1080Low,
    direct_play: DirectPlayMode::Disabled,
    size: SubtitleSize::ExtraLarge,
    position: SubtitlePosition::Middle,
    next_episode: NextEpisodeMode::Off,
    skip: SkipInterval::Seconds60,
    deck_press: DeckPress::Play,
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
    s.with_playback_quality(STORED.quality)
        .with_direct_play_mode(STORED.direct_play)
        .with_subtitle_size(STORED.size)
        .with_subtitle_position(STORED.position)
        .with_next_episode_mode(STORED.next_episode)
        .with_skip_interval(STORED.skip)
        .with_deck_press(STORED.deck_press)
        .with_subtitle_tone(STORED.tone)
        .with_audio_enhancements(STORED.enhancements)
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
        profiles: others.iter().map(|u| ProfileCreds { uuid: (*u).into(), user: user(u),
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
            super::boot::restore_player_preferences(&session::load(), false);
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
