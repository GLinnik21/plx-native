//! Physical subtitle packet decode for one FFmpeg format context. The player
//! session owns dialect, charset, selection and render-store publication.

use super::*;
use plx_media_codecs::subtitle_extract::ImageDecoders;
use plx_playback_contract::demux::DemuxSubtitle;
use plx_playback_contract::subtitle::{CaptionRetention, DecodeKind, RawCue, TrackInfo};
use std::collections::VecDeque;

/// `AV_INPUT_BUFFER_PADDING_SIZE` of every FFmpeg this app links: the zeroed tail a decoder may
/// read past a packet's `size`.
const INPUT_PADDING: usize = 64;

/// The segment type that opens a PGS display set (`pgssubdec.c`'s `PRESENTATION_SEGMENT`). The
/// others, in a set's order, are the window (0x17), palette (0x14) and object (0x15) segments and
/// the closing END (0x80).
const PGS_PRESENTATION_SEGMENT: u8 = 0x16;

/// How a track's packets group into the display sets a decoder needs whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetFraming {
    /// One packet is one display set (Matroska, which carries a set per block, and VobSub / DVB
    /// wherever they are muxed): the packet on screen at the playhead supersedes every one before it.
    PerPacket,
    /// PGS. A set is one packet in Matroska, but libavformat's MPEG-TS demuxer emits one packet per
    /// PES, and a Blu-ray stream carries one segment per PES (PCS, WDS, PDS, ODS, END): the set on
    /// screen begins at the newest packet that opens with a presentation segment, which can be
    /// several packets behind the one at the playhead.
    PgsSegments,
}

impl SetFraming {
    /// The framing of a track by the codec name `avcodec_get_name` gives it.
    pub fn of_codec(codec_name: &str) -> Self {
        if codec_name == "hdmv_pgs_subtitle" { Self::PgsSegments } else { Self::PerPacket }
    }
}

/// One image-subtitle packet the demuxer read while no subtitle was on, kept until one is. It owns
/// its bytes (the demuxer's packet is unreferenced as soon as the loop moves on) with the zero
/// padding libavcodec's decoders may read past `size`.
pub struct HeldPacket {
    time_ns: Option<i64>,
    pts: i64,
    dts: i64,
    duration: i64,
    flags: c_int,
    size: usize,
    bytes: Vec<u8>,
}

impl HeldPacket {
    /// `time_ns` is the packet's clock for retention (`pts_ns_opt`); the rest is what a decoder
    /// reads off an `AVPacket`.
    pub fn new(time_ns: Option<i64>, pts: i64, dts: i64, duration: i64, flags: c_int, payload: &[u8]) -> Self {
        let mut bytes = Vec::with_capacity(payload.len() + INPUT_PADDING);
        bytes.extend_from_slice(payload);
        bytes.resize(payload.len() + INPUT_PADDING, 0);
        Self { time_ns, pts, dts, duration, flags, size: payload.len(), bytes }
    }

    /// Whether the payload opens with a PGS presentation segment: the start of a display set.
    fn opens_pgs_display_set(&self) -> bool {
        self.size > 0 && self.bytes[0] == PGS_PRESENTATION_SEGMENT
    }
}

/// What the demuxer read of each image track while subtitles were Off. Text tracks are observed
/// whatever the selection, so the cues of the read-ahead window are in the store the moment a text
/// track is chosen; an image packet had no such afterlife, and the window it covers (the demuxer runs
/// ahead of the playhead) was thrown away, leaving a gap as long as the read-ahead after an image
/// track was chosen mid-play. This keeps the packets instead, and the choice decodes them in file
/// order, per track, through the decoder a live packet takes.
///
/// Retention is the read-ahead window, with no cap of its own: per track, a display set is kept from
/// the one on screen at the playhead onward, because that one supersedes everything behind it for
/// the one thing a selection made now can show (the line on screen at the playhead, and what
/// follows). Where a set is a packet, that is the last packet at or before the playhead; where a
/// set is a run of segment packets ([`SetFraming::PgsSegments`]), it is the run from the PCS at or
/// before it. With the playhead unknown nothing is retired. A seek discards all of it.
pub struct ImageBacklog {
    tracks: Vec<VecDeque<HeldPacket>>,
    framing: Vec<SetFraming>,
    held: usize,
}

impl ImageBacklog {
    /// One track per entry of `framing`, in file order.
    pub fn new(framing: Vec<SetFraming>) -> Self {
        Self { tracks: framing.iter().map(|_| VecDeque::new()).collect(), framing, held: 0 }
    }

    pub fn is_empty(&self) -> bool { self.held == 0 }

