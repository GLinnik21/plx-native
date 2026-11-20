//! **An ordinary title through the REAL planner, for each composition a platform can supply**
//! (platform plan W6c): a URI driver and no Feed, a source that driver declines, no driver at all,
//! and both drivers. Every case but (c)'s planner half drives `request_play` -> the resolve worker ->
//! `pump_play` over a loopback PMS ((c) reads the plan's own decline fields from `build_stream`
//! directly, and lands its refusal through `pump_play` in its second test), with the session told what the adapter's installed drivers form
//! (`PlayerAdapter::composed_backends`) and the resolver a composition publishes, exactly as the
//! app's `publish_composition` tells it. The URI driver is the contract's `FakeUriDriver`, which
//! carries no display binding of any platform, and it certifies itself: the resolver's evidence is
//! the driver's own capabilities and display, the viewer's experimental setting stays Off.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::route::uri_preflight::{OriginalQualifier, OriginalRequest, PmsRequest, PmsVerdict,
    ProductEvidence, ResolvePort, Verdict, SOURCE_VERDICT_TTL};
use plx_playback_contract::test_support::{FakeUriDriver, FakeUriHandle};
use plx_playback_contract::uri::UriDriver;
use plx_playback_contract::{BackendKind, BackendSet, Capabilities};
use std::sync::{Arc, Mutex};

/// An h264/aac Matroska file with a duration: what Standard direct-plays, and so what any
/// qualifier may carry as an original.
const ORDINARY_TITLE: &[u8] = br#"{"MediaContainer":{"Metadata":[{"ratingKey":"rk-v","type":"movie","duration":600000,"Media":[
  {"id":3086,"duration":450000,"bitrate":2372,"width":1920,"height":1080,"videoCodec":"h264","audioCodec":"aac","container":"mkv",
   "Part":[{"id":3134,"duration":450000,"key":"/library/parts/3134/1/file.mkv","container":"mkv","Stream":[
     {"id":31341,"streamType":1,"codec":"h264","index":0},
     {"id":31342,"streamType":2,"codec":"aac","channels":2,"index":1,"languageCode":"eng","default":true,"selected":true}]}]}]}]}}"#;

/// What a Tizen-like driver's composition would supply: no viewer opt-in is asked for, the driver
/// is the platform's only path and says what it plays. An original Standard would direct-play is a
/// URL carrier; a PMS conversion is certified only when `pms` is armed. It records the opt-in each
/// request carried.
struct SelfCertified {
    pms: Option<Arc<Mutex<Vec<(String, String, String)>>>>,
    opt_in_seen: Arc<Mutex<Vec<bool>>>,
}

impl OriginalQualifier for SelfCertified {
    fn qualify(&self, _: &plx_base::task::OffFrame, request: OriginalRequest) -> Verdict {
        self.opt_in_seen.lock().unwrap().push(request.uri_opt_in);
        if let Some(gap) = request.standard.first_gap() { return Verdict::Unknown(gap); }
        let url = request.source_url.clone();
        Verdict::Known(crate::route::uri_preflight::KnownSource::url_carrier(
            &request, url, SOURCE_VERDICT_TTL))
    }

    fn qualify_pms(&self, off: &plx_base::task::OffFrame, request: PmsRequest) -> PmsVerdict {
        match &self.pms {
            Some(seen) => OriginalQualifier::qualify_pms(&QualifiedPms(seen.clone()), off, request),
            None => PmsVerdict::Unknown,
        }
    }
}

const URI_CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false,
    packet_feed: false, pause: true, seek: true, audio_track: true, subtitle_track: true,
    audio_off: false, subtitle_off: false };

/// A URI-only composition as a platform's `compose` builds it: the driver seated on a fresh
/// adapter, the resolver whose evidence is that driver's own display and capabilities (the driver
/// certifies itself), and the session told the set the adapter forms.
struct Composed {
    adapter: crate::player::adapter::PlayerAdapter,
    handle: FakeUriHandle,
    opt_in_seen: Arc<Mutex<Vec<bool>>>,
}

