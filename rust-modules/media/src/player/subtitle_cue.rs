//! Common subtitle cue presentation. No FFmpeg handle or packet enters this module; the
//! demux adapters hand it text or owned indexed bitmap data, and this layer applies the
//! playback clock, charset-independent ASS admission, and bitmap store publication.

use super::text_ass::TextDialect;
use std::os::raw::c_int;

/// How a subtitle stream's payload turns into displayable text — classified by the codec's
/// name (avcodec_get_name is already linked, so we avoid hardcoding the n3.3 subtitle codec-id
/// block, which the ABI probe never verified). Bitmap subs (PGS/VobSub/DVB/teletext) carry no
/// text and are software-decoded to indexed rectangles; they also occupy a file-order slot so the track
/// menu's desired_sub_idx stays aligned with the metadata subs list.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PresentationTrack {
    /// SRT / subrip / webvtt / sami / text / mov_text ...: the packet payload is text, UTF-8 or (see
    /// `sub_charset`) a legacy encoding, converted to ASS by the dialect (`player::text_ass`) and
    /// rendered by libass like an authored track. mov_text (mp4 tx3g) also carries a 2-byte
    /// big-endian text-length prefix before the text.
    Text(TextDialect),
    Ass,    // ASS / SSA: dialogue line; text is the field after the 8th comma
    Bitmap, // PGS / VobSub / DVB / teletext: software-decoded image subtitle
}

impl PresentationTrack {
    pub(crate) fn from_codec_name(name: &str) -> Self {
        if matches!(name, "ass" | "ssa") { return Self::Ass; }
        TextDialect::from_codec_name(name).map_or(Self::Bitmap, Self::Text)
    }
}

/// One cue produced from one subtitle packet, in the stream's clock (`pts_ns`), not yet delivered.
pub(crate) enum SubCue<'a> {
    /// SRT / WebVTT / mov_text ... payload (mov_text's length prefix already dropped), converted to
    /// ASS by the track's dialect in `ass_source::push_text`.
    /// `payload` is always UTF-8 here: the demux adapter ran the track's charset step.
    /// `identity` is `ass_source::packet_identity` of the packet bytes BEFORE the charset step: the
    /// dedup key of a re-read, which must not depend on how the detector decoded it this time.
    Text { track: i32, start: i64, end: i64, payload: std::borrow::Cow<'a, [u8]>, identity: u64 },
    /// An ASS/SSA dialogue line for `ass_source`.
    Ass { track: i32, start: i64, end: i64, payload: std::borrow::Cow<'a, [u8]> },
    /// A decoded image display set (every rect, indexed) on its authoring canvas.
    BitmapSet {
        track: c_int,
        pts: i64,
        cw: i32,
        ch: i32,
        rects: Vec<crate::player::SubRect>,
        /// Rects the decoder produced, before the byte budget and the unusable-rect skip (for the
        /// log line that reports a set the store had to thin).
        total_rects: usize,
    },
    /// An image-subtitle CLEAR: closes the track's open bitmap cue at `pts`.
    BitmapClear { track: c_int, pts: i64 },
}

impl<'a> SubCue<'a> {
    /// The cue with every time moved by `by_ns` (negative = earlier). The side reader's clock
    /// mapping: a cue is stamped in the original Part's time and drawn against the remux's playhead,
    /// so each one goes through this with `-delta` before [`Self::push`].
    pub(crate) fn shifted(self, by_ns: i64) -> SubCue<'a> {
        match self {
            SubCue::Text { track, start, end, payload, identity } => {
                SubCue::Text { track, start: start.saturating_add(by_ns), end: end.saturating_add(by_ns), payload, identity }
            }
            SubCue::Ass { track, start, end, payload } => {
                SubCue::Ass { track, start: start.saturating_add(by_ns), end: end.saturating_add(by_ns), payload }
            }
            SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects } => {
                SubCue::BitmapSet { track, pts: pts.saturating_add(by_ns), cw, ch, rects, total_rects }
            }
            SubCue::BitmapClear { track, pts } => SubCue::BitmapClear { track, pts: pts.saturating_add(by_ns) },
        }
    }
}

