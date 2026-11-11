//! Play Version: a picked `Media[]` entry reaches every reader of the play path — the MDE ask,
//! the transcode decision and start URL, the direct-play part, the stream selection and the track
//! store — and none of them takes `Media[0]`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// An episode with two versions, listed default-first: a 1080p h264/aac file and a 4K hevc/eac3
/// one, each with its own part and its own audio stream ids.
const TWO_VERSIONS: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"episode","Media":[
  {"id":3086,"bitrate":2372,"width":1920,"height":1080,"videoResolution":"1080","videoCodec":"h264","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3134,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"aac","channels":2,"index":1,"languageCode":"eng","default":true,"selected":true}]}]},
  {"id":3082,"bitrate":14663,"width":3840,"height":2160,"videoResolution":"4k","videoCodec":"hevc","audioCodec":"eac3","container":"mkv",
   "Part":[{"id":3130,"key":"/library/parts/3130/1/file.mkv","container":"mkv","Stream":[
     {"id":31301,"streamType":1,"codec":"hevc","index":0},
     {"id":31302,"streamType":2,"codec":"eac3","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

const FOUR_K_FIRST: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-4k","type":"movie","Media":[
  {"id":3082,"bitrate":14663,"width":3840,"height":2160,"videoResolution":"4k","videoCodec":"hevc","audioCodec":"eac3","container":"mkv",
   "Part":[{"id":3130,"key":"/library/parts/3130/1/file.mkv","container":"mkv","Stream":[
     {"id":31301,"streamType":1,"codec":"hevc","index":0},
     {"id":31302,"streamType":2,"codec":"eac3","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true}]}]},
  {"id":3086,"bitrate":2372,"width":1920,"height":800,"videoResolution":"1080","videoCodec":"h264","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3134,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"aac","channels":2,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

/// A loopback PMS that serves [`TWO_VERSIONS`] for the item, `mde` for every `/decision`, a
/// bounded body for `start.mkv`, and records every request line until it is told to stop.
fn versions_pms_observed_with_master(body: &'static [u8], mde: &'static [u8],
    observed: Option<std::sync::mpsc::Sender<String>>, master: Option<&'static [u8]>)
    -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    listener.set_nonblocking(true).unwrap();
    let (done, stop) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let line = drain_http(&mut socket);
                    let body: &[u8] = if line.starts_with("GET /library/metadata/rk-v") {
                        body
                    } else if line.starts_with("GET /library/metadata/rk-4k") {
                        FOUR_K_FIRST
                    } else if line.contains("/decision?") {
                        mde
                    } else if line.starts_with("GET /video/:/transcode/universal/start.m3u8?") {
                        master.unwrap_or(EMPTY_MC)
                    } else {
                        EMPTY_MC
                    };
                    write_json(&mut socket, body);
                    if let Some(observed) = &observed { let _ = observed.send(line.clone()); }
                    requests.push(line);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() {
                        break;
                    }
                    std::thread::yield_now();
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        }
        requests
    });
    (port, done, handle)
}

fn versions_pms_observed(body: &'static [u8], mde: &'static [u8],
    observed: Option<std::sync::mpsc::Sender<String>>)
    -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<String>>) {
    versions_pms_observed_with_master(body, mde, observed, None)
}

fn versions_pms_with(body: &'static [u8], mde: &'static [u8]) -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<String>>) {
    versions_pms_observed(body, mde, None)
}

fn versions_pms(mde: &'static [u8]) -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<String>>) {
    versions_pms_with(TWO_VERSIONS, mde)
}

/// Resolve `rk-v` as the request a menu's play makes: the default's part and codecs (what the
/// listing row carries), plus the version the viewer picked.
fn resolve(mde: &'static [u8], version: Option<plx_plex::plex::MediaId>) -> (Plan, Vec<String>) {
    resolve_on("rk-v", ("/library/parts/3134/1/file.mkv", "h264", "aac"), mde, version, None)
}

fn resolve_on(
    rk: &str,
    (part, vcodec, acodec): (&str, &str, &str),
    mde: &'static [u8],
    version: Option<plx_plex::plex::MediaId>,
    caps: Option<plx_platform::devcaps::Caps>,
) -> (Plan, Vec<String>) {
    resolve_with(rk, (part, vcodec, acodec), mde, |env| {
        env.version = version;
        if let Some(caps) = caps {
            env.caps = std::sync::Arc::new(caps);
        }
    })
}

fn resolve_with(
    rk: &str,
    (part, vcodec, acodec): (&str, &str, &str),
    mde: &'static [u8],
    prepare: impl FnOnce(&mut ResolveEnv),
) -> (Plan, Vec<String>) {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, done, server) = versions_pms(mde);
    let sid = plx_plex::plex::register_for_test("versions", "127.0.0.1", port, "token", "versions-client");
    let mut env = ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, rk);
    prepare(&mut env);
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), rk, part, vcodec, acodec, &env);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
    (plan, requests)
}

const FOUR_K_DEFAULT: (&str, &str, &str) = ("/library/parts/3130/1/file.mkv", "hevc", "eac3");

fn full_hd_set() -> plx_platform::devcaps::Caps {
    plx_platform::devcaps::Caps { hevc_max: (1920, 1088), ..plx_platform::devcaps::Caps::assumed() }
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_1080p_decoder_plays_the_1080p_version_listed_after_the_4k() {
    let (plan, requests) = resolve_on("rk-4k", FOUR_K_DEFAULT, MDE_DIRECTPLAY, None, Some(full_hd_set()));
    let mde = requests.iter().find(|l| l.contains("/decision?")).unwrap_or_else(|| panic!("{requests:?}"));
    assert_eq!(query_param(mde, "mediaIndex"), Some("1"), "{mde}");
    assert_eq!(query_param(mde, "audioStreamID"), Some("31342"), "{mde}");
    assert!(plan.url.contains("/library/parts/3134/"), "{}", plan.url);
    assert_eq!((plan.part_id, plan.media_index), (3134, 1));
    assert_eq!((plan.src_vcodec.as_str(), plan.src_acodec.as_str()), ("h264", "aac"));
    assert_eq!(plan.playing.as_ref().map(|p| (p.width, p.audio[0].id)), Some((1920, 31342)));
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_4k_decoder_plays_the_4k_version_listed_first_as_before() {
    let (plan, requests) = resolve_on("rk-4k", FOUR_K_DEFAULT, MDE_DIRECTPLAY, None, None);
    let mde = requests.iter().find(|l| l.contains("/decision?")).unwrap_or_else(|| panic!("{requests:?}"));
    assert_eq!(query_param(mde, "mediaIndex"), Some("0"), "{mde}");
    assert_eq!((plan.part_id, plan.media_index), (3130, 0));
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_picked_version_wins_over_the_decoder() {
    let picked = Some(plx_plex::plex::MediaId(3082));
    let (plan, requests) = resolve_on("rk-4k", FOUR_K_DEFAULT, MDE_DIRECTPLAY, picked, Some(full_hd_set()));
    let mde = requests.iter().find(|l| l.contains("/decision?")).unwrap_or_else(|| panic!("{requests:?}"));
    assert_eq!(query_param(mde, "mediaIndex"), Some("0"), "{mde}");
    assert_eq!((plan.part_id, plan.media_index), (3130, 0));
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_fixed_quality_measures_the_1080p_version_it_resolved() {
    let (plan, requests) = resolve_with("rk-4k", FOUR_K_DEFAULT, MDE_DIRECTPLAY, |env| {
        env.caps = std::sync::Arc::new(full_hd_set());
        env.quality = Quality::P1080;
    });
    assert_eq!(plan.src_measure, (2372, 1920, 800));
    assert!(plan.url.contains("/library/parts/3134/"), "{}", plan.url);
    assert!(!requests.iter().any(|l| l.contains("/transcode/universal/start")), "{requests:?}");
}

fn retry_holding(part_id: i64, audio_sid: i64, sub_sid: i64) -> RetryContext {
    RetryContext {
        direct_play_mode: DirectPlayMode::Auto,
        resume_ns: 0,
        candidate_purpose: CandidatePurpose::ReopenCurrent,
        selection: Some(HeldSelection { part_id, audio_sid, sub_sid }),
        sub_offset_ms: 0,
        suppress_enhancement: false,
        sub_mode: SubMode::Track,
    }
}

#[test]
fn a_retry_keeps_its_tracks_on_their_own_part_and_lets_another_part_choose_again() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let meta = plx_data::stores::metadata::MetadataStore::default();
    let fresh_env = ResolveEnv::snapshot(&ps, meta.view(), ServerId::from_raw(0), "rk-4k");
    let fresh = (fresh_env.audio_sid, fresh_env.sub_sid, fresh_env.subtitle_override, fresh_env.sub_mode);

    let mut env = fresh_env.clone();
    apply_retry(&mut env, retry_holding(3130, 31302, 31303));
    assert!(env.with_selection_for_part(3130).is_none(), "the same part keeps the retry's ids");
    let moved = env.with_selection_for_part(3134).expect("another part chooses its own tracks");
    assert_eq!((moved.audio_sid, moved.sub_sid, moved.subtitle_override, moved.sub_mode), fresh);

    let mut unknown = fresh_env;
    apply_retry(&mut unknown, retry_holding(0, 31302, 31303));
    assert!(unknown.with_selection_for_part(3134).is_none(), "ids of an unknown part are kept as before");
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_retry_that_moves_to_the_1080p_version_does_not_carry_the_4k_tracks() {
    let retry = retry_holding(3130, 31302, 0);
    let (plan, requests) = resolve_with("rk-4k", FOUR_K_DEFAULT, MDE_TRANSCODE_VIDEO, |env| {
        env.caps = std::sync::Arc::new(full_hd_set());
        apply_retry(env, retry);
    });
    assert_eq!(plan.part_id, 3134);
    for line in requests.iter().chain(std::iter::once(&plan.url)) {
        assert!(!line.contains("31302"), "{line}");
    }
    assert!(
        requests.iter().any(|l| l.contains("/decision?") && query_param(l, "audioStreamID") == Some("31342")),
        "{requests:?}"
    );
}

#[test]
#[cfg(feature = "devtriggers")]
fn force_direct_play_attempts_the_4k_original_listed_first() {
    let (plan, _) = resolve_with("rk-4k", FOUR_K_DEFAULT, MDE_DIRECTPLAY, |env| {
        env.caps = std::sync::Arc::new(full_hd_set());
        env.direct_play_mode = DirectPlayMode::Forced;
    });
    assert_eq!((plan.part_id, plan.media_index), (3130, 0));
    assert!(plan.url.contains("/library/parts/3130/"), "{}", plan.url);
}

#[test]
fn a_page_whose_default_the_set_cannot_decode_gives_up_its_cached_copy() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    let sid = ServerId::from_raw(0);
    let version = |id, kbps, resolution: &str, width, height, vcodec: &str| plx_data::pms::MediaVersion {
        id: plx_plex::plex::MediaId(id),
        kbps,
        resolution: resolution.into(),
        width,
        height,
        vcodec: vcodec.into(),
        ..Default::default()
    };
    plx_data::metadata::set_current_for_test(
        meta.state_mut(),
        Some(plx_data::metadata::Detail {
            sid,
            rk: "rk-4k".into(),
            audio: vec![plx_data::metadata::Stream::default()],
            versions: vec![
                version(3082, 14663, "4k", 3840, 2160, "hevc"),
                version(3086, 2372, "1080", 1920, 800, "h264"),
            ],
            ..Default::default()
        }),
    );
    let mut env = ResolveEnv::snapshot(&ps, meta.view(), sid, "rk-4k");
    assert!(env.cached_item.is_some(), "the page is the item");
    env.caps = std::sync::Arc::new(plx_platform::devcaps::Caps::assumed());
    leave_a_cached_default_the_set_cannot_decode(&mut env, meta.view(), sid, "rk-4k");
    assert!(env.cached_item.is_some(), "a 4K decoder plays the page's own default");
    env.caps = std::sync::Arc::new(full_hd_set());
    env.direct_play_mode = DirectPlayMode::Forced;
    leave_a_cached_default_the_set_cannot_decode(&mut env, meta.view(), sid, "rk-4k");
    assert!(env.cached_item.is_some(), "Force Direct Play attempts the page's own default");
    env.direct_play_mode = DirectPlayMode::Auto;
    leave_a_cached_default_the_set_cannot_decode(&mut env, meta.view(), sid, "rk-4k");
    assert!(env.cached_item.is_none(), "the page describes the 4K, which this decoder cannot take");
    plx_data::metadata::set_current_for_test(meta.state_mut(), None);
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_picked_version_direct_plays_its_own_part_after_asking_about_it() {
    let (plan, requests) = resolve(MDE_DIRECTPLAY, Some(plx_plex::plex::MediaId(3082)));
    let mde = requests.iter().find(|l| l.contains("/decision?")).unwrap_or_else(|| panic!("{requests:?}"));
    assert_eq!(query_param(mde, "mediaIndex"), Some("1"), "the MDE asks about the picked version: {mde}");
    assert_eq!(query_param(mde, "audioStreamID"), Some("31302"), "with that version's audio: {mde}");
    assert!(plan.url.contains("/library/parts/3130/"), "the picked version's part plays: {}", plan.url);
    assert_eq!((plan.part_id, plan.media_index), (3130, 1));
    assert_eq!((plan.src_vcodec.as_str(), plan.src_acodec.as_str()), ("hevc", "eac3"));
    let playing = plan.playing.as_ref().expect("track store");
    assert_eq!((playing.width, playing.audio[0].id), (3840, 31302), "the track store is the picked version's");
    for put in requests.iter().filter(|l| l.starts_with("PUT /library/parts/")) {
        assert!(put.starts_with("PUT /library/parts/3130"), "a selection names the picked part: {put}");
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_picked_version_transcodes_from_its_own_index() {
    let (plan, requests) = resolve(MDE_TRANSCODE_VIDEO, Some(plx_plex::plex::MediaId(3082)));
    let decisions: Vec<&String> = requests.iter().filter(|l| l.contains("/decision?")).collect();
    assert!(decisions.len() >= 2, "an MDE ask and a transcode decision: {requests:?}");
    for line in &decisions {
        assert_eq!(query_param(line, "mediaIndex"), Some("1"), "{line}");
    }
    assert!(plan.url.contains("start.mkv"), "{}", plan.url);
    assert!(plan.url.contains("mediaIndex=1&"), "the start URL names the picked version: {}", plan.url);
    assert_eq!(plan.part_id, 3130);
    for put in requests.iter().filter(|l| l.starts_with("PUT /library/parts/")) {
        assert!(put.starts_with("PUT /library/parts/3130"), "a selection names the picked part: {put}");
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_version_the_item_no_longer_lists_plays_the_default() {
    let (plan, requests) = resolve(MDE_DIRECTPLAY, Some(plx_plex::plex::MediaId(9)));
    let mde = requests.iter().find(|l| l.contains("/decision?")).unwrap_or_else(|| panic!("{requests:?}"));
    assert_eq!(query_param(mde, "mediaIndex"), Some("0"), "{mde}");
    assert!(plan.url.contains("/library/parts/3134/"), "{}", plan.url);
    assert_eq!((plan.part_id, plan.media_index), (3134, 0));
    assert_eq!(plan.playing.as_ref().map(|p| p.audio[0].id), Some(31342));
}

#[test]
#[cfg(feature = "devtriggers")]
fn no_pick_is_the_default_version_exactly_as_before() {
    let (plan, requests) = resolve(MDE_DIRECTPLAY, None);
    let mde = requests.iter().find(|l| l.contains("/decision?")).unwrap_or_else(|| panic!("{requests:?}"));
    assert_eq!(query_param(mde, "mediaIndex"), Some("0"), "{mde}");
    assert_eq!((plan.part_id, plan.media_index), (3134, 0));
}

const URI_DTS: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

const URI_DTS_SELECTED_SUBTITLE: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"episode","grandparentRatingKey":"show-off","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true},
     {"id":31343,"streamType":3,"codec":"srt","index":2,"languageCode":"eng","selected":true}]}]}]}]}}"#;

const URI_DTS_EXTERNAL_SUBTITLE: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true},
     {"id":31343,"streamType":3,"codec":"srt","index":2,"languageCode":"eng","selected":true,"key":"/library/streams/31343"}]}]}]}]}}"#;