    /// Keep `packet` as the newest of track `pos`, then retire what the playhead (`anchor_ns`,
    /// [`CaptionRetention::anchor_ns`]) leaves superseded.
    pub fn hold(&mut self, pos: usize, packet: HeldPacket, anchor_ns: Option<i64>) {
        let Some(track) = self.tracks.get_mut(pos) else { return; };
        track.push_back(packet);
        self.held += 1;
        let Some(anchor_ns) = anchor_ns else { return; };
        let Some(newest_behind) = track.iter().rposition(|held| held.time_ns.is_some_and(|t| t <= anchor_ns)) else {
            return;
        };
        let keep_from = match self.framing.get(pos) {
            Some(SetFraming::PgsSegments) => {
                // The newest packet at or before the playhead can be the END of the set on screen:
                // that set began at its PCS. With no PCS behind the playhead what is held there is the
                // tail of a set whose start was never held (a seek landed inside it), and there is
                // no start to cut at until the next set's PCS arrives.
                match track.iter().take(newest_behind + 1).rposition(HeldPacket::opens_pgs_display_set) {
                    Some(set_start) => set_start,
                    None => return,
                }
            }
            _ => newest_behind,
        };
        track.drain(..keep_from);
        self.held -= keep_from;
    }

    pub fn clear(&mut self) {
        for track in &mut self.tracks { track.clear(); }
        self.held = 0;
    }

    /// Everything held, each track in file order, leaving the backlog empty.
    pub fn take(&mut self) -> Vec<(usize, HeldPacket)> {
        self.held = 0;
        let mut out = Vec::new();
        for (pos, track) in self.tracks.iter_mut().enumerate() {
            out.extend(track.drain(..).map(|packet| (pos, packet)));
        }
        out
    }
}

/// Stream `index` of the context's table AS IT IS NOW. The table is read through `fmt` each time,
/// with its bound, because a transport stream can grow streams while it is read
/// (`AVFMTCTX_NOHEADER`), which reallocates `streams`: the pointer taken at open is not the table
/// the live `nb_streams` describes.
unsafe fn live_stream(fmt: *mut AVFormatContext, index: c_int) -> Option<*mut AVStream> {
    let streams = (*fmt).streams;
    if streams.is_null() || index < 0 || index as c_uint >= (*fmt).nb_streams { return None; }
    Some(*streams.add(index as usize))
}

