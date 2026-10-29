//! **The write-back to PMS** (`track-memory-plan.md` §4.8, W6-W11): a remembered choice a fresh play
//! APPLIED is parked at the landing and sent on the landing's first decoded frame, once, off the
//! frame thread, to the right server and the right user.
//!
//! Every test drives the real resolve (`build_stream`), the real landing (`apply_plan`) and the real
//! selection queue against [`selection_pms`], the loopback PMS with a per-Part selection model, and
//! grades the requests the server saw and the selection it holds. The provenance rows (what
//! `plan.write_back` names) and `fresh_play` are C3's, in `decision_track_memory_tests`.
//!
//! The queue's worker is a thread, and a panic there is swallowed, so every assertion runs on the
//! test thread against the fake's request log after the queue has drained.
use super::test_support::apply_plan;
use super::test_support::*;
use super::*;
use plx_plex::plex::session::prefs::{AudioMemo, SubtitleMemo, SubtitleTrackMemo, TrackMemoryEntry};

const MACHINE: &str = "machine-wb";
const SHOW: &str = "4242";
const PART: i64 = 36013;
const RK: &str = "rk-4k";

/// Japanese E-AC3 5.1: the second audio track of the Part.
fn japanese() -> AudioMemo {
    AudioMemo { lang: "jpn".into(), codec: "eac3".into(), channels: 6, ..Default::default() }
}

fn english_text() -> SubtitleMemo {
    SubtitleMemo::Track(SubtitleTrackMemo { lang: "eng".into(), kind: "text".into(), ..Default::default() })
}

fn memory(audio: Option<AudioMemo>, subtitle: Option<SubtitleMemo>) -> Vec<TrackMemoryEntry> {
    vec![TrackMemoryEntry::new(MACHINE, SHOW, audio, subtitle)]
}

/// The Part PMS holds: English (default, selected) and Japanese audio; English (55001) and Spanish
/// (55002) embedded subtitles; `sub_sel` is the subtitle PMS has selected for the viewer (0 = none).
fn part(sub_sel: i64) -> PartModel {
    PartModel {
        id: PART,
        audio: vec![36014, 36015],
        subs: vec![(55001, false), (55002, false)],
        audio_sel: 36014,
        sub_sel,
        forced: Vec::new(),
    }
}

/// What one resolve is asked to do. Defaults to a viewer's launch of a direct play with the
/// ordinary memory (Japanese audio, English subtitle) and a FRESH metadata fetch.
struct Resolve {
    memory: Vec<TrackMemoryEntry>,
    cached: Option<plx_data::metadata::PlayingItem>,
    fresh_play: bool,
    mode: DirectPlayMode,
    quality: Option<Quality>,
    mde: &'static [u8],
    /// The account's subtitle rule `(language, mode, forced)`, standing in for plex.tv.
    account: Option<(Option<String>, i64, i64)>,
}

impl Resolve {
    fn launch() -> Resolve {
        Resolve {
            memory: memory(Some(japanese()), Some(english_text())),
            cached: None,
            fresh_play: true,
            mode: DirectPlayMode::Auto,
            quality: None,
            mde: MDE_DIRECTPLAY,
            account: None,
        }
    }

    /// A retry of the same play that is forced onto a copy remux: the shape a refused direct play's
    /// rescue lands on. Never fresh, and the route PUTs its own selection before it starts.
    fn rescue() -> Resolve {
        Resolve { fresh_play: false, mode: DirectPlayMode::Disabled, mde: MDE_TRANSCODE_COPY, ..Resolve::launch() }
    }

    /// A retry that lands on a direct play again (a seek rebuild, the Original trial's handover).
    fn direct_retry() -> Resolve {
        Resolve { fresh_play: false, ..Resolve::launch() }
    }
}

struct World {
    ps: PlaybackSession,
    pms: SelectionPms,
    sid: ServerId,
    log_from: u64,
    _serial: plx_base::testlock::Serial,
}

fn event_log() -> std::path::PathBuf {
    plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::EVENTS)
}

fn world(part: PartModel) -> World {
    let mut ps = PlaybackSession::IDLE;
    let serial = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let pms = selection_pms(vec![part]);
    let sid = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", pms.port, "token-a", "wb-client");
    let log_from = std::fs::metadata(event_log()).map(|m| m.len()).unwrap_or(0);
    publish_container_names(&[]);
    World { ps, pms, sid, log_from, _serial: serial }
}

impl Drop for World {
    fn drop(&mut self) {
        // Nothing may be left queued for the next test's server, and the session goes back to idle.
        wait_idle();
        plx_plex::plex::reset_servers_for_test();
        reset_session(&mut self.ps);
        publish_container_names(&[]);
    }
}

