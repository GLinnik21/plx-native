//! **Track memory, the read path, through the real resolve** (`track-memory-plan.md` §4.2-§4.4,
//! §4.8 W1-W5 and the `fresh_play` rule W4): what `build_stream` decides for an episode when the
//! profile remembered something for its show, what the plan says it would write back, and where the
//! memory comes from (the mirror, on the main thread, at the request).
//!
//! The memory is INJECTED (`ResolveEnv::memory`) for the ladder rows, and read through the real
//! mirror for the capture rows. The pure ladder is graded in `plan_track_memory_tests`; this file
//! grades the wiring around it: the MDE handshake, the plan, the route, the requests.
use super::test_support::apply_plan;
use super::test_support::*;
use super::*;
use plx_plex::plex::session::prefs::{AudioMemo, SubtitleMemo, SubtitleTrackMemo, TrackMemoryEntry};
use super::prefs_door_tests::Parked;
use crate::route::{memo_of_audio, memo_of_subtitle};
use plx_plex::plex::session::prefs::ProfileList;
use plx_plex::plex::session::{self as session, Preferences, Session};

const MACHINE: &str = "machine-tm";
/// A plain numeric ratingKey: the one shape the show-preferences read accepts, so a resolve under
/// test makes the same requests a real episode's does.
const SHOW: &str = "4242";

fn stream(id: i64, index: i64, lang: &str, codec: &str, channels: i64) -> plx_data::metadata::Stream {
    plx_data::metadata::Stream {
        id,
        index,
        lang_code: lang.into(),
        codec: codec.into(),
        channels,
        ..Default::default()
    }
}

/// An embedded SRT, `index` in container order.
fn srt(id: i64, index: i64, lang: &str, forced: bool, selected: bool) -> plx_data::metadata::Stream {
    plx_data::metadata::Stream { forced, selected, ..stream(id, index, lang, "srt", 0) }
}

/// English (the file's default, PMS-selected) and Japanese, both E-AC3 5.1: either direct-plays.
fn audio_pair() -> Vec<plx_data::metadata::Stream> {
    vec![eac3_track(), stream(36015, 2, "jpn", "eac3", 6)]
}

fn memory(audio: Option<AudioMemo>, subtitle: Option<SubtitleMemo>) -> Vec<TrackMemoryEntry> {
    vec![TrackMemoryEntry::new(MACHINE, SHOW, audio, subtitle)]
}

fn english_track() -> SubtitleMemo {
    SubtitleMemo::Track(SubtitleTrackMemo { lang: "eng".into(), kind: "text".into(), ..Default::default() })
}

fn japanese_eac3() -> AudioMemo {
    AudioMemo { lang: "jpn".into(), codec: "eac3".into(), channels: 6, ..Default::default() }
}

/// Everything below that talks to a loopback PMS.
#[cfg(feature = "devtriggers")]
mod through_build_stream {
    use super::*;
    use std::time::Duration;

    pub(super) struct Case {
        pub kind: &'static str,
        pub show_rk: &'static str,
        pub audio: Vec<plx_data::metadata::Stream>,
        pub subs: Vec<plx_data::metadata::Stream>,
        pub memory: Vec<TrackMemoryEntry>,
        pub fresh_play: bool,
        /// The account's subtitle rule `(language, mode, forced)`, standing in for plex.tv.
        pub account: Option<(Option<String>, i64, i64)>,
        pub mode: DirectPlayMode,
        pub mde: &'static [u8],
        /// Requests beyond the PlayQueue, the show-preferences read and `/decision`.
        pub extra: usize,
        /// `env.quality`; `None` keeps the process default (Original).
        pub quality: Option<Quality>,
        /// The link the client is recorded on; `None` leaves it unrecorded.
        pub link: Option<plx_plex::plex::probe::Location>,
        /// The Part the resolve is asked to play, and the item's whole-stream bitrate in kbps.
        pub part: &'static str,
        pub bitrate: i64,
        /// A known retry's own pick (`env.sub_sid` and `env.subtitle_override`); `0` is a cold start.
        pub sub_sid: i64,
        /// The subtitle mode that retry held (`env.sub_mode`); meaningful only beside `sub_sid`.
        pub sub_mode: SubMode,
    }

    impl Case {
        /// A fresh play of an episode of show [`SHOW`] whose item has `subs` and the audio pair.
        pub fn episode(subs: Vec<plx_data::metadata::Stream>, memory: Vec<TrackMemoryEntry>) -> Case {
            Case {
                kind: "episode",
                show_rk: SHOW,
                audio: audio_pair(),
                subs,
                memory,
                fresh_play: true,
                account: None,
                mode: DirectPlayMode::Auto,
                mde: MDE_DIRECTPLAY,
                extra: 0,
                quality: None,
                link: None,
                part: "/library/parts/36013/1/file.mkv",
                bitrate: 48_000,
                sub_sid: 0,
                sub_mode: SubMode::Track,
            }
        }
    }

    /// The `n` the mock must see: the PlayQueue, `/decision`, and the two reads of the show's
    /// preferences an episode with a plain ratingKey makes.
    fn requests_expected(case: &Case) -> usize {
        let show_reads = if !case.show_rk.is_empty() && case.show_rk.bytes().all(|b| b.is_ascii_digit()) { 2 } else { 0 };
        2 + show_reads + case.extra
    }

    /// Resolve `case` against a loopback PMS: the plan, and every request line the server saw.
    pub(super) fn resolve(case: Case) -> (Plan, Vec<String>) {
        let (plan, requests, _) = resolve_logged(case);
        (plan, requests)
    }

    /// The `track-memory:` lines in `appended` (what the event log gained) that name track `id`. The
    /// log [`resolve_in`] captures is private to the resolve (`eventlog::with_private_log`), so every
    /// line in it is this resolve's own; a line is still claimed by the track it names, because the
    /// one resolve logs lines about both halves.
    pub(super) fn memory_lines(appended: &str, id: i64) -> Vec<String> {
        appended
            .lines()
            .filter(|l| l.contains("track-memory:") && l.contains(&id.to_string()))
            .map(str::to_owned)
            .collect()
    }

    pub(super) fn resolve_logged(case: Case) -> (Plan, Vec<String>, String) {
        let mut ps = PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        resolve_in(&mut ps, case, |_, plan| plan)
    }

    /// [`resolve_logged`] on a session the caller owns (and has made fresh), for a test that goes on
    /// to land the plan: `then` gets the plan while the loopback PMS is still up, and what it returns
    /// comes back beside the requests the server saw and what the event log gained.
    ///
    /// **The log it returns is this resolve's alone.** A host test run resolves the event log to the
    /// bare `/tmp/plxnative-events.log` (only `hostsim` can steer it), one file for the whole machine:
    /// every test binary of this `make check`, and of the other run beside it or another checkout's,
    /// appends to it, and `serial()` is one process's lock. Slicing "what the file gained" off that
    /// file therefore also returned a second run's `track-memory: applied subtitle (track 55001)` (the
    /// same fixtures name the same stream ids in every run), and "one line per resolve" read 3. The
    /// whole resolve and `then` run on THIS thread, which `eventlog::with_private_log` points at a
    /// fresh private file; a line from any other thread or process cannot land in it.
    pub(super) fn resolve_in<T>(
        ps: &mut PlaybackSession, case: Case, then: impl FnOnce(&mut PlaybackSession, Plan) -> T,
    ) -> (T, Vec<String>, String) {
        plx_base::eventlog::with_private_log(|| {
            let (out, requests) = resolve_on_this_thread(ps, case, then);
            let appended = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            (out, requests, appended)
        })
    }

    fn resolve_on_this_thread<T>(
        ps: &mut PlaybackSession, case: Case, then: impl FnOnce(&mut PlaybackSession, Plan) -> T,
    ) -> (T, Vec<String>) {
        assert!(plx_net::net::global_init());
        let (port, rx, server) = plan_pms(requests_expected(&case), case.mde);
        let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", port, "token", "tm-client");
        if let Some(link) = case.link {
            plx_plex::plex::client_for(sid).expect("registered").set_link(link);
        }
        let mut env = ResolveEnv::snapshot(ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
        env.direct_play_mode = case.mode;
        if let Some(quality) = case.quality {
            env.quality = quality;
        }
        if case.sub_sid > 0 {
            env.sub_sid = case.sub_sid;
            env.subtitle_override = Some(case.sub_sid);
            env.sub_mode = case.sub_mode;
        }
        env.memory = case.memory;
        env.fresh_play = case.fresh_play;
        env.account_subtitles = case.account;
        env.cached_item = Some(plx_data::metadata::PlayingItem {
            kind: case.kind.into(),
            show_rk: case.show_rk.into(),
            bitrate: case.bitrate,
            ..fourk_item_with_subs(sid, case.audio, case.subs)
        });
        let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-4k", case.part, "hevc", "eac3", &env);
        let plan = then(ps, plan);
        let requests = rx.recv_timeout(Duration::from_secs(15)).expect("PMS never saw the resolve");
        server.join().unwrap();
        plx_plex::plex::reset_servers_for_test();
        (plan, requests)
    }

    pub(super) fn is_direct_play(plan: &Plan) -> bool {
        plan.url.contains("/library/parts/36013/") && !plan.url.contains("start.mkv")
    }

    pub(super) fn puts(requests: &[String]) -> usize {
        requests.iter().filter(|r| r.starts_with("PUT ")).count()
    }

    /// The request lines with their per-run noise (session ids, tokens, query values) taken off.
    pub(super) fn shape(requests: &[String]) -> Vec<String> {
        requests
            .iter()
            .map(|r| r.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
            .map(|r| r.split('?').next().unwrap_or("").to_string())
            .collect()
    }

    pub(super) fn decision(requests: &[String]) -> &String {
        requests.iter().find(|r| r.contains("/decision?")).unwrap_or_else(|| panic!("MDE was never asked: {requests:?}"))
    }
}

#[cfg(feature = "devtriggers")]
use through_build_stream::*;

// ---- the subtitle rungs, through the plan ------------------------------------------------------

/// Q1: a remembered track outranks the track PMS has selected, and the MDE handshake names it (so
/// the server and the client agree on what is drawn). The control is today's pick.
#[test]
#[cfg(feature = "devtriggers")]
fn a_remembered_subtitle_beats_the_pms_embedded_selection() {
    let subs = || vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let (control, _) = resolve(Case::episode(subs(), Vec::new()));
    assert_eq!(control.sub_sid, 55002, "control: with no record PMS's selection plays");

    let (plan, requests) = resolve(Case::episode(subs(), memory(None, Some(english_track()))));
    assert!(is_direct_play(&plan), "{}", plan.url);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (55001, Some(0)));
    assert_eq!(query_param(decision(&requests), "subtitleStreamID"), Some("55001"), "{requests:?}");
    assert_eq!(query_param(decision(&requests), "subtitles"), Some("none"), "{requests:?}");
}

/// Off beats the account's "always show English" (mode 2). The control is the account turning the
/// track on.
#[test]
#[cfg(feature = "devtriggers")]
fn a_remembered_off_beats_account_mode_2() {
    let subs = || vec![srt(55001, 3, "eng", false, false)];
    let mut control = Case::episode(subs(), Vec::new());
    control.account = Some((Some("eng".into()), 2, 0));
    assert_eq!(resolve(control).0.sub_sid, 55001, "control: the account rule turns English on");

    let mut case = Case::episode(subs(), memory(None, Some(SubtitleMemo::Off)));
    case.account = Some((Some("eng".into()), 2, 0));
    let (plan, requests) = resolve(case);
    assert!(is_direct_play(&plan), "{}", plan.url);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (0, None));
    assert_eq!(query_param(decision(&requests), "subtitleStreamID"), Some("0"), "{requests:?}");
}

/// Off is absolute: a forced track PMS has selected, and the account's forced-only setting, stay
/// hidden. Nothing falls through to them.
#[test]
#[cfg(feature = "devtriggers")]
fn a_remembered_off_hides_a_pms_selected_forced_track_and_the_accounts_forced_only() {
    let subs = || vec![srt(55003, 3, "eng", true, true), srt(55001, 4, "eng", false, false)];
    let account = Some((Some("eng".into()), 1, 2));
    let mut control = Case::episode(subs(), Vec::new());
    control.account = account.clone();
    assert_eq!(resolve(control).0.sub_sid, 55003, "control: PMS's forced track plays");

    let mut case = Case::episode(subs(), memory(None, Some(SubtitleMemo::Off)));
    case.account = account;
    let (plan, _) = resolve(case);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (0, None));
}

/// Forced only shows the forced track of the audio that plays, and with none it shows nothing: not
/// the full track the account's rule would turn on.
#[test]
#[cfg(feature = "devtriggers")]
fn forced_only_shows_the_forced_track_or_nothing() {
    let forced_only = || memory(None, Some(SubtitleMemo::ForcedOnly { lang: String::new() }));
    let with_forced = vec![srt(55001, 3, "eng", false, false), srt(55003, 4, "eng", true, false)];
    let (plan, _) = resolve(Case::episode(with_forced, forced_only()));
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (55003, Some(1)));

    let only_full = || vec![srt(55001, 3, "eng", false, false)];
    let mut from_mode = Case::episode(only_full(), Vec::new());
    from_mode.account = Some((Some("eng".into()), 2, 0));
    assert_eq!(resolve(from_mode).0.sub_sid, 55001, "control: the account turns the full track on");
    let mut case = Case::episode(only_full(), forced_only());
    case.account = Some((Some("eng".into()), 2, 0));
    let (plan, _) = resolve(case);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (0, None), "no forced track: nothing, never the full one");
}