const URI_AV1_EXTERNAL_SUBTITLE: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"av1","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"av1","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true},
     {"id":31343,"streamType":3,"codec":"srt","index":2,"languageCode":"eng","selected":true,"key":"/library/streams/31343"}]}]}]}]}}"#;

const URI_AV1: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":450000,"Media":[
  {"id":3086,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"av1","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"av1","index":0},
     {"id":31342,"streamType":2,"codec":"aac","channels":2,"index":1,"languageCode":"eng","default":true,"selected":true}]}]},
  {"id":3087,"bitrate":1800,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3135,"duration":450000,"key":"/library/parts/3135/1/file.mkv","container":"mkv","Stream":[
     {"id":31351,"streamType":1,"codec":"h264","index":0},
     {"id":31352,"streamType":2,"codec":"aac","channels":2,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

const MDE_PMS_OUTPUT: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"copy","codec":"h264"},{"streamType":2,"decision":"transcode","codec":"ac3"}]}]}]}]}}"#;

const MDE_PMS_BURNED_EXTERNAL: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"transcode","codec":"h264"},{"streamType":2,"decision":"transcode","codec":"ac3"},{"streamType":3,"id":31343,"decision":"burn"}]}]}]}]}}"#;

const MDE_PMS_HLS_OUTPUT: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"transcode","codec":"h264"},{"streamType":2,"decision":"transcode","codec":"aac"}]}]}]}]}}"#;