fn wait_idle() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !selection_queue_idle() {
        assert!(std::time::Instant::now() < deadline, "the selection worker never drained");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

impl World {
    /// The item as a detail page cached it: `model`'s streams, `selected` as `model` says.
    fn item_of(&self, model: &PartModel) -> plx_data::metadata::PlayingItem {
        let audio = model
            .audio
            .iter()
            .enumerate()
            .map(|(n, id)| plx_data::metadata::Stream {
                id: *id,
                index: n as i64 + 1,
                lang_code: if n == 0 { "eng" } else { "jpn" }.into(),
                codec: "eac3".into(),
                channels: 6,
                default: n == 0,
                selected: *id == model.audio_sel,
                ..Default::default()
            })
            .collect();
        let subs = model
            .subs
            .iter()
            .enumerate()
            .map(|(n, (id, external))| plx_data::metadata::Stream {
                id: *id,
                index: n as i64 + 10,
                lang_code: if n == 0 || model.forced.contains(id) { "eng" } else { "spa" }.into(),
                codec: "srt".into(),
                selected: *id == model.sub_sel,
                external: *external,
                key: if *external { format!("/library/streams/{id}") } else { String::new() },
                forced: model.forced.contains(id),
                ..Default::default()
            })
            .collect();
        plx_data::metadata::PlayingItem {
            kind: "episode".into(),
            show_rk: SHOW.into(),
            bitrate: 48_000,
            ..fourk_item_with_subs(self.sid, audio, subs)
        }
    }

    /// `build_stream` for the episode against the fake, exactly as `request_play` runs it.
    fn resolve(&self, how: Resolve) -> Plan {
        self.pms.set_mde(how.mde);
        let store = plx_data::stores::metadata::MetadataStore::default();
        let mut env = ResolveEnv::snapshot(&self.ps, store.view(), self.sid, RK);
        env.direct_play_mode = how.mode;
        if let Some(quality) = how.quality {
            env.quality = quality;
        }
        env.memory = how.memory;
        env.account_subtitles = how.account;
        env.fresh_play = how.fresh_play;
        env.cached_item = how.cached;
        build_stream(
            &plx_base::task::OffFrame::for_test(),
            RK,
            &format!("/library/parts/{PART}/1/file.mkv"),
            "hevc",
            "eac3",
            &env,
        )
    }

    fn land(&mut self, plan: Plan) {
        apply_plan(&mut self.ps, plan, RK);
    }

    /// A landing's decoded frame, through the one function the pump calls for it
    /// ([`on_decoded_frame`]: the container-name re-match, the resume confirmation, then the
    /// write-back's send), then the queue drained. The order is graded where it runs, not copied.
    fn first_frame(&mut self) {
        on_decoded_frame(&mut self.ps);
        wait_idle();
    }

    fn parked(&self) -> bool {
        self.ps.write_back.is_some()
    }

    /// The event log's lines since this world began.
    fn log(&self) -> String {
        let written = std::fs::read(event_log()).unwrap_or_default();
        String::from_utf8_lossy(&written[(self.log_from as usize).min(written.len())..]).into_owned()
    }
}

fn param<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    query_param(line, key)
}

// ---- W6: parked at the landing, sent on the first decoded frame --------------------------------

#[test]
fn write_back_parks_at_landing_and_sends_on_the_first_frame() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    assert_eq!(
        plan.write_back,
        Some(WriteBack { audio: Some(36015), subtitle: Some(SubWrite::Id(55001)) }),
        "setup: the memory applied on a direct play: {:?}",
        w.pms.requests()
    );
    w.land(plan);
    wait_idle();
    assert!(w.parked(), "the landing parks the write");
    assert!(w.pms.puts().is_empty(), "nothing is written at landing: {:?}", w.pms.puts());
    assert_eq!(w.pms.selection(PART), (36014, 55002), "the server still holds its own selection");

    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "exactly one PUT on the first frame: {puts:?}");
    assert!(puts[0].starts_with(&format!("PUT /library/parts/{PART}?")), "{}", puts[0]);
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55001"));
    assert_eq!(param(&puts[0], "allParts"), Some("1"));
    assert_eq!(w.pms.selection(PART), (36015, 55001), "the server now agrees with the screen");
    assert!(w.log().contains(&format!("track-memory: write-back part={PART} audio=36015 sub=55001")), "{}", w.log());

    // The frame after: the value was taken, so nothing more is sent.
    w.first_frame();
    assert_eq!(w.pms.puts().len(), 1, "a play writes once");
}

// ---- M1: a refused direct play never sends; the rescue's PUT is the only one -------------------

#[test]
fn write_back_waits_for_the_first_frame_and_skips_a_refused_direct_play() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    assert!(w.parked());
    assert!(w.pms.puts().is_empty(), "the landing wrote nothing: the television has accepted nothing yet");

    // The television refuses the direct play: the rescue resolves again (never fresh) onto a route
    // that PUTs its own selection, and lands through `apply_plan`, which parks nothing.
    let rescue = w.resolve(Resolve::rescue());
    assert!(rescue.url.contains("start.mkv"), "setup: the rescue is a remux: {}", rescue.url);
    assert_eq!(rescue.write_back, None);
    let after_resolve = w.pms.puts();
    assert_eq!(after_resolve.len(), 1, "the rescue's own start PUT: {after_resolve:?}");
    w.land(rescue);
    assert!(!w.parked(), "the rescue's session carries nothing parked");
    w.first_frame();
    assert_eq!(w.pms.puts(), after_resolve, "the first frame of the rescue adds no write");
}

// ---- W8: once per play --------------------------------------------------------------------------

#[test]
fn write_back_once_per_play() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    w.first_frame();
    assert_eq!(w.pms.puts().len(), 1);

    // A seek rebuild, a quality change, the Original trial's handover and a re-resolve after resume
    // are all retries of this play: each lands with nothing parked and writes nothing.
    for _ in 0..3 {
        let retry = w.resolve(Resolve::direct_retry());
        assert_eq!(retry.write_back, None, "a retry is never fresh");
        w.land(retry);
        assert!(!w.parked());
        w.first_frame();
    }
    assert_eq!(w.pms.puts().len(), 1, "one PUT for the play: {:?}", w.pms.puts());
}

