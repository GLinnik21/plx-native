//! Cancellable, owned byte input for the optional subtitle demux. The caller owns the
//! transport and its cancellation wake; this crate owns only the FFmpeg AVIO callback state.
//! No URL, route, network client, or player registry is visible here.
use crate::ffi::{self, AVCodecContext, AVDictionary, AVFormatContext, AVIOContext, AVPacket, AVStream, AVERROR_EOF, AVERROR_IO};
use std::ffi::c_void;
use std::os::raw::c_int;

const AVIO_BUFFER_BYTES: usize = 65_536;
const AVSEEK_SIZE: c_int = 0x10000;

/// Packet time in nanoseconds, preferring PTS and then DTS. Missing timestamps are zero
/// when a cue requires a concrete start.
pub unsafe fn pts_ns(pkt: *const AVPacket, st: *mut AVStream) -> i64 {
    pts_ns_opt(pkt, st).unwrap_or(0)
}

/// The same packet clock without inventing a time when neither timestamp exists.
pub unsafe fn pts_ns_opt(pkt: *const AVPacket, st: *mut AVStream) -> Option<i64> {
    let t = if (*pkt).pts != ffi::AV_NOPTS_VALUE { (*pkt).pts } else { (*pkt).dts };
    if t == ffi::AV_NOPTS_VALUE { None }
    else { Some(ffi::av_rescale_q(t, ffi::stream_time_base(st), ffi::NS_TB)) }
}

pub use plx_playback_contract::subtitle::{Manifest, EmbeddedFont};

/// A subtitle stream whose authored ASS header may be copied. Other stream kinds are omitted.
pub struct AssTrack {
    pub stream_index: c_int,
    pub authored: bool,
}

/// Read ASS headers and font attachments from the container, with admission BEFORE any payload
/// copy. The caller supplies the presentation layer's exact font budget policy.
///
/// # Safety
/// `fmt` and `streams` must name a live FFmpeg format and its full stream table. Every track
/// index must be a valid index into that table.
pub unsafe fn collect_manifest(
    fmt: *mut AVFormatContext,
    streams: *mut *mut AVStream,
    tracks: &[AssTrack],
    max_header_bytes: usize,
    mut admit_font: impl FnMut(usize) -> bool,
) -> Manifest {
    let mut headers = Vec::with_capacity(tracks.len());
    for track in tracks {
        if !track.authored {
            headers.push(None);
            continue;
        }
        let cp = ffi::stream_codecpar(*streams.add(track.stream_index as usize));
        let len = (*cp).extradata_size.max(0) as usize;
        if len > max_header_bytes {
            headers.push(None);
        } else if len == 0 || (*cp).extradata.is_null() {
            headers.push(Some(Vec::new()));
        } else {
            headers.push(Some(std::slice::from_raw_parts((*cp).extradata, len).to_vec()));
        }
    }
    let mut fonts = Vec::new();
    let mut font_bytes = 0usize;
    for i in 0..(*fmt).nb_streams {
        let st = *streams.add(i as usize);
        let cp = ffi::stream_codecpar(st);
        let len = (*cp).extradata_size.max(0) as usize;
        if (*cp).codec_type != ffi::AVMEDIA_TYPE_ATTACHMENT || len < 4 || (*cp).extradata.is_null() { continue; }
        let data = std::slice::from_raw_parts((*cp).extradata, len);
        if !matches!(&data[..4], b"\x00\x01\x00\x00" | b"OTTO" | b"ttcf" | b"true") { continue; }
        let dict = *((st as *const u8).add(ffi::OFF_STREAM_METADATA) as *const *const AVDictionary);
        let entry = if dict.is_null() { std::ptr::null_mut() }
            else { ffi::av_dict_get(dict, c"filename".as_ptr(), std::ptr::null(), 0) };
        let name = if !entry.is_null() && !(*entry).value.is_null() {
            std::ffi::CStr::from_ptr((*entry).value).to_string_lossy().into_owned()
        } else { format!("attachment-{i}.ttf") };
        if !admit_font(len) { continue; }
        font_bytes += len;
        fonts.push(EmbeddedFont { name, data: data.into() });
    }
    Manifest { headers, fonts, font_bytes }
}

/// Software image-subtitle decoders owned by this codec job, one slot per file-order subtitle
/// track. Text tracks have a null slot. Close before the format context that supplied params.
pub struct ImageDecoders {
    contexts: Vec<*mut AVCodecContext>,
}

impl ImageDecoders {
    pub fn empty() -> Self { Self { contexts: Vec::new() } }