/// File-order subtitle streams and their image decoders. The player supplies
/// one decode plan entry per inspected stream before any decoder is opened.
pub struct SubTracks {
    infos: Vec<TrackInfo>,
    plan: Vec<DecodeKind>,
    decoders: ImageDecoders,
    held: ImageBacklog,
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
        let held = ImageBacklog::new(infos.iter().map(|info| SetFraming::of_codec(&info.codec_name)).collect());
        Self { infos, plan, decoders, held }
    }

    pub fn empty() -> Self {
        Self { infos: Vec::new(), plan: Vec::new(), decoders: ImageDecoders::empty(), held: ImageBacklog::new(Vec::new()) }
    }

    pub fn entries(&self) -> &[TrackInfo] { &self.infos }
    pub fn plan(&self) -> &[DecodeKind] { &self.plan }
    pub fn position(&self, stream_index: c_int) -> Option<usize> {
        self.infos.iter().position(|info| info.stream_index == stream_index)
    }

    /// Every text track is observed; image tracks are decoded while subtitles
    /// are enabled, matching live track-switch behavior. The demux loop reaches it through
    /// [`on_packet`](Self::on_packet), which holds an image packet instead while they are not.
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

    /// The per-packet step of the demux loop for a subtitle stream's packet at `pos`: observe a text
    /// track's cue, decode an image track's while a subtitle is on, and hold an image track's packet
    /// while none is (so a track chosen later has the read-ahead window to show). Cues go to
    /// `subtitle` under `generation`; `retention` is asked for only when a packet is kept, and bounds
    /// what is already held. The caller still owns `pkt` and unreferences it.
    ///
    /// Run [`release_if_selected`](Self::release_if_selected) first, on every packet read: a
    /// selection that lands between that check and this call waits for the next packet's, because
    /// decoding this one now would put it ahead of the held ones and the cues must stay in file order.
    ///
    /// # Safety
    /// `pkt` must be a live packet of the track at `pos`, read from `fmt`.
    pub unsafe fn on_packet(
        &mut self, pos: usize, pkt: *mut AVPacket, fmt: *mut AVFormatContext,
        subtitle: &mut dyn DemuxSubtitle, generation: u64,
        retention: impl FnOnce() -> Option<CaptionRetention>,
    ) {
        let Some(st) = live_stream(fmt, (*pkt).stream_index) else { return; };
        let decode_image = subtitle.selected_track() >= 0 && !self.holds_images();
        let bitmap_budget = subtitle.bitmap_budget();
        if let Some(cue) = self.decode(pos, pkt, st, decode_image, bitmap_budget) {
            subtitle.emit(generation, cue);
        }
        if !decode_image { self.hold(pos, pkt, st, retention); }
    }

    /// Run on EVERY packet the demuxer reads, whatever its stream, before the packet is looked at:
    /// if a subtitle was turned on since image packets were held, decode them now (each track in
    /// file order) and hand the cues to `subtitle`, so they precede the cues of the packet that
    /// comes next.
    ///
    /// # Safety
    /// `fmt` must be the format context the tracks were opened from.
    pub unsafe fn release_if_selected(
        &mut self, fmt: *mut AVFormatContext, subtitle: &mut dyn DemuxSubtitle, generation: u64,
    ) {
        if !self.holds_images() || subtitle.selected_track() < 0 { return; }
        let bitmap_budget = subtitle.bitmap_budget();
        self.release_held(fmt, bitmap_budget, |cue| subtitle.emit(generation, cue));
    }

    /// Keep an image track's packet for [`release_held`](Self::release_held) when `decode` was
    /// not allowed to decode it (a text track's packet is ignored: `decode` already observed it).
    unsafe fn hold(
        &mut self, pos: usize, pkt: *const AVPacket, st: *mut AVStream,
        retention: impl FnOnce() -> Option<CaptionRetention>,
    ) {
        if !matches!(self.plan.get(pos), Some(DecodeKind::Bitmap)) { return; }
        if (*pkt).data.is_null() || (*pkt).size <= 0 { return; }
        let payload = std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize);
        let packet = HeldPacket::new(
            pts_ns_opt(pkt, st), (*pkt).pts, (*pkt).dts, (*pkt).duration, (*pkt).flags, payload);
        self.held.hold(pos, packet, retention().and_then(CaptionRetention::anchor_ns));
    }

    /// Whether any image packet waits for a subtitle to be turned on.
    pub fn holds_images(&self) -> bool { !self.held.is_empty() }

    /// Decode everything [`hold`](Self::hold) kept, each track in file order, and hand every cue to
    /// `emit` exactly as a live packet's would be.
    unsafe fn release_held(
        &mut self, fmt: *mut AVFormatContext, bitmap_budget: usize, mut emit: impl FnMut(RawCue),
    ) {
        let held = self.held.take();
        if held.is_empty() { return; }
        let mut scratch = av_packet_alloc();
        if scratch.is_null() { return; }
        for (pos, packet) in &held {
            let Some(info) = self.infos.get(*pos) else { continue; };
            let Some(st) = live_stream(fmt, info.stream_index) else { continue; };
            // The scratch packet points at the owned bytes and owns nothing (no `buf`), so the
            // decode reads them in place and `av_packet_free` below releases no payload.
            (*scratch).data = packet.bytes.as_ptr() as *mut u8;
            (*scratch).size = packet.size as c_int;
            (*scratch).pts = packet.pts;
            (*scratch).dts = packet.dts;
            (*scratch).duration = packet.duration;
            (*scratch).flags = packet.flags;
            (*scratch).stream_index = info.stream_index;
            if let Some(cue) = self.decode(*pos, scratch, st, true, bitmap_budget) { emit(cue); }
        }
        (*scratch).data = std::ptr::null_mut();
        (*scratch).size = 0;
        av_packet_free(&mut scratch);
    }

    /// Forget what is held: a seek, after which none of it belongs to the playhead.
    pub fn discard_held(&mut self) { self.held.clear(); }

    pub unsafe fn close(&mut self) {
        self.held.clear();
        self.decoders.close();
    }
}

#[cfg(test)]
mod backlog_tests {
    use super::*;

    const SEC: i64 = 1_000_000_000;

    const PCS: u8 = 0x16;
    const WDS: u8 = 0x17;
    const PDS: u8 = 0x14;
    const ODS: u8 = 0x15;
    const END: u8 = 0x80;

    fn per_packet(tracks: usize) -> ImageBacklog { ImageBacklog::new(vec![SetFraming::PerPacket; tracks]) }

    fn pgs() -> ImageBacklog { ImageBacklog::new(vec![SetFraming::PgsSegments]) }