#[cfg(feature = "devtriggers")]
const TWO_VARIANT_MASTER: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000000\nhigh/index.m3u8\n";

#[cfg(feature = "devtriggers")]
const FIXED_MEDIA_PLAYLIST: &[u8] = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n";

#[cfg(feature = "devtriggers")]
struct QualifiedNativeMaster;

#[cfg(feature = "devtriggers")]
impl crate::route::uri_preflight::OriginalQualifier for QualifiedNativeMaster {
    fn qualify(&self, _: &plx_base::task::OffFrame,
        _: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        crate::route::uri_preflight::Verdict::Unknown(
            crate::route::uri_preflight::UriDecline::NotCertified)
    }

    fn qualify_pms(&self, _: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::PmsRequest) -> crate::route::uri_preflight::PmsVerdict {
        use crate::route::uri_preflight::{HlsController, HlsMasterEvidence, PmsCertificate,
            PmsRecipe, PmsTimeDomain, PmsVerdict};
        use plx_playback_contract::{Fidelity, Requirements, SourceKind, TrackChoice, TrackKind,
            TrackSelection};
        use std::io::{Read, Write};
        if request.recipe != PmsRecipe::Hls || !request.adaptive {
            return PmsVerdict::Unknown;
        }
        let (origin, path) = plx_net::net::origin::split(&request.source_url);
        assert_eq!(origin.host(), "127.0.0.1", "test qualifier fetches only its loopback PMS");
        let mut socket = std::net::TcpStream::connect((origin.host(), origin.port() as u16))
            .expect("fetch accepted PMS HLS source");
        socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        write!(socket, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .expect("master request");
        let mut response = Vec::new();
        socket.read_to_end(&mut response).expect("master response");
        let Some(body_at) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            return PmsVerdict::Unknown;
        };
        let Some(master) = HlsMasterEvidence::from_response(request.source_url.clone(),
            request.resource_session.clone(), &response[body_at + 4..]) else {
            return PmsVerdict::Unknown;
        };
        PmsVerdict::Known(PmsCertificate { selection: request.selection,
            source_url: request.source_url, resource_session: request.resource_session,
            recipe: request.recipe,
            requirements: Requirements { source: SourceKind::HlsUri,
                video: Fidelity::Compatible, audio: Fidelity::Compatible,
                audio_track: TrackChoice::Selected(31342),
                subtitle_track: TrackChoice::Unspecified },
            native_tracks: vec![plx_playback_contract::uri::NativeTrackIndex {
                selection: TrackSelection { kind: TrackKind::Audio, id: 31342 }, index: 1,
            }],
            applied_audio: request.applied_audio, video_codec: request.video_codec,
            audio_output: request.audio_output, output_dovi: request.output_dovi,
            certified_dovi_output: None, accepted_burn_sid: request.accepted_burn_sid,
            time_domain: PmsTimeDomain::ContentAbsolute,
            hls_controller: Some(HlsController::NativeMaster), hls_master: Some(master),
            release_certified: true,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10) })
    }
}