/// A recovery flight, a quality change or a rebuild can replace the route WITHOUT `apply_plan`. The
/// parked write belonged to the direct play it was parked for, so it is dropped rather than doubled
/// onto a transcode that PUTs its own selection (W3).
#[test]
fn a_parked_write_is_dropped_when_the_route_was_replaced_in_place() {
    for replace in ["a transcode took over", "a different url landed"] {
        let mut w = world(part(55002));
        let plan = w.resolve(Resolve::launch());
        w.land(plan);
        assert!(w.parked());
        match replace {
            "a transcode took over" => w.ps.tsession = "encoder-1".into(),
            _ => w.ps.url = format!("{}&replaced=1", w.ps.url),
        }
        w.first_frame();
        assert!(w.pms.puts().is_empty(), "{replace}: {:?}", w.pms.puts());
        assert!(!w.parked(), "{replace}: the value is spent either way");
        assert!(w.log().contains("write-back dropped (the route changed)"), "{replace}");
    }
}

/// A pick made before the first frame carries the live ids in its own PUT, so the parked write is
/// dropped instead of overriding it.
#[test]
fn a_pick_before_the_first_frame_supersedes_the_parked_write() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    set_subtitle(&mut w.ps, 55002);
    w.first_frame();
    assert!(w.pms.puts().is_empty(), "{:?}", w.pms.puts());
    assert!(w.log().contains("write-back dropped (the selection on screen changed)"));
}

// ---- §4.9: the first frame's re-match re-points the parked write ---------------------------------

/// FO's container-name re-match runs on the first decoded frame BEFORE the send, and changes the
/// subtitle on screen. Re-pointing the parked write keeps the remembered audio half and names the
/// re-matched subtitle, instead of the write being dropped as "the selection changed".
#[test]
fn a_rematched_subtitle_is_written_with_the_parked_audio() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: Some(SubWrite::Id(55001)) }), "setup");
    w.land(plan);
    assert!(w.parked());

    // The re-match settles on another track, as `commit_subtitle_selection` would leave the session.
    assert!(repoint_parked_subtitle(&mut w.ps, 55002), "a write is parked, so it takes the re-matched id");
    set_subtitle(&mut w.ps, 55002);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"), "the remembered audio half survives");
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55002"), "the write names the re-matched track");
    assert_eq!(w.pms.selection(PART), (36015, 55002));
    assert!(w.log().contains(&format!("track-memory: write-back part={PART} audio=36015 sub=55002")), "{}", w.log());
}

/// W5 holds for a re-matched track too: a Part that carries an external stream gets no subtitle
/// half, whatever the subtitle on screen becomes. The audio half is still sent, naming the Part's
/// own selection (here none, so the field is omitted).
#[test]
fn a_rematched_subtitle_is_withheld_while_the_part_carries_an_external() {
    let mut model = part(0);
    model.subs.push((55009, true));
    let mut w = world(model);
    let plan = w.resolve(Resolve::launch());
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }), "setup: W5 withheld the subtitle half");
    w.land(plan);
    assert!(repoint_parked_subtitle(&mut w.ps, 55002));
    set_subtitle(&mut w.ps, 55002);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), None, "the re-matched track is not written: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36015, 0));
}

/// W7 holds too: a re-match that settles on Off over a Part with nothing selected writes no explicit
/// Off, and a write with no half left sends nothing.
#[test]
fn a_rematch_to_off_creates_no_explicit_off() {
    // Audio and subtitle remembered: the subtitle half goes, the audio half is still sent.
    let mut w = world(part(0));
    let plan = w.resolve(Resolve::launch());
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: Some(SubWrite::Id(55001)) }), "setup");
    w.land(plan);
    assert!(repoint_parked_subtitle(&mut w.ps, 0));
    set_subtitle(&mut w.ps, 0);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), None, "no explicit Off over nothing: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36015, 0));
    drop(w);

    // Only the subtitle remembered: nothing is left to write, so nothing is parked or sent.
    let mut w = world(part(0));
    let plan = w.resolve(Resolve { memory: memory(None, Some(english_text())), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Id(55001)) }), "setup");
    w.land(plan);
    assert!(repoint_parked_subtitle(&mut w.ps, 0), "the parked write accounts for it: the caller sends no PUT");
    assert!(!w.parked(), "no half is left");
    set_subtitle(&mut w.ps, 0);
    w.first_frame();
    assert!(w.pms.puts().is_empty(), "{:?}", w.pms.puts());
}

/// With nothing parked there is nothing to re-point: the caller commits as an ordinary derived
/// selection, which queues its own PUT.
#[test]
fn repointing_with_nothing_parked_is_a_no_op() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve { memory: Vec::new(), ..Resolve::launch() });
    assert_eq!(plan.write_back, None, "setup: no memory, nothing to write");
    w.land(plan);
    assert!(!repoint_parked_subtitle(&mut w.ps, 55001));
    assert!(!w.parked());
    w.first_frame();
    assert!(w.pms.puts().is_empty(), "{:?}", w.pms.puts());
}

// ---- FO x WB: the first frame's names re-match, through the pump's own call ----------------------
//
// An MP4 part: PMS sends no `title` and no `forced` flag, so the resolve cannot find a forced track;
// the demuxer's names say which track is one. The re-match runs on the first decoded frame BEFORE the
// write-back's send, and the server must hear ONE write that names its result. PMS holds the
// Japanese audio as the Part's own, so the remembered English audio is a half worth writing.