    /// One PGS segment as its own packet, the way an MPEG-TS PES carries it (type, then a length and
    /// a body this backlog never reads).
    fn segment(sec: i64, kind: u8) -> HeldPacket {
        HeldPacket::new(Some(sec * SEC), sec * SEC, sec * SEC, 0, 0, &[kind, 0, 1, 0])
    }

    /// A display set sent segment by segment, all stamped `sec`.
    fn set(sec: i64) -> Vec<HeldPacket> { [PCS, WDS, PDS, ODS, END].map(|kind| segment(sec, kind)).into() }

    /// The first payload byte of every packet held on track 0, in order.
    fn kinds(backlog: &ImageBacklog) -> Vec<u8> { backlog.tracks[0].iter().map(|held| held.bytes[0]).collect() }

    fn packet(sec: i64) -> HeldPacket { packet_at(Some(sec * SEC)) }

    fn packet_at(time_ns: Option<i64>) -> HeldPacket {
        HeldPacket::new(time_ns, time_ns.unwrap_or(0), time_ns.unwrap_or(0), 0, 0, &[1, 2, 3])
    }

    fn times(backlog: &ImageBacklog, pos: usize) -> Vec<Option<i64>> {
        backlog.tracks[pos].iter().map(|packet| packet.time_ns).collect()
    }

    fn secs(backlog: &ImageBacklog, pos: usize) -> Vec<i64> {
        times(backlog, pos).into_iter().map(|t| t.unwrap() / SEC).collect()
    }

    /// The reported bug: subtitles Off, an image track chosen at 232 s with the demuxer ten seconds
    /// ahead. Everything the track carried from the line on screen onward is still there.
    #[test]
    fn the_line_on_screen_at_the_playhead_and_the_read_ahead_survive() {
        let mut backlog = per_packet(2);
        for sec in [200, 210, 225, 228, 231, 235, 240] {
            backlog.hold(1, packet(sec), Some(232 * SEC));
        }
        assert_eq!(secs(&backlog, 1), [231, 235, 240], "from the last packet at or before 232 s");
        assert!(!backlog.is_empty());
        assert_eq!(secs(&backlog, 0), Vec::<i64>::new(), "another track holds nothing of this one's");
    }

    #[test]
    fn a_packet_exactly_at_the_playhead_supersedes_the_ones_before_it() {
        let mut backlog = per_packet(1);
        for sec in [10, 20, 30] { backlog.hold(0, packet(sec), Some(20 * SEC)); }
        assert_eq!(secs(&backlog, 0), [20, 30]);
    }

    #[test]
    fn nothing_is_retired_while_the_playhead_is_unknown() {
        let mut backlog = per_packet(1);
        for sec in [10, 20, 30, 40] { backlog.hold(0, packet(sec), None); }
        assert_eq!(secs(&backlog, 0), [10, 20, 30, 40], "everything since the last seek");
    }

    #[test]
    fn a_packet_ahead_of_the_playhead_supersedes_nothing() {
        let mut backlog = per_packet(1);
        for sec in [40, 50, 60] { backlog.hold(0, packet(sec), Some(10 * SEC)); }
        assert_eq!(secs(&backlog, 0), [40, 50, 60]);
    }

    #[test]
    fn the_playhead_moving_on_retires_what_it_passed_at_the_next_packet() {
        let mut backlog = per_packet(1);
        for sec in [10, 20, 30] { backlog.hold(0, packet(sec), Some(5 * SEC)); }
        assert_eq!(secs(&backlog, 0), [10, 20, 30]);
        backlog.hold(0, packet(40), Some(32 * SEC));
        assert_eq!(secs(&backlog, 0), [30, 40]);
    }

    #[test]
    fn a_packet_with_no_time_never_supersedes_and_goes_when_a_timed_one_does() {
        let mut backlog = per_packet(1);
        backlog.hold(0, packet(10), Some(15 * SEC));
        backlog.hold(0, packet_at(None), Some(15 * SEC));
        backlog.hold(0, packet(20), Some(15 * SEC));
        assert_eq!(times(&backlog, 0), [Some(10 * SEC), None, Some(20 * SEC)]);
        backlog.hold(0, packet(30), Some(25 * SEC));
        assert_eq!(secs(&backlog, 0), [20, 30]);
    }

    #[test]
    fn take_returns_each_track_in_file_order_and_empties_the_backlog() {
        let mut backlog = per_packet(3);
        backlog.hold(2, packet(1), None);
        backlog.hold(0, packet(2), None);
        backlog.hold(2, packet(3), None);
        backlog.hold(0, packet(4), None);
        let order: Vec<(usize, Option<i64>)> = backlog.take().into_iter()
            .map(|(pos, held)| (pos, held.time_ns.map(|t| t / SEC))).collect();
        assert_eq!(order, [(0, Some(2)), (0, Some(4)), (2, Some(1)), (2, Some(3))]);
        assert!(backlog.is_empty());
        assert!(backlog.take().is_empty());
    }