#[cfg(feature = "devtriggers")]
struct QualifiedPms(std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>);

#[cfg(feature = "devtriggers")]
impl crate::route::uri_preflight::OriginalQualifier for QualifiedPms {
    fn qualify(&self, _: &plx_base::task::OffFrame,
        _: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        crate::route::uri_preflight::Verdict::Unknown(
            crate::route::uri_preflight::UriDecline::NotCertified)
    }

    fn qualify_pms(&self, _: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::PmsRequest) -> crate::route::uri_preflight::PmsVerdict {
        use crate::route::uri_preflight::{PmsAudioOutput, PmsCertificate, PmsRecipe,
            PmsTimeDomain, PmsVerdict};
        use plx_playback_contract::{Fidelity, Requirements, SourceKind, TrackChoice, TrackKind,
            TrackSelection};
        assert!(matches!(request.recipe, PmsRecipe::Remux | PmsRecipe::Encode));
        let audio_codec = match &request.audio_output {
            PmsAudioOutput::Codec(codec) => codec.clone(), PmsAudioOutput::Silent => String::new(),
        };
        self.0.lock().unwrap().push((request.video_codec.clone(), audio_codec,
            request.resource_session.clone()));
        PmsVerdict::Known(PmsCertificate { selection: request.selection,
            source_url: request.source_url, resource_session: request.resource_session,
            recipe: request.recipe,
            requirements: Requirements { source: SourceKind::ProgressiveUri,
                video: Fidelity::Compatible, audio: Fidelity::Compatible,
                audio_track: TrackChoice::Selected(31342),
                subtitle_track: TrackChoice::Unspecified },
            native_tracks: vec![plx_playback_contract::uri::NativeTrackIndex {
                selection: TrackSelection { kind: TrackKind::Audio, id: 31342 }, index: 1,
            }],
            applied_audio: request.applied_audio, video_codec: request.video_codec,
            audio_output: request.audio_output, output_dovi: request.output_dovi,
            certified_dovi_output: None, accepted_burn_sid: request.accepted_burn_sid,
            time_domain: PmsTimeDomain::ContentAbsolute,
            hls_controller: None, hls_master: None, release_certified: true,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10) })
    }
}

#[cfg(feature = "devtriggers")]
struct QualifiedOriginal { video: &'static str, audio: &'static str }

#[cfg(feature = "devtriggers")]
impl crate::route::uri_preflight::OriginalQualifier for QualifiedOriginal {
    fn qualify(&self, _: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        use plx_playback_contract::{Fidelity, Requirements, SourceKind, TrackChoice};
        assert_eq!(request.video_codec, self.video);
        assert_eq!(request.audio_codec, self.audio);
        assert!(!request.dolby_vision.present);
        assert_eq!(request.selection.audio_sid, 31342);
        assert_eq!(request.duration_ms, Some(450_000), "the selected Part overrides item duration");
        assert!(request.source_url.contains("/library/parts/3134/1/file.mkv"));
        let selected = request.selection;
        let subtitle = request.subtitle_source.clone();
        let mut tracks = vec![plx_playback_contract::uri::NativeTrackIndex {
            selection: plx_playback_contract::TrackSelection {
                kind: plx_playback_contract::TrackKind::Audio, id: 31342,
            }, index: 1,
        }];
        let subtitle_choice = if selected.subtitle_sid > 0 {
            tracks.push(plx_playback_contract::uri::NativeTrackIndex {
                selection: plx_playback_contract::TrackSelection {
                    kind: plx_playback_contract::TrackKind::Subtitle,
                    id: selected.subtitle_sid as u64,
                }, index: 2,
            });
            TrackChoice::Selected(selected.subtitle_sid as u64)
        } else { TrackChoice::Unspecified };
        crate::route::uri_preflight::Verdict::Known(crate::route::uri_preflight::KnownSource {
            selection: selected,
            source: plx_playback_contract::uri::UriSource::new(request.source_url, tracks),
            requirements: Requirements { source: SourceKind::ProgressiveUri,
                video: Fidelity::Exact, audio: Fidelity::Exact,
                audio_track: TrackChoice::Selected(31342), subtitle_track: subtitle_choice },
            certified_dolby_vision: None,
            certified_subtitle_source: subtitle,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(15),
        })
    }
}

/// Two audio streams listed OUT of container order: the German one (PMS stream index 2) comes first in
/// the document, the English one (index 1) second, so a position in the list and a position in the file
/// are different numbers.
#[cfg(feature = "devtriggers")]
const URI_TWO_AUDIO_OUT_OF_ORDER: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31344,"streamType":2,"codec":"aac","channels":1,"index":2,"languageCode":"ger"},
     {"id":31342,"streamType":2,"codec":"aac","channels":1,"index":1,"languageCode":"eng","default":true}]}]}]}]}}"#;

