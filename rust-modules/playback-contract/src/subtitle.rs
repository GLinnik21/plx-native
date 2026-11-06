//! Subtitle clock evidence shared by a media reader and presentation owner. These values
//! carry no demuxer handle, stream index, URL, or player registry.

/// How far ahead of the remux playhead a side reader may be before it parks.
pub const WINDOW_NS: i64 = 12_000_000_000;

/// Bytes from each end of a keyframe used to identify it across a remux.
const FINGERPRINT: usize = 32;

/// The first video keyframe in a remux: stream time and enough bytes to find the same packet
/// in the original Part. Size alone is insufficient because two keyframes may share a size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub pts_ns: i64,
    pub size: usize,
    pub head: [u8; FINGERPRINT],
    pub tail: [u8; FINGERPRINT],
}
impl Anchor {
    pub fn of(pts_ns: i64, data: &[u8]) -> Self {
        let (head, tail) = fingerprint(data);
        Self { pts_ns, size: data.len(), head, tail }
    }

    /// Size, head and tail must all agree. A short packet is zero-padded consistently.
    pub fn matches(&self, data: &[u8]) -> bool {
        let (head, tail) = fingerprint(data);
        data.len() == self.size && head == self.head && tail == self.tail
    }
}

fn fingerprint(data: &[u8]) -> ([u8; FINGERPRINT], [u8; FINGERPRINT]) {
    let mut head = [0u8; FINGERPRINT];
    let mut tail = [0u8; FINGERPRINT];
    let h = data.len().min(FINGERPRINT);
    head[..h].copy_from_slice(&data[..h]);
    tail[..h].copy_from_slice(&data[data.len() - h..]);
    (head, tail)
}

/// Remux playhead of a packet: fed PTS minus the feed shift plus display base.
pub fn playhead_of(anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    anchor_pts_ns.saturating_sub(pts_shift).saturating_add(disp_base)
}

/// Part time minus remux playhead from a byte-exact keyframe match.
pub fn delta_exact(part_kf_pts: i64, anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    part_kf_pts.saturating_sub(playhead_of(anchor_pts_ns, pts_shift, disp_base))
}

/// Fallback for embedded cues when no keyframe matched. Sidecar draw must use only exact delta.
pub fn delta_assumed(k_le: i64, anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    delta_exact(k_le, anchor_pts_ns, pts_shift, disp_base)
}

/// Whether a Part-time packet is beyond the side reader's lead window.
pub fn pacing_parks(pkt_part_ns: i64, delta_ns: i64, playpos_ns: i64) -> bool {
    pkt_part_ns.saturating_sub(delta_ns) > playpos_ns.saturating_add(WINDOW_NS)
}

/// Subtitle clock used to judge a cue while a seek's first displayed frame is pending.
pub fn judged_clock(playpos_ns: i64, anchor_playhead_ns: i64, offset_ns: i64) -> i64 {
    let now = playpos_ns.max(anchor_playhead_ns);
    now.min(now.saturating_sub(offset_ns))
}

/// One indexed subtitle rectangle in the stream's own canvas.
pub struct IndexedRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub index: Vec<u8>,
    pub palette: Box<[[u8; 4]; 256]>,
}
impl IndexedRect {
    pub fn bytes(&self) -> usize { self.index.capacity() + std::mem::size_of::<[[u8; 4]; 256]>() }
}

/// A decoded display set or a clear instruction; media assigns the stream clock and publishes it.
pub enum BitmapDisplay {
    Clear,
    Set { cw: i32, ch: i32, rects: Vec<IndexedRect>, total_rects: usize },
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
    pub data: std::sync::Arc<[u8]>,
}

/// File-order subtitle inventory copied while the format context is alive.
/// The codec name is used by the caller's text dialect policy; none of these fields is logged.
pub struct TrackInfo {
    pub stream_index: i32,
    pub codec_name: String,
    pub language: Option<String>,
}

/// The presentation owner's choice for one subtitle track. This keeps text dialect and charset
/// policy outside the codec while letting the codec open only image decoders.
pub enum DecodeKind {
    Text { max_bytes: usize },
    Ass { max_bytes: usize },
    Bitmap,
}

/// One decoded, owned observation. Raw text stays in its container encoding for the caller's
/// per-track charset step. Bitmap data has already passed its byte budget inside the decoder.
pub enum RawCue {
    Bytes { track: usize, start: i64, end: i64, payload: Vec<u8> },
    Bitmap { track: usize, pts: i64, display: crate::subtitle::BitmapDisplay },
}
/// Timing observed by a caption producer when it publishes a cue. An absent
/// playhead cannot justify retiring cues by time; store byte budgets still apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionRetention {
    pub playhead_ns: Option<i64>,
    pub seek_target_ns: Option<i64>,
}