/// Rung 3b is judged by the audio that PLAYS: the remembered Japanese audio re-points the forced
/// track at the Japanese one.
#[test]
#[cfg(feature = "devtriggers")]
fn forced_only_follows_the_audio_that_plays() {
    let subs = vec![srt(55003, 3, "eng", true, false), srt(55004, 4, "jpn", true, false)];
    let record = memory(Some(japanese_eac3()), Some(SubtitleMemo::ForcedOnly { lang: String::new() }));
    let (plan, _) = resolve(Case::episode(subs, record));
    assert_eq!(plan.audio.as_ref().map(|a| a.sid), Some(36015));
    assert_eq!(plan.sub_sid, 55004);
}

/// A subtitle the SERVER selected and the app draws itself (a downloaded sidecar) beats memory:
/// moving the selection off a download makes PMS delete it. The route is today's, and no write-back
/// is made.
#[test]
#[cfg(feature = "devtriggers")]
fn a_server_selected_sidecar_beats_memory() {
    let sidecar = plx_data::metadata::Stream {
        key: "/library/streams/9".into(),
        external: true,
        selected: true,
        ..stream(9, 9, "eng", "srt", 0)
    };
    let subs = || vec![srt(55002, 3, "spa", false, false), sidecar.clone()];
    let (control, control_requests) = resolve(Case::episode(subs(), Vec::new()));
    let (plan, requests) = resolve(Case::episode(subs(), memory(None, Some(SubtitleMemo::Track(SubtitleTrackMemo {
        lang: "spa".into(),
        kind: "text".into(),
        ..Default::default()
    })))));
    assert_eq!(plan.sub_sid, control.sub_sid, "the sidecar's route is untouched: {}", plan.sub_sid);
    assert_eq!(plan.sub_render_ordinal, control.sub_render_ordinal);
    assert_eq!(shape(&requests), shape(&control_requests));
    assert_eq!(plan.write_back, None, "nothing remembered was applied");
}

/// A record that matches no track of this file changes nothing: today's pick, the same route.
#[test]
#[cfg(feature = "devtriggers")]
fn a_record_that_matches_nothing_gives_todays_pick() {
    let subs = || vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let (control, control_requests) = resolve(Case::episode(subs(), Vec::new()));
    let french = SubtitleMemo::Track(SubtitleTrackMemo { lang: "fra".into(), kind: "text".into(), ..Default::default() });
    let (plan, requests) = resolve(Case::episode(subs(), memory(None, Some(french))));
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (control.sub_sid, control.sub_render_ordinal));
    assert_eq!(plan.audio.as_ref().map(|a| a.sid), control.audio.as_ref().map(|a| a.sid));
    assert_eq!(shape(&requests), shape(&control_requests));
}

// ---- the audio rung, through the plan ----------------------------------------------------------

#[test]
#[cfg(feature = "devtriggers")]
fn a_remembered_audio_track_is_the_one_direct_play_feeds() {
    let (control, _) = resolve(Case::episode(Vec::new(), Vec::new()));
    assert_eq!(control.audio.as_ref().map(|a| a.sid), Some(36014), "control: the file's default");

    let (plan, requests) = resolve(Case::episode(Vec::new(), memory(Some(japanese_eac3()), None)));
    assert!(is_direct_play(&plan), "{}", plan.url);
    assert_eq!(plan.audio.as_ref().map(|a| a.sid), Some(36015));
    assert_eq!(query_param(decision(&requests), "audioStreamID"), Some("36015"), "{requests:?}");
}

// ---- the gate: episodes only ------------------------------------------------------------------

/// Music and movies get nothing, and so does an episode that names no show.
#[test]
#[cfg(feature = "devtriggers")]
fn only_an_episode_of_a_known_show_reads_the_memory() {
    let record = || memory(Some(japanese_eac3()), Some(SubtitleMemo::Off));
    let subs = || vec![srt(55001, 3, "eng", false, true)];
    for (kind, show_rk) in [("movie", SHOW), ("track", SHOW), ("", SHOW), ("episode", "")] {
        let mut case = Case::episode(subs(), record());
        case.kind = kind;
        case.show_rk = show_rk;
        let (plan, _) = resolve(case);
        assert_eq!(plan.audio.as_ref().map(|a| a.sid), Some(36014), "{kind:?}/{show_rk:?}: audio is today's");
        assert_eq!(plan.sub_sid, 55001, "{kind:?}/{show_rk:?}: subtitle is today's");
        assert_eq!(plan.write_back, None, "{kind:?}/{show_rk:?}");
    }
}

// ---- the requests ------------------------------------------------------------------------------

/// `build_stream` sends no PUT and no request of its own for the memory: a direct play with a
/// remembered pick makes exactly the requests the same play makes without one.
#[test]
#[cfg(feature = "devtriggers")]
fn build_stream_sends_no_new_request_for_a_remembered_pick() {
    let subs = || vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let (_, plain) = resolve(Case::episode(subs(), Vec::new()));
    let (plan, remembered) = resolve(Case::episode(subs(), memory(Some(japanese_eac3()), Some(english_track()))));
    assert!(is_direct_play(&plan), "{}", plan.url);
    assert_eq!(puts(&remembered), 0, "{remembered:?}");
    assert_eq!(puts(&plain), 0);
    assert_eq!(shape(&remembered), shape(&plain));
}

// ---- plan.write_back (W1-W5) -------------------------------------------------------------------

#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_applied_track() {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let (plan, _) = resolve(Case::episode(subs, memory(Some(japanese_eac3()), Some(english_track()))));
    assert_eq!(
        plan.write_back,
        Some(WriteBack { audio: Some(36015), subtitle: Some(SubWrite::Id(55001)) }),
        "both halves applied and on screen",
    );
}

#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_off() {
    let subs = vec![srt(55001, 3, "eng", false, true)];
    let (plan, _) = resolve(Case::episode(subs, memory(None, Some(SubtitleMemo::Off))));
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Off) }));
}

/// Forced only with no forced track is not a decline: nothing is its applied result, and it writes Off.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_forced_only_none() {
    let subs = vec![srt(55001, 3, "eng", false, false)];
    let (plan, _) = resolve(Case::episode(subs, memory(None, Some(SubtitleMemo::ForcedOnly { lang: String::new() }))));
    assert_eq!(plan.sub_sid, 0);
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Off) }));
}

/// A declined memory writes nothing, and never `subtitleStreamID=0`.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_declined() {
    let subs = vec![srt(55001, 3, "eng", false, false)];
    let french = SubtitleMemo::Track(SubtitleTrackMemo { lang: "fra".into(), kind: "text".into(), ..Default::default() });
    let spanish = AudioMemo { lang: "spa".into(), codec: "eac3".into(), channels: 6, ..Default::default() };
    let (plan, _) = resolve(Case::episode(subs, memory(Some(spanish), Some(french))));
    assert_eq!(plan.write_back, None);
}

/// A fall-through (rungs 4-7) is today's pick and is not the viewer's word: no record, no write.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_fall_through() {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let (plan, _) = resolve(Case::episode(subs, Vec::new()));
    assert_eq!(plan.sub_sid, 55002, "setup: PMS's selection played");
    assert_eq!(plan.write_back, None);
}

#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_audio_only() {
    let subs = vec![srt(55001, 3, "eng", false, true)];
    let (plan, _) = resolve(Case::episode(subs, memory(Some(japanese_eac3()), None)));
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }));
}

/// A stand-in sibling chosen for its codec is not the remembered track: the remembered Japanese
/// TrueHD cannot be fed, the Japanese AC3 beside it is, and writing it would record the stand-in.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_stand_in_has_no_audio_half() {
    let mut case = Case::episode(Vec::new(), memory(
        Some(AudioMemo { lang: "jpn".into(), codec: "truehd".into(), channels: 8, ..Default::default() }),
        None,
    ));
    case.audio = vec![eac3_track(), stream(36020, 2, "jpn", "truehd", 8), stream(36021, 3, "jpn", "ac3", 6)];
    let (plan, _) = resolve(case);
    assert_eq!(plan.audio.as_ref().map(|a| a.sid), Some(36021), "setup: the AC3 sibling is what plays");
    assert_eq!(plan.write_back, None);
}

/// W5: the subtitle half is withheld while the Part carries an external stream. This row: one the
/// viewer has not selected (another Home user's download, which moving the selection would make PMS
/// discard); the next: one the viewer has selected but rung 2 cannot take.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_external_on_part_withholds_the_subtitle_half() {
    let download = plx_data::metadata::Stream {
        key: "/library/streams/9".into(),
        external: true,
        ..stream(9, 9, "eng", "srt", 0)
    };
    let subs = || vec![srt(55001, 3, "eng", false, false), download.clone()];
    let (plan, _) = resolve(Case::episode(subs(), memory(Some(japanese_eac3()), Some(english_track()))));
    assert_eq!(plan.sub_sid, 55001, "setup: the remembered track still plays");
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }));
    let (only_subtitle, _) = resolve(Case::episode(subs(), memory(None, Some(english_track()))));
    assert_eq!(only_subtitle.write_back, None, "no half left, no write");
}

/// W5's headline rule, for the externals rung 2 cannot see: a SELECTED external subtitle the app
/// cannot draw (a PGS `.sup`, a VobSub, a format outside the sidecar allow-list) is not rung 2's
/// (`server_selected_sidecar` wants a renderable one), so the remembered embedded track plays.
/// Writing it back would move the selection off that download, and PMS discards a downloaded
/// subtitle once the selection leaves it. The memory still applies on screen; only the server write
/// is withheld.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_a_selected_external_rung_two_cannot_see_withholds_the_subtitle_half() {
    let download = plx_data::metadata::Stream {
        key: "/library/streams/9".into(),
        external: true,
        selected: true,
        ..stream(9, 9, "eng", "pgs", 0)
    };
    assert!(!download.sidecar_renderable(), "setup: rung 2 does not take it");
    let subs = || vec![srt(55001, 3, "eng", false, false), download.clone()];
    let (plan, _) = resolve(Case::episode(subs(), memory(Some(japanese_eac3()), Some(english_track()))));
    assert_eq!(plan.sub_sid, 55001, "setup: the remembered track still plays");
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }));
    let (only_subtitle, _) = resolve(Case::episode(subs(), memory(None, Some(english_track()))));
    assert_eq!(only_subtitle.write_back, None, "no half left, no write");
}

/// W4 as `build_stream` reads it: a resolve that is not a fresh play (a retry, a rescue, an
/// in-flight re-resolve) applies the memory and writes nothing.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_a_retry_applies_but_does_not_write() {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let mut case = Case::episode(subs, memory(Some(japanese_eac3()), Some(english_track())));
    case.fresh_play = false;
    let (plan, _) = resolve(case);
    assert_eq!((plan.sub_sid, plan.audio.as_ref().map(|a| a.sid)), (55001, Some(36015)), "the memory still reads");
    assert_eq!(plan.write_back, None);
}

/// W3: only the direct-play return carries a write-back. A route that PUTs its own selection before
/// it starts (here Direct Play disabled, so a remux) sets none, and sends exactly that one PUT.
#[test]
#[cfg(feature = "devtriggers")]
fn write_back_provenance_is_set_on_the_direct_play_return_only() {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let mut case = Case::episode(subs, memory(Some(japanese_eac3()), Some(english_track())));
    case.mode = DirectPlayMode::Disabled;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    let (plan, requests) = resolve(case);
    assert!(plan.url.contains("start.mkv"), "setup: not a direct play: {}", plan.url);
    assert_eq!(plan.write_back, None, "{requests:?}");
    assert_eq!(puts(&requests), 1, "the route's own selection PUT, and no other: {requests:?}");
}

/// A remembered subtitle that this route shows none of (a copy remux the app cannot draw) is noted
/// for the menu, and is never written back.
#[test]
#[cfg(feature = "devtriggers")]
fn a_remembered_subtitle_a_remux_does_not_show_sets_the_note() {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)];
    let mut case = Case::episode(subs, memory(None, Some(english_track())));
    case.mode = DirectPlayMode::Disabled;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    let (plan, requests) = resolve(case);
    assert!(plan.url.contains("start.mkv"), "setup: a remux: {}", plan.url);
    assert!(plan.memory_note, "{requests:?}");
    assert_eq!(plan.write_back, None);
}

// ---- Q-TOP (b): a remembered subtitle burns only on a route that already re-encodes video ------

/// The request line of the transcode `/decision` (the one that is not the MDE handshake).
#[cfg(feature = "devtriggers")]
fn transcode_decision(requests: &[String]) -> &String {
    requests
        .iter()
        .find(|r| r.contains("/decision?") && !r.contains("hasMDE=1"))
        .unwrap_or_else(|| panic!("no transcode decision was asked: {requests:?}"))
}