/// English E-AC3 5.1: the first audio track of the Part (PMS selected the Japanese one in
/// [`mp4_part`]).
fn english() -> AudioMemo {
    AudioMemo { lang: "eng".into(), codec: "eac3".into(), channels: 6, ..Default::default() }
}

fn forced_only() -> SubtitleMemo {
    SubtitleMemo::ForcedOnly { lang: "eng".into() }
}

/// The Part of an MP4: PMS selected the Japanese audio and `sub_sel`, and flags no subtitle forced.
/// The English track (55001, ordinal 0) is the one the container's names will call "Forced".
fn mp4_part(sub_sel: i64) -> PartModel {
    PartModel { audio_sel: 36015, ..part(sub_sel) }
}

/// **The two-step flow.** The landing applies the remembered English audio under a Forced only memo
/// that finds no forced track among the embedded streams (nothing shown, written as Off); the
/// container's names then reveal one. The server hears ONE write, on the first frame, naming the
/// remembered audio and the re-matched subtitle: the re-match hands its result to the parked write
/// instead of PUTting on its own and making the write-back drop itself ("the selection changed").
#[test]
fn a_forced_track_found_by_the_containers_names_is_written_with_the_remembered_audio() {
    let mut w = world(mp4_part(55002));
    let plan = w.resolve(Resolve { memory: memory(Some(english()), Some(forced_only())), ..Resolve::launch() });
    assert_eq!(plan.sub_sid, 0, "setup: no title and no flag, so the resolve finds no forced track");
    assert_eq!(
        plan.write_back,
        Some(WriteBack { audio: Some(36014), subtitle: Some(SubWrite::Off) }),
        "setup: the remembered audio, and nothing as the applied result: {:?}",
        w.pms.requests()
    );
    w.land(plan);
    assert!(w.parked(), "setup: the landing parks the write");
    assert!(w.ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_some()), "setup: a direct play owes the re-match");
    assert!(w.pms.puts().is_empty(), "nothing is written at landing: {:?}", w.pms.puts());

    publish_container_names(&["Forced", "", ""]);
    w.first_frame();
    assert_eq!((cur_sub_sid(&w.ps), cur_sub_mode(&w.ps)), (55001, SubMode::ForcedOnly), "the name revealed the track");
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "exactly one PUT: {puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36014"), "the remembered audio half survives");
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55001"), "and names the re-matched track");
    assert_eq!(w.pms.selection(PART), (36014, 55001), "the server now agrees with the screen");
    let log = w.log();
    assert!(log.contains("track-memory: re-matched with the container's names (track 55001)"), "{log}");
    assert!(log.contains(&format!("track-memory: write-back part={PART} audio=36014 sub=55001")), "{log}");
    assert!(!log.contains("write-back dropped"), "the write-back sent; it did not drop itself: {log}");
}

/// W7 for a re-matched Off: the landing drew a track PMS flags forced, and the container's name says
/// it is a commentary, so Forced only comes to nothing. The Part has NO subtitle selected, so writing
/// `subtitleStreamID=0` would create an explicit Off nobody chose: the audio is sent alone.
#[test]
fn a_rematch_to_off_over_a_part_with_nothing_selected_sends_no_explicit_off() {
    let mut model = mp4_part(0);
    model.forced = vec![55001];
    let mut w = world(model);
    let plan = w.resolve(Resolve { memory: memory(Some(english()), Some(forced_only())), ..Resolve::launch() });
    assert_eq!(plan.sub_sid, 55001, "setup: the flag drew the forced track");
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36014), subtitle: Some(SubWrite::Id(55001)) }), "setup");
    w.land(plan);

    publish_container_names(&["Director's Commentary", "", ""]);
    w.first_frame();
    assert_eq!((cur_sub_sid(&w.ps), cur_sub_mode(&w.ps)), (0, SubMode::ForcedOnly), "the name says commentary: nothing");
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36014"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), None, "no explicit Off over nothing: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36014, 0));
}

/// W5 for a re-matched track: a Part that carries an external stream (a download, maybe another Home
/// user's) gets no subtitle half, whatever the container's names make of the embedded set. The track
/// shows on screen; the remembered audio is still written, and moves nothing else.
#[test]
fn a_rematched_forced_track_is_not_written_while_the_part_carries_an_external() {
    let mut model = mp4_part(0);
    model.subs.push((55009, true));
    let mut w = world(model);
    let plan = w.resolve(Resolve { memory: memory(Some(english()), Some(forced_only())), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36014), subtitle: None }), "setup: W5 withheld the subtitle half");
    w.land(plan);

    publish_container_names(&["Forced", "", ""]);
    w.first_frame();
    assert_eq!(cur_sub_sid(&w.ps), 55001, "the re-matched track is on screen");
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36014"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), None, "the re-matched track is not written: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36014, 0));
}

/// With NOTHING parked (a retry is never fresh) the re-match is an ordinary derived commit and PUTs
/// its own selection, as any subtitle change on a direct play does.
#[test]
fn with_nothing_parked_the_rematch_commits_with_its_own_put() {
    let mut w = world(mp4_part(55002));
    let plan = w.resolve(Resolve { memory: memory(Some(english()), Some(forced_only())), ..Resolve::direct_retry() });
    assert_eq!(plan.write_back, None, "setup: a retry writes nothing back");
    w.land(plan);
    assert!(!w.parked(), "setup: and parks nothing");

    publish_container_names(&["Forced", "", ""]);
    w.first_frame();
    assert_eq!((cur_sub_sid(&w.ps), cur_sub_mode(&w.ps)), (55001, SubMode::ForcedOnly));
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "the commit's own PUT: {puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36014"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55001"));
    assert_eq!(w.pms.selection(PART), (36014, 55001));
    assert!(!w.log().contains("track-memory: write-back"), "{}", w.log());
}