    #[test]
    fn clear_forgets_everything_and_a_position_outside_the_plan_is_ignored() {
        let mut backlog = per_packet(1);
        backlog.hold(0, packet(1), None);
        backlog.hold(7, packet(2), None);
        assert!(!backlog.is_empty());
        backlog.clear();
        assert!(backlog.is_empty());
        assert!(backlog.take().is_empty());
    }

    #[test]
    fn a_held_packet_carries_its_bytes_and_the_decoder_padding() {
        let held = HeldPacket::new(Some(1), 1, 1, 0, 0, &[9, 8, 7]);
        assert_eq!(&held.bytes[..held.size], [9, 8, 7]);
        assert_eq!(held.bytes.len(), 3 + INPUT_PADDING);
        assert!(held.bytes[3..].iter().all(|&b| b == 0));
    }

    /// A PGS set that reaches the demuxer one segment per packet (MPEG-TS, M2TS): the set on screen
    /// at the playhead is kept from its PCS to its END, not from its last packet.
    #[test]
    fn a_pgs_set_on_screen_at_the_playhead_is_kept_whole_from_its_pcs() {
        let mut backlog = pgs();
        for sec in [200, 210, 240] {
            for packet in set(sec) { backlog.hold(0, packet, Some(232 * SEC)); }
        }
        assert_eq!(kinds(&backlog), [PCS, WDS, PDS, ODS, END, PCS, WDS, PDS, ODS, END],
            "the set shown at 232 s (stamped 210 s) whole, then the read-ahead");
        assert_eq!(secs(&backlog, 0), [210, 210, 210, 210, 210, 240, 240, 240, 240, 240]);
    }

    #[test]
    fn a_pgs_set_straddling_the_playhead_is_kept_from_its_pcs() {
        let mut backlog = pgs();
        for packet in set(200) { backlog.hold(0, packet, Some(231 * SEC)); }
        backlog.hold(0, segment(231, PCS), Some(232 * SEC));
        backlog.hold(0, segment(231, PDS), Some(232 * SEC));
        backlog.hold(0, segment(233, ODS), Some(232 * SEC));
        backlog.hold(0, segment(233, END), Some(232 * SEC));
        assert_eq!(kinds(&backlog), [PCS, PDS, ODS, END], "the 200 s set is superseded, the 231 s one is whole");
    }

    #[test]
    fn a_pgs_track_with_no_pcs_behind_the_playhead_retires_nothing() {
        let mut backlog = pgs();
        for kind in [ODS, END] { backlog.hold(0, segment(200, kind), Some(232 * SEC)); }
        assert_eq!(kinds(&backlog), [ODS, END], "the tail of a set whose start was never held is not a place to cut");
        for packet in set(210) { backlog.hold(0, packet, Some(232 * SEC)); }
        assert_eq!(kinds(&backlog), [PCS, WDS, PDS, ODS, END], "the next set's PCS is");
    }

    /// Matroska carries a display set per packet, so every PGS packet there opens with its PCS and
    /// the rule is the same one the other image codecs follow.
    #[test]
    fn a_pgs_set_in_one_packet_is_retired_like_any_other() {
        let mut backlog = pgs();
        for sec in [200, 210, 225, 228, 231, 235, 240] {
            backlog.hold(0, HeldPacket::new(Some(sec * SEC), sec, sec, 0, 0, &[PCS, 0, 1, 0, END]), Some(232 * SEC));
        }
        assert_eq!(secs(&backlog, 0), [231, 235, 240]);
    }

    /// VobSub and DVB carry a packet per set: a packet that happens to open with 0x16 is no PCS there.
    #[test]
    fn a_track_that_is_not_pgs_keeps_the_packet_at_the_playhead_not_a_segment_start() {
        let mut backlog = per_packet(1);
        for packet in set(210) { backlog.hold(0, packet, Some(232 * SEC)); }
        assert_eq!(kinds(&backlog), [END], "the last packet at or before the playhead, as before");
    }

    #[test]
    fn framing_follows_the_codec_name_ffmpeg_gives_a_track() {
        assert_eq!(SetFraming::of_codec("hdmv_pgs_subtitle"), SetFraming::PgsSegments);
        for codec in ["dvd_subtitle", "dvb_subtitle", "subrip", "ass"] {
            assert_eq!(SetFraming::of_codec(codec), SetFraming::PerPacket, "{codec}");
        }
    }
}
