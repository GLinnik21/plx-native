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
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, done, server) = versions_pms(mde);
    let sid = plx_plex::plex::register_for_test("versions", "127.0.0.1", port, "token", "versions-client");
    let mut env = ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    env.version = version;
    let plan = build_stream(
        &plx_base::task::OffFrame::for_test(),
        "rk-v",
        "/library/parts/3134/1/file.mkv",
        "h264",
        "aac",
        &env,
    );
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
    (plan, requests)
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

#[cfg(feature = "play-uri")]
const URI_DTS: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

#[cfg(feature = "play-uri")]
const URI_DTS_SELECTED_SUBTITLE: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"episode","grandparentRatingKey":"show-off","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true},
     {"id":31343,"streamType":3,"codec":"srt","index":2,"languageCode":"eng","selected":true}]}]}]}]}}"#;

#[cfg(feature = "play-uri")]
const URI_DTS_EXTERNAL_SUBTITLE: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"dca","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"dca","channels":6,"index":1,"languageCode":"eng","default":true,"selected":true},
     {"id":31343,"streamType":3,"codec":"srt","index":2,"languageCode":"eng","selected":true,"key":"/library/streams/31343"}]}]}]}]}}"#;

#[cfg(feature = "play-uri")]
const URI_AV1: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":450000,"Media":[
  {"id":3086,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"av1","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"av1","index":0},
     {"id":31342,"streamType":2,"codec":"aac","channels":2,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

#[cfg(feature = "play-uri")]
const MDE_PMS_OUTPUT: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"copy","codec":"h264"},{"streamType":2,"decision":"transcode","codec":"ac3"}]}]}]}]}}"#;

#[cfg(feature = "play-uri")]
const MDE_PMS_HLS_OUTPUT: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"transcode","codec":"h264"},{"streamType":2,"decision":"transcode","codec":"aac"}]}]}]}]}}"#;

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
const TWO_VARIANT_MASTER: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000000\nhigh/index.m3u8\n";

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
const FIXED_MEDIA_PLAYLIST: &[u8] = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n";

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
struct QualifiedNativeMaster;

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
impl crate::route::uri_preflight::OriginalQualifier for QualifiedNativeMaster {
    fn qualify(&self, _: &plx_base::task::OffFrame,
        _: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        crate::route::uri_preflight::Verdict::Unknown
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
            native_tracks: vec![plx_play_uri::backend::NativeTrackIndex {
                selection: TrackSelection { kind: TrackKind::Audio, id: 31342 }, index: 1,
            }],
            applied_audio: request.applied_audio, time_domain: PmsTimeDomain::ContentAbsolute,
            hls_controller: Some(HlsController::NativeMaster), hls_master: Some(master),
            release_certified: true,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10) })
    }
}

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
struct QualifiedPms(std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>);

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
impl crate::route::uri_preflight::OriginalQualifier for QualifiedPms {
    fn qualify(&self, _: &plx_base::task::OffFrame,
        _: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        crate::route::uri_preflight::Verdict::Unknown
    }

    fn qualify_pms(&self, _: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::PmsRequest) -> crate::route::uri_preflight::PmsVerdict {
        use crate::route::uri_preflight::{PmsCertificate, PmsRecipe, PmsTimeDomain, PmsVerdict};
        use plx_playback_contract::{Fidelity, Requirements, SourceKind, TrackChoice, TrackKind,
            TrackSelection};
        assert!(matches!(request.recipe, PmsRecipe::Remux | PmsRecipe::Encode));
        self.0.lock().unwrap().push((request.video_codec.clone(), request.audio_codec.clone(),
            request.resource_session.clone()));
        PmsVerdict::Known(PmsCertificate { selection: request.selection,
            source_url: request.source_url, resource_session: request.resource_session,
            recipe: request.recipe,
            requirements: Requirements { source: SourceKind::ProgressiveUri,
                video: Fidelity::Compatible, audio: Fidelity::Compatible,
                audio_track: TrackChoice::Selected(31342),
                subtitle_track: TrackChoice::Unspecified },
            native_tracks: vec![plx_play_uri::backend::NativeTrackIndex {
                selection: TrackSelection { kind: TrackKind::Audio, id: 31342 }, index: 1,
            }],
            applied_audio: request.applied_audio, time_domain: PmsTimeDomain::ContentAbsolute,
            hls_controller: None, hls_master: None, release_certified: true,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(10) })
    }
}

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
struct QualifiedOriginal { video: &'static str, audio: &'static str }