/// **A parked write the send would drop never carries the re-match.** The re-match hands its result
/// to the parked write only while that write will really be sent; when the route was replaced in
/// place before the first frame, the parked write is spent (the log says so) and the re-match
/// commits with its own PUT, as it does with nothing parked. Otherwise NO PUT names the subtitle on
/// screen: the server keeps the landing's selection while the screen shows the re-matched track.
#[test]
fn a_stale_parked_write_does_not_swallow_the_rematchs_put_when_the_route_changed() {
    let mut w = world(mp4_part(55002));
    let plan = w.resolve(Resolve { memory: memory(Some(english()), Some(forced_only())), ..Resolve::launch() });
    w.land(plan);
    assert!(w.parked(), "setup: the landing parks the write");
    w.ps.url = format!("{}&replaced=1", w.ps.url);

    publish_container_names(&["Forced", "", ""]);
    w.first_frame();
    assert_eq!(cur_sub_sid(&w.ps), 55001, "the name revealed the track: {}", w.log());
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "the re-match's own PUT, and nothing else: {puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55001"), "{}", puts[0]);
    assert_eq!(w.pms.selection(PART).1, cur_sub_sid(&w.ps), "the server holds what the screen shows");
    let log = w.log();
    assert_eq!(log.matches("write-back dropped (the route changed)").count(), 1, "one drop line: {log}");
    assert!(!w.parked(), "the stale value is spent");
}

/// The same when the viewer picked an AUDIO track before the first frame: the pick's own PUT names
/// the subtitle the landing had, so the parked write is dropped (the selection on screen changed)
/// and the re-match must write its result itself. The invariant is that the last word the server
/// hears names the subtitle on screen. A remembered forced TRACK (not Forced only, whose pick
/// follows the audio's language) is re-matched whatever audio plays, and nothing else ends the
/// owed re-match.
#[test]
fn a_stale_parked_write_does_not_swallow_the_rematchs_put_after_an_audio_pick() {
    let mut w = world(mp4_part(55002));
    let forced_english = SubtitleMemo::Track(SubtitleTrackMemo {
        lang: "eng".into(),
        kind: "text".into(),
        forced: true,
        ..Default::default()
    });
    let plan = w.resolve(Resolve { memory: memory(Some(english()), Some(forced_english)), ..Resolve::launch() });
    w.land(plan);
    assert!(w.parked(), "setup: the landing parks the write");
    let japanese_track = w.item_of(&mp4_part(55002)).audio[1].clone();
    assert_eq!(japanese_track.id, 36015, "setup: the Part's second audio track");
    assert!(commit_audio_selection(&mut w.ps, CarriedAudio::from_stream(&japanese_track, 1)), "setup: the pick stands");
    assert_eq!(cur_audio_sid(&w.ps), 36015, "setup: the viewer is on the Japanese audio");
    assert!(
        w.ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_some()),
        "setup: an audio pick leaves the re-match owed"
    );
    wait_idle();

    publish_container_names(&["Forced", "", ""]);
    w.first_frame();
    assert_eq!(cur_sub_sid(&w.ps), 55001, "the name revealed the track: {}", w.log());
    let puts = w.pms.puts();
    let last = puts.last().unwrap_or_else(|| panic!("no PUT at all\n{}", w.log()));
    assert_eq!(param(last, "subtitleStreamID"), Some("55001"), "the last write names the screen: {puts:?}\n{}", w.log());
    assert_eq!(w.pms.selection(PART), (36015, 55001), "the server holds what the screen shows");
    let log = w.log();
    assert_eq!(log.matches("write-back dropped (the selection on screen changed)").count(), 1, "one drop line: {log}");
    assert!(!w.parked(), "the stale value is spent");
}

/// W5 for the re-match when NOTHING is parked: a subtitle-only memory withholds its subtitle half
/// under W5 (the Part carries an external stream), so no write is parked, and the re-match's own
/// commit must not PUT either: a selection move off a downloaded stream is discarded by PMS along
/// with the download. The re-matched subtitle is still shown on screen.
#[test]
fn an_unparked_rematch_writes_nothing_while_the_part_carries_an_external() {
    let mut model = part(0);
    model.subs.push((55009, true));
    let mut w = world(model);
    let plan = w.resolve(Resolve { memory: memory(None, Some(forced_only())), ..Resolve::launch() });
    w.land(plan);
    assert!(!w.parked(), "setup: W5 withheld the only half, so nothing is parked");
    assert!(w.ps.track_ctx.as_ref().is_some_and(|c| c.rematch.is_some()), "setup: the direct play owes the re-match");

    publish_container_names(&["Forced", "", ""]);
    w.first_frame();
    assert_eq!(cur_sub_sid(&w.ps), 55001, "the re-matched track is on screen: {}", w.log());
    assert!(w.log().contains("track-memory: re-matched with the container's names (track 55001)"), "{}", w.log());
    assert!(w.pms.puts().is_empty(), "no write moves the selection off the download: {:?}", w.pms.puts());
    assert_eq!(w.pms.selection(PART), (36014, 0));
}

// ---- W7: idempotent against this play's own streams ---------------------------------------------

