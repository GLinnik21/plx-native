//! Resolved inputs for one buffer-feed open. The route's prepared transaction remains the
//! authority; this value freezes its applied projection before native construction begins.
//! URLs and reader credentials are deliberately absent from `Debug` output.

use crate::route;
use plx_base::eventlog::log;
use plx_playback_contract::feed::{bf_split, SampleBuf};

pub(super) struct FeedOpenRequest {
    /// The stream's address as the network layer spells it, for the credential policy; `stream`
    /// carries the same address in the feed's plain form.
    origin: plx_net::net::origin::Origin,
    pub stream: plx_playback_contract::feed::OpenStream,
    /// The player's end of the channel the open's adaptive worker reports on. The open is claimed
    /// under an attempt only after it is captured, so the caller takes this out
    /// ([`Self::take_readouts`]) and holds it beside the attempt.
    readouts: Option<crate::adaptive::AbrInbox>,
}

/// Original wire evidence for the declaration log. The converted sink
/// presentation is separately represented by `feed_payload.dv`.
pub(super) fn original_dv(dv: plx_data::metadata::Dovi) -> plx_playback_contract::feed::OriginalDvFacts {
    plx_playback_contract::feed::OriginalDvFacts {
        present: dv.present, profile: dv.profile,
        bl_compat: dv.bl_compat, el_present: dv.el_present,
    }
}

/// What the route decided about the side subtitle reader for this open: no reader unless the
/// start is a stream with a reader target. The Feed builds the reader's port from it
/// (`plx_play_feed::side_reader::SideReaderOpen`).
pub(super) fn side_reader_request(ps: &route::PlaybackSession, stream: bool)
    -> plx_playback_contract::feed::SideReaderRequest {
    let Some(target) = stream.then(|| route::side_reader_target(ps)).flatten() else {
        return Default::default();
    };
    plx_playback_contract::feed::SideReaderRequest {
        selected: true,
        clock_only: target.clock_only(),
        spec: super::subside::spec_for(&target, super::subside::capture_offset_ns(ps)),
    }
}

/// The presentation facts consumed by the native Load declaration. These come from the
/// already applied route and can also be passed to the pure payload builder in host tests.
pub(super) struct PayloadFacts {
    pub pcm: Option<Result<route::PcmFormat, String>>,
    pub max_raster: (u16, u16),
    pub fps: f64,
    pub dovi: plx_data::metadata::Dovi,
    pub dv_presentation: plx_data::metadata::DvPresentation,
    pub immersive: bool,
    pub force_direct_play: bool,
}

impl From<route::PcmFormat> for plx_playback_contract::feed::PcmFormat {
    fn from(f: route::PcmFormat) -> Self {
        Self {
            sample_rate: f.sample_rate,
            bits: f.bits,
            channels: f.channels,
            source_rate: f.source_rate,
        }
    }
}

impl PayloadFacts {
    pub fn to_feed(&self) -> plx_playback_contract::feed::PayloadFacts {
        plx_playback_contract::feed::PayloadFacts {
            pcm: self.pcm.clone().map(|value| value.map(Into::into)),
            max_raster: self.max_raster,
            fps: self.fps,
            dv: self.dv_presentation.declared().map(|n| plx_playback_contract::feed::DolbyHdrInfo {
                profile_id: n.profile_id,
                track_type: n.track_type,
                encryption_type: n.encryption_type,
            }),
            immersive: self.immersive,
            force_direct_play: self.force_direct_play,
        }
    }

    pub fn capture(ps: &route::PlaybackSession) -> Self {
        Self {
            pcm: route::stream_pcm(ps),
            max_raster: route::sink_max_raster(ps),
            fps: route::stream_fps(ps),
            dovi: route::stream_dovi(ps),
            dv_presentation: route::stream_dv_presentation(ps),
            immersive: route::stream_immersive(ps),
            force_direct_play: route::forced_direct_play(ps),
        }
    }
}

