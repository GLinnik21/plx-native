//! Resolved inputs for one buffer-feed open. The route's prepared transaction remains the
//! authority; this value freezes its applied projection before native construction begins.
//! URLs and reader credentials are deliberately absent from `Debug` output.

use plx_play_feed::shared::FEED_SHARED;
use crate::route;
use plx_base::eventlog::log;
use plx_play_feed::bf_split;
use plx_play_feed::feed_state::SampleBuf;

pub(super) struct FeedOpenRequest {
    pub physical: plx_play_feed::open::PhysicalOpen,
}

/// Original wire evidence for Force preflight and the declaration log. The converted sink
/// presentation is separately represented by `feed_payload.dv`.
pub(super) fn original_dv(dv: plx_data::metadata::Dovi) -> plx_play_feed::open::OriginalDvFacts {
    plx_play_feed::open::OriginalDvFacts {
        present: dv.present, profile: dv.profile,
        bl_compat: dv.bl_compat, el_present: dv.el_present,
    }
}

/// Common reader IO for one feed attempt. It remains attached to the Engine through teardown,
/// so a failed Load spawn and a later dynamic track switch get the same stop-before-join barrier.
pub(super) struct SideReaderOpen {
    selected: bool,
    clock_only: bool,
    spec: Option<plx_play_feed::side_reader::Spec>,
}

impl SideReaderOpen {
    pub fn capture(ps: &route::PlaybackSession, stream: bool) -> Self {
        let Some(target) = stream.then(|| route::side_reader_target(ps)).flatten() else {
            return Self::empty();
        };
        Self {
            selected: true,
            clock_only: target.clock_only(),
            spec: super::subside::spec_for(&target),
        }
    }

    pub fn empty() -> Self {
        Self { selected: false, clock_only: false, spec: None }
    }
}

impl plx_play_feed::side_reader::SideReaderPort for SideReaderOpen {
    fn embedded_owner(&self) -> bool { self.selected && !self.clock_only }

    fn start_after_load_spawn(&mut self) {
        if !self.selected { return; }
        let started = self.spec.take().is_some_and(|spec| plx_play_feed::side_reader::start(spec, super::subside::JOB));
        if !started {
            plx_play_feed::side_reader::failed_reading(self.clock_only, "subside: could not start the reader thread");
            if !self.clock_only {
                FEED_SHARED.side_subs_owner.store(false, std::sync::atomic::Ordering::Release);
            }
        }
    }

    fn stop_before_demux_join(&mut self, reason: &str) {
        plx_play_feed::side_reader::stop(reason);
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

impl From<route::PcmFormat> for plx_play_feed::PcmFormat {
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
    pub fn to_feed(&self) -> plx_play_feed::PayloadFacts {
        plx_play_feed::PayloadFacts {
            pcm: self.pcm.clone().map(|value| value.map(Into::into)),
            max_raster: self.max_raster,
            fps: self.fps,
            dv: self.dv_presentation.declared().map(|n| plx_play_feed::DolbyHdrInfo {
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
        let source = plx_plex::plex::StreamUrl::parse(&url);
        let (video_bytes, audio_bytes) = super::aq_caps();
        let (video_lead_ms, audio_lead_ms) = super::feed_leads_ms();
        let plant_geometry = crate::abr::PlantGeometry::from_feed(
            video_bytes, audio_bytes, video_lead_ms, audio_lead_ms,
        );
        Self {
            physical: plx_play_feed::open::PhysicalOpen {
                origin: source.origin,
                path: source.path,
                video_codec: route::stream_vcodec(ps),
                audio_codec: route::stream_acodec(ps),
                pcm_is_lossy: route::stream_pcm_is_lossy(ps),
                audio_channels: route::stream_achannels(ps),
                feed_payload,
                payload_options,
                original_dv,
                // The exact route attempt and native epoch do not exist until after claim. The
                // common owner attaches their addressed observer before launching the worker.
                session: None,
                hls_abr: route::hls_abr_control(ps).map(|control| {
                    crate::adaptive::boxed_session(
                        crate::adaptive::Start::from(control).with_plant_geometry(plant_geometry),
                        Box::new(crate::adaptive::PlayerAdaptiveEffects),
                    )
                }),
                auto_original: route::auto_original_watch(ps).map(|watch| {
                    crate::adaptive::boxed_original_session(
                        crate::adaptive::OriginalStart::from(watch).with_plant_geometry(plant_geometry)
                    )
                }),
            },
        }
    }

    /// The common owner checks the credential policy before claiming the native attempt.
    pub fn credential_transport_allowed(&self) -> bool {
        plx_plex::http::credential_transport_allowed(&self.physical.origin, &self.physical.path, &[])
    }

    pub fn into_physical(self) -> plx_play_feed::open::PhysicalOpen {
        self.physical
    }
}

/// The developer switches are read once by the common open runner. The physical payload builder
/// receives resolved values and never consults route metadata or trigger files.
pub(super) fn payload_options(facts: &plx_play_feed::PayloadFacts) -> plx_play_feed::PayloadOptions {
    let with_fps = !plx_base::devtrig::flag("nofps");
    if !with_fps {
        #[cfg(feature = "devtriggers")]
        log("esInfo: videoFps WITHHELD by /tmp/plxnative-nofps");
    }
    plx_play_feed::PayloadOptions {
        with_fps,
        suppress_dv_node: facts.dv.is_some() && plx_data::metadata::dv_node_suppressed(),
    }
}

/// Apply the current route's selected codec and Force policy to the native speed ceiling.
pub(crate) fn speed_fps_ceiling_now(ps: &route::PlaybackSession) -> f64 {
    super::engine::speed_fps_ceiling(
        plx_play_feed::LoadVideo::of_stream(&route::stream_vcodec(ps)),
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
        assert_eq!(open.physical.origin.base(), "https://example.invalid:32400");
        assert_eq!(open.physical.path, "/first");
        assert_eq!(open.physical.video_codec, "h264");
        assert_eq!(open.physical.audio_codec, "aac");
    }

    /// `present` is DERIVED, so it cannot disagree with the profile in either direction.
    #[test]
    fn dovi_presence_follows_the_profile() {
        use plx_play_feed::playurl::PlayDovi;
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
        match plx_play_feed::playurl::playurl() {
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
fn play_dovi(d: plx_play_feed::playurl::PlayDovi) -> plx_data::metadata::Dovi {
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
    play: &plx_play_feed::playurl::PlayUrl,
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