fn compose_uri_only(ps: &mut PlaybackSession, pms: Option<Arc<Mutex<Vec<(String, String, String)>>>>)
    -> Composed {
    let mut adapter = crate::player::adapter::PlayerAdapter::new(unsafe {
        plx_base::task::MainThread::assume() });
    let (driver, handle) = FakeUriDriver::new(URI_CAPS);
    let evidence = ProductEvidence { display: driver.display(), callable: URI_CAPS,
        certified: URI_CAPS };
    assert!(adapter.install_uri_driver(Box::new(driver)).is_ok());
    let opt_in_seen = Arc::new(Mutex::new(Vec::new()));
    ps.install_uri_preflight_port(ResolvePort::new(evidence,
        Arc::new(SelfCertified { pms, opt_in_seen: opt_in_seen.clone() })));
    ps.install_backends(adapter.composed_backends());
    Composed { adapter, handle, opt_in_seen }
}

fn play_ordinary(ps: &mut PlaybackSession, meta: &mut plx_data::stores::metadata::MetadataStore,
    sid: plx_plex::plex::ServerId, vcodec: &str, acodec: &str) -> Option<ResolvedStart> {
    assert!(request_play(ps, meta, plx_os::devcaps::caps(), sid,
        "rk-v", "/library/parts/3134/1/file.mkv", vcodec, acodec, "Film", "", None, ""));
    assert!(wait_resolve_workers_for_test(std::time::Duration::from_secs(15)));
    pump_play(ps, meta)
}

/// (a) URI only, the experimental setting Off, the driver certifying itself: an ordinary title
/// resolves to a URI original, the adapter takes the offer and the Start reaches the driver.
#[test]
fn a_uri_only_composition_plays_an_ordinary_title_with_the_experimental_setting_off() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    assert_eq!(uri_engine(), plx_plex::plex::session::UriEngine::Off, "the viewer opted into nothing");
    let mut composed = compose_uri_only(&mut ps, None);
    assert_eq!(ps.backends(), BackendSet::URI, "no Feed driver is composed");
    let (port, done, server) = versions_pms_with(ORDINARY_TITLE, MDE_DIRECTPLAY);
    let sid = plx_plex::plex::register_for_test("uri-only", "127.0.0.1", port, "token", "uri-only-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    let start = play_ordinary(&mut ps, &mut meta, sid, "h264", "aac").expect("the title resolves");
    let offer = start.uri_offer.expect("an ordinary title resolves to a URI offer");
    assert_eq!(offer.offer_id(), crate::route::backend_policy::OfferId::URI_ORIGINAL);
    assert_eq!(*composed.opt_in_seen.lock().unwrap(), [false],
        "the qualifier was asked once, and the viewer's opt-in was not part of the answer");
    assert!(composed.adapter.offer_qualified_uri(&mut ps, offer).is_ok());
    assert!(matches!(ps.backend_offer(), crate::route::backend_policy::Selection::Open(choice)
        if choice.backend == BackendKind::Uri));
    let started = crate::player::start_playback_tracked(&mut ps, &mut composed.adapter);
    assert!(matches!(started, crate::player::BufferfeedStartOutcome::Launched(_)), "{started:?}");
    assert!(composed.handle.dispatched().iter().any(|(_, _, op)|
        matches!(op, plx_playback_contract::Operation::Start { .. })),
        "the Start reached the URI driver: {:?}", composed.handle.dispatched());
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert!(!requests.iter().any(|line| line.contains("/decision?") || line.contains("/transcode/")),
        "no Feed decision or encoder: {requests:?}");
    scrobble_stop(&mut ps, None, None);
    drain_scrobble();
    plx_plex::plex::reset_servers_for_test();
}