/// The ordinal of a side-reader run that reads no subtitle and only measures the remux's clock
/// against the Part's (a sidecar is drawn by the app). Negative, so it is never a track position and
/// two runs of different roles never compare equal when a reader is asked to switch.
pub const SIDE_READER_CLOCK_ONLY: i32 = -1;

// ---- the side reader's job seam ---------------------------------------------------------------
//
// The side subtitle reader's thread is the Feed's (`plx_play_feed::side_reader`); what one pass of
// it does with the Part (open, scan, extract, present) is the common caption runner's. The types
// below are what the two exchange, so the runner names the contract and never the crate that runs
// the thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// What the reader thread asks of the common caption runner. Unlike
/// `plx_play_feed`'s `SideReaderPort`, which one Engine owns and drives from the main
/// thread, this is shared with the reader thread and with `sync`'s worker, neither of which has an
/// Engine, so it is a `'static` value handed to the reader's `start` and `sync` and kept by the
/// reader for its `stop`.
pub trait SideReaderJobPort: Sync {
    /// One pass of the transport and presentation loop over a codec-owned extraction job.
    #[allow(clippy::too_many_arguments)]
    fn read_once(
        &self,
        cfg: &ReadCfg,
        stop: &Arc<SideStop>,
        anchor: &Anchor,
        delta: &mut Option<i64>,
        resume_part_ns: &dyn Fn(i64) -> i64,
        playpos: &dyn Fn() -> i64,
        clock: &dyn Fn() -> (i64, i64),
        retention: &dyn Fn() -> CaptionRetention,
        on_exact_delta: &dyn Fn(i64),
    ) -> RunEnd;
    /// The viewer's subtitle selection (`-1` = off).
    fn desired_sub_idx(&self) -> i32;
    /// Select a subtitle track (the reader's own pick, or giving the viewer's back).
    fn request_subtitle(&self, ordinal: i32);
    /// Clear the bitmap cue store the reader filled.
    fn clear_bitmaps(&self);
}

/// The half-width of the keyframe scan around the start offset, both directions.
pub const SCAN_NS: i64 = 15_000_000_000;

/// What wakes a blocked read: called with the owner's lock held, from any thread.
type Waker = Box<dyn Fn() + Send + Sync>;

/// What the controlling thread uses to make a reader that is blocked in IO return: a flag every
/// callback checks, plus the two ways a read can block (a socket and a private curl transfer), each
/// registered as a waker by whoever owns that transport. Shared (`Arc`) between the caption
/// runner's transport, which registers its wakers here, and the reader thread (`stop`), which
/// fires them.
pub struct SideStop {
    flag: AtomicBool,
    /// The socket's waker. Written under the lock and fired under it by [`SideStop::trigger`], and
    /// the transport's drop clears it under the same lock BEFORE the stream is freed, so `trigger`
    /// can never wake a stream that is gone.
    sock: Mutex<Option<Waker>>,
    /// The private curl transfer's abort.
    curl: Mutex<Option<Waker>>,
}

impl SideStop {
    pub fn new() -> Arc<SideStop> {
        Arc::new(SideStop { flag: AtomicBool::new(false), sock: Mutex::new(None), curl: Mutex::new(None) })
    }

    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Set the flag, then wake whatever is blocked. Idempotent; callable from any thread.
    pub fn trigger(&self) {
        self.flag.store(true, Ordering::Release);
        if let Some(wake) = self.sock.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            wake();
        }
        if let Some(wake) = self.curl.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            wake();
        }
    }

    /// Register the private curl transfer's abort. Fires at once when the stop was set before the
    /// handle existed.
    pub fn publish_curl(&self, wake: impl Fn() + Send + Sync + 'static) {
        let mut slot = self.curl.lock().unwrap_or_else(|e| e.into_inner());
        let wake = slot.insert(Box::new(wake));
        if self.is_set() {
            wake();
        }
    }

    /// Register the socket's shutdown. Fires at once when the stop was set before the socket existed.
    pub fn publish_sock(&self, wake: impl Fn() + Send + Sync + 'static) {
        let mut slot = self.sock.lock().unwrap_or_else(|e| e.into_inner());
        let wake = slot.insert(Box::new(wake));
        if self.is_set() {
            wake();
        }
    }

    pub fn clear_sock(&self) {
        *self.sock.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// What one pass of the reader needs. The URL carries the token and is never logged.
pub struct ReadCfg<'a> {
    pub url: &'a str,
    /// Where the remux was started in the film, nanoseconds.
    pub offset_ns: i64,
    /// The 0-based position of the drawn track among the Part's subtitle streams.
    pub ordinal: usize,
    /// The Part's whole-file bitrate, for the log line.
    pub kbps: u32,
    /// Measure the clock mapping only: open the Part, scan for the anchor's keyframe and return
    /// ([`RunEnd::Measured`]). No subtitle track is opened, enumerated or read (`ordinal` is unused),
    /// no render store is touched, and the Part's connection is closed on return. For a SIDECAR the
    /// app fetches itself, drawn over a remux.
    pub clock_only: bool,
}