/// The subtitle half of the selection PUT, `None` for a route that sends no PUT.
#[cfg(feature = "devtriggers")]
fn put_subtitle(requests: &[String]) -> Option<&str> {
    requests.iter().find(|r| r.starts_with("PUT ")).and_then(|r| query_param(r, "subtitleStreamID"))
}

/// A route that BURNS `id`: the decision and the start URL both say `subtitles=burn` with the id, the
/// selection PUT names it, the plan reports it as the session's subtitle (so a rebuild reads it back),
/// and the menu has nothing to explain.
#[cfg(feature = "devtriggers")]
#[track_caller]
fn assert_burned(plan: &Plan, requests: &[String], id: i64) {
    let id_text = id.to_string();
    assert!(!plan.contract.remux, "a burn is a re-encode, never a copy: {}", plan.url);
    for line in [transcode_decision(requests).as_str(), plan.url.as_str()] {
        assert_eq!(query_param(line, "subtitleStreamID"), Some(id_text.as_str()), "{line}");
        assert_eq!(query_param(line, "subtitles"), Some("burn"), "{line}");
    }
    assert_eq!(put_subtitle(requests), Some(id_text.as_str()), "{requests:?}");
    assert_eq!(plan.sub_sid, id, "N2: the burn is the plan's subtitle");
    assert!(!plan.memory_note, "the subtitle is shown, so the menu has nothing to say");
}

fn two_subs() -> Vec<plx_data::metadata::Stream> {
    vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, true)]
}

/// A video re-encode is already running, so the remembered subtitle rides it. Fixed 720p rung: the
/// ceiling denies the remux, MDE is not asked, and the encoder is told to burn the track.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_a_fixed_rung_burns_the_remembered_subtitle() {
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.quality = Some(Quality::P720);
    case.mde = MDE_TRANSCODE;
    case.extra = 1;
    let (plan, requests, log) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv"), "setup: a fixed-rung encode: {}", plan.url);
    assert_burned(&plan, &requests, 55001);
    assert_eq!(memory_lines(&log, 55001).len(), 1, "one line per resolve: {log}");
    assert!(memory_lines(&log, 55001)[0].ends_with("track-memory: applied subtitle (track 55001)"), "{log}");
}

/// **The log a resolve returns is that resolve's alone.** The event log of a host test run is one file
/// for the whole machine (`/tmp/plxnative-events.log`; only `hostsim` can steer it), and a second
/// `make check`, another checkout or another lane appends to it from its own process while this one
/// resolves, in the same fixtures' words: `q_top_b_a_fixed_rung_burns_the_remembered_subtitle` read
/// three `track 55001` lines for its one resolve. `serial()` is one process's lock, so it cannot keep
/// them out; the capture has to be a file they do not write. The foreign writer is a thread spawned
/// inside the capture window (the redirect is per thread, so its line lands in the shared file just as
/// another process's does) that logs a line `memory_lines` claims: the interleave a second process
/// produces by timing alone, made deterministic.
#[test]
#[cfg(feature = "devtriggers")]
fn the_log_a_resolve_returns_is_not_the_file_other_writers_append_to() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let case = Case::episode(two_subs(), memory(None, Some(english_track())));
    let ((captured, shared), _, log) = resolve_in(&mut ps, case, |_, _plan| {
        let captured = plx_base::eventlog::events_log();
        let shared = std::thread::spawn(|| {
            plx_base::eventlog::log("track-memory: applied subtitle (track 55001) [a foreign writer, regression test]");
            plx_base::eventlog::events_log()
        })
        .join()
        .unwrap();
        (captured, shared)
    });
    assert_eq!(memory_lines(&log, 55001).len(), 1, "the foreign writer's line is not this resolve's: {log}");
    assert!(!log.contains("foreign writer"), "{log}");
    assert_ne!(captured, shared, "setup: the foreign writer appended to a different file than the capture");
}

/// A relay cannot carry a copy either; its encode takes the burn.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_a_relay_burns_the_remembered_subtitle() {
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.link = Some(plx_plex::plex::probe::Location::Relay);
    case.mde = MDE_TRANSCODE;
    case.extra = 1;
    let (plan, requests, _) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv"), "setup: a relay encode: {}", plan.url);
    assert_burned(&plan, &requests, 55001);
}

/// A copy remux the app cannot draw (Direct Play disabled, a bitrate over the side reader's bound)
/// stays a copy: no re-encode is forced for a subtitle. The menu is told, the start PUT names no
/// subtitle (as for any undrawn selection), nothing is written back, and the resolve speaks ONCE.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_a_copy_remux_is_not_re_encoded_for_the_subtitle() {
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.mode = DirectPlayMode::Disabled;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    let (plan, requests, log) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv") && plan.contract.remux, "setup: a copy remux: {}", plan.url);
    for line in [transcode_decision(&requests).as_str(), plan.url.as_str()] {
        assert_ne!(query_param(line, "subtitles"), Some("burn"), "{line}");
        assert_eq!(query_param(line, "subtitleStreamID").unwrap_or("0"), "0", "no subtitle id rides: {line}");
    }
    assert_eq!(put_subtitle(&requests), Some("0"), "{requests:?}");
    assert_eq!(plan.sub_sid, 0);
    assert!(plan.memory_note);
    assert_eq!(plan.write_back, None, "the pick is not on screen, so it is not written back");
    let lines = memory_lines(&log, 55001);
    assert_eq!(lines.len(), 1, "one truthful line, not `applied` and `not shown`: {log}");
    assert!(lines[0].ends_with("track-memory: not shown (copy remux) subtitle 55001"), "{}", lines[0]);
}

/// Where the app draws the track (a local link, a Part the side reader admits, a container the server
/// must remux), nothing is burned and nothing needs explaining: the wire says `subtitles=none`.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_a_side_reader_remux_draws_the_remembered_subtitle() {
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.link = Some(plx_plex::plex::probe::Location::Local);
    case.part = "/library/parts/36013/1/file.avi";
    case.bitrate = 7_400;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    let (plan, requests, log) = resolve_logged(case);
    assert!(plan.contract.remux && plan.url.contains("start.mkv"), "setup: a remux: {}", plan.url);
    assert_eq!(query_param(&plan.url, "subtitles"), Some("none"), "{}", plan.url);
    assert_eq!(query_param(&plan.url, "subtitleStreamID"), Some("0"), "{}", plan.url);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (55001, Some(0)));
    assert!(!plan.memory_note);
    assert!(requests.iter().all(|r| !r.contains("subtitles=burn")), "{requests:?}");
    let lines = memory_lines(&log, 55001);
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].ends_with("track-memory: applied subtitle (track 55001)"), "{}", lines[0]);
}

/// Direct play draws it too: no burn on any request, no selection PUT (the direct-play return has
/// none), the write-back names the pick for the first decoded frame, and the menu has no note.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_direct_play_draws_the_remembered_subtitle_and_writes_it_back() {
    let (plan, requests, log) = resolve_logged(Case::episode(two_subs(), memory(None, Some(english_track()))));
    assert!(is_direct_play(&plan), "{}", plan.url);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (55001, Some(0)));
    assert!(!plan.memory_note);
    assert_eq!(puts(&requests), 0, "{requests:?}");
    assert!(requests.iter().all(|r| !r.contains("subtitles=burn")), "{requests:?}");
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Id(55001)) }));
    assert_eq!(memory_lines(&log, 55001).len(), 1, "{log}");
}

/// Adaptive HLS is an encode from its first segment: the remembered subtitle rides the HLS spec.
/// (A remote Auto whose probe of the Part finds no capacity starts on HLS.)
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_adaptive_hls_burns_the_remembered_subtitle() {
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.quality = Some(Quality::Auto);
    case.link = Some(plx_plex::plex::probe::Location::Remote);
    case.extra = 3;
    let (plan, requests, _) = resolve_logged(case);
    assert!(matches!(plan.contract.delivery, plx_plex::plex::TranscodeDelivery::FixedHls { .. }), "setup: HLS: {requests:?}");
    assert_burned(&plan, &requests, 55001);
}

/// The commentary stand-in's burn (rule 3): an intended TrueHD the TV cannot decode, only a commentary
/// to stand in, a subtitle to show. The route is a re-encode, so the remembered pick is what burns.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_the_commentary_stand_in_burn_carries_the_remembered_subtitle() {
    let truehd = plx_data::metadata::Stream { default: true, selected: true, ..stream(1, 1, "eng", "truehd", 8) };
    let commentary = plx_data::metadata::Stream { title: "Commentary".into(), ..stream(2, 2, "eng", "ac3", 2) };
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.audio = vec![truehd, commentary];
    case.mde = MDE_TRANSCODE;
    case.extra = 2; // the selection PUT, and the MDE beside the transcode decision
    let (plan, requests, _) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv"), "setup: an encode: {}", plan.url);
    assert_eq!(plan.audio.as_ref().map(|a| a.sid), Some(1), "setup: the track asked for is converted");
    assert_burned(&plan, &requests, 55001);
}

/// The control: what ISN'T a remembered pick keeps main's wire exactly. An account auto-pick on the
/// same 720p rung is not burned: the PUT names no subtitle and the plan shows none.
#[test]
#[cfg(feature = "devtriggers")]
fn q_top_b_an_account_auto_pick_on_a_fixed_rung_is_still_not_burned() {
    let mut case = Case::episode(vec![srt(55001, 3, "eng", false, false)], Vec::new());
    case.account = Some((Some("eng".into()), 2, 0));
    case.quality = Some(Quality::P720);
    case.mde = MDE_TRANSCODE;
    case.extra = 1;
    let (plan, requests, _) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv") && !plan.contract.remux, "setup: an encode: {}", plan.url);
    for line in [transcode_decision(&requests).as_str(), plan.url.as_str()] {
        assert_ne!(query_param(line, "subtitles"), Some("burn"), "{line}");
        assert_eq!(query_param(line, "subtitleStreamID").unwrap_or("0"), "0", "no subtitle id rides: {line}");
    }
    assert_eq!(put_subtitle(&requests), Some("0"), "{requests:?}");
    assert_eq!((plan.sub_sid, plan.memory_note), (0, false));
}

/// U10: a known retry that carries its own subtitle (`env.sub_sid`) on a re-encode burns THAT id, and
/// the plan says so; the first rebuild would otherwise forget it.
#[test]
#[cfg(feature = "devtriggers")]
fn u10_a_retry_carrying_a_subtitle_on_a_re_encode_reports_it_in_the_plan() {
    let mut case = Case::episode(two_subs(), Vec::new());
    case.quality = Some(Quality::P720);
    case.sub_sid = 55002;
    case.mde = MDE_TRANSCODE;
    case.extra = 1;
    let (plan, requests, _) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv"), "setup: an encode: {}", plan.url);
    assert_burned(&plan, &requests, 55002);
}

/// N2 is scoped to a real re-encode. A retry carrying `env.sub_sid` onto a copy remux the app does
/// not draw keeps main's plan (`sub_sid` 0): reporting the id would make every later rebuild
/// (`transcode_spec` rewrites a remux contract's id to `(0, client_subtitles)`) switch the burn off
/// while the checkmark and the timeline still named it. The remux-carried seek is a follow-up.
#[test]
#[cfg(feature = "devtriggers")]
fn n2_a_carried_subtitle_on_a_copy_remux_is_not_reported_in_the_plan() {
    let mut case = Case::episode(two_subs(), Vec::new());
    case.mode = DirectPlayMode::Disabled;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    case.sub_sid = 55002;
    let (plan, requests) = resolve(case);
    assert!(plan.url.contains("start.mkv") && plan.contract.remux, "setup: a copy remux: {}", plan.url);
    assert_eq!(plan.sub_sid, 0, "as on main: a remux plan names no burn: {requests:?}");
}

/// N2: after a remembered burn the plan's subtitle is the burn, so a seek rebuild (`plan_rebase` reads
/// `cur_sub_sid`) names the id on the new encoder's decision and URL instead of dropping it.
#[test]
#[cfg(feature = "devtriggers")]
fn n2_a_seek_after_a_remembered_burn_still_names_the_subtitle() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let mut case = Case::episode(two_subs(), memory(None, Some(english_track())));
    case.quality = Some(Quality::P720);
    case.mde = MDE_TRANSCODE;
    case.extra = 2; // the selection PUT, and the seek's own decision
    let ((installed, seek_url), requests, _) = resolve_in(&mut ps, case, |ps, plan| {
        assert_eq!(plan.sub_sid, 55001, "setup: the resolve reports the burn");
        apply_plan(ps, plan, "rk-4k");
        let installed = cur_sub_sid(ps);
        (installed, transcode_seek(ps, 90))
    });
    assert_eq!(installed, 55001, "the landed session carries the burn");
    let seek_url = seek_url.expect("the seek rebuilt the encode");
    assert_eq!(query_param(&seek_url, "subtitleStreamID"), Some("55001"), "{seek_url}");
    assert_eq!(query_param(&seek_url, "subtitles"), Some("burn"), "{seek_url}");
    let seek_decision = requests.iter().rfind(|r| r.contains("/decision?")).expect("the seek's decision");
    assert_eq!(query_param(seek_decision, "subtitleStreamID"), Some("55001"), "{seek_decision}");
    assert_eq!(query_param(seek_decision, "subtitles"), Some("burn"), "{seek_decision}");
}