#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
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
        let mut tracks = vec![plx_play_uri::backend::NativeTrackIndex {
            selection: plx_playback_contract::TrackSelection {
                kind: plx_playback_contract::TrackKind::Audio, id: 31342,
            }, index: 1,
        }];
        let subtitle_choice = if selected.subtitle_sid > 0 {
            tracks.push(plx_play_uri::backend::NativeTrackIndex {
                selection: plx_playback_contract::TrackSelection {
                    kind: plx_playback_contract::TrackKind::Subtitle,
                    id: selected.subtitle_sid as u64,
                }, index: 2,
            });
            TrackChoice::Selected(selected.subtitle_sid as u64)
        } else { TrackChoice::Unspecified };
        crate::route::uri_preflight::Verdict::Known(crate::route::uri_preflight::KnownSource {
            selection: selected,
            source: plx_play_uri::backend::UriSource::new(request.source_url, tracks),
            requirements: Requirements { source: SourceKind::ProgressiveUri,
                video: Fidelity::Exact, audio: Fidelity::Exact,
                audio_track: TrackChoice::Selected(31342), subtitle_track: subtitle_choice },
            certified_dolby_vision: None,
            certified_subtitle_source: subtitle,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(15),
        })
    }
}

#[test]
#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
fn remembered_subtitle_off_remains_off_during_uri_source_qualification() {
    use plx_play_uri::backend::Binding;
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
#[cfg(feature = "play-uri")]
fn selected_external_subtitle_remains_common_source_intent_across_resolve() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let (port, done, server) = versions_pms_with(URI_DTS_EXTERNAL_SUBTITLE, MDE_TRANSCODE_VIDEO);
    let sid = plx_plex::plex::register_for_test("external-source", "127.0.0.1", port,
        "token", "external-source-client");
    let env = ResolveEnv::snapshot(&ps,
        plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "dca", &env);
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    assert_eq!(plan.source_selection.expect("source intent").subtitle_sid, 31343,
        "a client-drawn sidecar is still the viewer's selected source track");
    plx_plex::plex::reset_servers_for_test();
}

#[cfg(all(feature = "play-uri", feature = "play-feed", feature = "devtriggers"))]
struct CountingQualified(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(all(feature = "play-uri", feature = "play-feed", feature = "devtriggers"))]
impl crate::route::uri_preflight::OriginalQualifier for CountingQualified {
    fn qualify(&self, off: &plx_base::task::OffFrame,
        request: crate::route::uri_preflight::OriginalRequest) -> crate::route::uri_preflight::Verdict {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::route::uri_preflight::OriginalQualifier::qualify(
            &QualifiedOriginal { video: "h264", audio: "dca" }, off, request)
    }
}