impl SubCue<'_> {
    /// Hand the cue to the render store. `ass_generation` is the `ass_source::begin` token a
    /// `Text` or `Ass` cue is pushed under; the image kinds ignore it.
    pub(crate) fn push(self, ass_generation: u64, retention: plx_playback_contract::subtitle::CaptionRetention) {
        self.push_with_bitmap_generation(ass_generation, None, retention);
    }

    /// The main demux's bitmap store is generation-fenced independently of
    /// ASS. Charset and image decode have already completed outside native
    /// retirement; the store mutex serializes this check with reset.
    pub(crate) fn push_addressed(self, ass_generation: u64, bitmap_generation: u64, retention: plx_playback_contract::subtitle::CaptionRetention) {
        self.push_with_bitmap_generation(ass_generation, Some(bitmap_generation), retention);
    }

    fn push_with_bitmap_generation(self, ass_generation: u64, bitmap_generation: Option<u64>, retention: plx_playback_contract::subtitle::CaptionRetention) {
        match self {
            SubCue::Text { track, start, end, payload, identity } => {
                let text = String::from_utf8_lossy(&payload);
                crate::player::ass_source::push_text(ass_generation, track, start, end, &text, identity, retention);
            }
            SubCue::Ass { track, start, end, payload } => {
                crate::player::ass_source::push(ass_generation, track, start, end, &payload, retention);
            }
            SubCue::BitmapClear { track, pts } => {
                if let Some(generation) = bitmap_generation {
                    crate::player::close_subtitle_bitmap_for_generation(generation, track, pts);
                } else {
                    crate::player::close_subtitle_bitmap(track, pts);
                }
            }
            SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects } => {
                let rects_kept = rects.len();
                let first_rect = rects.first().map(|r| (r.w, r.h, r.x, r.y));
                let accepted = if let Some(generation) = bitmap_generation {
                    crate::player::push_subtitle_bitmap_for_generation(generation, track, pts, cw, ch, rects, retention)
                } else {
                    crate::player::push_subtitle_bitmap(track, pts, cw, ch, rects, retention);
                    true
                };
                if !accepted { return; }
                if track == crate::player::desired_sub_idx() {
                    let (w, h, x, y) = first_rect.expect("bitmap display set has at least one rect");
                    crate::player::log(&format!(
                        "image cue [{}ms] {}x{} at {},{} rects={} canvas={cw}x{ch}",
                        pts / 1_000_000,
                        w, h, x, y, rects_kept,
                    ));
                }
                if rects_kept < total_rects {
                    crate::player::log(&format!(
                        "ff: image-sub track#{track} kept {rects_kept} of {total_rects} rects"
                    ));
                }
            }
        }
    }
}


pub(crate) fn bitmap_cue(display: plx_media_codecs::subtitle::BitmapDisplay, track: c_int, pts: i64) -> SubCue<'static> {
    match display {
        plx_media_codecs::subtitle::BitmapDisplay::Clear => SubCue::BitmapClear { track, pts },
        plx_media_codecs::subtitle::BitmapDisplay::Set { cw, ch, rects, total_rects } => {
            SubCue::BitmapSet {
                track, pts, cw, ch,
                rects: rects.into_iter().map(|r| crate::player::SubRect {
                    x: r.x, y: r.y, w: r.w, h: r.h, index: r.index, palette: r.palette,
                }).collect(),
                total_rects,
            }
        }
    }
}


/// Install a codec-extracted inventory into the common ASS store. The job has already admitted
/// header bytes before copying and collected every font attachment; this layer gives tracks
/// dialects and source generation.
pub(crate) fn begin_sources(
    media_key: &str,
    kinds: &[PresentationTrack],
    manifest: plx_media_codecs::subtitle_extract::Manifest,
) -> u64 {
    let inventory = source_inventory(media_key, kinds, manifest);
    super::ass_source::begin_inventory(inventory)
}

/// Prepare the exact source inventory without taking the native epoch gate.
pub(crate) fn source_inventory(
    media_key: &str,
    kinds: &[PresentationTrack],
    manifest: plx_media_codecs::subtitle_extract::Manifest,
) -> super::ass_source::SourceInventory {
    use super::{ass::Font, ass_source};
    let mut headers = Vec::new();
    let mut text_tracks = 0;
    for (track, (kind, authored)) in kinds.iter().zip(manifest.headers).enumerate() {
        match kind {
            PresentationTrack::Text(dialect) => {
                headers.push((track as i32, ass_source::TrackHeader::Text(*dialect)));
                text_tracks += 1;
            }
            PresentationTrack::Ass => {
                if let Some(header) = authored {
                    headers.push((track as i32, ass_source::TrackHeader::Authored(header)));
                }
            }
            PresentationTrack::Bitmap => {}
        }
    }
    let authored_tracks = headers.len() - text_tracks;
    let fonts: Vec<Font> = manifest.fonts.into_iter()
        .map(|f| Font { name: f.name, data: f.data }).collect();
    if !headers.is_empty() || !fonts.is_empty() {
        super::log(&format!(
            "ass: embedded tracks={authored_tracks} text_tracks={text_tracks} fonts={} font_bytes={}",
            fonts.len(), manifest.font_bytes,
        ));
    }
    ass_source::SourceInventory::new(media_key, headers, fonts)
}