impl FeedOpenRequest {
    /// Call only after `prepare_route_start` and the dev source precedence have settled.
    /// Worker tickets are captured alongside the same projection consumed by Load.
    pub fn capture(ps: &route::PlaybackSession, url: String) -> Self {
        let payload = PayloadFacts::capture(ps);
        let feed_payload = payload.to_feed();
        let payload_options = payload_options(&feed_payload);
        let original_dv = original_dv(payload.dovi);
        let audio_codec = route::stream_acodec(ps);
        let audio_channels = if audio_codec.eq_ignore_ascii_case("opus") {
            let lane = route::opus_lane(ps);
            let kind = if lane.converted { "converted" } else { "copy" };
            plx_base::eventlog::log(&format!(
                "route: Opus Load channels={} lane={kind} source={} bound={}",
                lane.width, lane.source, lane.bound
            ));
            lane.width
        } else {
            route::stream_achannels(ps)
        };
        let source = plx_plex::plex::StreamUrl::parse(&url);
        let (video_bytes, audio_bytes) = super::aq_caps();
        let (video_lead_ms, audio_lead_ms) = super::feed_leads_ms();
        let plant_geometry = crate::abr::PlantGeometry::from_feed(
            video_bytes, audio_bytes, video_lead_ms, audio_lead_ms,
        );
        let (readouts_tx, readouts) = crate::adaptive::readout_channel();
        Self {
            readouts: Some(readouts),
            origin: source.origin.clone(),
            stream: plx_playback_contract::feed::OpenStream {
                origin: plx_playback_contract::feed::OpenOrigin {
                    tls: source.origin.scheme() == plx_net::net::origin::Scheme::Https,
                    host: source.origin.host().to_string(),
                    port: source.origin.port(),
                },
                path: source.path,
                video_codec: route::stream_vcodec(ps),
                audio_codec,
                pcm_is_lossy: route::stream_pcm_is_lossy(ps),
                audio_channels,
                feed_payload,
                payload_options,
                original_dv,
                // The exact route attempt and native epoch do not exist until after claim. The
                // common owner attaches their addressed observer before launching the worker.
                hls_abr: route::hls_abr_control(ps).map(|control| {
                    crate::adaptive::boxed_session(
                        crate::adaptive::Start::from(control).with_plant_geometry(plant_geometry),
                        Box::new(crate::adaptive::PlayerAdaptiveEffects::new(readouts_tx.clone())),
                    )
                }),
                auto_original: route::auto_original_watch(ps).map(|watch| {
                    crate::adaptive::boxed_original_session(
                        crate::adaptive::OriginalStart::from(watch).with_plant_geometry(plant_geometry),
                        Box::new(crate::adaptive::PlayerAdaptiveEffects::new(readouts_tx.clone())),
                    )
                }),
            },
        }
    }

    /// The common owner checks the credential policy before claiming the native attempt.
    pub fn credential_transport_allowed(&self) -> bool {
        plx_plex::http::credential_transport_allowed(&self.origin, &self.stream.path, &[])
    }

    /// The inbox of the open's adaptive worker (once).
    pub(super) fn take_readouts(&mut self) -> crate::adaptive::AbrInbox {
        self.readouts.take().expect("an open's readout inbox is taken once, before it is consumed")
    }

    pub fn into_stream(self) -> plx_playback_contract::feed::OpenStream {
        self.stream
    }
}

/// Whether the `dvnonode` diagnostic holds back the `DolbyHdrInfo` node of a Load that declares one
/// (`declared`). **The one predicate** [`payload_options`] hands the Feed's payload splice and
/// [`sends_dolby_hdr_info`] arms the Dolby Vision landing guard by.
fn dv_node_suppressed(declared: bool) -> bool {
    declared && plx_data::metadata::dv_node_suppressed()
}

/// The developer switches are read once by the common open runner. The physical payload builder
/// receives resolved values and never consults route metadata or trigger files.
pub(super) fn payload_options(facts: &plx_playback_contract::feed::PayloadFacts) -> plx_playback_contract::feed::PayloadOptions {
    let with_fps = !plx_base::devtrig::flag("nofps");
    if !with_fps {
        #[cfg(feature = "devtriggers")]
        log("esInfo: videoFps WITHHELD by /tmp/plxnative-nofps");
    }
    plx_playback_contract::feed::PayloadOptions {
        with_fps,
        suppress_dv_node: dv_node_suppressed(facts.dv.is_some()),
    }
}

/// Whether a Load of a route frozen as `dv` carries the `DolbyHdrInfo` node: it declares one and the
/// `dvnonode` diagnostic does not hold it back, the same answer [`payload_options`] gives the splice,
/// so the Dolby Vision landing guard never watches a play whose Load sent no node.
pub(super) fn sends_dolby_hdr_info(dv: plx_data::metadata::DvPresentation) -> bool {
    dv.declared().is_some() && !dv_node_suppressed(true)
}