#[test]
#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
fn a_qualified_dts_original_lands_from_the_real_resolve_worker_before_feed_decision() {
    use plx_play_uri::backend::Binding;
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
#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
fn a_certified_av1_uri_original_does_not_inherit_feeds_video_refusal() {
    use plx_play_uri::backend::Binding;
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
#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
fn an_accepted_pms_decision_produces_an_owned_uri_offer_from_its_output() {
    use plx_play_uri::backend::Binding;
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
#[cfg(all(feature = "play-uri", feature = "devtriggers"))]
fn native_hls_master_offer_requires_the_fetched_multi_variant_response() {
    use plx_play_uri::backend::Binding;
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
        if !qualifies && !cfg!(feature = "play-feed") {
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
#[cfg(all(feature = "play-uri", feature = "play-feed", feature = "devtriggers"))]
fn unknown_uri_qualification_keeps_the_existing_feed_decision_path() {
    use plx_play_uri::backend::Binding;
    use plx_playback_contract::Capabilities;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
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

#[test]
#[cfg(all(feature = "play-uri", feature = "play-feed", feature = "devtriggers"))]
fn expired_qualified_landing_retries_feed_once_without_refilling_a_spent_episode() {
    use plx_play_uri::backend::Binding;
    use plx_playback_contract::{BackendKind, Capabilities};
    use super::super::backend_policy::{BackendPolicy, OfferId, Qualification, Selection};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
        pause: true, seek: true, audio_track: true, subtitle_track: true,
        audio_off: false, subtitle_off: false };
    let qualification_calls = std::sync::Arc::new(AtomicUsize::new(0));
    ps.install_uri_preflight_port(crate::route::uri_preflight::ResolvePort::new(
        crate::route::uri_preflight::ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) }, callable: capabilities,
            certified: capabilities, preview_rectangle: false },
        std::sync::Arc::new(CountingQualified(qualification_calls.clone()))));
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    let (port, done, server) = versions_pms_observed(URI_DTS, MDE_TRANSCODE_VIDEO, Some(seen_tx));
    let sid = plx_plex::plex::register_for_test("expiring-dts", "127.0.0.1", port,
        "token", "expiring-dts-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(request_play(&mut ps, &mut meta, plx_platform::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", "h264", "dca", "Film", "", None));

    // This is an existing episode, not a fresh catalog: Feed Original was admitted, then
    // URI Original crossed from Feed, and another URI recipe learned a refusal. Re-resolving
    // after expiry must preserve all three facts even if Feed chooses a different audio wire.
    let mut policy = BackendPolicy::for_request(false);
    policy.qualify_planned_feed(OfferId::FEED_ORIGINAL);
    assert!(policy.admitted(BackendKind::Feed, OfferId::FEED_ORIGINAL));
    policy.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Qualified);
    assert!(policy.admitted(BackendKind::Uri, OfferId::URI_ORIGINAL));
    policy.qualify(BackendKind::Uri, OfferId::URI_ORIGINAL, Qualification::Refused);
    policy.qualify(BackendKind::Uri, OfferId::URI_PMS_REMUX, Qualification::Refused);
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
    assert!(play_pending(), "the same episode begins a bounded Feed-only re-resolve");
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    {
        let slot = PLAY_SLOT.lock().unwrap_or_else(|e| e.into_inner());
        assert!(slot.as_ref().is_some_and(|landing| landing.plan.uri_original.is_none()),
            "the Feed retry's captured worker input omits URI qualification");
    }
    let _ = pump_play(&mut ps, &mut meta);
    assert_ne!(ps.sess, first_session, "the Feed retry owns a fresh resource identity");
    assert_eq!(qualification_calls.load(AtomicOrdering::SeqCst), 1,
        "the same source episode may not loop through an expired URI offer");
    assert_eq!(ps.source_selection(), Some(source_selection));
    assert!(std::sync::Arc::ptr_eq(&recovery_caps,
        &ps.recovery.as_ref().expect("same recovery episode").attempt()));
    let policy = ps.backend_policy.as_mut().expect("same finite catalog");
    assert!(policy.can_reopen(BackendKind::Uri, OfferId::URI_ORIGINAL),
        "the admitted URI crossing remains in history");
    assert_eq!(policy.offer(BackendKind::Uri, OfferId::URI_PMS_REMUX)
        .expect("catalog recipe").qualification, Qualification::Refused);
    policy.qualify(BackendKind::Feed, OfferId::FEED_PMS_REMUX, Qualification::Qualified);
    assert!(!policy.admitted(BackendKind::Feed, OfferId::FEED_PMS_REMUX),
        "crossing back to Feed cannot replenish a spent handoff");
    assert!(!matches!(policy.choose(), Selection::Open(offer) if offer.backend == BackendKind::Feed));

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
        "the bounded retry used the existing Feed resolve: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
#[cfg(all(feature = "play-uri", not(feature = "play-feed"), feature = "devtriggers"))]
fn unknown_uri_without_feed_has_a_named_refusal_before_pms_encoder_work() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
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
#[cfg(all(feature = "play-uri", not(feature = "play-feed"), feature = "devtriggers"))]
fn unknown_pms_output_in_uri_only_retires_the_accepted_encoder_once() {
    use plx_play_uri::backend::Binding;
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