/// Common side-reader adaptation: classifies codec names and performs the per-track charset
/// step. The codec job owns demux and decode; this type owns only presentation policy.
pub(crate) struct SideTracks {
    kinds: Vec<PresentationTrack>,
    charsets: Vec<super::sub_charset::TrackCharset>,
}

impl SideTracks {
    pub(crate) fn new(infos: &[plx_media_codecs::subtitle_extract::TrackInfo]) -> Self {
        let kinds = infos.iter().map(|info| PresentationTrack::from_codec_name(&info.codec_name)).collect();
        let charsets = infos.iter().map(|info| super::sub_charset::TrackCharset::new(info.language.as_deref())).collect();
        Self { kinds, charsets }
    }

    pub(crate) fn len(&self) -> usize { self.kinds.len() }

    pub(crate) fn kinds(&self) -> &[PresentationTrack] { &self.kinds }

    pub(crate) fn decode_plan(&self) -> Vec<plx_media_codecs::subtitle_extract::DecodeKind> {
        use plx_media_codecs::subtitle_extract::DecodeKind;
        self.kinds.iter().map(|kind| match kind {
            PresentationTrack::Text(_) => DecodeKind::Text { max_bytes: super::ass_source::MAX_EVENT_BYTES.saturating_add(2) },
            PresentationTrack::Ass => DecodeKind::Ass { max_bytes: super::ass_source::MAX_PACKET_BYTES },
            PresentationTrack::Bitmap => DecodeKind::Bitmap,
        }).collect()
    }

    pub(crate) fn decode(&self, raw: plx_media_codecs::subtitle_extract::RawCue) -> SubCue<'static> {
        use plx_media_codecs::subtitle_extract::RawCue;
        match raw {
            RawCue::Bitmap { track, pts, display } => bitmap_cue(display, track as i32, pts),
            RawCue::Bytes { track, start, end, mut payload } => match self.kinds[track] {
                PresentationTrack::Ass => SubCue::Ass { track: track as i32, start, end, payload: payload.into() },
                PresentationTrack::Text(dialect) => {
                    if dialect == TextDialect::MovText && payload.len() >= 2 {
                        let len = (usize::from(payload[0]) << 8) | usize::from(payload[1]);
                        payload.drain(..2);
                        payload.truncate(len.min(payload.len()));
                    }
                    let identity = super::ass_source::packet_identity(&payload);
                    let decoded = super::sub_charset::subtitle_text(&payload, &self.charsets[track]);
                    let bytes = match decoded {
                        std::borrow::Cow::Borrowed(_) => payload,
                        std::borrow::Cow::Owned(text) => text.into_bytes(),
                    };
                    SubCue::Text { track: track as i32, start, end, payload: bytes.into(), identity }
                }
                PresentationTrack::Bitmap => unreachable!("codec plan never emits bytes for an image track"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_media_codecs::subtitle_extract::{RawCue, TrackInfo};

    #[test]
    fn side_job_owned_bytes_keep_mov_text_length_and_authored_ass() {
        let tracks = SideTracks::new(&[
            TrackInfo { stream_index: 4, codec_name: "mov_text".into(), language: None },
            TrackInfo { stream_index: 6, codec_name: "ass".into(), language: None },
        ]);
        let cue = tracks.decode(RawCue::Bytes {
            track: 0, start: 10, end: 20,
            payload: vec![0, 5, b'H', b'e', b'l', b'l', b'o', b'!', b'!'],
        });
        match cue {
            SubCue::Text { track, payload, identity, .. } => {
                assert_eq!(track, 0);
                assert_eq!(&*payload, b"Hello");
                assert_eq!(identity, super::super::ass_source::packet_identity(b"Hello"));
            }
            _ => panic!("mov_text remains a text cue"),
        }
        let cue = tracks.decode(RawCue::Bytes {
            track: 1, start: 30, end: 40, payload: b"0,0,Default,,0,0,0,,ASS".to_vec(),
        });
        match cue {
            SubCue::Ass { track, payload, .. } => {
                assert_eq!(track, 1);
                assert!(matches!(payload, std::borrow::Cow::Owned(_)), "the FFmpeg packet is already released");
            }
            _ => panic!("ASS stays authored"),
        }
    }
}
