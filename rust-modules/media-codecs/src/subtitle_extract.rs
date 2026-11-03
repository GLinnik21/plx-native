//! Cancellable, owned byte input for the optional subtitle demux. The caller owns the
//! transport and its cancellation wake; this crate owns only the FFmpeg AVIO callback state.
//! No URL, route, network client, or player registry is visible here.
use crate::ffi::{self, AVDictionary, AVFormatContext, AVIOContext, AVPacket, AVStream, AVERROR_EOF, AVERROR_IO};
use std::ffi::c_void;
use std::os::raw::c_int;
use std::sync::Arc;

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

/// The lower codec's owned, bounded container metadata. Each header slot corresponds to the
/// caller's track list in file order; `None` means that track had no admitted ASS header.
pub struct Manifest {
    pub headers: Vec<Option<Vec<u8>>>,
    pub fonts: Vec<EmbeddedFont>,
    pub font_bytes: usize,
}

pub struct EmbeddedFont {
    pub name: String,
    pub data: Arc<[u8]>,
}

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
