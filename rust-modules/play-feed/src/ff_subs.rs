//! Physical subtitle packet decode for one FFmpeg format context. The player
//! session owns dialect, charset, selection and render-store publication.

use super::*;
use plx_media_codecs::subtitle_extract::ImageDecoders;
use plx_playback_contract::subtitle::{DecodeKind, RawCue, TrackInfo};

/// File-order subtitle streams and their image decoders. The player supplies
/// one decode plan entry per inspected stream before any decoder is opened.
pub struct SubTracks {
    infos: Vec<TrackInfo>,
    plan: Vec<DecodeKind>,
    decoders: ImageDecoders,
}

impl SubTracks {
    pub unsafe fn inspect(fmt: *mut AVFormatContext, streams: *mut *mut AVStream) -> Vec<TrackInfo> {
        if fmt.is_null() || streams.is_null() { return Vec::new(); }
        let mut infos = Vec::new();
        for i in 0..(*fmt).nb_streams {
            let st = *streams.add(i as usize);
            let cp = stream_codecpar(st);
            if (*cp).codec_type == AVMEDIA_TYPE_SUBTITLE {
                infos.push(TrackInfo {
                    stream_index: i as c_int,
                    codec_name: std::ffi::CStr::from_ptr(avcodec_get_name((*cp).codec_id))
                        .to_string_lossy().into_owned(),
                    language: stream_language(st),
                });
            }
        }
        infos
    }

    pub unsafe fn open(streams: *mut *mut AVStream, infos: Vec<TrackInfo>, plan: Vec<DecodeKind>) -> Self {
        assert_eq!(infos.len(), plan.len(), "subtitle plan must cover every file-order track");
        let specs: Vec<_> = infos.iter().zip(&plan)
            .map(|(info, kind)| (info.stream_index, matches!(kind, DecodeKind::Bitmap)))
            .collect();
        let decoders = ImageDecoders::open(streams, &specs);
        Self { infos, plan, decoders }
    }

    pub fn empty() -> Self {
        Self { infos: Vec::new(), plan: Vec::new(), decoders: ImageDecoders::empty() }
    }

    pub fn entries(&self) -> &[TrackInfo] { &self.infos }
    pub fn plan(&self) -> &[DecodeKind] { &self.plan }
    pub fn position(&self, stream_index: c_int) -> Option<usize> {
        self.infos.iter().position(|info| info.stream_index == stream_index)
    }

    /// Every text track is observed; image tracks are decoded while subtitles
    /// are enabled, matching live track-switch behavior.
    pub unsafe fn decode(
        &self, pos: usize, pkt: *mut AVPacket, st: *mut AVStream,
        decode_image: bool, bitmap_budget: usize,
    ) -> Option<RawCue> {
        match self.plan.get(pos)? {
            DecodeKind::Bitmap if decode_image => self.decoders.decode(pos, pkt, bitmap_budget)
                .map(|display| RawCue::Bitmap { track: pos, pts: pts_ns(pkt, st), display }),
            DecodeKind::Text { max_bytes } | DecodeKind::Ass { max_bytes } => {
                let size = (*pkt).size.max(0) as usize;
                if size == 0 || size > *max_bytes || (*pkt).data.is_null() { return None; }
                let start = pts_ns(pkt, st);
                let duration = (*pkt).duration;
                let end = if duration > 0 {
                    start + av_rescale_q(duration, stream_time_base(st), NS_TB)
                } else { start + 4_000_000_000 };
                let payload = std::slice::from_raw_parts((*pkt).data, size).to_vec();
                Some(RawCue::Bytes { track: pos, start, end, payload })
            }
            _ => None,
        }
    }

    pub unsafe fn close(&mut self) { self.decoders.close(); }
}