    /// # Safety
    /// `streams` must be the live format's full stream table, and all indices must be valid.
    pub unsafe fn open(streams: *mut *mut AVStream, tracks: &[(c_int, bool)]) -> Self {
        let mut contexts = Vec::with_capacity(tracks.len());
        for (si, image) in tracks {
            if *image {
                contexts.push(crate::subtitle::open_sub_decoder(ffi::stream_codecpar(*streams.add(*si as usize))));
            } else {
                contexts.push(std::ptr::null_mut());
            }
        }
        Self { contexts }
    }

    pub fn context(&self, pos: usize) -> *mut AVCodecContext { self.contexts[pos] }

    /// # Safety
    /// `pkt` must be live for the call and belong to the decoder at `pos`.
    pub unsafe fn decode(&self, pos: usize, pkt: *mut AVPacket, budget: usize) -> Option<crate::subtitle::BitmapDisplay> {
        let dec = self.contexts[pos];
        if dec.is_null() { None } else { crate::subtitle::decode_bitmap(dec, pkt, budget) }
    }

    pub unsafe fn close(&mut self) {
        for dec in &mut self.contexts {
            if !dec.is_null() { ffi::avcodec_free_context(dec); }
        }
    }
}

impl Drop for ImageDecoders {
    fn drop(&mut self) {
        unsafe { self.close() }
    }
}

/// One read from the caller's owned source. Payload bytes are never retained by AVIO.
/// Zero bytes have the same meaning as `End` at the FFmpeg callback boundary.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadOutcome { Bytes(usize), End, Failed }

/// The source contract of a subtitle extraction job. The owner must arrange for `cancelled`
/// to become true and wake a blocked read or seek before it joins the job. `seek` is absolute;
/// the source may reopen its own transport, but must refuse that after cancellation.
pub trait ReadSeek {
    fn read(&mut self, dst: &mut [u8]) -> ReadOutcome;
    fn seek(&mut self, absolute: i64) -> bool;
    fn size(&self) -> i64;
    fn cancelled(&self) -> bool;
}

struct CallbackState { source: Box<dyn ReadSeek>, off: i64 }

/// Custom AVIO and its owned source. Drop it after `avformat_close_input`: libavformat
/// borrows `context()` and never frees a caller-supplied `pb`.
pub struct AvioInput {
    avio: *mut AVIOContext,
    state: *mut CallbackState,
}

impl AvioInput {
    pub fn new(source: Box<dyn ReadSeek>) -> Option<Self> {
        let state = Box::into_raw(Box::new(CallbackState { source, off: 0 }));
        // SAFETY: the allocated state outlives the AVIO context and its callbacks; every
        // failure branch frees the allocations in the reverse order.
        unsafe {
            let buffer = ffi::av_malloc(AVIO_BUFFER_BYTES) as *mut u8;
            if buffer.is_null() {
                drop(Box::from_raw(state));
                return None;
            }
            let avio = ffi::avio_alloc_context(
                buffer, AVIO_BUFFER_BYTES as c_int, 0, state as *mut c_void,
                Some(read_cb), None, Some(seek_cb),
            );
            if avio.is_null() {
                ffi::free_ptr(buffer as *mut c_void);
                drop(Box::from_raw(state));
                return None;
            }
            Some(Self { avio, state })
        }
    }

    pub fn context(&self) -> *mut AVIOContext { self.avio }
}

impl Drop for AvioInput {
    fn drop(&mut self) {
        // SAFETY: the AVIO callbacks cannot run after the owning format context was closed.
        // The caller's transport is dropped only after the AVIO allocation is freed.
        unsafe {
            ffi::free_avio(self.avio);
            drop(Box::from_raw(self.state));
        }
    }
}

extern "C" fn read_cb(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    let state = unsafe { &mut *(op as *mut CallbackState) };
    if state.source.cancelled() { return AVERROR_EOF; }
    if dst.is_null() || n <= 0 { return 0; }
    let result = state.source.read(unsafe { std::slice::from_raw_parts_mut(dst, n as usize) });
    if state.source.cancelled() { return AVERROR_EOF; }
    match result {
        ReadOutcome::Bytes(0) | ReadOutcome::End => AVERROR_EOF,
        ReadOutcome::Bytes(bytes) if bytes <= n as usize => {
            state.off = state.off.saturating_add(bytes as i64);
            bytes as c_int
        }
        ReadOutcome::Bytes(_) | ReadOutcome::Failed => AVERROR_IO,
    }
}

