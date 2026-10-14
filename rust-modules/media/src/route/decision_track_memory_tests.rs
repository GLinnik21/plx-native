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
    /// log is a file the whole process writes, so a line is claimed by the track it names.
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
    pub(super) fn resolve_in<T>(
        ps: &mut PlaybackSession, case: Case, then: impl FnOnce(&mut PlaybackSession, Plan) -> T,
    ) -> (T, Vec<String>, String) {
        assert!(plx_net::net::global_init());
        let log_path = plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::EVENTS);
        let before = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
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
        let written = std::fs::read(&log_path).unwrap_or_default();
        let appended = String::from_utf8_lossy(&written[(before as usize).min(written.len())..]).into_owned();
        (plan, requests, appended)
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
    let truehd = CarriedAudio { sid: 9, ordinal: 0, codec: "truehd".into(), channels: 8, can_normalize_loudness: false, immersive: false };
    assert!(!commit_audio_selection(&mut forced, truehd), "Force Direct Play refuses a track it cannot feed");
    assert_eq!(cur_audio_sid(&forced), 0, "and the selection is untouched");
    restore_direct_play_mode(DirectPlayMode::Auto);
    reset_session(&mut ps);
}