/// Records, for every Original the plan asks about, the audio stream it selected and where the Part
/// lists it, and qualifies nothing.
#[cfg(feature = "devtriggers")]
struct RecordingOriginal(std::sync::Arc<std::sync::Mutex<Vec<(i64, Option<i64>)>>>);

#[cfg(feature = "devtriggers")]
impl crate::route::uri_preflight::OriginalQualifier for RecordingOriginal {
    fn qualify(&self, _: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        self.0.lock().unwrap().push((request.selection.audio_sid, request.audio_ordinal));
        crate::route::uri_preflight::Verdict::Unknown(
            crate::route::uri_preflight::UriDecline::NotCertified)
    }
}

/// W6: the plan hands the qualifier the selected audio stream's position among the Part's audio
/// streams in CONTAINER order (PMS `Stream.index`), not its place in the response document and not
/// its stream id.
#[test]
#[cfg(feature = "devtriggers")]
fn the_original_request_carries_the_selected_audio_streams_container_position() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: false,
        audio_off: false, subtitle_off: false };
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(RecordingOriginal(asked.clone()))));
    let (port, done, server) = versions_pms_with(URI_TWO_AUDIO_OUT_OF_ORDER, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("two-audio", "127.0.0.1", port, "token", "two-audio-client");
    let mut env = ResolveEnv::snapshot(&ps,
        plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    for audio_sid in [31344, 31342] {
        env.audio_sid = audio_sid;
        build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
            "/library/parts/3134/1/file.mkv", "h264", "aac", &env);
    }
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    assert_eq!(*asked.lock().unwrap(), [(31344, Some(1)), (31342, Some(0))],
        "the German stream is listed first but is the file's second audio stream");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn remembered_subtitle_off_remains_off_during_uri_source_qualification() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    use plx_plex::plex::session::prefs::{SubtitleMemo, TrackMemoryEntry};
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(QualifiedOriginal { video: "h264", audio: "dca" })));
    let (port, done, server) = versions_pms_with(URI_DTS_SELECTED_SUBTITLE, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("remembered-off", "127.0.0.1", port,
        "token", "remembered-off-client");
    let mut env = ResolveEnv::snapshot(&ps,
        plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    env.memory = vec![TrackMemoryEntry::new("remembered-off", "show-off", None,
        Some(SubtitleMemo::Off))];
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    env.memory.clear();
    let selected = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(plan.uri_original.is_none(),
        "remembered Off requires native subtitle disable, which the URI driver cannot execute");
    assert_eq!(plan.source_selection.expect("source intent").subtitle_sid, 0,
        "remembered Off outranks the PMS-selected embedded subtitle");
    assert_eq!(plan.sub_sid, 0, "the native route must keep subtitles Off");
    assert!(selected.uri_original.is_some(),
        "a selected embedded subtitle with an exact codec and index certificate can qualify");
    assert_eq!(selected.source_selection.expect("selected source intent").subtitle_sid, 31343);
    assert!(requests.iter().any(|line| line.contains("/decision?")),
        "the unsupported native Off intent falls through to Feed MDE: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn selected_external_subtitle_remains_common_source_intent_across_resolve() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let (port, done, server) = versions_pms_with(URI_DTS_EXTERNAL_SUBTITLE, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("external-source", "127.0.0.1", port,
        "token", "external-source-client");
    let mut env = ResolveEnv::snapshot(&ps,
        plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    env.subtitle_override = Some(31343);
    env.sub_sid = 31343;
    let explicit = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    assert_eq!(plan.source_selection.expect("source intent").subtitle_sid, 31343,
        "a client-drawn sidecar is still the viewer's selected source track");
    assert_eq!(explicit.source_selection.expect("explicit source intent").subtitle_sid, 31343,
        "a viewer reconfiguration must retain the selected external source SID");
    plx_plex::plex::reset_servers_for_test();
}

#[cfg(feature = "devtriggers")]
struct CountingQualified {
    original: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pms: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "devtriggers")]
impl crate::route::uri_preflight::OriginalQualifier for CountingQualified {
    fn qualify(&self, off: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        self.original.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::route::uri_preflight::OriginalQualifier::qualify(
            &QualifiedOriginal { video: "h264", audio: "dca" }, off, request)
    }

    fn qualify_pms(&self, off: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::PmsRequest)
        -> crate::route::uri_preflight::PmsVerdict {
        self.pms.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let prior = QualifiedPms(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        crate::route::uri_preflight::OriginalQualifier::qualify_pms(&prior, off, request)
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_qualified_dts_original_lands_from_the_real_resolve_worker_before_feed_decision() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let all = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    let evidence = crate::route::uri_preflight::ProductEvidence { display_exported: true,
        binding: Binding::Exported { canvas: (1920, 1080) }, callable: all,
        certified: all, preview_rectangle: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        evidence, std::sync::Arc::new(QualifiedOriginal { video: "h264", audio: "dca" })));
    let (port, done, server) = versions_pms_with(URI_DTS, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("qualified-dts", "127.0.0.1", port, "token", "qualified-dts-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let start = loop {
        if let Some(start) = pump_play(&mut ps, &mut meta) { break start; }
        assert!(std::time::Instant::now() < deadline, "resolve did not publish an owned URI start");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(start.uri_offer.is_some(), "the addressed landing carries the owned URI source");
    assert!(source_decodable(&ps),
        "a qualified URI original still retains the source's separate Feed conversion feasibility");
    assert!(ps.source_selection().is_some_and(|selection| selection.audio_sid == 31342));
    assert_eq!(ps.source_selection().expect("source intent").subtitle_sid, 0,
        "a file with no subtitle streams has no native track to disable");
    assert_eq!(meta.view().playing().and_then(|item| item.duration_ms), Some(450_000));
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(requests.iter().any(|line| line.starts_with("GET /library/metadata/rk-v")));
    assert!(requests.iter().any(|line| line.starts_with("POST /playQueues")));
    assert!(!requests.iter().any(|line| line.contains("/decision?") || line.contains("/transcode/")
        || line.starts_with("PUT /library/parts/")), "URI original must not acquire a Feed encoder or substitute source: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn a_certified_av1_uri_original_does_not_inherit_feeds_video_refusal() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let all = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) }, callable: all,
            certified: all, preview_rectangle: false },
        std::sync::Arc::new(QualifiedOriginal { video: "av1", audio: "aac" })));
    let (port, done, server) = versions_pms_with(URI_AV1, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("qualified-av1", "127.0.0.1", port, "token", "qualified-av1-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    let feed_cannot_decode_av1 = std::sync::Arc::new(plx_platform::devcaps::Caps {
        av1: false, ..plx_platform::devcaps::Caps::assumed()
    });
    assert!(request_play(&mut ps, &mut meta, feed_cannot_decode_av1, sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "av1", "aac", "Film", "", None));
    let finished = wait_resolve_workers_for_test(std::time::Duration::from_secs(15));
    let start = pump_play(&mut ps, &mut meta);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(finished, "the off-frame resolve did not finish");
    assert!(start.is_some_and(|start| start.uri_offer.is_some()),
        "a source-qualified URI AV1 must not inherit Feed's video gate");
    assert!(!requests.iter().any(|line| line.contains("/decision?") || line.contains("/transcode/")),
        "URI AV1 must not acquire Feed video conversion: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn an_unqualified_uri_original_still_uses_the_feed_compatible_version() {
    use crate::route::uri_preflight::{ProductEvidence, ProductQualifier, ResolvePort};
    use plx_playback_contract::{uri::Binding, Capabilities};
    let (plan, requests) = resolve_with("rk-4k", FOUR_K_DEFAULT, MDE_DIRECTPLAY, |env| {
        env.caps = std::sync::Arc::new(full_hd_set());
        env.quality = Quality::Original;
        let capabilities = Capabilities {
            progressive_uri: true, hls_uri: false, packet_feed: false, pause: true, seek: true,
            audio_track: true, subtitle_track: true, audio_off: false, subtitle_off: false,
        };
        env.uri_preflight_port = Some(ResolvePort::new(ProductEvidence {
            display_exported: true, binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false,
        }, std::sync::Arc::new(ProductQualifier)));
    });
    assert!(plan.uri_original.is_none());
    assert_eq!((plan.part_id, plan.media_index), (3134, 1));
    assert!(plan.url.contains("/library/parts/3134/"), "{}", plan.url);
    assert_eq!(requests.iter().filter(|line| line.starts_with("GET /library/metadata/rk-4k")).count(), 1);
    assert_eq!(requests.iter().filter(|line| line.starts_with("POST /playQueues")).count(), 1);
}

#[test]
#[cfg(feature = "devtriggers")]
fn an_accepted_pms_decision_produces_an_owned_uri_offer_from_its_output() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(QualifiedPms(observed.clone()))));
    let (port, done, server) = versions_pms_with(URI_DTS, MDE_PMS_OUTPUT);
    let sid = plx_plex::plex::register_for_test("qualified-pms", "127.0.0.1", port,
        "token", "qualified-pms-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    let start = pump_play(&mut ps, &mut meta).expect("accepted PMS output start");
    assert!(start.uri_offer.is_some(), "the actual resolve must carry its owned URI offer");
    let seen = observed.lock().unwrap();
    assert_eq!(seen.len(), 1, "the accepted PMS output is qualified once");
    assert_eq!((seen[0].0.as_str(), seen[0].1.as_str()), ("h264", "ac3"),
        "the certificate sees output codecs, not the DTS source codec");
    assert_eq!(seen[0].2, ps.tsession, "the offer owns the accepted encoder session");
    drop(seen);
    scrobble_stop(&mut ps, None, None);
    drain_scrobble();
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(requests.iter().any(|line| line.contains("/decision?")));
    assert!(requests.iter().any(|line| line.contains("/video/:/transcode/universal/stop?")),
        "the accepted PMS encoder is retired at the ordinary timeline-before-stop boundary");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn an_explicit_external_source_pick_can_use_a_server_accepted_burned_wire() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(QualifiedPms(std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))))));
    let (port, done, server) = versions_pms_with(URI_DTS_EXTERNAL_SUBTITLE, MDE_PMS_BURNED_EXTERNAL);
    let sid = plx_plex::plex::register_for_test("burned-external", "127.0.0.1", port,
        "token", "burned-external-client");
    let mut env = ResolveEnv::snapshot(&ps,
        plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    env.subtitle_override = Some(31343);
    env.sub_sid = 31343;
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    assert_eq!(plan.source_selection.expect("external source intent").subtitle_sid, 31343);
    assert!(plan.uri_pms.is_some(),
        "the exact accepted burn is video output, so the external SID needs no native subtitle selection");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn viewer_off_re_resolves_a_burned_pms_uri_without_reusing_its_subtitle() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::{BackendKind, Capabilities};
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(QualifiedPms(std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))))));
    let (port, done, server) = versions_pms_with(URI_AV1_EXTERNAL_SUBTITLE, MDE_PMS_BURNED_EXTERNAL);
    let sid = plx_plex::plex::register_for_test("burned-off", "127.0.0.1", port,
        "token", "burned-off-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    let feed_cannot_decode_av1 = std::sync::Arc::new(plx_platform::devcaps::Caps {
        av1: false, ..plx_platform::devcaps::Caps::assumed()
    });
    assert!(request_play(&mut ps, &mut meta, feed_cannot_decode_av1, sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "av1", "dca", "Film", "", None));
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    let first = pump_play(&mut ps, &mut meta).expect("accepted burned PMS output");
    let burned = first.uri_offer.expect("accepted server burn is a qualified URI output");
    assert_eq!(ps.source_selection().expect("source intent").subtitle_sid, 31343);
    let ticket = first.route_start.expect("the accepted plan owns one start transaction");
    assert!(prepare_route_start(ticket));
    let attempt = claim_route_start_attempt(ticket).expect("one physical URI attempt");
    // The player adapter's `offer_qualified_uri` records this evidence on the real path; this
    // route-level fixture has no adapter, so it states the same fact on the session directly.
    ps.qualify_backend_offer(BackendKind::Uri, burned.offer_id(),
        crate::route::backend_policy::Qualification::Qualified);
    assert!(ps.backend_offer_admitted(BackendKind::Uri, burned.offer_id()),
        "this source has no Feed original, so the accepted URI PMS output is the catalog choice");
    ps.begin_attempt_view(attempt, crate::player::attempt_view::Backend::Uri);
    assert!(settle_route_start(&mut ps, attempt, RouteStartResult::Started));
    let prior_request = ps.metric_request();
    assert!(request_viewer_reconfigure(&mut ps, &mut meta,
        ViewerReconfigure::SourceSubtitle { sid: 0, mode: SubMode::Off, forced_lang: None },
        Some(12_000_000_000), Some(true)),
        "Off is an explicit source change even when the current URI output burned subtitles");
    assert_ne!(ps.metric_request(), prior_request);
    assert_eq!(ps.reconfigure_pause_goal(), Some(true));
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    let next = pump_play(&mut ps, &mut meta).expect("Off resolves the same source anew");
    assert_eq!(next.resume_ns, 12_000_000_000);
    assert_eq!(ps.source_selection().expect("Off source intent").subtitle_sid, 0);
    assert!(next.uri_offer.is_none(),
        "the old accepted burn cannot certify a new output whose viewer selected Off");
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(requests.iter().filter(|line| line.contains("/decision?")).count() >= 2,
        "the new subtitle choice must reach PMS, not merely change the menu: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn native_hls_master_offer_requires_the_fetched_multi_variant_response() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    for (master_body, qualifies) in [
        (TWO_VARIANT_MASTER as &[u8], true), (FIXED_MEDIA_PLAYLIST as &[u8], false)
    ] {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        assert!(plx_net::net::global_init());
        restore_quality(Quality::Auto);
        let capabilities = Capabilities { progressive_uri: true, hls_uri: true, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
            crate::route::uri_preflight::ProductEvidence { display_exported: true,
                binding: Binding::Exported { canvas: (1920, 1080) },
                callable: capabilities, certified: capabilities, preview_rectangle: false },
            std::sync::Arc::new(QualifiedNativeMaster)));
        let (port, done, server) = versions_pms_observed_with_master(URI_DTS,
            MDE_PMS_HLS_OUTPUT, None, Some(master_body));
        let sid = plx_plex::plex::register_for_test("native-master", "127.0.0.1", port,
            "token", "native-master-client");
        let mut meta = plx_data::stores::metadata::MetadataStore::default();
        assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
            "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));
        assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
        let start = pump_play(&mut ps, &mut meta);
        assert_eq!(start.as_ref().is_some_and(|landed| landed.uri_offer.is_some()), qualifies,
            "only the actual multi-variant response certifies NativeMaster");
        if !qualifies && !ps.backends().feed {
            assert_eq!(ps.play_verdict, Some(PlayVerdict::NoQualifiedBackend));
        }
        scrobble_stop(&mut ps, None, None);
        drain_scrobble();
        done.send(()).unwrap();
        let requests = server.join().unwrap();
        assert!(requests.iter().any(|line|
            line.starts_with("GET /video/:/transcode/universal/start.m3u8?")),
            "qualifier fetched the exact resolved master locator");
        plx_plex::plex::reset_servers_for_test();
    }
}