#[test]
fn write_back_skips_when_a_fresh_fetch_agrees() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    w.first_frame();
    assert_eq!(w.pms.puts().len(), 1, "setup: the first play wrote the selection");
    assert_eq!(w.pms.selection(PART), (36015, 55001));

    // The second play of the episode: the fresh fetch shows both halves selected already.
    let again = w.resolve(Resolve::launch());
    assert!(again.playing_fresh, "setup: the item was fetched, not cached");
    assert!(again.write_back.is_some(), "the provenance still names the applied choice");
    w.land(again);
    assert!(!w.parked(), "PMS already agrees, so nothing is parked");
    w.first_frame();
    assert_eq!(w.pms.puts().len(), 1, "a second play sends none: {:?}", w.pms.puts());
}

#[test]
fn write_back_resends_an_id_from_a_cached_item() {
    // The cached copy (a detail page) already shows the remembered tracks selected, but it may
    // predate an earlier write, so an Id half is sent anyway: re-sending what PMS holds is harmless.
    // Each half is graded alone, because a half that is skipped would otherwise be hidden by the
    // other one's PUT.
    let mut model = part(55001);
    model.audio_sel = 36015;

    // The subtitle half alone: the cache shows 55001 selected, and it is still named.
    let mut w = world(part(55002));
    let cached = w.item_of(&model);
    let plan = w.resolve(Resolve { memory: memory(None, Some(english_text())), cached: Some(cached), ..Resolve::launch() });
    assert!(!plan.playing_fresh);
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Id(55001)) }), "setup");
    w.land(plan);
    assert!(w.parked(), "an Id half from a cache is never skipped");
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!((param(&puts[0], "audioStreamID"), param(&puts[0], "subtitleStreamID")), (None, Some("55001")));
    drop(w);

    // The audio half alone: the cache shows 36015 selected, and it is still named.
    let mut w = world(part(55002));
    let cached = w.item_of(&model);
    let plan = w.resolve(Resolve { memory: memory(Some(japanese()), None), cached: Some(cached), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }), "setup");
    w.land(plan);
    assert!(w.parked(), "an Id half from a cache is never skipped");
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
}

#[test]
fn cached_off_with_nothing_selected_is_skipped() {
    // A remembered Off, and a cached copy that shows nothing selected: a stale cache can never
    // create an explicit Off on the server.
    let mut w = world(part(55002));
    let cached = w.item_of(&part(0));
    let plan = w.resolve(Resolve { memory: memory(None, Some(SubtitleMemo::Off)), cached: Some(cached), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Off) }), "setup");
    w.land(plan);
    assert!(!w.parked(), "nothing selected in the copy: the Off half is skipped");
    w.first_frame();
    assert!(w.pms.puts().is_empty(), "{:?}", w.pms.puts());

    // The same Off over a copy that DOES show a subtitle selected is written, as `subtitleStreamID=0`.
    let cached = w.item_of(&part(55002));
    let plan = w.resolve(Resolve { memory: memory(None, Some(SubtitleMemo::Off)), cached: Some(cached), ..Resolve::launch() });
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!((param(&puts[0], "subtitleStreamID"), param(&puts[0], "audioStreamID")), (Some("0"), None));
    assert_eq!(w.pms.selection(PART), (36014, 0));
}

// ---- W3: a route that PUTs its own selection is not written twice -------------------------------

#[test]
fn transcode_start_put_is_not_doubled() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve {
        mode: DirectPlayMode::Disabled,
        quality: Some(Quality::P720),
        mde: MDE_TRANSCODE,
        ..Resolve::launch()
    });
    assert!(plan.url.contains("start.mkv") || plan.contract.ceiling.is_some(), "setup: a 720p rung: {}", plan.url);
    assert_eq!(plan.write_back, None);
    let route_put = w.pms.puts();
    assert_eq!(route_put.len(), 1, "the route's own selection PUT: {route_put:?}");
    w.land(plan);
    assert!(!w.parked());
    w.first_frame();
    assert_eq!(w.pms.puts(), route_put, "the first frame writes nothing on top of it");
}

// ---- W5: an audio-only write never moves the subtitle -------------------------------------------

#[test]
fn audio_only_write_back_keeps_a_server_selected_download() {
    // The viewer's own selected subtitle is a downloaded .srt (external). Rung 2 outranks any
    // remembered subtitle, so the memory is audio only; moving the selection off the download would
    // make PMS delete it, so the write names the download unchanged.
    let mut model = part(55009);
    model.subs.push((55009, true));
    let mut w = world(model);
    let plan = w.resolve(Resolve { memory: memory(Some(japanese()), None), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }), "setup");
    assert_eq!(plan.sub_sid, 55009, "setup: the download is what is drawn");
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55009"), "the download stays selected: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36015, 55009));
    // The sidecar is still there to fetch: a fresh read of the item still lists it.
    let client = plx_plex::plex::client_for(w.sid).expect("the registered slot");
    let fresh = plx_data::metadata::fetch_playing_item(client, w.sid, RK, None).expect("item").0;
    assert!(fresh.subs.iter().any(|s| s.id == 55009 && s.selected && s.external), "the download survived");
}