// ---- where the memory comes from: the mirror, at the request -----------------------------------

fn record_with(lists: Vec<ProfileList<TrackMemoryEntry>>) -> Session {
    Session {
        client_id: "cid-1".into(),
        account_token: "acct".into(),
        prefs: Preferences { track_memory: lists, ..Default::default() },
        ..Default::default()
    }
}

fn list(user: &str, account: &str, items: Vec<TrackMemoryEntry>) -> ProfileList<TrackMemoryEntry> {
    ProfileList { user: user.into(), account: account.into(), items, ..Default::default() }
}

fn tidy() {
    plx_plex::plex::reset_servers_for_test();
    unseed_memory_for_test::<TrackMemoryEntry>();
}

/// Put `user` (`""` for the account owner) on the screen as the profile whose memory is read: the
/// fixture publishes it, and its Drop unpublishes. This is the ONE place a test here says "this
/// profile is the active one". Memory is read only for a SEATED profile (`recall` and `recall_all`
/// ask `session::is_seated_as`), and a seat is a publication that carries a stamp, so this signs
/// the fixture in with the token [`record_with`] uses (`"acct"`) and the seat's stamp is
/// `account_key("acct")`, the stamp the mirror seeded from that record carries.
fn seat(session: &session::TempSession, user: &str) {
    let stamp = session.seated(user, "acct");
    assert_eq!(stamp, plx_plex::plex::grant::account_key("acct"), "the seat's stamp is the fixture record's");
}

/// The capture: the snapshot carries THIS profile's entries for THIS server's machine, read through
/// the mirror, and nothing of another server, another profile or an unseeded mirror. The profile
/// axis is graded with two profiles that both remembered something on the same server: the owner
/// (no Home profile published) reads the owner's list, and a published profile reads only its own.
#[test]
fn the_snapshot_captures_this_profiles_memory_for_the_servers_machine() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    // The fixture's Drop unpublishes whatever profile this test published.
    let session = session::TempSession::new("track-memory-capture");
    let stamp = plx_plex::plex::grant::account_key("acct");
    let here = TrackMemoryEntry::new("machine-a", "10", None, Some(SubtitleMemo::Off));
    let there = TrackMemoryEntry::new("machine-b", "20", None, Some(SubtitleMemo::Off));
    let other_profile = TrackMemoryEntry::new("machine-a", "30", None, Some(SubtitleMemo::Off));
    let kids_here = TrackMemoryEntry::new("machine-a", "40", None, Some(SubtitleMemo::Off));
    let kids_there = TrackMemoryEntry::new("machine-b", "50", None, Some(SubtitleMemo::Off));
    let sid_a = plx_plex::plex::register_for_test("machine-a", "127.0.0.1", 32401, "tok", "cid-a");
    let sid_b = plx_plex::plex::register_for_test("machine-b", "127.0.0.1", 32402, "tok", "cid-b");
    let snapshot_memory = |ps: &PlaybackSession, sid| {
        ResolveEnv::snapshot(ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk").memory
    };

    // Seated, so that what answers the first read is the mirror and not the seat gate (which has
    // its own test below).
    seat(&session, "");
    assert_eq!(snapshot_memory(&ps, sid_a), Vec::<TrackMemoryEntry>::new(), "a mirror never seeded holds nothing");

    seed_memory_for_test::<TrackMemoryEntry>(&record_with(vec![
        list("", &stamp, vec![here.clone(), there.clone()]),
        list("kid", &stamp, vec![kids_here.clone(), kids_there.clone()]),
        list("someone-else", &stamp, vec![other_profile]),
    ]));
    assert_eq!(snapshot_memory(&ps, sid_a), vec![here.clone()], "the owner: this profile, this machine");
    assert_eq!(snapshot_memory(&ps, sid_b), vec![there], "the other server's entries are its own");
    assert!(!ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), sid_a, "rk").fresh_play,
        "a snapshot is nobody's fresh play");

    seat(&session, "kid");
    assert_eq!(snapshot_memory(&ps, sid_a), vec![kids_here.clone()], "a published profile reads its own list, not the owner's");
    assert_eq!(snapshot_memory(&ps, sid_b), vec![kids_there]);

    seed_memory_for_test::<TrackMemoryEntry>(&record_with(vec![list("kid", "a-stale-stamp", vec![kids_here])]));
    assert_eq!(snapshot_memory(&ps, sid_a), Vec::<TrackMemoryEntry>::new(), "another account's stamp reads as nothing");
    tidy();
}

/// **The seat gate on the whole-list read.** `recall_all` answers only for the SEATED profile: a
/// profile published without a stamp (`watching`: an unseated boot, a dev token) seats nobody and
/// reads no track memory, even with a seeded mirror holding that profile's stamped record; the same
/// record read once the profile is seated is the whole list.
#[test]
fn track_memory_is_read_only_for_the_seated_profile() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-seat-gate");
    let stamp = plx_plex::plex::grant::account_key("acct");
    let entry = TrackMemoryEntry::new("machine-a", "10", None, Some(SubtitleMemo::Off));
    let sid = plx_plex::plex::register_for_test("machine-a", "127.0.0.1", 32401, "tok", "cid-a");
    seed_memory_for_test::<TrackMemoryEntry>(&record_with(vec![list("", &stamp, vec![entry.clone()])]));
    let snapshot_memory = |ps: &PlaybackSession| {
        ResolveEnv::snapshot(ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk").memory
    };

    session.watching("");
    assert_eq!(snapshot_memory(&ps), Vec::<TrackMemoryEntry>::new(), "an unstamped publication seats nobody");

    seat(&session, "");
    assert_eq!(snapshot_memory(&ps), vec![entry], "the seated profile reads its own list");
    tidy();
}

// ---- W4: fresh_play ----------------------------------------------------------------------------

fn request(sid: ServerId, preview: bool) -> PlaybackRequest {
    PlaybackRequest {
        sid,
        rk: "rk-ep".into(),
        part: "/library/parts/1/1/file.mkv".into(),
        vcodec: "hevc".into(),
        acodec: "eac3".into(),
        title: "Episode".into(),
        ctx: String::new(),
        preview,
        version: None,
    }
}

fn env_of(ps: &PlaybackSession, request: &PlaybackRequest, retry: Option<RetryContext>) -> ResolveEnv {
    request_env(ps, plx_data::stores::metadata::MetadataStore::default().view(), request, retry)
}

/// A viewer's launch is the one fresh play. Every retry, whichever caller built it, and every
/// preview is not: only a fresh play may write a remembered choice back to the server.
#[test]
fn fresh_play_is_false_for_every_retry_and_preview() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, Plan { url: "http://fixture.invalid/ep.mkv".into(), ..Default::default() }, "rk-ep");
    let sid = unregistered_sid();

    assert!(env_of(&ps, &request(sid, false), None).fresh_play, "a viewer's launch is fresh");
    assert!(!env_of(&ps, &request(sid, true), None).fresh_play, "a preview is not");

    let retries = [
        ("current_retry_context (the in-flight contract re-resolve)", current_retry_context(&ps, 0)),
        ("rescue_retry_context (the start-failure rescue)", rescue_retry_context(&ps, 0)),
        ("retry_context_with (the Retry press)", retry_context_with(&ps, 0, None)),
        ("retry_context_with (Switch to Auto and play)", retry_context_with(&ps, 0, Some(DirectPlayMode::Auto))),
        ("an unknown selection (a refused plan's retry)", RetryContext { selection: None, ..current_retry_context(&ps, 0) }),
    ];
    for (name, retry) in retries {
        for preview in [false, true] {
            assert!(!env_of(&ps, &request(sid, preview), Some(retry)).fresh_play, "{name}, preview={preview}");
        }
    }
    reset_session(&mut ps);
}

/// A retry that carries the viewer's own pair drops the memory (that pair outranks it); one that
/// carries none (nothing had landed) keeps it, and is still not a fresh play.
#[test]
fn a_retry_with_a_known_selection_drops_the_memory_and_one_without_keeps_it() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-retry");
    seat(&session, "");
    let sid = plx_plex::plex::register_for_test("machine-a", "127.0.0.1", 32401, "tok", "cid-a");
    let entry = TrackMemoryEntry::new("machine-a", "10", None, Some(SubtitleMemo::Off));
    seed_memory_for_test::<TrackMemoryEntry>(&record_with(vec![list("", &plx_plex::plex::grant::account_key("acct"), vec![entry.clone()])]));
    apply_plan(&mut ps, Plan { url: "http://fixture.invalid/ep.mkv".into(), sub_sid: 23, ..Default::default() }, "rk-ep");

    let fresh = env_of(&ps, &request(sid, false), None);
    assert_eq!(fresh.memory, vec![entry.clone()]);

    let known = env_of(&ps, &request(sid, false), Some(current_retry_context(&ps, 0)));
    assert!(known.memory.is_empty(), "the viewer's pair for THIS playback outranks the memory");
    assert_eq!(known.subtitle_override, Some(23));

    let unknown = env_of(&ps, &request(sid, false), Some(RetryContext { selection: None, ..current_retry_context(&ps, 0) }));
    assert_eq!(unknown.memory, vec![entry], "nothing was chosen yet, so the memory still reads");
    assert!(!unknown.fresh_play);
    tidy();
}

// ---- the session, and the audio commit -----------------------------------------------------------

/// The landing keeps the two facts the memory is keyed and gated on, and the menu's note.
#[test]
fn the_landing_carries_the_show_the_kind_and_the_note() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let playing = plx_data::metadata::PlayingItem {
        kind: "episode".into(),
        show_rk: SHOW.into(),
        ..plx_data::metadata::PlayingItem::with_subs(Vec::new())
    };
    apply_plan(
        &mut ps,
        Plan { url: "http://fixture.invalid/ep.mkv".into(), playing: Some(playing), memory_note: true, ..Default::default() },
        "rk-ep",
    );
    assert_eq!((cur_show_rk(&ps), cur_kind(&ps), cur_memory_note(&ps)), (SHOW, "episode", true));
    let published = ps.publication();
    assert_eq!((cur_show_rk(&published), cur_kind(&published), cur_memory_note(&published)), (SHOW, "episode", true));

    apply_plan(&mut ps, Plan { url: "http://fixture.invalid/next.mkv".into(), ..Default::default() }, "rk-next");
    assert_eq!((cur_show_rk(&ps), cur_kind(&ps), cur_memory_note(&ps)), ("", "", false), "the next item starts clean");
    reset_session(&mut ps);
}

/// The audio commit says whether the pick stood: the memory is written on `true` only. The Forced
/// refusal is the one `false`.
#[test]
fn commit_audio_selection_is_false_only_for_the_forced_refusal() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    apply_plan(&mut ps, Plan::default(), "rk-ep");
    assert!(commit_audio_selection(&mut ps, CarriedAudio::named(31, -1)), "an ordinary pick stands");

    let mut forced = PlaybackSession::IDLE;
    forced.direct_play_mode = DirectPlayMode::Forced;
    restore_direct_play_mode(DirectPlayMode::Disabled);
    assert!(forced_direct_play(&forced));
    let truehd = CarriedAudio { sid: 9, ordinal: 0, codec: "truehd".into(), channels: 8, can_normalize_loudness: false, immersive: false, sample_rate: 0, bit_depth: 0 };
    assert!(!commit_audio_selection(&mut forced, truehd), "Force Direct Play refuses a track it cannot feed");
    assert_eq!(cur_audio_sid(&forced), 0, "and the selection is untouched");
    restore_direct_play_mode(DirectPlayMode::Auto);
    reset_session(&mut ps);
}

// ---- the write: a track-menu pick is remembered (`commit_audio_pick`, `commit_subtitle_pick`) -----

fn signed_in() -> Session {
    Session { client_id: "cid-1".into(), account_token: "acct".into(), ..Default::default() }
}

/// What `user` has remembered for the playing show, read through the door's own `recall`.
fn remembered(user: &str) -> Option<TrackMemoryEntry> {
    recall::<TrackMemoryEntry>(user, &(MACHINE.to_string(), SHOW.to_string()))
}

/// Everything `user` has remembered, for any show.
fn remembered_anything(user: &str) -> Vec<TrackMemoryEntry> {
    recall_all::<TrackMemoryEntry>(user)
}

/// What the FILE holds for `user`, once the storage worker has run the queued write.
fn filed(user: &str) -> Vec<TrackMemoryEntry> {
    let stamp = plx_plex::plex::grant::account_key("acct");
    session::peek()
        .prefs
        .track_memory
        .iter()
        .find(|l| l.user == user && l.account == stamp)
        .map(|l| l.items.clone())
        .unwrap_or_default()
}

/// The state of a stored-session boot: signed in, `user` seated, the mirror seeded from the record.
fn sign_in_and_seat(session: &session::TempSession, user: &str) {
    session::save(&signed_in());
    session.seated(user, "acct");
    seed_memory_for_test::<TrackMemoryEntry>(&session::peek());
}