#[test]
#[cfg(feature = "devtriggers")]
fn unknown_uri_qualification_keeps_the_existing_feed_decision_path() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    ps.install_backends(plx_playback_contract::BackendSet::BOTH);
    assert!(plx_net::net::global_init());
    let callable = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) }, callable,
            certified: callable, preview_rectangle: false },
        std::sync::Arc::new(crate::route::uri_preflight::ProductQualifier)));
    let (port, done, server) = versions_pms_with(URI_DTS, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("unknown-dts", "127.0.0.1", port, "token", "unknown-dts-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));
    let finished = wait_resolve_workers_for_test(std::time::Duration::from_secs(15));
    let start = pump_play(&mut ps, &mut meta);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(finished, "the off-frame resolve did not finish");
    assert!(start.map_or(true, |start| start.uri_offer.is_none()),
        "an Unknown qualifier must not publish a URI offer");
    assert!(requests.iter().any(|line| line.contains("/decision?")),
        "the existing Feed decision must still run: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[cfg(feature = "devtriggers")]
fn expired_original_skips_its_spent_recipe_but_qualifies_unspent_pms_uri(backends: plx_playback_contract::BackendSet) {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::{BackendKind, Capabilities};
    use super::super::backend_policy::{BackendPolicy, OfferId, Qualification};
    use super::super::backend_policy::Selection;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    ps.install_backends(backends);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    let qualification_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let pms_calls = std::sync::Arc::new(AtomicUsize::new(0));
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) }, callable: capabilities,
            certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(CountingQualified {
            original: qualification_calls.clone(), pms: pms_calls.clone(),
        })));
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    let (port, done, server) = versions_pms_observed(URI_DTS, MDE_PMS_OUTPUT, Some(seen_tx));
    let sid = plx_plex::plex::register_for_test("expiring-dts", "127.0.0.1", port,
        "token", "expiring-dts-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));

    // This is an existing episode, not a fresh catalog. The both-backend variant has
    // already crossed from Feed to URI; both variants have spent URI Original and learned
    // a refusal for another URI recipe.
    let mut policy = BackendPolicy::for_request(false);
    if backends.feed {
    policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
    assert!(policy.admitted(BackendKind::Feed, OfferId::FEED_ORIGINAL));
    }
    policy.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Qualified);
    assert!(policy.admitted(BackendKind::Uri, OfferId::URI_ORIGINAL));
    policy.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Refused);
    policy.qualify(BackendKind::Uri, OfferId::URI_PMS_ENCODE, Qualification::Refused);
    ps.backend_policy = Some(policy);
    let source_selection = crate::route::SourceSelection { server: sid, part_id: 3134,
        audio_sid: 31342, subtitle_sid: 0,
        audio_contract: plx_plex::plex::AudioEnhancements::NONE };
    ps.bind_source_selection(source_selection);
    let recovery_caps = ps.recovery.as_ref().expect("initial recovery").attempt();
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    let first_session = {
        let mut slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        let landing = slot.as_mut().expect("qualified worker landing");
        assert_eq!(landing.plan.source_selection, Some(source_selection));
        landing.plan.uri_original.as_mut().expect("owned URI result").expires_at =
            std::time::Instant::now() - std::time::Duration::from_secs(1);
        landing.plan.sess.clone()
    };
    assert!(!first_session.is_empty(), "the qualified Part owns a resource identity");
    assert!(pump_play(&mut ps, &mut meta).is_none(), "expired URI must not start natively");
    assert!(play_pending(), "the same episode begins a bounded re-resolve");
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    {
        let slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        assert!(slot.as_ref().is_some_and(|landing| landing.plan.uri_original.is_none()),
            "the captured eligibility skips the spent Original recipe");
        assert!(slot.as_ref().is_some_and(|landing| landing.plan.uri_pms.is_some()),
            "an unspent compatible PMS URI offer still receives source qualification");
    }
    let next = pump_play(&mut ps, &mut meta);
    assert!(next.is_some_and(|start| start.uri_offer.is_some()),
        "the qualified PMS source reaches the URI executor even without Feed");
    assert_ne!(ps.sess, first_session, "the successor owns a fresh resource identity");
    assert_eq!(qualification_calls.load(AtomicOrdering::SeqCst), 1,
        "the same source episode may not loop through an expired URI offer");
    assert_eq!(pms_calls.load(AtomicOrdering::SeqCst), 1,
        "the refreshed accepted PMS source is certified once");
    assert_eq!(ps.source_selection(), Some(source_selection));
    assert!(std::sync::Arc::ptr_eq(&recovery_caps,
        &ps.recovery.as_ref().expect("same recovery episode").attempt()));
    let policy = ps.backend_policy.as_mut().expect("same finite catalog");
    assert!(policy.can_reopen(BackendKind::Uri, OfferId::URI_ORIGINAL),
        "the admitted URI recipe remains in history");
    assert_eq!(policy.offer(BackendKind::Uri, OfferId::URI_PMS_ENCODE)
        .expect("catalog recipe").qualification, Qualification::Refused);
    if backends.feed {
        policy.qualify(BackendKind::Feed, OfferId::FEED_PMS_REMUX, Qualification::Qualified);
        assert!(!policy.admitted(BackendKind::Feed, OfferId::FEED_PMS_REMUX),
            "crossing back to Feed cannot replenish a spent handoff");
        assert!(!matches!(policy.choose(), Selection::Open(offer) if offer.backend == BackendKind::Feed));
    }

    scrobble_stop(&mut ps, None, None);
    drain_scrobble();
    let old_stop = format!("session={first_session}");
    loop {
        let line = seen_rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("old URI resource stop");
        if line.contains("/video/:/transcode/universal/stop?") && line.contains(&old_stop) { break; }
    }
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(requests.iter().filter(|line|
        line.contains("/video/:/transcode/universal/stop?") && line.contains(&old_stop)).count(), 1,
        "the expired resource closes exactly once: {requests:?}");
    assert!(requests.iter().any(|line| line.contains("/decision?")),
        "the bounded retry used the existing PMS resolve: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn expired_original_skips_its_spent_recipe_but_qualifies_unspent_pms_uri_with_both_backends() {
    expired_original_skips_its_spent_recipe_but_qualifies_unspent_pms_uri(
        plx_playback_contract::BackendSet::BOTH);
}

