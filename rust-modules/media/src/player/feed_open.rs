//! Resolved inputs for one buffer-feed open. The route's prepared transaction remains the
//! authority; this value freezes its applied projection before native construction begins.
//! URLs and reader credentials are deliberately absent from `Debug` output.

use crate::route;
use plx_base::eventlog::log;
use plx_play_feed::bf_split;

pub(super) struct FeedOpenRequest {
    pub origin: plx_net::net::origin::Origin,
    pub path: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub pcm_is_lossy: bool,
    pub audio_channels: i64,
    /// Backend-owned Load facts, with route and metadata types mapped at capture time.
    pub feed_payload: plx_play_feed::PayloadFacts,
    pub payload_options: plx_play_feed::PayloadOptions,
    pub original_dv: OriginalDvFacts,
    pub session: Option<Box<dyn plx_playback_contract::demux::DemuxSession>>,
    pub hls_abr: Option<Box<dyn plx_playback_contract::adaptive::AdaptiveSession>>,
    pub auto_original: Option<Box<dyn plx_playback_contract::adaptive::OriginalSession>>,
}

/// Original wire evidence for Force preflight and the declaration log. The converted sink
/// presentation is separately represented by `feed_payload.dv`.
#[derive(Clone, Copy)]
pub(super) struct OriginalDvFacts {
    pub present: bool,
    pub profile: i64,
    pub bl_compat: i64,
    pub el_present: bool,
}

impl From<plx_data::metadata::Dovi> for OriginalDvFacts {
    fn from(dv: plx_data::metadata::Dovi) -> Self {
        Self { present: dv.present, profile: dv.profile,
            bl_compat: dv.bl_compat, el_present: dv.el_present }
    }
}

/// Common reader IO for one feed attempt. It remains attached to the Engine through teardown,
/// so a failed Load spawn and a later dynamic track switch get the same stop-before-join barrier.
pub(super) struct SideReaderOpen {
    selected: bool,
    clock_only: bool,
    spec: Option<super::subside::Spec>,
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
        let started = self.spec.take().is_some_and(super::subside::start);
        if !started {
            super::subside::failed_reading(self.clock_only, "subside: could not start the reader thread");
            if !self.clock_only {
                super::SHARED.side_subs_owner.store(false, std::sync::atomic::Ordering::Release);
            }
        }
    }

    fn stop_before_demux_join(&mut self, reason: &str) {
        super::subside::stop(reason);
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
        let original_dv = payload.dovi.into();
        let source = plx_plex::plex::StreamUrl::parse(&url);
        Self {
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
                    crate::adaptive::Start::from(control),
                    Box::new(crate::adaptive::PlayerAdaptiveEffects),
                )
            }),
            auto_original: route::auto_original_watch(ps).map(|watch| {
                crate::adaptive::boxed_original_session(crate::adaptive::OriginalStart::from(watch))
            }),
        }
    }

    /// The common owner checks the credential policy before claiming the native attempt.
    pub fn credential_transport_allowed(&self) -> bool {
        plx_plex::http::credential_transport_allowed(&self.origin, &self.path, &[])
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
        assert_eq!(open.origin.base(), "https://example.invalid:32400");
        assert_eq!(open.path, "/first");
        assert_eq!(open.video_codec, "h264");
        assert_eq!(open.audio_codec, "aac");
    }
}

/// One source selected under the route transaction, before any native resource is built.
pub(super) struct PreparedSource {
    pub url: String,
    pub sample: Option<SampleSource>,
    pub is_h265: bool,
}

pub(super) struct SampleSource {
    pub data: Vec<u8>,
    pub au: Vec<usize>,
}

pub(super) fn prepare_source(
    ps: &mut route::PlaybackSession,
    route_start: route::RouteStartTransaction,
) -> Result<PreparedSource, route::RouteStartResult> {
    // resolve the URL, in precedence order: route (a selected movie) wins, then
    // /tmp/plxnative-playurl (a URL + its declaration), then /tmp/plxnative-url (a URL
    // alone), then a local sample.
    let mut url = crate::route::url(ps);
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
        match crate::player::playurl::playurl() {
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
    let mut sample: Option<SampleSource> = None;
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
            sample = Some(SampleSource { data, au });
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
            sample = Some(SampleSource { data, au });
        } else {
            // nothing to play: no selected item, no /tmp/plxnative-url, no local sample. (The old
            // baked-in demo-movie fallback is gone — the binary carries no URLs/credentials.)
            #[cfg(feature = "devtriggers")]
            log("start_bufferfeed: no URL — select an item (or set /tmp/plxnative-url)");
            return Err(crate::route::RouteStartResult::NoRoute);
        }
    }
    if !crate::route::prepare_route_start(route_start) {
        log("start_bufferfeed: prepared route no longer owns the transaction");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    Ok(PreparedSource { url, sample, is_h265 })

}

pub(super) fn install_synthetic_playurl(
    ps: &mut crate::route::PlaybackSession,
    play: &crate::player::playurl::PlayUrl,
) -> Result<String, crate::route::RouteStartResult> {
    if !crate::route::set_stream_declaration(
        ps,
        &play.vcodec,
        &play.acodec,
        play.fps,
        play.dovi.to_dovi(),
        play.atmos,
    ) {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let url = play.url.clone();
    crate::route::set_url(ps, &url);
    Ok(url)
}