/// Land a playback of `kind` / `show_rk` on a registered server (machine [`MACHINE`]). The plan names
/// no Part, so a commit sends no selection PUT.
fn land_playing(ps: &mut PlaybackSession, kind: &str, show_rk: &str) -> ServerId {
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", 32401, "tok", "cid-tm");
    let playing = plx_data::metadata::PlayingItem {
        kind: kind.into(),
        show_rk: show_rk.into(),
        ..plx_data::metadata::PlayingItem::with_subs(Vec::new())
    };
    apply_plan(ps, Plan { sid, url: "http://fixture.invalid/ep.mkv".into(), playing: Some(playing), ..Default::default() }, "rk-ep");
    sid
}

/// Leave the process as the next test expects it.
fn tidy_write(ps: &mut PlaybackSession) {
    reset_session(ps);
    tidy();
}

/// **The write.** A menu pick on an episode is remembered under the profile, the server's machine and
/// the show; the other half of the entry is left as it was.
#[test]
fn a_menu_pick_is_remembered_for_the_show_and_each_half_keeps_the_other() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-write");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);
    assert_eq!(remembered(""), None, "setup: nothing is remembered yet");

    assert!(commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), Some(japanese_eac3())));
    assert_eq!(remembered(""), Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), None)));

    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
    assert_eq!(
        remembered(""),
        Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), Some(english_track()))),
        "the subtitle pick kept the audio half"
    );

    let spanish = AudioMemo { lang: "spa".into(), ..japanese_eac3() };
    assert!(commit_audio_pick(&mut ps, CarriedAudio::named(36016, 2), Some(spanish.clone())));
    assert_eq!(
        remembered(""),
        Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(spanish), Some(english_track()))),
        "and a second audio pick replaced the audio half and kept the subtitle"
    );
    tidy_write(&mut ps);
}

/// A commit that is not the viewer's choice (a harness setup) carries no memo and remembers nothing.
#[test]
fn a_commit_that_carries_no_memo_remembers_nothing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-no-memo");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);

    assert!(commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), None), "the route still took the pick");
    commit_subtitle_pick(&mut ps, 0, 55001, true, None);
    assert_eq!(cur_audio_sid(&ps), 36015, "setup: the picks stood");
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new());
    tidy_write(&mut ps);
}

/// **A subtitle commit that changes nothing remembers nothing.** The track menu opens on the
/// checked row and an OK there still commits (the route republishes), so a press to close the menu
/// on the Off the ladder chose, or on the track it chose, reaches the door with a descriptor; the
/// ladder's pick is not the viewer's choice (`track-memory-plan.md` §4.4: "Not written: any
/// auto-pick"), so the stream the route already shows is never written. The controls: a pick of
/// another stream is, and so is an Off after a track.
#[test]
fn a_subtitle_commit_that_changes_nothing_remembers_nothing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-unchanged");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);
    assert_eq!(cur_sub_sid(&ps), 0, "setup: the ladder landed on Off");

    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "OK on the Off the ladder chose wrote nothing");

    set_subtitle(&mut ps, 55001);
    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "nor did OK on the track the ladder chose");

    commit_subtitle_pick(&mut ps, 1, 55002, true, Some(english_track()));
    assert_eq!(
        remembered(""),
        Some(TrackMemoryEntry::new(MACHINE, SHOW, None, Some(english_track()))),
        "control: another track is the viewer's choice"
    );
    commit_subtitle_pick(&mut ps, 1, 55002, true, Some(SubtitleMemo::Off));
    assert_eq!(
        remembered("").and_then(|e| e.subtitle),
        Some(english_track()),
        "OK on that track again is not a second choice, whatever it carries"
    );
    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    assert_eq!(
        remembered("").and_then(|e| e.subtitle),
        Some(SubtitleMemo::Off),
        "control: Off after a track is a change, remembered as Off"
    );
    tidy_write(&mut ps);
}

/// **The remembered-subtitle note ends when what it explained does.** `cur_memory_note` is set by a
/// landing, but a sidecar the app draws over the copy remux is drawn in place: no landing runs, so
/// the flag would outlive the fact (the chosen subtitle "can't be shown") while the viewer's own
/// subtitle is on screen. Clearing it belongs to a commit that changes what is shown. The control
/// is the press that does not: Off, which the route already shows, leaves the note true.
#[test]
fn an_in_place_subtitle_commit_ends_the_remembered_note() {
    let mut idle = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut idle);
    let (mut ps, _sid) =
        enhancement_test_session(EnhTestFixture { remux: Some(true), memory_note: true, ..Default::default() });
    assert!(cur_memory_note(&ps) && is_transcoding(&ps), "setup: the note case, on a live copy remux");
    assert_eq!(cur_sub_sid(&ps), 0, "setup: Off is on screen");

    commit_subtitle_pick(&mut ps, -1, 0, false, None);
    assert!(cur_memory_note(&ps), "control: Off over Off changes nothing shown, so the note is still true");

    commit_subtitle_pick(&mut ps, -1, 78, true, None);
    assert_eq!(cur_sub_sid(&ps), 78, "setup: the sidecar stands");
    assert!(
        !pending_user_route_intent(UserRouteIntent::Retranscode),
        "setup: it is drawn in place, so no landing will ever clear the note for it"
    );
    assert!(!cur_memory_note(&ps), "the viewer's subtitle is on screen: the note is no longer true");
    reset_player_control_for_test(&ps);
    tidy();
}

/// **A pick the route refuses is not remembered.** Force Direct Play refuses a track it cannot feed
/// (`commit_audio_selection` is `false`); remembering it would put a track on the next episode that
/// this route will not play. The control: a track it CAN feed, under the same mode, is remembered.
#[test]
fn a_pick_the_route_refuses_writes_nothing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-refused");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);
    ps.direct_play_mode = DirectPlayMode::Forced;
    restore_direct_play_mode(DirectPlayMode::Disabled);
    assert!(forced_direct_play(&ps), "setup: Force Direct Play is on");

    let truehd = CarriedAudio { sid: 9, ordinal: 0, codec: "truehd".into(), channels: 8, can_normalize_loudness: false, immersive: false, sample_rate: 0, bit_depth: 0 };
    assert!(!commit_audio_pick(&mut ps, truehd, Some(japanese_eac3())), "the route refuses TrueHD it cannot feed");
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "and the refused pick wrote nothing");

    let eac3 = CarriedAudio { sid: 10, ordinal: 1, codec: "eac3".into(), channels: 6, can_normalize_loudness: false, immersive: false, sample_rate: 0, bit_depth: 0 };
    assert!(commit_audio_pick(&mut ps, eac3, Some(japanese_eac3())), "control: a track the route can feed stands");
    assert_eq!(remembered(""), Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), None)));
    restore_direct_play_mode(DirectPlayMode::Auto);
    tidy_write(&mut ps);
}

/// **Off is remembered as Off**, absolute: it is stored as `SubtitleMemo::Off` (not as the absence of
/// a track), replaces the track that was remembered, and the next resolve honours it over the
/// account's rule and the server's selection.
#[test]
fn a_menu_off_is_stored_as_off_and_beats_the_accounts_rule() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-off");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);

    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    let entry = remembered("").expect("Off is a choice, and a choice is remembered");
    assert_eq!(entry.subtitle, Some(SubtitleMemo::Off), "Off replaced the remembered track");

    #[cfg(feature = "devtriggers")]
    {
        let mut case = Case::episode(vec![srt(55001, 3, "eng", false, false)], remembered_anything(""));
        case.account = Some((Some("eng".into()), 2, 0));
        let (plan, _, _) = resolve_in(&mut ps, case, |_, plan| plan);
        assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (0, None), "the next episode shows nothing, whatever the account says");
    }
    tidy_write(&mut ps);
}

/// **Episodes only.** A movie, an item no show claims, a track and an item of no kind write nothing;
/// the control (an episode of a known show) writes.
#[test]
fn only_an_episode_of_a_known_show_is_remembered() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-kinds");
    sign_in_and_seat(&session, "");
    for (kind, show_rk) in [("movie", SHOW), ("episode", ""), ("track", SHOW), ("", SHOW)] {
        land_playing(&mut ps, kind, show_rk);
        commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), Some(japanese_eac3()));
        commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
        assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "{kind:?}/{show_rk:?} wrote nothing");
    }
    land_playing(&mut ps, "episode", SHOW);
    commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), Some(japanese_eac3()));
    assert_eq!(remembered_anything("").len(), 1, "control: an episode of a known show is remembered");
    tidy_write(&mut ps);
}

/// **Unseated, nothing is written.** A profile published without a seat (an unseated boot, a dev
/// token) reaches the door, and the door refuses. Read back once the profile IS seated, so the empty
/// answer is the mirror's and not the seat gate's.
#[test]
fn a_pick_made_while_unseated_writes_nothing() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-unseated");
    sign_in_and_seat(&session, "");
    session.watching("");
    land_playing(&mut ps, "episode", SHOW);

    assert!(commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), Some(japanese_eac3())), "the route took the pick");
    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
    plx_base::storage_worker::drain_for_test();
    assert_eq!(filed(""), Vec::<TrackMemoryEntry>::new(), "the file got nothing");

    seat(&session, "");
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "and neither did the mirror");
    tidy_write(&mut ps);
}

/// **A second profile on the same server does not see the first's memory**, and its own picks land in
/// its own list, leaving the first's as they were.
#[test]
fn a_second_profile_on_the_same_server_has_its_own_memory() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-profiles");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);
    commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), Some(japanese_eac3()));
    let owners = TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), None);
    assert_eq!(remembered(""), Some(owners.clone()), "setup: the owner remembered a pick");

    seat(&session, "kid");
    assert_eq!(remembered("kid"), None, "the second profile sees nothing of the owner's");
    assert_eq!(remembered_anything("kid"), Vec::<TrackMemoryEntry>::new());

    let spanish = AudioMemo { lang: "spa".into(), ..japanese_eac3() };
    commit_audio_pick(&mut ps, CarriedAudio::named(36016, 2), Some(spanish.clone()));
    assert_eq!(remembered("kid"), Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(spanish), None)));

    seat(&session, "");
    assert_eq!(remembered(""), Some(owners), "and the owner's pick is as it was");
    tidy_write(&mut ps);
}

/// **The write is the single door.** The pick reaches the file through `remember`'s QUEUED update (not
/// applied in line), so it carries the door's guard: a write queued before the client id changed is
/// dropped when it runs (the file never gets it), while an unguarded write would have landed in the
/// new client's file. The mirror keeps it, the declared delta of `decision_memory_tests`.
#[test]
fn the_pick_reaches_the_file_only_through_the_guarded_door() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-door");
    sign_in_and_seat(&session, "");
    land_playing(&mut ps, "episode", SHOW);

    let parked = Parked::new();
    commit_audio_pick(&mut ps, CarriedAudio::named(36015, 1), Some(japanese_eac3()));
    assert_eq!(filed(""), Vec::<TrackMemoryEntry>::new(), "setup: the write is queued behind the parked worker, not applied in line");
    parked.release();
    plx_base::storage_worker::drain_for_test();
    assert_eq!(
        filed(""),
        vec![TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), None)],
        "the queued update wrote the stamped entry"
    );

    let parked = Parked::new();
    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
    session::save(&Session { client_id: "cid-2".into(), account_token: "acct".into(), ..Default::default() });
    parked.release();
    plx_base::storage_worker::drain_for_test();
    assert_eq!(session::peek().client_id, "cid-2", "setup: the client id moved under the queued write");
    assert_eq!(filed(""), Vec::<TrackMemoryEntry>::new(), "the guard dropped the write; the new client's file has nothing");
    tidy_write(&mut ps);
}