extern "C" fn seek_cb(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let state = unsafe { &mut *(op as *mut CallbackState) };
    if whence == AVSEEK_SIZE { return state.source.size(); }
    if state.source.cancelled() { return -1; }
    let target = match whence {
        libc::SEEK_SET => Some(offset),
        libc::SEEK_CUR => state.off.checked_add(offset),
        libc::SEEK_END => state.source.size().checked_add(offset),
        _ => None,
    };
    let Some(target) = target.filter(|t| *t >= 0) else { return -1 };
    if !state.source.seek(target) || state.source.cancelled() { return -1; }
    state.off = target;
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Source {
        cancelled: Rc<Cell<bool>>,
        fail: bool,
        end: bool,
        size: i64,
        seeks: Rc<Cell<usize>>,
    }

    impl ReadSeek for Source {
        fn read(&mut self, dst: &mut [u8]) -> ReadOutcome {
            if self.fail { return ReadOutcome::Failed; }
            if self.end { return ReadOutcome::End; }
            if dst.is_empty() { return ReadOutcome::End; }
            dst[0] = 7;
            ReadOutcome::Bytes(1)
        }
        fn seek(&mut self, _: i64) -> bool {
            self.seeks.set(self.seeks.get() + 1);
            true
        }
        fn size(&self) -> i64 { self.size }
        fn cancelled(&self) -> bool { self.cancelled.get() }
    }

    #[test]
    fn callback_codes_and_stop_gate_are_preserved() {
        let cancelled = Rc::new(Cell::new(false));
        let seeks = Rc::new(Cell::new(0));
        let source = Source { cancelled: cancelled.clone(), fail: false, end: false, size: 8, seeks: seeks.clone() };
        let mut state = CallbackState { source: Box::new(source), off: 0 };
        let op = &mut state as *mut CallbackState as *mut c_void;
        let mut dst = [0u8; 4];
        assert_eq!(read_cb(op, dst.as_mut_ptr(), 4), 1);
        assert_eq!(dst[0], 7);
        assert_eq!(seek_cb(op, 2, libc::SEEK_CUR), 3);
        assert_eq!(seek_cb(op, -1, libc::SEEK_END), 7);
        assert_eq!(seeks.get(), 2);
        assert_eq!(seek_cb(op, -1, libc::SEEK_SET), -1);
        assert_eq!(seek_cb(op, 0, 0x20000), -1);
        assert_eq!(seeks.get(), 2);
        cancelled.set(true);
        assert_eq!(read_cb(op, dst.as_mut_ptr(), 4), AVERROR_EOF);
        assert_eq!(seek_cb(op, 0, libc::SEEK_SET), -1);
        assert_eq!(seek_cb(op, 0, AVSEEK_SIZE), 8);
        assert_eq!(seeks.get(), 2, "stop cannot open a new transport");
    }

    #[test]
    fn failed_read_uses_io_error_and_end_uses_eof() {
        let cancelled = Rc::new(Cell::new(false));
        let mut state = CallbackState {
            source: Box::new(Source { cancelled, fail: true, end: false, size: -1, seeks: Rc::new(Cell::new(0)) }),
            off: 0,
        };
        let op = &mut state as *mut CallbackState as *mut c_void;
        let mut dst = [0u8; 1];
        assert_eq!(read_cb(op, dst.as_mut_ptr(), 1), AVERROR_IO);
        assert_eq!(read_cb(op, dst.as_mut_ptr(), 0), 0);
        assert_eq!(seek_cb(op, 0, AVSEEK_SIZE), -1);
        state.source = Box::new(Source {
            cancelled: Rc::new(Cell::new(false)), fail: false, end: true, size: 0,
            seeks: Rc::new(Cell::new(0)),
        });
        assert_eq!(read_cb(op, dst.as_mut_ptr(), 1), AVERROR_EOF);
    }
}

pub use plx_playback_contract::subtitle::{TrackInfo, DecodeKind, RawCue};

