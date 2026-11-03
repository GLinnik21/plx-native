//! Cancellable, owned byte input for the optional subtitle demux. The caller owns the
//! transport and its cancellation wake; this crate owns only the FFmpeg AVIO callback state.
//! No URL, route, network client, or player registry is visible here.
use crate::ffi::{self, AVIOContext, AVERROR_EOF, AVERROR_IO};
use std::ffi::c_void;
use std::os::raw::c_int;

const AVIO_BUFFER_BYTES: usize = 65_536;
const AVSEEK_SIZE: c_int = 0x10000;

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
