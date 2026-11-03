//! Resolved inputs for one buffer-feed open. The route's prepared transaction remains the
//! authority; this value freezes its applied projection before native construction begins.
//! URLs and reader credentials are deliberately absent from `Debug` output.

use crate::route;

pub(super) struct FeedOpenRequest {
    pub url: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub pcm_is_lossy: bool,
    pub audio_channels: i64,
    pub payload: PayloadFacts,
    pub side_reader: Option<route::SideReaderTarget>,
    pub hls_abr: Option<(route::HlsAbrControl, route::WorkerTicket)>,
    pub auto_original: Option<route::AutoOriginalWatch>,
    pub trace_generation: u32,
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

impl PayloadFacts {
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
        Self {
            url,
            video_codec: route::stream_vcodec(ps),
            audio_codec: route::stream_acodec(ps),
            pcm_is_lossy: route::stream_pcm_is_lossy(ps),
            audio_channels: route::stream_achannels(ps),
            payload,
            side_reader: route::side_reader_target(ps),
            hls_abr: route::hls_abr_control(ps),
            auto_original: route::auto_original_watch(ps),
            trace_generation: route::playback_trace_generation(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_open_keeps_its_source_and_declaration_after_route_changes() {
        let mut ps = route::PlaybackSession::IDLE;
        route::set_stream_codecs(&mut ps, "h264", "aac");
        let open = FeedOpenRequest::capture(&ps, "https://example.invalid/first".into());
        route::set_stream_codecs(&mut ps, "hevc", "eac3");
        assert_eq!(open.url, "https://example.invalid/first");
        assert_eq!(open.video_codec, "h264");
        assert_eq!(open.audio_codec, "aac");
    }
}