/// How a pass of the reader ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// The stop was set (or the Part ended and the reader idled until it was).
    Stopped,
    /// A transport or demux error: worth reopening.
    Failed,
    /// Retrying cannot help: the Part has no such track, no video stream, or the open was refused.
    Unusable,
    /// A [`ReadCfg::clock_only`] pass finished its scan (matched or not): nothing is left to read.
    Measured,
}

/// Everything one run of the reader is started with. `url` carries the token: held, never logged
/// (so `Debug` leaves it out).
#[derive(Clone, PartialEq, Eq)]
pub struct Spec {
    /// The fresh-session URL of the original Part (`direct_play_url(part, "<live>-subs")`).
    pub url: String,
    /// Where the remux was started in the film, nanoseconds.
    pub offset_ns: i64,
    /// The Part's whole-file bitrate, for the log line only.
    pub part_kbps: u32,
    /// The 0-based position of the drawn track among the Part's subtitle streams, or
    /// [`SIDE_READER_CLOCK_ONLY`].
    pub ordinal: i32,
    /// Start reading at the playhead instead of the run's start offset: the reader begins mid-play
    /// (a pick on a live remux), not with the engine run.
    pub from_playhead: bool,
    /// The Part-to-playhead clock delta a reader of this same run already measured, so a reader
    /// that replaces it (a track switch) does not scan for the anchor again.
    pub delta: Option<i64>,
}

impl std::fmt::Debug for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spec")
            .field("offset_ns", &self.offset_ns).field("part_kbps", &self.part_kbps)
            .field("ordinal", &self.ordinal).field("from_playhead", &self.from_playhead)
            .field("delta", &self.delta).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    /// The URL carries the token, and a `Spec` rides in a `FeedCommand` that derives `Debug`.
    #[test]
    fn a_spec_debug_never_shows_the_url() {
        let spec = Spec { url: "http://host/part?X-Plex-Token=SECRET".into(), offset_ns: 5, part_kbps: 7,
            ordinal: 2, from_playhead: false, delta: None };
        let shown = format!("{spec:?}");
        assert!(!shown.contains("SECRET") && !shown.contains("http"), "{shown}");
        assert!(shown.contains("ordinal: 2"), "{shown}");
        assert_eq!(spec.clone(), spec);
    }
}

#[cfg(test)]
mod side_stop_tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn counter() -> (Arc<AtomicU32>, impl Fn() + Send + Sync + 'static) {
        let n = Arc::new(AtomicU32::new(0));
        let n2 = Arc::clone(&n);
        (n, move || { n2.fetch_add(1, Ordering::SeqCst); })
    }

    #[test]
    fn a_trigger_after_both_wakers_were_published_fires_each_and_sets_the_flag() {
        let stop = SideStop::new();
        let ((sock, w_sock), (curl, w_curl)) = (counter(), counter());
        stop.publish_sock(w_sock);
        stop.publish_curl(w_curl);
        assert!(!stop.is_set());
        stop.trigger();
        assert!(stop.is_set());
        assert_eq!((sock.load(Ordering::SeqCst), curl.load(Ordering::SeqCst)), (1, 1));
    }

    #[test]
    fn a_waker_published_after_the_trigger_fires_at_once() {
        let stop = SideStop::new();
        stop.trigger();
        let ((sock, w_sock), (curl, w_curl)) = (counter(), counter());
        stop.publish_sock(w_sock);
        stop.publish_curl(w_curl);
        assert_eq!((sock.load(Ordering::SeqCst), curl.load(Ordering::SeqCst)), (1, 1));
    }

    #[test]
    fn a_cleared_socket_is_never_woken() {
        let stop = SideStop::new();
        let (sock, w_sock) = counter();
        stop.publish_sock(w_sock);
        stop.clear_sock();
        stop.trigger();
        assert_eq!(sock.load(Ordering::SeqCst), 0);
    }
}