/// (b, the refusal) A source the URI driver's qualifier declines (DTS audio the set cannot play
/// natively, so Standard would not direct-play it), in a URI-only composition whose qualifier
/// certifies no PMS conversion: the planner today returns the typed `NoQualifiedBackend` refusal
/// and names the PMS decline. It does not fall to a Feed that is not there.
#[test]
fn a_source_the_only_driver_declines_is_a_typed_refusal_when_no_conversion_is_certified() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let composed = compose_uri_only(&mut ps, None);
    let (port, done, server) = versions_pms_with(URI_DTS, MDE_PMS_OUTPUT);
    let sid = plx_plex::plex::register_for_test("declined", "127.0.0.1", port, "token", "declined-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(play_ordinary(&mut ps, &mut meta, sid, "h264", "dca").is_none());
    assert_eq!(ps.play_verdict, Some(PlayVerdict::NoQualifiedBackend));
    assert_eq!(ps.pms_decline(),
        Some(crate::route::uri_preflight::PmsDecline::UnknownCertificate));
    assert_eq!(composed.opt_in_seen.lock().unwrap().len(), 1, "the driver was asked about the original");
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    scrobble_stop(&mut ps, None, None);
    drain_scrobble();
    plx_plex::plex::reset_servers_for_test();
}

/// (b, the conversion) The same declined original, where the driver's qualifier DOES certify the
/// server's converted output: the planner offers the `URI_PMS_*` recipe, not the original.
#[test]
fn a_source_the_only_driver_declines_becomes_a_uri_pms_conversion_when_one_is_certified() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let _composed = compose_uri_only(&mut ps, Some(seen.clone()));
    let (port, done, server) = versions_pms_with(URI_DTS, MDE_PMS_OUTPUT);
    let sid = plx_plex::plex::register_for_test("converted", "127.0.0.1", port, "token", "converted-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    let start = play_ordinary(&mut ps, &mut meta, sid, "h264", "dca").expect("the conversion starts");
    let offer = start.uri_offer.expect("the converted output is offered to the URI driver");
    use crate::route::backend_policy::OfferId;
    assert!(matches!(offer.offer_id(), OfferId::URI_PMS_REMUX | OfferId::URI_PMS_ENCODE),
        "{:?}", offer.offer_id());
    assert_eq!(seen.lock().unwrap().len(), 1, "the converted output was qualified once");
    scrobble_stop(&mut ps, None, None);
    drain_scrobble();
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
}

/// (c) Zero drivers: the set the adapter forms is `NONE`, no resolver is published, and the
/// planner refuses an ordinary title with the typed verdict before any PMS decision or encoder
/// request (the PlayQueue request precedes the refusal, `plan.rs`'s `NoQualifiedBackend`).
#[test]
fn a_composition_with_no_driver_refuses_an_ordinary_title_with_a_typed_verdict() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let adapter = crate::player::adapter::PlayerAdapter::new(unsafe {
        plx_base::task::MainThread::assume() });
    ps.install_uri_preflight_port(Option::<ResolvePort>::None);
    ps.install_backends(adapter.composed_backends());
    assert_eq!(ps.backends(), BackendSet::NONE, "a bare adapter composes nothing");
    let (port, done, server) = versions_pms_with(ORDINARY_TITLE, MDE_DIRECTPLAY);
    let sid = plx_plex::plex::register_for_test("none", "127.0.0.1", port, "token", "none-client");
    let env = ResolveEnv::snapshot(&ps,
        plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-v");
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "rk-v",
        "/library/parts/3134/1/file.mkv", "h264", "aac", &env);
    done.send(()).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(plan.verdict, Some(PlayVerdict::NoQualifiedBackend));
    assert_eq!(plan.uri_decline, Some(crate::route::uri_preflight::UriDecline::NoPort),
        "no resolver, so no URI original was even asked about");
    assert!(plan.url.is_empty() && plan.sess.is_empty() && plan.uri_original.is_none());
    assert!(!requests.iter().any(|line| line.contains("/decision?") || line.contains("/transcode/")),
        "no Feed work for a platform with no Feed: {requests:?}");
    plx_plex::plex::reset_servers_for_test();
}