/// **U8, the media half of the bug.** One viewer, one server, two episodes the server answers
/// differently (a ratingKey-keyed responder): the viewer picks Japanese audio and English subtitles
/// on E1, and E2, a file with other stream ids, plays the Japanese track of ITS file, and knows the
/// remembered subtitle is withheld (the server remuxes E2, which the app cannot draw on).
#[test]
#[cfg(feature = "devtriggers")]
fn a_pick_on_one_episode_is_what_the_next_episode_resolves_to() {
    fn by_episode(line: &str) -> Option<&'static [u8]> {
        if line.contains("rk-e2") {
            Some(MDE_TRANSCODE_COPY)
        } else if line.contains("rk-e1") {
            Some(MDE_DIRECTPLAY)
        } else {
            None
        }
    }
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-chain");
    sign_in_and_seat(&session, "");
    assert!(plx_net::net::global_init());
    // E1: PlayQueue, MDE decision, two show reads, then the two selection PUTs the picks queue.
    // E2: PlayQueue, two show reads, the remux decision, its selection PUT.
    let (port, rx, server) = plan_pms_keyed(11, MDE_DIRECTPLAY, by_episode);
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", port, "token", "tm-client");
    let store = plx_data::stores::metadata::MetadataStore::default();
    let view = store.view();
    // A viewer's launch of `rk`, in the order `request_play_inner` does it: the outgoing item's live
    // selection is cleared, then the env is built (reading the memory off the mirror).
    let resolve_episode = |ps: &PlaybackSession, rk: &str, part: &str, audio: Vec<plx_data::metadata::Stream>,
                           subs: Vec<plx_data::metadata::Stream>, mode: DirectPlayMode| {
        reset_track_selection(sid, rk, None);
        let launch = PlaybackRequest { rk: rk.into(), part: part.into(), ..request(sid, false) };
        let mut env = request_env(ps, view, &launch, None);
        env.direct_play_mode = mode;
        env.cached_item = Some(plx_data::metadata::PlayingItem {
            kind: "episode".into(),
            show_rk: SHOW.into(),
            ..fourk_item_with_subs(sid, audio, subs)
        });
        build_stream(&plx_base::task::OffFrame::for_test(), rk, part, "hevc", "eac3", &env)
    };

    let e1_audio = audio_pair();
    let e1_subs = two_subs();
    let e1 = resolve_episode(&ps, "rk-e1", "/library/parts/36013/1/file.mkv", e1_audio.clone(), e1_subs.clone(), DirectPlayMode::Auto);
    assert_eq!((e1.audio.as_ref().map(|a| a.sid), e1.sub_sid), (Some(36014), 55002), "E1 starts on the server's own selection");
    apply_plan(&mut ps, e1, "rk-e1");

    // The viewer picks, through the menu's commits, the Japanese audio and the English subtitle.
    assert!(commit_audio_pick(&mut ps, CarriedAudio::from_stream(&e1_audio[1], 1), memo_of_audio(&e1_audio, 1)));
    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(memo_of_subtitle(&e1_subs[0])));

    // The next launch retires the outgoing item's selection before it builds its env.
    begin_track_selection(&mut ps, None);
    // E2's file has other ids for the same tracks. The snapshot reads the real mirror.
    let e2_audio = vec![
        plx_data::metadata::Stream { id: 46014, ..eac3_track() },
        stream(46015, 2, "jpn", "eac3", 6),
    ];
    let e2_subs = vec![srt(65001, 3, "eng", false, false), srt(65002, 4, "spa", false, true)];
    let e2 = resolve_episode(&ps, "rk-e2", "/library/parts/46013/1/file.mkv", e2_audio, e2_subs, DirectPlayMode::Disabled);
    let requests = rx.recv_timeout(std::time::Duration::from_secs(15)).expect("PMS never saw both resolves");
    server.join().unwrap();

    assert_eq!(e2.audio.as_ref().map(|a| a.sid), Some(46015), "E2 plays the Japanese track of its own file");
    assert!(e2.url.contains("start.mkv") && e2.contract.remux, "setup: the server remuxes E2: {}", e2.url);
    assert_eq!(e2.sub_sid, 0, "a copy remux does not show the remembered subtitle");
    assert!(e2.memory_note, "and the menu is told the remembered subtitle was withheld: {requests:?}");
    assert_eq!(e2.write_back, None);
    assert_eq!(query_param(transcode_decision(&requests), "audioStreamID"), Some("46015"), "{requests:?}");
    tidy_write(&mut ps);
}

// ---- C5: a remembered EXTERNAL subtitle selects this episode's same-language sidecar -------------

/// A sidecar the app can draw (`sidecar_renderable`): external, an SRT, with the key it is fetched by.
fn sidecar_stream(id: i64, lang: &str) -> plx_data::metadata::Stream {
    plx_data::metadata::Stream {
        key: format!("/library/streams/{id}"),
        external: true,
        ..stream(id, 9, lang, "srt", 0)
    }
}

/// What the menu remembers for an English sidecar picked on the PREVIOUS episode (another stream id,
/// another file): the writer's own descriptor, so the rows below prove it reads back.
#[cfg(feature = "devtriggers")]
fn english_sidecar_memo() -> SubtitleMemo {
    let memo = memo_of_subtitle(&sidecar_stream(41009, "eng"));
    assert!(matches!(&memo, SubtitleMemo::Track(t) if t.external && t.lang == "eng"), "{memo:?}");
    memo
}

/// E2: PMS selected its Spanish track, and the English file beside it is a sidecar of its own.
#[cfg(feature = "devtriggers")]
fn e2_with_english_sidecar() -> Vec<plx_data::metadata::Stream> {
    vec![srt(55002, 4, "spa", false, true), sidecar_stream(55009, "eng")]
}

/// **Direct play.** The remembered external English subtitle plans E2's English sidecar: the plan
/// names it for the landing (`memory_sidecar`), no embedded ordinal rides it, no PUT is sent (the
/// direct-play return has none) and nothing is written back (the Part carries an external, W5).
#[test]
#[cfg(feature = "devtriggers")]
fn c5_a_remembered_external_subtitle_plans_this_episodes_sidecar_on_direct_play() {
    let memo = english_sidecar_memo();
    let (plan, requests, log) = resolve_logged(Case::episode(e2_with_english_sidecar(), memory(None, Some(memo))));
    assert!(is_direct_play(&plan), "{}", plan.url);
    assert_eq!(plan.memory_sidecar, Some(55009), "{requests:?}");
    assert_eq!(plan.sub_render_ordinal, None, "a sidecar has no container ordinal; the landing restores it");
    assert!(!plan.memory_note);
    assert_eq!(puts(&requests), 0, "{requests:?}");
    assert_eq!(plan.write_back, None, "an external on the Part withholds the subtitle half (W5)");
    let lines = memory_lines(&log, 55009);
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].ends_with("track-memory: applied subtitle (track 55009)"), "{}", lines[0]);
}

/// With NO same-language external file on E2 the memo declines and the ladder continues: PMS's own
/// selection plays (never Off), nothing is planned for the landing, and the menu has no note.
#[test]
#[cfg(feature = "devtriggers")]
fn c5_with_no_sidecar_a_remembered_external_subtitle_falls_through() {
    let subs = || vec![srt(55002, 4, "spa", false, true)];
    let (control, _) = resolve(Case::episode(subs(), Vec::new()));
    let (plan, _, log) = resolve_logged(Case::episode(subs(), memory(None, Some(english_sidecar_memo()))));
    assert_eq!(plan.memory_sidecar, None);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (control.sub_sid, control.sub_render_ordinal));
    assert_eq!(plan.sub_sid, 55002, "the next rung: PMS's selection");
    assert!(!plan.memory_note);
    assert!(log.contains("no match (language) subtitle"), "{log}");
}

/// A sidecar in ANOTHER language is not E2's English file: it is not selected, and the route is
/// today's.
#[test]
#[cfg(feature = "devtriggers")]
fn c5_a_different_language_sidecar_is_not_selected() {
    let subs = || vec![srt(55002, 4, "spa", false, true), sidecar_stream(55010, "fra")];
    let (control, _) = resolve(Case::episode(subs(), Vec::new()));
    let (plan, _) = resolve(Case::episode(subs(), memory(None, Some(english_sidecar_memo()))));
    assert_eq!(plan.memory_sidecar, None);
    assert_eq!((plan.sub_sid, plan.sub_render_ordinal), (control.sub_sid, control.sub_render_ordinal));
}

/// Forced vs full is the matcher's own hard constraint: a remembered FULL external subtitle does not
/// take a forced sidecar, and a remembered FORCED one does.
#[test]
#[cfg(feature = "devtriggers")]
fn c5_a_forced_sidecar_is_not_the_full_one() {
    let forced_sidecar = plx_data::metadata::Stream { forced: true, ..sidecar_stream(55011, "eng") };
    let subs = || vec![srt(55002, 4, "spa", false, true), forced_sidecar.clone()];
    let (full, _) = resolve(Case::episode(subs(), memory(None, Some(english_sidecar_memo()))));
    assert_eq!((full.memory_sidecar, full.sub_sid), (None, 55002), "the full memo declines the forced file");
    let forced_memo = memo_of_subtitle(&plx_data::metadata::Stream { forced: true, ..sidecar_stream(41011, "eng") });
    let (forced, _) = resolve(Case::episode(subs(), memory(None, Some(forced_memo))));
    assert_eq!(forced.memory_sidecar, Some(55011));
}

/// **A copy remux the app draws over.** A sidecar is not an embedded track: the app fetches and
/// draws it beside the remux, so unlike a remembered embedded track on this route it is shown, the
/// menu has no note, `subtitles=none` rides the wire and the start PUT keeps naming the file.
#[test]
#[cfg(feature = "devtriggers")]
fn c5_a_remembered_sidecar_is_drawn_over_a_copy_remux_with_no_note() {
    let mut case = Case::episode(e2_with_english_sidecar(), memory(None, Some(english_sidecar_memo())));
    case.mode = DirectPlayMode::Disabled;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    let (plan, requests, log) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv") && plan.contract.remux, "setup: a copy remux: {}", plan.url);
    assert_eq!(plan.memory_sidecar, Some(55009));
    assert_eq!(plan.sub_sid, 55009, "the session's subtitle is the sidecar");
    assert!(!plan.memory_note, "the app draws it, so there is nothing to explain");
    assert_eq!(query_param(&plan.url, "subtitles"), Some("none"), "{}", plan.url);
    assert_eq!(put_subtitle(&requests), Some("55009"), "a selection PUT keeps naming the file: {requests:?}");
    assert!(requests.iter().all(|r| !r.contains("subtitles=burn")), "{requests:?}");
    let lines = memory_lines(&log, 55009);
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].ends_with("track-memory: applied subtitle (track 55009)"), "{}", lines[0]);
}

/// A route that re-encodes video anyway burns the remembered sidecar, as it burns a server-selected
/// one (Q-TOP (b)): the decision, the start URL and the PUT all name the file.
#[test]
#[cfg(feature = "devtriggers")]
fn c5_a_remembered_sidecar_rides_a_video_re_encode_as_a_burn() {
    let mut case = Case::episode(e2_with_english_sidecar(), memory(None, Some(english_sidecar_memo())));
    case.quality = Some(Quality::P720);
    case.mde = MDE_TRANSCODE;
    case.extra = 1;
    let (plan, requests, _) = resolve_logged(case);
    assert!(plan.url.contains("start.mkv"), "setup: a fixed-rung encode: {}", plan.url);
    assert_burned(&plan, &requests, 55009);
}

/// **The landing.** The plan's `memory_sidecar` is looked up in the playing item (its key and codec
/// are THIS episode's) and selected through the one sidecar door: the session shows it as the
/// drawn sidecar. The control is a plan that names none; the other rows are ids the item cannot
/// carry (an embedded track's, a stream the app cannot draw, a stranger).
#[test]
fn c5_the_landing_selects_the_remembered_sidecar_from_the_playing_item() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", 32401, "tok", "cid-tm");
    let pgs = plx_data::metadata::Stream { codec: "pgs".into(), ..sidecar_stream(55012, "eng") };
    let item = || plx_data::metadata::PlayingItem {
        kind: "episode".into(),
        show_rk: SHOW.into(),
        ..plx_data::metadata::PlayingItem::with_subs(vec![
            srt(55002, 4, "spa", false, false),
            sidecar_stream(55009, "eng"),
            pgs.clone(),
        ])
    };
    let land = |ps: &mut PlaybackSession, memory_sidecar: Option<i64>| {
        crate::player::sidecar::reset();
        let plan = Plan {
            sid,
            url: "http://fixture.invalid/ep.mkv".into(),
            playing: Some(item()),
            memory_sidecar,
            ..Default::default()
        };
        apply_plan(ps, plan, "rk-ep");
    };

    land(&mut ps, None);
    assert!(!crate::player::sidecar::selected() && cur_sub_sid(&ps) == 0, "control: nothing names a sidecar");
    for stranger in [55002, 55012, 99999] {
        land(&mut ps, Some(stranger));
        assert!(
            !crate::player::sidecar::selected() && cur_sub_sid(&ps) == 0,
            "{stranger} is not a sidecar this episode can draw"
        );
    }
    land(&mut ps, Some(55009));
    assert!(crate::player::sidecar::selected(), "the sidecar is selected");
    assert_eq!(cur_sub_sid(&ps), 55009);
    assert!(ps.cur_sub_sidecar && ps.cur_sub_client_drawable, "and it is the one the app draws itself");
    crate::player::sidecar::reset();
    tidy_write(&mut ps);
}

// ---- Forced only (FO, §4.9): the third remembered subtitle state -------------------------------

fn forced_only_memo() -> SubtitleMemo {
    SubtitleMemo::ForcedOnly { lang: "eng".into() }
}

/// One full English track and a forced track per language: what a feature film with a foreign-
/// language passage carries. Container order is the id order, so the forced English track is the
/// second subtitle (ordinal 1) and the forced Japanese one the third.
fn forced_subs() -> Vec<plx_data::metadata::Stream> {
    vec![
        srt(55001, 3, "eng", false, false),
        srt(55003, 4, "eng", true, false),
        srt(55004, 5, "jpn", true, false),
    ]
}

/// Land an episode of [`SHOW`] the way a direct-play landing in `mode` does, English audio playing:
/// the item's streams parked on the session for the audio follow and the names re-match, and
/// subtitle `shown` (an index into `subs`; `None` = nothing) on screen.
fn land_with_context(
    ps: &mut PlaybackSession, subs: Vec<plx_data::metadata::Stream>, shown: Option<usize>, mode: SubMode,
) -> ServerId {
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", 32401, "tok", "cid-tm");
    let item = plx_data::metadata::PlayingItem {
        kind: "episode".into(),
        show_rk: SHOW.into(),
        audio: audio_pair(),
        subs,
        ..plx_data::metadata::PlayingItem::with_subs(Vec::new())
    };
    let sub_sid = shown.map_or(0, |i| item.subs[i].id);
    let sub_render_ordinal = shown.map(|i| plx_data::metadata::sub_render_ordinal(&item.subs, i));
    let track_ctx = Some(TrackContext::of_landing(&item, "eng", Some(&forced_only_memo())));
    let plan = Plan {
        sid,
        url: "http://fixture.invalid/ep.mkv".into(),
        playing: Some(item),
        sub_sid,
        sub_render_ordinal,
        sub_mode: mode,
        track_ctx,
        ..Default::default()
    };
    apply_plan(ps, plan, "rk-ep");
    sid
}