/// The route's half of the Dolby Vision landing guard (#1166) for the Engine `ps`'s route is about to
/// start: what the Feed builds its guard from at the Engine's construction. It reads the firmware row
/// from the ATTEMPT's caps (`route::dv_first_picture`), never from the process-wide `devcaps::caps()`.
pub(super) fn dv_landing_plan(ps: &route::PlaybackSession) -> plx_playback_contract::feed::DvLandingPlan {
    use plx_playback_contract::feed::{DvLandingMode, DvLandingPlan};
    if !sends_dolby_hdr_info(route::stream_dv_presentation(ps)) {
        return DvLandingPlan::INERT;
    }
    let mode = if route::preview_request(ps) {
        DvLandingMode::Inert
    } else if !route::is_transcoding(ps) && route::dv_withdrawable(ps) {
        DvLandingMode::Act
    } else {
        // A Profile 5 direct play has no base layer to fall back to, and a remux that copies a declared
        // video is a flight's fresh Load: there is no rung for either, so the guard only says so.
        DvLandingMode::ReportOnly
    };
    DvLandingPlan {
        mode,
        idr_first: route::dv_first_picture(ps) == plx_platform::devcaps::DvFirstPicture::IdrOnly,
    }
}

/// Apply the current route's selected codec and Force policy to the native speed ceiling.
pub(crate) fn speed_fps_ceiling_now(ps: &route::PlaybackSession) -> f64 {
    super::engine::speed_fps_ceiling(
        plx_platform::devcaps::LoadVideo::of_stream(&route::stream_vcodec(ps)),
        &plx_platform::devcaps::caps(),
        plx_platform::devcaps::measured() && !route::forced_direct_play(ps),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_open_keeps_its_source_and_declaration_after_route_changes() {
        let _serial = plx_base::testlock::serial();
        let mut ps = route::PlaybackSession::IDLE;
        route::set_stream_codecs(&mut ps, "h264", "aac");
        let open = FeedOpenRequest::capture(&ps, "https://example.invalid/first".into());
        route::set_stream_codecs(&mut ps, "hevc", "eac3");
        assert_eq!(open.origin.base(), "https://example.invalid:32400");
        assert_eq!(open.stream.path, "/first");
        assert_eq!(open.stream.video_codec, "h264");
        assert_eq!(open.stream.audio_codec, "aac");
    }

    /// `present` is DERIVED, so it cannot disagree with the profile in either direction.
    #[test]
    fn dovi_presence_follows_the_profile() {
        use plx_playback_contract::playurl::PlayDovi;
        let none = play_dovi(PlayDovi { profile: 0, bl_compat: 1, el_present: true });
        assert!(!none.present, "profile 0 is not Dolby Vision whatever else is set");
        let p7 = play_dovi(PlayDovi { profile: 7, bl_compat: 6, el_present: true });
        assert!(p7.present);
        assert_eq!((p7.profile, p7.bl_compat, p7.el_present), (7, 6, true));
        let p8 = play_dovi(PlayDovi { profile: 8, bl_compat: 1, el_present: false });
        assert!(p8.present, "profile 8 must derive present=true");
        assert_eq!((p8.profile, p8.bl_compat, p8.el_present), (8, 1, false));
        assert_eq!(
            play_dovi(PlayDovi::default()),
            plx_data::metadata::Dovi::NONE,
            "an absent dovi node must be silence itself"
        );
    }
}

/// The source a start will play, before the route transaction is prepared and before any native
/// resource is built.
pub(super) struct ResolvedSource {
    pub url: String,
    pub sample: Option<Box<SampleBuf>>,
    pub is_h265: bool,
}

/// Choose the source of a start, and qualify the Feed recipe of one that no plan resolved.
///
/// **This runs before the start door's offer check, not after it.** A source a route plan resolved
/// had its recipe qualified by `apply_plan`. A dev trigger's has no plan, so the only place it
/// becomes a source is below, and the check that refuses a Start without a qualified recipe
/// must see the result: asked first, it refused every such start (`playurl`, `url`, the samples)
/// with "no qualified feed offer for this resolved route", with nothing qualified yet to find.
///
/// Nothing here touches a native resource or the route transaction; the caller prepares the
/// transaction once the offer check has passed.
pub(super) fn resolve_source(
    ps: &mut route::PlaybackSession,
    pa: &super::adapter::PlayerAdapter,
) -> Result<ResolvedSource, route::RouteStartResult> {
    // resolve the URL, in precedence order: route (a selected movie) wins, then
    // /tmp/plxnative-playurl (a URL + its declaration), then /tmp/plxnative-url (a URL
    // alone), then a local sample.
    let mut url = crate::route::url(ps);
    // A route that holds a source (a plan's, or the one a reload re-reads) has qualified it
    // already. Only a source named below, over an empty route, is a new explicit one.
    let explicit = url.is_empty();
    if url.is_empty() {
        // dev: /tmp/plxnative-playurl — a URL *and the Load declaration to play it with*, with no
        // library item behind it. This is the player-PIPELINE test tier's entry (tests/README.md):
        // `plxnative-url` below hands over a URL only, which leaves the payload describing
        // whatever the route happened to hold — an empty string, i.e. H264 + "AC3" — so a 4K HEVC
        // or a Dolby file could never be declared honestly without Plex. Read HERE rather than at
        // the app.rs entry point because the declaration has to be in `route` before the payload
        // is composed a few lines below, and because a reload (a seek that escalates) comes back
        // through this same function and must re-apply it.
        //
        // `route::url()` still wins: a real selection is never overridden by a stale trigger.
        match pa.play_url() {
            Some(Ok(p)) => {
                url = install_synthetic_playurl(ps, &p)?;
                if let Some([w, h]) = p.source_raster {
                    crate::route::set_stream_source_raster(ps, w, h);
                }
                if p.auto_source_kbps > 0 && !p.auto_hls_base.is_empty() {
                    // A fixture that starts in HLS hands back the playlist to open: the route's
                    // own `url` has already moved, and this local copy is what everything below
                    // reads.
                    if let Some(hls) = crate::route::arm_auto_fixture(
                        ps,
                        &p.url,
                        p.auto_source_kbps,
                        &p.auto_hls_base,
                        p.auto_start_hls,
                        p.source_raster.map_or((1_920, 1_080), |[w, h]| (w, h)),
                    ) {
                        url = hls;
                    }
                }
            }
            // Log and fall through rather than refuse: the next candidate may well be armed. The
            // harness grades this as a failure anyway, because the `load:` line below will not say
            // what the case expected.
            Some(Err(e)) => log(&format!("playurl: unreadable spec ({e}) — ignored")),
            None => {}
        }
    }
    if url.is_empty() {
        if let Some(t) = plx_base::devtrig::read("url") {
            if !t.is_empty() {
                url = t;
                crate::route::set_url(ps, &url);
            }
        }
    }
    let mut sample: Option<Box<SampleBuf>> = None;
    let mut is_h265 = false;
    if url.is_empty() {
        if let Some(data) = plx_base::devtrig::read_sample("sample.h264") {
            let au = bf_split(&data, 0x09);
            log(&format!(
                "bf_split h264: {} AUs in {} bytes",
                au.len(),
                data.len()
            ));
            if au.len() < 2 {
                return Err(crate::route::RouteStartResult::StartFailed);
            }
            sample = Some(Box::new(SampleBuf::new(data, au)));
        } else if let Some(data) = plx_base::devtrig::read_sample("sample.h265") {
            // Phase 0 probe: feed a local HEVC Annex-B sample to test native HEVC decode.
            let au = bf_split(&data, 0x46);
            log(&format!(
                "bf_split h265: {} AUs in {} bytes",
                au.len(),
                data.len()
            ));
            if au.len() < 2 {
                return Err(crate::route::RouteStartResult::StartFailed);
            }
            is_h265 = true;
            sample = Some(Box::new(SampleBuf::new(data, au)));
        } else {
            // nothing to play: no selected item, no /tmp/plxnative-url, no local sample. (The old
            // baked-in demo-movie fallback is gone — the binary carries no URLs/credentials.)
            #[cfg(feature = "devtriggers")]
            log("start_bufferfeed: no URL — select an item (or set /tmp/plxnative-url)");
            return Err(crate::route::RouteStartResult::NoRoute);
        }
    }
    if explicit {
        // False only where no Feed driver is composed, and then the start door refuses all the same.
        let _ = ps.qualify_explicit_feed_source();
    }
    Ok(ResolvedSource { url, sample, is_h265 })
}

/// The `plxnative-playurl` trigger's DV declaration as the data layer's record. `present` is
/// DERIVED from a non-zero profile rather than carried separately: two fields that can disagree is a
/// way to declare "Dolby Vision, profile 0", which is not a thing, and the harness would have to
/// keep them in step by hand in every case.
fn play_dovi(d: plx_playback_contract::playurl::PlayDovi) -> plx_data::metadata::Dovi {
    plx_data::metadata::Dovi {
        present: d.profile > 0,
        profile: d.profile,
        bl_compat: d.bl_compat,
        el_present: d.el_present,
        ..plx_data::metadata::Dovi::NONE
    }
}

pub(super) fn install_synthetic_playurl(
    ps: &mut crate::route::PlaybackSession,
    play: &plx_playback_contract::playurl::PlayUrl,
) -> Result<String, crate::route::RouteStartResult> {
    if !crate::route::set_stream_declaration(
        ps,
        &play.vcodec,
        &play.acodec,
        play.fps,
        play_dovi(play.dovi),
        play.atmos,
    ) {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let url = play.url.clone();
    crate::route::set_url(ps, &url);
    Ok(url)
}