/// (c, through the landing) The same refusal reaches the session as its play verdict, which is what
/// the player's read-out reads.
#[test]
fn a_composition_with_no_driver_lands_the_refusal_on_the_session() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    ps.install_uri_preflight_port(Option::<ResolvePort>::None);
    ps.install_backends(BackendSet::NONE);
    let (port, done, server) = versions_pms_with(ORDINARY_TITLE, MDE_DIRECTPLAY);
    let sid = plx_plex::plex::register_for_test("none-landing", "127.0.0.1", port, "token", "none-landing-client");
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    assert!(play_ordinary(&mut ps, &mut meta, sid, "h264", "aac").is_none());
    assert_eq!(ps.play_verdict, Some(PlayVerdict::NoQualifiedBackend));
    done.send(()).unwrap();
    let _ = server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
}

/// (d) Two drivers composed (Target shape 6d): the planner's choice between them is today's
/// `Offer::rank`. A URI original that the resolver qualifies precedes the Feed's own original (both
/// are the original handoff, and the URI tie-break wins); where the resolver declines, Feed's
/// original plays and no URI offer exists.
#[test]
fn with_both_drivers_a_qualified_uri_original_precedes_the_feed_original() {
    use crate::route::backend_policy::{OfferId, Qualification, Selection};
    for (certified, expect) in [(true, BackendKind::Uri), (false, BackendKind::Feed)] {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _g = fresh_registry(&mut ps);
        assert!(plx_net::net::global_init());
        let seen = Arc::new(Mutex::new(Vec::new()));
        // The resolver answers for the URI driver; the session carries both backends.
        let _composed = compose_uri_only(&mut ps, None);
        if !certified {
            ps.install_uri_preflight_port(ResolvePort::new(
                ProductEvidence { display: plx_playback_contract::uri::DisplayEvidence::ready("fake"),
                    callable: URI_CAPS, certified: URI_CAPS },
                Arc::new(SelfCertified { pms: None, opt_in_seen: seen.clone() })));
        }
        ps.install_backends(BackendSet::BOTH);
        let (port, done, server) = versions_pms_with(
            if certified { ORDINARY_TITLE } else { URI_DTS },
            if certified { MDE_DIRECTPLAY } else { MDE_TRANSCODE_VIDEO });
        let sid = plx_plex::plex::register_for_test("both", "127.0.0.1", port, "token", "both-client");
        let mut meta = plx_data::stores::metadata::MetadataStore::default();
        let acodec = if certified { "aac" } else { "dca" };
        let start = play_ordinary(&mut ps, &mut meta, sid, "h264", acodec);
        match start.and_then(|s| s.uri_offer) {
            Some(offer) => {
                assert!(certified);
                ps.qualify_backend_offer(BackendKind::Uri, offer.offer_id(), Qualification::Qualified);
                // The Feed driver could carry the same original too: both are candidates, and
                // the rank (the URI tie-break) alone chooses.
                assert!(ps.qualify_resolved_feed_offer(OfferId::FEED_ORIGINAL,
                    CandidatePurpose::NewCandidate));
            }
            None => assert!(!certified, "a certified original must reach the planner as an offer"),
        }
        assert!(matches!(ps.backend_offer(), Selection::Open(choice)
            if choice.backend == expect && (certified == (choice.id == OfferId::URI_ORIGINAL))),
            "{:?}", ps.backend_offer());
        scrobble_stop(&mut ps, None, None);
        drain_scrobble();
        done.send(()).unwrap();
        let _ = server.join().unwrap();
        plx_plex::plex::reset_servers_for_test();
    }
}