fn japanese_audio() -> CarriedAudio {
    CarriedAudio::from_stream(&audio_pair()[1], 1)
}

fn english_audio() -> CarriedAudio {
    CarriedAudio::from_stream(&audio_pair()[0], 0)
}

/// **The landing holds the mode, and a retry carries it.** A remembered Forced only that decided the
/// landing puts the session in Forced only (`cur_sub_stream` alone would read as a pick of that
/// forced track); a retry of that playback (a quality change, a rescue) names the SAME stream by
/// id, so the mode has to ride it or the next audio switch would no longer re-point the subtitle.
/// The controls: a remembered track, and no memory at all, land in their own states.
#[test]
#[cfg(feature = "devtriggers")]
fn a_forced_only_landing_holds_the_mode_and_a_retry_carries_it() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let landed = |ps: &mut PlaybackSession, memory: Vec<TrackMemoryEntry>| {
        resolve_in(ps, Case::episode(forced_subs(), memory), |ps, plan| {
            let landed = (plan.sub_sid, plan.sub_mode);
            apply_plan(ps, plan, "rk-4k");
            landed
        })
        .0
    };

    assert_eq!(landed(&mut ps, Vec::new()), (0, SubMode::Off), "control: no memory, no forced track in play: Off");
    assert_eq!(landed(&mut ps, memory(None, Some(english_track()))), (55001, SubMode::Track), "control: a remembered track");
    assert_eq!(landed(&mut ps, memory(None, Some(SubtitleMemo::Off))), (0, SubMode::Off), "control: a remembered Off");
    assert_eq!(landed(&mut ps, memory(None, Some(forced_only_memo()))), (55003, SubMode::ForcedOnly));
    assert_eq!(cur_sub_mode(&ps), SubMode::ForcedOnly, "the session holds it");

    // The retry names the stream by id; the mode rides it through the same `RetryContext`.
    let retry = current_retry_context(&ps, 0);
    assert_eq!((retry.selection.map(|s| s.1), retry.sub_mode), (Some(55003), SubMode::ForcedOnly));
    let sid = cur_sid(&ps);
    let env = env_of(&ps, &request(sid, false), Some(retry));
    assert_eq!((env.subtitle_override, env.sub_mode), (Some(55003), SubMode::ForcedOnly));
    begin_track_selection(&mut ps, Some(retry));
    assert_eq!(cur_sub_mode(&ps), SubMode::ForcedOnly, "kept while the retry's plan has not landed");
    begin_track_selection(&mut ps, None);
    assert_eq!(cur_sub_mode(&ps), SubMode::Track, "a new item starts in no mode");

    // …and the re-resolve it feeds lands in Forced only, where the control (the same override
    // without the mode) is a plain track.
    let mut retried = Case::episode(forced_subs(), Vec::new());
    retried.sub_sid = 55003;
    let mut control = Case::episode(forced_subs(), Vec::new());
    control.sub_sid = 55003;
    retried.sub_mode = SubMode::ForcedOnly;
    assert_eq!(resolve_in(&mut ps, control, |_, plan| plan).0.sub_mode, SubMode::Track, "control: a known pick is a track");
    let (plan, _, _) = resolve_in(&mut ps, retried, |_, plan| plan);
    assert_eq!((plan.sub_sid, plan.sub_mode), (55003, SubMode::ForcedOnly));
    tidy_write(&mut ps);
}

/// **Forced only is remembered with the language it drew** (or the playing audio's, when nothing is
/// drawn), through the C4 door, and only when the viewer changed something. The audio half is kept.
#[test]
fn the_forced_only_pick_is_remembered_with_its_language_and_only_when_it_changed() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-forced-only-write");
    sign_in_and_seat(&session, "");
    land_with_context(&mut ps, forced_subs(), Some(0), SubMode::Track);
    assert!(commit_audio_pick(&mut ps, japanese_audio(), Some(japanese_eac3())));

    // OK on the row that is already checked closes the menu: the mode is held, nothing is written.
    commit_forced_only_pick(&mut ps, 1, 55003, true, "eng", false);
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (55003, SubMode::ForcedOnly));
    assert_eq!(remembered("").and_then(|e| e.subtitle), None, "an unchanged press writes nothing");

    commit_forced_only_pick(&mut ps, 1, 55003, true, "eng", true);
    assert_eq!(
        remembered(""),
        Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), Some(forced_only_memo()))),
        "`mode: forced_only` with the drawn track's language, and the audio half kept"
    );

    // The language is the caller's: the drawn track's, not a hard-coded one.
    commit_forced_only_pick(&mut ps, 2, 55004, true, "jpn", true);
    assert_eq!(cur_sub_sid(&ps), 55004);
    assert_eq!(
        remembered("").and_then(|e| e.subtitle),
        Some(SubtitleMemo::ForcedOnly { lang: "jpn".into() }),
        "the Japanese forced track was drawn, so Japanese is what is remembered"
    );
    tidy_write(&mut ps);
}

/// **OK on the Forced only row that is already the mode changes nothing the session holds**: the
/// subtitle on screen stays, and so does the language the mode falls back to for untagged audio
/// (an untagged episode drew its forced track through it). A press that CHANGES the mode names the
/// language it chose, which is the control.
#[test]
fn an_unchanged_forced_only_press_keeps_the_subtitle_and_the_fallback_language() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-forced-only-repress");
    sign_in_and_seat(&session, "");
    land_with_context(&mut ps, forced_subs(), Some(1), SubMode::ForcedOnly);
    assert_eq!(forced_only_lang(&ps), "eng", "setup: the landing's memo named the fallback language");

    commit_forced_only_pick(&mut ps, 1, 55003, true, "", false);
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (55003, SubMode::ForcedOnly), "what is shown stays");
    assert_eq!(forced_only_lang(&ps), "eng", "and a press that changes nothing does not blank the fallback");
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "nothing is written");

    commit_forced_only_pick(&mut ps, 2, 55004, true, "jpn", true);
    assert_eq!(forced_only_lang(&ps), "jpn", "control: a changed press names the language it chose");
    tidy_write(&mut ps);
}

/// **A retry of Forced only keeps the language the mode falls back to.** The retry names the stream
/// by id and consults no memory (`apply_retry` clears it), so the memo that supplied the fallback
/// is no longer in the resolve: the landing's context is seeded from the outgoing one. A fresh play
/// is the control: it never inherits the previous item's fallback.
#[test]
#[cfg(feature = "devtriggers")]
fn a_retry_of_forced_only_keeps_the_language_the_mode_falls_back_to() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    // What a resolve would land in, and the fallback it would hold; the session is left as it was,
    // so every case below is asked of the same outgoing landing.
    let context = |ps: &mut PlaybackSession, case: Case| {
        resolve_in(ps, case, |_, plan| (plan.sub_mode, plan.track_ctx.as_ref().map(|c| c.forced_lang.clone()))).0
    };
    resolve_in(&mut ps, Case::episode(forced_subs(), memory(None, Some(forced_only_memo()))), |ps, plan| {
        apply_plan(ps, plan, "rk-4k")
    });
    assert_eq!(forced_only_lang(&ps), "eng", "setup: the memo's language rides the landing");

    let mut retried = Case::episode(forced_subs(), Vec::new());
    retried.sub_sid = 55003;
    retried.sub_mode = SubMode::ForcedOnly;
    assert_eq!(
        context(&mut ps, retried),
        (SubMode::ForcedOnly, Some("eng".to_string())),
        "the retry holds the mode and the fallback"
    );

    let mut track_retry = Case::episode(forced_subs(), Vec::new());
    track_retry.sub_sid = 55003;
    assert_eq!(
        context(&mut ps, track_retry),
        (SubMode::Track, Some(String::new())),
        "control: a retry in no mode inherits no fallback"
    );

    let blank_memo = SubtitleMemo::ForcedOnly { lang: String::new() };
    assert_eq!(
        context(&mut ps, Case::episode(forced_subs(), memory(None, Some(blank_memo)))),
        (SubMode::ForcedOnly, Some(String::new())),
        "control: a fresh play under a memo that names no language inherits nothing"
    );
    tidy_write(&mut ps);
}

/// **Off -> Forced only with no forced track is remembered.** No stream changes (Off before, nothing
/// drawn after), so the C4 rule that an unchanged commit is not a choice must not swallow it: the
/// mode is the news. The control is the same press on a plain Off, which writes nothing.
#[test]
fn off_to_forced_only_with_no_forced_track_is_remembered() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-forced-only-nothing");
    sign_in_and_seat(&session, "");
    land_with_context(&mut ps, vec![srt(55001, 3, "eng", false, false)], None, SubMode::Off);
    assert_eq!(cur_sub_sid(&ps), 0, "setup: Off is on screen");

    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "control: OK on Off remembers nothing");

    commit_forced_only_pick(&mut ps, -1, 0, false, "eng", true);
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (0, SubMode::ForcedOnly), "it shows nothing and is still the mode");
    assert_eq!(
        remembered("").and_then(|e| e.subtitle),
        Some(forced_only_memo()),
        "the viewer chose Forced only, with the playing audio's language"
    );
    tidy_write(&mut ps);
}

/// **Leaving Forced only is a choice**, even onto the stream it was drawing: same stream id, a
/// different state. The control (the press before the mode was entered) is the C4 no-change case.
#[test]
fn leaving_forced_only_for_the_track_it_drew_is_remembered() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-forced-only-leave-track");
    sign_in_and_seat(&session, "");
    land_with_context(&mut ps, forced_subs(), Some(1), SubMode::Track);
    let forced_track = memo_of_subtitle(&forced_subs()[1]);

    commit_subtitle_pick(&mut ps, 1, 55003, true, Some(forced_track.clone()));
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "control: OK on the drawn track is not a choice");

    commit_forced_only_pick(&mut ps, 1, 55003, true, "eng", false);
    assert_eq!(cur_sub_mode(&ps), SubMode::ForcedOnly);
    commit_subtitle_pick(&mut ps, 1, 55003, true, Some(forced_track.clone()));
    assert_eq!(cur_sub_mode(&ps), SubMode::Track, "the pick settles the mode from the stream");
    assert_eq!(remembered("").and_then(|e| e.subtitle), Some(forced_track), "the very track Forced only drew, now chosen as itself");
    tidy_write(&mut ps);
}

/// The same for Off: Forced only with nothing drawn, then Off, is a change although no stream moves.
#[test]
fn leaving_forced_only_for_off_with_nothing_drawn_is_remembered() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-forced-only-leave-off");
    sign_in_and_seat(&session, "");
    land_with_context(&mut ps, vec![srt(55001, 3, "eng", false, false)], None, SubMode::Off);

    commit_forced_only_pick(&mut ps, -1, 0, false, "eng", false);
    assert_eq!(cur_sub_mode(&ps), SubMode::ForcedOnly);
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "setup: entering it unchanged wrote nothing");

    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    assert_eq!(cur_sub_mode(&ps), SubMode::Off);
    assert_eq!(remembered("").and_then(|e| e.subtitle), Some(SubtitleMemo::Off), "Off after Forced only is a change");
    tidy_write(&mut ps);
}

/// **The audio switch re-matches Forced only, and writes no memory.** English audio shows the English
/// forced track; the Japanese audio the Japanese one; back again. The subtitle half of the entry
/// stays Forced only throughout: the re-pointing is derived, not a pick of those tracks. The audio
/// half is the viewer's audio pick, as always.
#[test]
fn an_audio_switch_under_forced_only_re_points_the_subtitle_and_writes_no_memory() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-forced-only-audio");
    sign_in_and_seat(&session, "");
    land_with_context(&mut ps, forced_subs(), Some(1), SubMode::Track);
    commit_forced_only_pick(&mut ps, 1, 55003, true, "eng", true);
    let remembered_forced_only = TrackMemoryEntry::new(MACHINE, SHOW, None, Some(forced_only_memo()));
    assert_eq!(remembered(""), Some(remembered_forced_only.clone()), "setup");

    assert!(commit_audio_selection(&mut ps, japanese_audio()));
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (55004, SubMode::ForcedOnly), "the Japanese forced track");
    assert_eq!(remembered(""), Some(remembered_forced_only.clone()), "an automatic re-match remembers nothing");

    assert!(commit_audio_selection(&mut ps, english_audio()));
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (55003, SubMode::ForcedOnly), "and back to the English one");

    assert!(commit_audio_pick(&mut ps, japanese_audio(), Some(japanese_eac3())));
    assert_eq!(cur_sub_sid(&ps), 55004);
    assert_eq!(
        remembered(""),
        Some(TrackMemoryEntry::new(MACHINE, SHOW, Some(japanese_eac3()), Some(forced_only_memo()))),
        "the viewer's audio pick is written; the subtitle half is still Forced only, not the Japanese track"
    );
    tidy_write(&mut ps);
}