#[test]
#[cfg(feature = "devtriggers")]
fn expired_original_skips_its_spent_recipe_but_qualifies_unspent_pms_uri_in_uri_only() {
    expired_original_skips_its_spent_recipe_but_qualifies_unspent_pms_uri(
        plx_playback_contract::BackendSet::URI);
}

#[test]
#[cfg(feature = "devtriggers")]
fn unknown_uri_without_feed_has_a_named_refusal_before_pms_encoder_work() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    ps.install_backends(plx_playback_contract::BackendSet::URI);
    assert!(plx_net::net::global_init());
    let evidence = crate::route::uri_preflight::ProductEvidence::unqualified(
        plx_platform::tv::sink::VP_EXPORTED);
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        evidence, std::sync::Arc::new(crate::route::uri_preflight::ProductQualifier)));
    let (port, done, server) = versions_pms_with(URI_DTS, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("unqualified-dts", "127.0.0.1", port, "token", "unqualified-client");
    let env = ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(plan.verdict, Some(PlayVerdict::NoQualifiedBackend));
    assert_eq!(plan.pms_decline,
        Some(crate::route::uri_preflight::PmsDecline::UnavailableCapability));
    assert!(plan.url.is_empty() && plan.sess.is_empty(), "no Feed resource was acquired");
    assert!(!requests.iter().any(|line| line.contains("/decision?") || line.contains("/transcode/")
        || line.starts_with("PUT /library/parts/")), "Unknown URI must stop before Feed work: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(feature = "devtriggers")]
fn unknown_pms_output_in_uri_only_retires_the_accepted_encoder_once() {
    use plx_playback_contract::uri::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    ps.install_backends(plx_playback_contract::BackendSet::URI);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(crate::route::uri_preflight::ProductQualifier)));
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    let (port, done, server) = versions_pms_observed(URI_DTS, MDE_PMS_OUTPUT, Some(seen_tx));
    let sid = plx_plex::plex::register_for_test("unknown-pms", "127.0.0.1", port,
        "token", "unknown-pms-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    assert!(pump_play(&mut ps, &mut meta).is_none());
    assert_eq!(ps.play_verdict, Some(PlayVerdict::NoQualifiedBackend));
    assert_eq!(ps.pms_decline(),
        Some(crate::route::uri_preflight::PmsDecline::UnknownCertificate));
    let mut stop = None;
    for _ in 0..32 {
        let line = seen_rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("PMS cleanup request after unknown qualification");
        if line.contains("/video/:/transcode/universal/stop?") {
            stop = Some(line);
            break;
        }
    }
    assert!(stop.is_some(), "the accepted encoder must be retired");
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(requests.iter().any(|line| line.contains("/decision?")),
        "the certified-capability preflight reaches PMS before returning Unknown");
    assert_eq!(requests.iter().filter(|line|
        line.contains("/video/:/transcode/universal/stop?")).count(), 1,
        "the losing PMS resource has one stop owner");
    plx_plex::plex::reset_servers_for_test();
}

/// A retry or quality change resolves the same item again from the stored request, so the pick
/// rides it; the session installs the landed version for every later encoder of the playback.
#[test]
fn the_picked_version_rides_the_request_and_the_landing() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), ServerId::from_raw(0), "rk-v", "/library/parts/3134/1/file.mkv",
        "h264", "aac", "Episode", "", Some(plx_plex::plex::MediaId(3082))));
    assert_eq!(ps.request.as_ref().and_then(|r| r.version), Some(plx_plex::plex::MediaId(3082)));
    let env = request_env(&ps, meta.view(), ps.request.as_ref().unwrap(), None);
    assert_eq!(env.version, Some(plx_plex::plex::MediaId(3082)));
    assert!(env.cached_item.is_none() && env.src_kbps == 0, "the default's facts never stand in for the pick");
    cancel_play(&mut ps);
    test_support::apply_plan(&mut ps, Plan { media_index: 1, ..Default::default() }, "rk-v");
    assert_eq!(cur_media_index(&ps), 1);
}