/// The selection the first frame compares with is what the LANDING put on screen, read from the
/// session after the landing restored the server's sidecar, never the plan's `sub_sid`. A direct
/// play on a route that does not remux (Settings > Direct Play = Forced) never copies the sidecar's
/// id into `plan.sub_sid`, so the plan says 0 while the viewer is looking at the download: the
/// remembered audio must still be written, naming the download unchanged.
#[test]
fn audio_only_write_back_survives_a_sidecar_the_plan_does_not_name() {
    let mut model = part(55009);
    model.subs.push((55009, true));
    let mut w = world(model);
    let plan = w.resolve(Resolve { mode: DirectPlayMode::Forced, memory: memory(Some(japanese()), None), ..Resolve::launch() });
    assert!(!plan.url.contains("start.mkv"), "setup: a direct play: {}", plan.url);
    assert_eq!(plan.sub_sid, 0, "setup: the plan does not name the download the landing will restore");
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }), "setup");
    w.land(plan);
    assert_eq!(cur_sub_sid(&w.ps), 55009, "setup: the landing restored the server's sidecar");
    assert!(w.parked());
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "nobody changed the selection, so the write is sent: {puts:?}\n{}", w.log());
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55009"), "the download stays selected: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36015, 55009));
    assert!(!w.log().contains("write-back dropped"), "{}", w.log());
}

#[test]
fn audio_only_write_back_omits_the_subtitle_when_none_is_selected() {
    let mut w = world(part(0));
    let plan = w.resolve(Resolve { memory: memory(Some(japanese()), None), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }), "setup");
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), None, "no explicit Off is created: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36015, 0));
}

/// The subtitle field of an audio-only write is the Part's own selection, never what the account's
/// rules put on screen: a fall-through pick PMS never had must not be written (W2).
#[test]
fn audio_only_write_back_does_not_write_a_fall_through_subtitle() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve { memory: memory(Some(japanese()), None), ..Resolve::launch() });
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55002"), "the server's own selection, unchanged");
    drop(w);

    // The account's rule ("always show English") turns on a subtitle the server never had selected:
    // it is on the screen, and it is not written.
    let mut w = world(part(0));
    let plan = w.resolve(Resolve {
        memory: memory(Some(japanese()), None),
        account: Some((Some("eng".into()), 2, 0)),
        ..Resolve::launch()
    });
    assert_eq!(plan.sub_sid, 55001, "setup: the account rule's subtitle is what is drawn");
    assert_eq!(plan.write_back, Some(WriteBack { audio: Some(36015), subtitle: None }), "setup");
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "audioStreamID"), Some("36015"));
    assert_eq!(param(&puts[0], "subtitleStreamID"), None, "the fall-through pick is not written: {}", puts[0]);
    assert_eq!(w.pms.selection(PART), (36015, 0));
}

// ---- Forced only (FO) through the write path ----------------------------------------------------

#[test]
fn forced_only_writes_the_forced_track_or_zero() {
    // A forced English track on the Part: Forced only selects it and writes its id.
    let mut model = part(55002);
    model.subs.push((55003, false));
    model.forced = vec![55003];
    let mut w = world(model);
    let forced_only = || memory(None, Some(SubtitleMemo::ForcedOnly { lang: "eng".into() }));
    let plan = w.resolve(Resolve { memory: forced_only(), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Id(55003)) }), "setup");
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("55003"));
    drop(w);

    // No forced track: "nothing" is the applied result, written as 0 over the selected subtitle.
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve { memory: forced_only(), ..Resolve::launch() });
    assert_eq!(plan.write_back, Some(WriteBack { audio: None, subtitle: Some(SubWrite::Off) }), "setup");
    w.land(plan);
    w.first_frame();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert_eq!(param(&puts[0], "subtitleStreamID"), Some("0"));
    assert_eq!(w.pms.selection(PART), (36014, 0));
}

// ---- W10: a failure is logged and never fatal ---------------------------------------------------

#[test]
fn write_back_failure_is_logged_and_playback_continues() {
    let mut w = world(part(55002));
    w.pms.set_fault(true);
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    let (url, sess) = (w.ps.url.clone(), w.ps.sess.clone());
    w.first_frame();
    assert_eq!(w.pms.puts().len(), 1, "one attempt, no retry");
    assert!(
        w.log().contains(&format!("select streams: part={PART} audio=36015 sub=55001 -> HTTP 500")),
        "the failure is logged with its status: {}",
        w.log()
    );
    assert_eq!((w.ps.url.as_str(), w.ps.sess.as_str()), (url.as_str(), sess.as_str()), "playback is untouched");
    assert_eq!(w.pms.selection(PART), (36014, 55002), "the server kept its own selection");
    // No retry on a later frame either.
    w.first_frame();
    assert_eq!(w.pms.puts().len(), 1);
}

// ---- W9: off the frame thread, FIFO, the viewer's own pick lands last ---------------------------

#[test]
fn manual_pick_after_the_first_frame_lands_last() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    // A slow server holds the worker in a first request; the write-back and then the viewer's own
    // pick queue behind it.
    w.pms.set_hold(true);
    queue_put_selection(w.sid, PART, 36014, 55002);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while w.pms.puts().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the worker never reached the server");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    send_parked_write_back(&mut w.ps);
    queue_put_selection(w.sid, PART, 36014, 55001);
    w.pms.set_hold(false);
    wait_idle();
    let puts = w.pms.puts();
    let subs: Vec<_> = puts.iter().map(|p| param(p, "subtitleStreamID").unwrap_or("-")).collect();
    assert_eq!(subs, ["55002", "55001", "55001"], "FIFO: blocker, write-back, the viewer's pick: {puts:?}");
    let audios: Vec<_> = puts.iter().map(|p| param(p, "audioStreamID").unwrap_or("-")).collect();
    assert_eq!(audios, ["36014", "36015", "36014"], "the viewer's pick is last, so it wins: {puts:?}");
    assert_eq!(w.pms.selection(PART), (36014, 55001));
}