pub struct PacketObservation {
    pub time_ns: Option<i64>,
    pub cue: Option<RawCue>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JobOpenError { Failed, NoVideo }

/// One optional embedded subtitle extraction pass. Its only input is owned cancellable bytes;
/// no URL, network client, route, player store, or worker is reachable from this type.
pub struct Job {
    fmt: *mut AVFormatContext,
    pkt: *mut AVPacket,
    input: Option<AvioInput>,
    video_index: c_int,
    tracks: Vec<TrackInfo>,
    kinds: Vec<DecodeKind>,
    decoders: ImageDecoders,
}

impl Job {
    pub fn open(source: Box<dyn ReadSeek>, inspect_subtitles: bool) -> Result<Self, JobOpenError> {
        let input = AvioInput::new(source).ok_or(JobOpenError::Failed)?;
        let mut job = Self {
            fmt: std::ptr::null_mut(), pkt: std::ptr::null_mut(), input: Some(input),
            video_index: -1, tracks: Vec::new(), kinds: Vec::new(), decoders: ImageDecoders::empty(),
        };
        // SAFETY: the source and AVIO state outlive this format context; Drop reverses every
        // allocation and the format pointer is updated after open_input even on failure.
        unsafe {
            job.fmt = ffi::avformat_alloc_context();
            if job.fmt.is_null() { return Err(JobOpenError::Failed); }
            (*job.fmt).pb = job.input.as_ref().expect("AVIO input installed").context();
            let mut fmt = job.fmt;
            let r = ffi::avformat_open_input(&mut fmt, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
            job.fmt = fmt;
            if r < 0 || job.fmt.is_null() { return Err(JobOpenError::Failed); }
            job.pkt = ffi::av_packet_alloc();
            if job.pkt.is_null() { return Err(JobOpenError::Failed); }
            let streams = (*job.fmt).streams;
            for i in 0..(*job.fmt).nb_streams {
                let st = *streams.add(i as usize);
                let cp = ffi::stream_codecpar(st);
                if (*cp).codec_type == ffi::AVMEDIA_TYPE_VIDEO && job.video_index < 0 {
                    job.video_index = i as c_int;
                }
                if inspect_subtitles && (*cp).codec_type == ffi::AVMEDIA_TYPE_SUBTITLE {
                    let name = std::ffi::CStr::from_ptr(ffi::avcodec_get_name((*cp).codec_id))
                        .to_string_lossy().into_owned();
                    job.tracks.push(TrackInfo {
                        stream_index: i as c_int,
                        codec_name: name,
                        language: stream_language(st),
                    });
                }
            }
        }
        if job.video_index < 0 { return Err(JobOpenError::NoVideo); }
        Ok(job)
    }

    pub fn tracks(&self) -> &[TrackInfo] { &self.tracks }

    /// The caller configures dialects after seeing the inventory. Decoder contexts belong to
    /// this job and close before its format context.
    pub fn configure(&mut self, kinds: Vec<DecodeKind>) {
        assert_eq!(kinds.len(), self.tracks.len());
        let specs: Vec<_> = self.tracks.iter().zip(&kinds)
            .map(|(track, kind)| (track.stream_index, matches!(kind, DecodeKind::Bitmap)))
            .collect();
        self.decoders = unsafe { ImageDecoders::open((*self.fmt).streams, &specs) };
        self.kinds = kinds;
    }

    pub fn manifest(&self, max_header_bytes: usize, admit_font: impl FnMut(usize) -> bool) -> Manifest {
        let tracks: Vec<_> = self.tracks.iter().zip(&self.kinds).map(|(track, kind)| AssTrack {
            stream_index: track.stream_index, authored: matches!(kind, DecodeKind::Ass { .. }),
        }).collect();
        unsafe { collect_manifest(self.fmt, (*self.fmt).streams, &tracks, max_header_bytes, admit_font) }
    }

    /// Seek the video clock to a nonnegative time. The caller distinguishes cancellation from
    /// transport failure by its own stop token after a false return.
    pub fn seek_video(&mut self, from_ns: i64) -> bool {
        unsafe {
            let vst = *(*self.fmt).streams.add(self.video_index as usize);
            let ts = ffi::av_rescale_q(from_ns.max(0), ffi::NS_TB, ffi::stream_time_base(vst));
            ffi::av_seek_frame(self.fmt, self.video_index, ts, ffi::AVSEEK_FLAG_BACKWARD) >= 0
        }
    }

    /// Scan keyframes near the requested start and compare packet bytes through the caller's
    /// fingerprint policy. `None` means cancelled or a demux error; EOF returns the partial scan.
    pub fn scan_keyframes(
        &mut self, offset_ns: i64, window_ns: i64,
        cancelled: impl Fn() -> bool,
        matches_anchor: impl Fn(&[u8]) -> bool,
    ) -> Option<Scan> {
        if !self.seek_video(offset_ns.saturating_sub(window_ns)) { return None; }
        let mut scan = Scan { matched: None, k_le: None, bytes: 0 };
        unsafe {
            let vst = *(*self.fmt).streams.add(self.video_index as usize);
            loop {
                if cancelled() { return None; }
                let r = ffi::av_read_frame(self.fmt, self.pkt);
                if r < 0 { return (r == AVERROR_EOF && !cancelled()).then_some(scan); }
                let size = (*self.pkt).size.max(0);
                scan.bytes += i64::from(size);
                if (*self.pkt).stream_index == self.video_index && (*self.pkt).flags & ffi::AV_PKT_FLAG_KEY != 0 {
                    if let Some(t) = pts_ns_opt(self.pkt, vst) {
                        if t > offset_ns.saturating_add(window_ns) {
                            ffi::av_packet_unref(self.pkt);
                            return Some(scan);
                        }
                        if t <= offset_ns { scan.k_le = Some(scan.k_le.map_or(t, |k| k.max(t))); }
                        if !(*self.pkt).data.is_null() && matches_anchor(std::slice::from_raw_parts((*self.pkt).data, size as usize)) {
                            scan.matched = Some(t);
                            ffi::av_packet_unref(self.pkt);
                            return Some(scan);
                        }
                    }
                }
                ffi::av_packet_unref(self.pkt);
            }
        }
    }

    /// Read one interleaved packet. EOF is `Ok(None)`; a transport/demux failure is `Err(())`.
    /// All returned cue payloads are owned before the FFmpeg packet is released. Admission is
    /// checked before a raw text/ASS copy; bitmap budget is enforced in the decoder before pixels
    /// are copied. Non-subtitle packet times still advance the caller's pacing and held-set rule.
    pub fn next(&mut self, ordinal: usize, decode_image: bool, bitmap_budget: usize) -> Result<Option<PacketObservation>, ()> {
        unsafe {
            let r = ffi::av_read_frame(self.fmt, self.pkt);
            if r < 0 { return if r == AVERROR_EOF { Ok(None) } else { Err(()) }; }
            let si = (*self.pkt).stream_index;
            if si < 0 || si as u32 >= (*self.fmt).nb_streams {
                ffi::av_packet_unref(self.pkt);
                return Ok(Some(PacketObservation { time_ns: None, cue: None }));
            }
            let st = *(*self.fmt).streams.add(si as usize);
            let time_ns = pts_ns_opt(self.pkt, st);
            let cue = if self.tracks.get(ordinal).is_some_and(|track| track.stream_index == si) {
                match self.kinds.get(ordinal) {
                    Some(DecodeKind::Bitmap) if decode_image => self.decoders.decode(ordinal, self.pkt, bitmap_budget)
                        .map(|display| RawCue::Bitmap { track: ordinal, pts: pts_ns(self.pkt, st), display }),
                    Some(DecodeKind::Text { max_bytes }) | Some(DecodeKind::Ass { max_bytes }) => {
                        let sz = (*self.pkt).size.max(0) as usize;
                        if sz == 0 || sz > *max_bytes || (*self.pkt).data.is_null() { None }
                        else {
                            let raw = std::slice::from_raw_parts((*self.pkt).data, sz);
                            let start = pts_ns(self.pkt, st);
                            let dur = (*self.pkt).duration;
                            let end = if dur > 0 {
                                start + ffi::av_rescale_q(dur, ffi::stream_time_base(st), ffi::NS_TB)
                            } else { start + 4_000_000_000 };
                            Some(RawCue::Bytes { track: ordinal, start, end, payload: raw.to_vec() })
                        }
                    }
                    _ => None,
                }
            } else { None };
            ffi::av_packet_unref(self.pkt);
            Ok(Some(PacketObservation { time_ns, cue }))
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Decoders first, then packet, format, AVIO and finally the caller's transport.
        unsafe {
            self.decoders.close();
            if !self.pkt.is_null() { ffi::av_packet_free(&mut self.pkt); }
            if !self.fmt.is_null() { ffi::avformat_close_input(&mut self.fmt); }
        }
        drop(self.input.take());
    }
}

pub struct Scan {
    pub matched: Option<i64>,
    pub k_le: Option<i64>,
    pub bytes: i64,
}

unsafe fn stream_language(st: *mut AVStream) -> Option<String> {
    let dict = *((st as *const u8).add(ffi::OFF_STREAM_METADATA) as *const *const AVDictionary);
    if dict.is_null() { return None; }
    let entry = ffi::av_dict_get(dict, c"language".as_ptr(), std::ptr::null(), 0);
    if entry.is_null() || (*entry).value.is_null() { return None; }
    let language = std::ffi::CStr::from_ptr((*entry).value).to_string_lossy().trim().to_string();
    (!language.is_empty()).then_some(language)
}