/// The controls of the audio follow: it exists only in Forced only (a picked track stays through an
/// audio switch), it shows NOTHING when the new audio has no forced track (never a full track), and
/// it does not turn the landing's "this copy remux cannot show the remembered subtitle" into a
/// re-encode.
#[test]
fn the_audio_follow_is_forced_only_alone_and_never_falls_back_to_a_full_track() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    land_with_context(&mut ps, forced_subs(), Some(1), SubMode::Track);
    assert!(commit_audio_selection(&mut ps, japanese_audio()));
    assert_eq!(cur_sub_sid(&ps), 55003, "control: a picked track stays when the audio changes");

    let english_only = vec![srt(55001, 3, "eng", false, false), srt(55003, 4, "eng", true, false)];
    land_with_context(&mut ps, english_only, Some(1), SubMode::ForcedOnly);
    assert!(commit_audio_selection(&mut ps, japanese_audio()));
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (0, SubMode::ForcedOnly), "no Japanese forced track: nothing, not 55001");

    land_with_context(&mut ps, forced_subs(), Some(1), SubMode::ForcedOnly);
    ps.cur_memory_note = true;
    assert!(commit_audio_selection(&mut ps, japanese_audio()));
    assert_eq!(cur_sub_sid(&ps), 55003, "the copy remux that shows no remembered subtitle is not rebuilt for one");
    reset_player_control_for_test(&ps);
    tidy_write(&mut ps);
}

/// On a live transcode the re-pointed subtitle is a burn, so the audio switch and the subtitle
/// change both rebuild: they must be ONE Retranscode, and the subtitle the rebuild names is the new
/// one.
#[test]
fn the_audio_follow_on_a_transcode_is_one_rebuild_naming_the_new_subtitle() {
    let mut idle = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut idle);
    let (mut ps, _sid) =
        enhancement_test_session(EnhTestFixture { remux: Some(false), ..Default::default() });
    let item = plx_data::metadata::PlayingItem { audio: audio_pair(), subs: forced_subs(), ..plx_data::metadata::PlayingItem::with_subs(Vec::new()) };
    ps.track_ctx = Some(TrackContext::of_landing(&item, "eng", Some(&forced_only_memo())));
    ps.sub_mode = SubMode::ForcedOnly;
    set_subtitle(&mut ps, 55003);
    reset_player_control_for_test(&ps);
    assert!(is_transcoding(&ps), "setup: a live transcode");
    assert!(!pending_user_route_intent(UserRouteIntent::Retranscode), "setup: nothing pending");

    assert!(commit_audio_selection(&mut ps, japanese_audio()));
    assert_eq!(cur_sub_sid(&ps), 55004, "the Japanese forced track is what the rebuild will burn");
    assert!(
        pending_user_route_intent(UserRouteIntent::Retranscode),
        "ONE rebuild is owed (the reducer holds a single pending user intent): the audio and the subtitle coalesced"
    );
    reset_player_control_for_test(&ps);
    tidy();
}

/// A resolve of an MP4-like part: PMS gives the subtitle streams no `title` and no forced flag, so
/// the resolve cannot find the forced track; the demuxer's own names say ordinal 1 is "Forced".
#[cfg(feature = "devtriggers")]
fn land_mp4_forced_only(ps: &mut PlaybackSession) {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55003, 4, "eng", false, false)];
    let case = Case::episode(subs, memory(None, Some(forced_only_memo())));
    let landed = resolve_in(ps, case, |ps, plan| {
        let shown = plan.sub_sid;
        apply_plan(ps, plan, "rk-4k");
        shown
    })
    .0;
    // The resolve took its loopback server down with it. Seat the same machine again, so that a
    // memory write made after the landing would find the server the playing item is on: without it
    // "writes no memory" below would hold for the wrong reason.
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", 32401, "tok", "cid-tm");
    assert_eq!(sid, cur_sid(ps), "setup: the landing's server is registered again");
    assert_eq!(landed, 0, "setup: nothing names a forced track yet, so nothing is shown");
    assert_eq!(cur_sub_mode(ps), SubMode::ForcedOnly, "setup: and Forced only is the mode");
}

/// **The first decoded frame's re-match.** The MP4's forced track is found once its names arrive:
/// one derived commit (the session and the server now agree), the mode kept, NO memory write, and
/// once only — a viewer who then moves off it is not pulled back by the next frame.
#[test]
#[cfg(feature = "devtriggers")]
fn mp4_forced_found_after_names_arrive() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-names");
    sign_in_and_seat(&session, "");
    land_mp4_forced_only(&mut ps);
    assert!(ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_some()), "setup: a direct play owes the re-match");

    rematch_subtitle_with_names(&mut ps);
    assert_eq!(cur_sub_sid(&ps), 0, "control: no names yet, so there is nothing to find");
    assert!(ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_none()), "(and it is spent: it asks once per landing)");

    land_mp4_forced_only(&mut ps);
    publish_container_names(&["", "Forced"]);
    rematch_subtitle_with_names(&mut ps);
    assert_eq!(cur_sub_sid(&ps), 55003, "the track the name reveals is now the one shown");
    assert_eq!(cur_sub_mode(&ps), SubMode::ForcedOnly);
    assert_eq!(ps.cur_sub_ordinal, 1);
    assert_eq!(remembered_anything(""), Vec::<TrackMemoryEntry>::new(), "a derived commit writes no memory");

    // Once per landing: the viewer's own move afterwards is left alone.
    set_subtitle(&mut ps, 0);
    rematch_subtitle_with_names(&mut ps);
    assert_eq!(cur_sub_sid(&ps), 0, "the re-match does not run a second time");
    publish_container_names(&[]);
    tidy_write(&mut ps);
}

/// **The re-match says what it committed**, which is what a write-back's send point needs to tell the
/// server: the id it put on screen (`0` = nothing), or `None` when it committed nothing (not owed, no
/// change found, or the viewer decided first). Once per landing.
#[test]
#[cfg(feature = "devtriggers")]
fn the_names_rematch_returns_what_it_committed() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-names-returns");
    sign_in_and_seat(&session, "");

    land_mp4_forced_only(&mut ps);
    assert_eq!(rematch_subtitle_with_names(&mut ps), None, "no names yet: nothing found, nothing committed");

    land_mp4_forced_only(&mut ps);
    publish_container_names(&["", "Forced"]);
    assert_eq!(rematch_subtitle_with_names(&mut ps), Some(55003), "the track the name reveals");
    assert_eq!(rematch_subtitle_with_names(&mut ps), None, "once per landing: the next frame commits nothing");

    land_mp4_forced_only(&mut ps);
    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    assert_eq!(rematch_subtitle_with_names(&mut ps), None, "a viewer who picked first has decided");

    // The other direction: the landing drew a track the PMS flags as forced, and the container's own
    // name says it is a commentary. Forced only then shows nothing, which is `Some(0)`, not `None`.
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55003, 4, "eng", true, false)];
    let shown = resolve_in(&mut ps, Case::episode(subs, memory(None, Some(forced_only_memo()))), |ps, plan| {
        let shown = plan.sub_sid;
        apply_plan(ps, plan, "rk-4k");
        shown
    })
    .0;
    plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", 32401, "tok", "cid-tm");
    assert_eq!(shown, 55003, "setup: the flag drew the forced track");
    publish_container_names(&["", "Director's Commentary"]);
    assert_eq!(rematch_subtitle_with_names(&mut ps), Some(0), "the names say it is a commentary: nothing is shown");
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (0, SubMode::ForcedOnly));
    publish_container_names(&[]);
    tidy_write(&mut ps);
}

/// A viewer who picks before the first frame has decided: the names do not override the pick. Off is
/// the pick the stream id cannot tell apart from the landing (nothing shown before, nothing after),
/// so it is the one the "viewer decided" rule itself has to hold; another track is the control.
#[test]
#[cfg(feature = "devtriggers")]
fn a_pick_before_the_first_frame_ends_the_names_rematch() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-names-pick");
    sign_in_and_seat(&session, "");

    land_mp4_forced_only(&mut ps);
    commit_subtitle_pick(&mut ps, -1, 0, false, Some(SubtitleMemo::Off));
    publish_container_names(&["", "Forced"]);
    rematch_subtitle_with_names(&mut ps);
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (0, SubMode::Off), "the viewer's Off stands");

    land_mp4_forced_only(&mut ps);
    commit_subtitle_pick(&mut ps, 0, 55001, true, Some(english_track()));
    rematch_subtitle_with_names(&mut ps);
    assert_eq!((cur_sub_sid(&ps), cur_sub_mode(&ps)), (55001, SubMode::Track), "and so does a picked track");
    publish_container_names(&[]);
    tidy_write(&mut ps);
}

/// Only a direct play owes the re-match: a transcode carries the server's own subtitle decision, and
/// the demuxer's names do not describe the stream the app plays.
#[test]
#[cfg(feature = "devtriggers")]
fn a_transcode_landing_owes_no_names_rematch() {
    let subs = vec![srt(55001, 3, "eng", false, false), srt(55003, 4, "eng", true, false)];
    let mut case = Case::episode(subs, memory(None, Some(forced_only_memo())));
    case.mode = DirectPlayMode::Disabled;
    case.mde = MDE_TRANSCODE_COPY;
    case.extra = 1;
    let (plan, _) = resolve(case);
    assert!(plan.url.contains("start.mkv"), "setup: a remux: {}", plan.url);
    assert!(plan.track_ctx.as_ref().is_some_and(|c| c.rematch.is_none()), "no re-match is owed off a direct play");
}

/// A direct-play landing of E2 under a remembered EXTERNAL English subtitle (C5): the plan names no
/// subtitle (`sub_sid` 0, the sidecar rides `memory_sidecar`), the landing restores the sidecar, and
/// the first frame's names re-match is owed with the memo. E2 also has an embedded English track.
#[cfg(feature = "devtriggers")]
fn land_with_remembered_sidecar(ps: &mut PlaybackSession) -> ServerId {
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", 32401, "tok", "cid-tm");
    let memo = SubtitleMemo::Track(SubtitleTrackMemo {
        lang: "eng".into(),
        kind: "text".into(),
        external: true,
        ..Default::default()
    });
    let item = plx_data::metadata::PlayingItem {
        kind: "episode".into(),
        show_rk: SHOW.into(),
        audio: audio_pair(),
        subs: vec![srt(55001, 3, "eng", false, false), srt(55002, 4, "spa", false, false), sidecar_stream(55009, "eng")],
        ..plx_data::metadata::PlayingItem::with_subs(Vec::new())
    };
    let mut ctx = TrackContext::of_landing(&item, "eng", Some(&memo));
    ctx.owe_rematch(Some(&memo), 0);
    crate::player::sidecar::reset();
    let plan = Plan {
        sid,
        url: "http://fixture.invalid/ep.mkv".into(),
        playing: Some(item),
        memory_sidecar: Some(55009),
        track_ctx: Some(ctx),
        ..Default::default()
    };
    assert_eq!(plan.sub_sid, 0, "setup: the plan names no subtitle for a sidecar");
    apply_plan(ps, plan, "rk-ep");
    sid
}

/// **The owed re-match holds the landing to what the SESSION shows, not to what the plan names.** The
/// plan names no sidecar (`sub_sid` is 0) and the landing's restore puts the remembered one on screen,
/// so a re-match owed against the plan's 0 would read the landing's own subtitle as "changed".
#[test]
#[cfg(feature = "devtriggers")]
fn the_owed_rematch_is_held_to_the_subtitle_the_landing_shows() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    land_with_remembered_sidecar(&mut ps);
    assert_eq!(cur_sub_sid(&ps), 55009, "setup: the restore put the remembered sidecar on screen");
    let owed = ps.track_ctx.as_ref().and_then(|c| c.rematch.as_ref()).expect("setup: the landing owes the re-match");
    assert_eq!(owed.landed_sub, 55009, "the comparison is made against what is shown, not the plan's 0");
    crate::player::sidecar::reset();
    tidy_write(&mut ps);
}

/// **A sidecar on screen is not displaced by the names re-match.** The container's names describe its
/// embedded tracks; the remembered sidecar was matched against the whole list at the resolve, and
/// outranks E2's embedded English track there. A re-match over the embedded set alone would swap it
/// for that track (or, finding none, for nothing) on the first frame.
#[test]
#[cfg(feature = "devtriggers")]
fn the_names_rematch_never_replaces_a_remembered_sidecar() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let session = session::TempSession::new("track-memory-names-sidecar");
    sign_in_and_seat(&session, "");
    land_with_remembered_sidecar(&mut ps);
    publish_container_names(&["", ""]);
    assert_eq!(rematch_subtitle_with_names(&mut ps), None, "nothing to re-match: the sidecar is the best match");
    assert_eq!(cur_sub_sid(&ps), 55009, "the remembered sidecar stays");
    assert!(ps.cur_sub_sidecar && ps.cur_sub_client_drawable, "and it is still the one the app draws itself");
    assert!(ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_none()), "(and the re-match is spent)");
    publish_container_names(&[]);
    crate::player::sidecar::reset();
    tidy_write(&mut ps);
}