// ---- W11: the right user, checked when the PUT runs ---------------------------------------------

/// A request queued under one profile's token must not run under the next profile's. The slot is the
/// same across a switch (its token is blanked and the next grant re-tokens it), so only the credential
/// epoch captured at queue time can tell the two apart. This is a fix to `main` too: the manual
/// picks share the queue.
#[test]
fn queued_put_is_dropped_after_a_profile_switch() {
    let w = world(part(55002));
    let client = plx_plex::plex::client_for(w.sid).expect("the registered slot");
    // Hold the worker inside a first request so the second one sits queued.
    w.pms.set_hold(true);
    queue_put_selection(w.sid, PART, 36014, 55002);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while w.pms.puts().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the worker never reached the server");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    queue_put_selection(w.sid, PART, 36015, 55001);

    // The viewer switches profile while it waits: every token is blanked and the new profile's grant
    // re-tokens the same slot.
    plx_plex::plex::revoke_for_profile_switch();
    client.set_token("token-b");
    let still = plx_plex::plex::client_for(w.sid).expect("the slot is still resolvable, now under the new profile");
    assert!(std::ptr::eq(still, client));

    w.pms.set_hold(false);
    wait_idle();
    let puts = w.pms.puts();
    assert_eq!(puts.len(), 1, "the request queued under the old profile never goes out: {puts:?}");
    assert_eq!(w.pms.selection(PART), (36014, 55002));
    assert!(w.log().contains("select streams: dropped (credential changed since queued)"), "{}", w.log());
}

/// The same drop, for a write-back, names itself in the log.
#[test]
fn a_queued_write_back_is_dropped_after_a_profile_switch() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    let client = plx_plex::plex::client_for(w.sid).expect("the registered slot");
    w.pms.set_hold(true);
    queue_put_selection(w.sid, PART, 36014, 55002);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while w.pms.puts().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the worker never reached the server");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    send_parked_write_back(&mut w.ps);
    plx_plex::plex::revoke_for_profile_switch();
    client.set_token("token-b");
    w.pms.set_hold(false);
    wait_idle();
    assert_eq!(w.pms.puts().len(), 1, "{:?}", w.pms.puts());
    assert!(w.log().contains("track-memory: write-back dropped (profile changed)"), "{}", w.log());
}

/// A live-to-live retoken (a token refresh, not a profile) is the SAME user: the request goes out.
#[test]
fn a_queued_put_survives_a_token_refresh_for_the_same_user() {
    let w = world(part(55002));
    let client = plx_plex::plex::client_for(w.sid).expect("the registered slot");
    w.pms.set_hold(true);
    queue_put_selection(w.sid, PART, 36014, 55002);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while w.pms.puts().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the worker never reached the server");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    queue_put_selection(w.sid, PART, 36015, 55001);
    client.set_token("token-a-refreshed");
    w.pms.set_hold(false);
    wait_idle();
    assert_eq!(w.pms.puts().len(), 2, "{:?}", w.pms.puts());
    assert_eq!(w.pms.selection(PART), (36015, 55001));
}

/// The same drop when the slot's CLIENT is replaced (the server re-registered at another address): a
/// request queued on the old client is not sent on the new one, whose credential was never the
/// asker's. A fresh client draws its `instance_gen` and its `grant_epoch` from the same counter, so
/// either one tells the two apart; this row grades the pair together.
#[test]
fn a_queued_put_is_dropped_when_the_slot_is_re_pointed_at_a_new_client() {
    let w = world(part(55002));
    let first = plx_plex::plex::client_for(w.sid).expect("the registered slot");
    let first_instance = first.instance_gen();
    w.pms.set_hold(true);
    queue_put_selection(w.sid, PART, 36014, 55002);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while w.pms.puts().is_empty() {
        assert!(std::time::Instant::now() < deadline, "the worker never reached the server");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    queue_put_selection(w.sid, PART, 36015, 55001);

    let again = plx_plex::plex::register_for_test(MACHINE, "127.0.0.1", w.pms.port.wrapping_add(1), "token-a", "wb-client");
    assert_eq!(again, w.sid, "setup: the same server keeps its slot");
    let now = plx_plex::plex::client_for(w.sid).expect("the slot still resolves");
    assert_ne!(now.instance_gen(), first_instance, "setup: the slot now holds a different client");

    w.pms.set_hold(false);
    wait_idle();
    assert_eq!(w.pms.puts().len(), 1, "the request queued on the old client never goes out: {:?}", w.pms.puts());
    assert!(w.log().contains("select streams: dropped (credential changed since queued)"), "{}", w.log());
}

// ---- the remembered pick is not shown -----------------------------------------------------------

#[test]
fn a_parked_write_is_dropped_while_the_memory_note_is_up() {
    let mut w = world(part(55002));
    let plan = w.resolve(Resolve::launch());
    w.land(plan);
    assert!(w.parked());
    // The Subtitles menu says the remembered subtitle is not what the route shows.
    w.ps.cur_memory_note = true;
    w.first_frame();
    assert!(w.pms.puts().is_empty(), "{:?}", w.pms.puts());
    assert!(!w.parked(), "the value is spent");
    assert!(w.log().contains("write-back dropped (the remembered pick is not shown)"), "{}", w.log());
}
