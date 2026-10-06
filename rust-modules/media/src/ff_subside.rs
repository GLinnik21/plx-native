//! The **side reader**'s FFmpeg half: a second demux of the film's ORIGINAL Part, reading only
//! subtitle packets, while the player plays a server remux of that same film (PMS cannot put a
//! subtitle in a remux, so the app draws it). A `#[path]` child of `ff.rs`, like `ff_subs.rs`,
//! whose [`SubTracks`] decode every packet this module reads.
//!
//! Three pieces, from the bottom up:
//!
//! - [`SideIo`]: the AVIO source. The main demuxer's `read_cb` cannot be reused — it writes
//!   `SHARED`'s transfer diagnostics and `curlio` keeps ONE `ACTIVE` abort handle for the movie —
//!   so this one owns its transport (a plain socket or a private curl transfer), writes no
//!   diagnostic of the main stream, and has its own [`SideStop`], which every callback consults and
//!   which wakes a read blocked in the socket or in `curl_multi_wait`.
//! - the clock mapping ([`Anchor`], [`delta_exact`], [`delta_assumed`]): the remux and the Part
//!   disagree about time by a constant, because PMS snaps `offset` to a keyframe while the app
//!   labels that keyframe `offset`; the cues are moved by `-delta` ([`SubCue::shifted`]).
//! - the pacing rule ([`pacing_parks`]): the reader stays [`WINDOW_NS`] ahead of the playhead.
//!
//! No URL, host, token or session identifier is ever logged from here.

use super::*;
use std::sync::{Arc, Mutex};

/// How far ahead of the playhead (Part time minus `delta`) the reader may be before it parks.
pub(super) const WINDOW_NS: i64 = 12_000_000_000;
/// How long it parks per decision.
pub(super) const PARK_MS: u64 = 250;
/// The half-width of the keyframe scan around the start offset, both directions.
pub(super) const SCAN_NS: i64 = 15_000_000_000;

/// **Pacing**: is this packet (stamped `pkt_part_ns` in the Part's own time) more than
/// [`WINDOW_NS`] ahead of the remux playhead `playpos_ns` once `delta_ns` maps one clock onto the
/// other? Pure, so the decision is graded without a thread.
pub(super) fn pacing_parks(pkt_part_ns: i64, delta_ns: i64, playpos_ns: i64) -> bool {
    pkt_part_ns.saturating_sub(delta_ns) > playpos_ns.saturating_add(WINDOW_NS)
}

// ---- the clock mapping ----------------------------------------------------------------------

/// How many bytes of the head and the tail of a packet identify it.
const FINGERPRINT: usize = 32;

/// The first video keyframe the MAIN demuxer read from the remux: its stream time and enough of its
/// payload to recognise the same picture in the Part. Size alone is not an identity (two keyframes
/// of one film can share a size), and the remux copies the video, so the bytes agree exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
    /// The packet's stream time in the remux's own clock, nanoseconds.
    pub pts_ns: i64,
    pub size: usize,
    pub head: [u8; FINGERPRINT],
    pub tail: [u8; FINGERPRINT],
}

impl Anchor {
    /// The anchor of a packet of stream time `pts_ns` holding `data`.
    pub(crate) fn of(pts_ns: i64, data: &[u8]) -> Anchor {
        let (head, tail) = fingerprint(data);
        Anchor { pts_ns, size: data.len(), head, tail }
    }

    /// Is `data` the same keyframe? Size, head AND tail must all agree.
    pub(crate) fn matches(&self, data: &[u8]) -> bool {
        let (head, tail) = fingerprint(data);
        data.len() == self.size && head == self.head && tail == self.tail
    }
}

/// The first and last [`FINGERPRINT`] bytes of `data`, zero-padded when it is shorter.
fn fingerprint(data: &[u8]) -> ([u8; FINGERPRINT], [u8; FINGERPRINT]) {
    let mut head = [0u8; FINGERPRINT];
    let mut tail = [0u8; FINGERPRINT];
    let h = data.len().min(FINGERPRINT);
    head[..h].copy_from_slice(&data[..h]);
    tail[..h].copy_from_slice(&data[data.len() - h..]);
    (head, tail)
}

/// The remux playhead that the packet at remux stream time `anchor_pts_ns` is shown at —
/// `fed − pts_shift + disp_base`, the formula `player::mod`'s position callback stores in
/// `playpos_ns`.
pub(super) fn playhead_of(anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    anchor_pts_ns.saturating_sub(pts_shift).saturating_add(disp_base)
}

/// `delta = part_time − playhead` from a keyframe the scan matched byte for byte.
pub(super) fn delta_exact(part_kf_pts: i64, anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    part_kf_pts.saturating_sub(playhead_of(anchor_pts_ns, pts_shift, disp_base))
}

/// The last keyframe at or before `offset_ns`, from keyframe times in any order.
pub(super) fn keyframe_at_or_before(keyframes: &[i64], offset_ns: i64) -> Option<i64> {
    keyframes.iter().copied().filter(|k| *k <= offset_ns).max()
}

/// `delta` when no keyframe matched: PMS starts a remux at the keyframe at or before `offset`, so
/// that keyframe is taken to be the remux's first, and the clock is labelled as it would have been.
pub(super) fn delta_assumed(k_le: i64, anchor_pts_ns: i64, pts_shift: i64, disp_base: i64) -> i64 {
    delta_exact(k_le, anchor_pts_ns, pts_shift, disp_base)
}

// ---- stopping -------------------------------------------------------------------------------

/// What the controlling thread uses to make a reader that is blocked in IO return: a flag every
/// callback checks, plus the two ways a read can block. Shared (`Arc`) between [`SideIo`], which
/// registers its transport here, and `player::subside`, which fires it.
pub(crate) struct SideStop {
    flag: AtomicBool,
    /// The socket's `HttpStream` address, or 0. Written under the lock and read under it by
    /// [`SideStop::trigger`], and [`SideIo`]'s drop clears it under the same lock BEFORE the stream
    /// is freed, so `trigger` can never shut down a stream that is gone.
    sock: Mutex<usize>,
    /// The private curl transfer's abort handle.
    curl: Mutex<Option<Arc<crate::curlio::Abort>>>,
}

impl SideStop {
    pub(crate) fn new() -> Arc<SideStop> {
        Arc::new(SideStop { flag: AtomicBool::new(false), sock: Mutex::new(0), curl: Mutex::new(None) })
    }

    pub(crate) fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Set the flag, then wake whatever is blocked. Idempotent; callable from any thread.
    pub(crate) fn trigger(&self) {
        self.flag.store(true, Ordering::Release);
        let sock = self.sock.lock().unwrap_or_else(|e| e.into_inner());
        if *sock != 0 {
            plx_net::stream::http_shutdown(*sock as *mut HttpStream);
        }
        drop(sock);
        if let Some(a) = self.curl.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            a.signal();
        }
    }

    fn publish_curl(&self, abort: &Arc<crate::curlio::Abort>) {
        *self.curl.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(abort));
        if self.is_set() {
            abort.signal(); // fired before the handle existed
        }
    }

    fn publish_sock(&self, hs: *mut HttpStream) {
        *self.sock.lock().unwrap_or_else(|e| e.into_inner()) = hs as usize;
        if self.is_set() {
            plx_net::stream::http_shutdown(hs);
        }
    }

    fn clear_sock(&self) {
        *self.sock.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    }
}

// ---- the AVIO source ------------------------------------------------------------------------

enum SideSrc {
    Socket { hs: Box<HttpStream>, origin: plx_plex::plex::Origin, path: String },
    Curl(Box<crate::curlio::CurlSource>),
}

/// Why a side open did not produce a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SideOpenErr {
    /// The stop was already set, or fired during the open.
    Stopped,
    Failed,
}

/// The AVIO state of the side reader. Its address is the callbacks' opaque pointer.
pub(crate) struct SideIo {
    src: SideSrc,
    stop: Arc<SideStop>,
    off: i64,
    size: i64,
}

impl Drop for SideIo {
    fn drop(&mut self) {
        if let SideSrc::Socket { hs, .. } = &mut self.src {
            self.stop.clear_sock();
            plx_net::stream::http_close(&mut **hs);
        }
    }
}

fn open_curl_private(url: &str, at: i64, stop: &Arc<SideStop>) -> Result<Box<crate::curlio::CurlSource>, SideOpenErr> {
    match crate::curlio::CurlSource::open_private_with(url, at, &|a| stop.publish_curl(a)) {
        Ok(cs) => Ok(cs),
        Err(crate::curlio::OpenErr::Aborted) => Err(SideOpenErr::Stopped),
        Err(_) if stop.is_set() => Err(SideOpenErr::Stopped),
        Err(_) => Err(SideOpenErr::Failed),
    }
}

impl SideIo {
    /// Open `url` from byte 0. `https` is a private curl transfer; `http` is an owned socket
    /// stream, with redirects followed and an https hop handed to curl.
    pub(crate) fn open(url: &str, stop: &Arc<SideStop>) -> Result<Box<SideIo>, SideOpenErr> {
        if stop.is_set() {
            return Err(SideOpenErr::Stopped);
        }
        let (origin, path) = plx_plex::plex::origin::split(url);
        let (src, size) = if origin.is_tls() {
            let cs = open_curl_private(url, 0, stop)?;
            let size = cs.size();
            (SideSrc::Curl(cs), size)
        } else {
            let mut hs = plx_net::stream::http_stream_boxed();
            stop.publish_sock(&mut *hs);
            let opened = Self::follow(&mut hs, &origin, path, None, stop);
            match opened {
                Ok((Followed::Socket { origin, path }, size)) => (SideSrc::Socket { hs, origin, path }, size),
                Ok((Followed::Tls(url), _)) => {
                    stop.clear_sock();
                    plx_net::stream::http_close(&mut *hs);
                    let cs = open_curl_private(&url, 0, stop)?;
                    let size = cs.size();
                    (SideSrc::Curl(cs), size)
                }
                Err(e) => {
                    stop.clear_sock();
                    plx_net::stream::http_close(&mut *hs);
                    return Err(e);
                }
            }
        };
        Ok(Box::new(SideIo { src, stop: Arc::clone(stop), off: 0, size }))
    }

    /// Redirect-following open of `path` on `origin` at `range_from`.
    fn follow(
        hs: &mut HttpStream,
        origin: &plx_plex::plex::Origin,
        path: &str,
        range_from: Option<i64>,
        stop: &SideStop,
    ) -> Result<(Followed, i64), SideOpenErr> {
        use plx_net::stream::redirect::Opened;
        plx_net::stream::http_close(hs);
        let req = plx_net::stream::redirect::Request {
            origin,
            path,
            credentials: None,
            range_from,
            deadline: None,
            same_origin_only: false,
            credential_gate: plx_plex::http::credential_transport_allowed,
        };
        match plx_net::stream::redirect::open_following(hs, &req, &mut plx_base::checkpoint::NoCheckpoint) {
            Ok(Opened::Socket(t)) => {
                let size = plx_net::stream::hs_content_length(hs);
                Ok((Followed::Socket { origin: t.origin, path: t.path }, size))
            }
            Ok(Opened::Tls(t)) => Ok((Followed::Tls(t.url()), -1)),
            Err(_) if stop.is_set() => Err(SideOpenErr::Stopped),
            Err(_) => Err(SideOpenErr::Failed),
        }
    }

    /// Is this source's stop set? (Test and lifecycle use.)
    pub(crate) fn stopped(&self) -> bool {
        self.stop.is_set()
    }
}

enum Followed {
    Socket { origin: plx_plex::plex::Origin, path: String },
    Tls(String),
}

/// `read_cb` for the side reader: bytes, `AVERROR_EOF` for a clean end or a stop, `AVERROR_IO` for a
/// transport failure. Touches NONE of `SHARED`'s transfer diagnostics.
pub(crate) extern "C" fn side_read_cb(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    // SAFETY: `op` is the `SideIo` the AVIO was allocated over, alive for the AVIO's life, and used
    // by the one thread that drives libavformat.
    let s = unsafe { &mut *(op as *mut SideIo) };
    if s.stop.is_set() {
        return AVERROR_EOF;
    }
    if dst.is_null() || n <= 0 {
        return 0;
    }
    let r = unsafe {
        match &mut s.src {
            SideSrc::Socket { hs, .. } => plx_net::stream::http_read(&mut **hs, dst as *mut c_uchar, n),
            SideSrc::Curl(cs) => cs.read(std::slice::from_raw_parts_mut(dst, n as usize)),
        }
    };
    if s.stop.is_set() {
        return AVERROR_EOF;
    }
    if r < 0 {
        return AVERROR_IO;
    }
    if r == 0 {
        return AVERROR_EOF;
    }
    s.off += r as i64;
    r
}

/// `seek_cb` for the side reader: answers `AVSEEK_SIZE` from a field, and refuses every other seek
/// once the stop is set (a seek is a new connection, which a teardown's one wake cannot reach).
pub(crate) extern "C" fn side_seek_cb(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    // SAFETY: as `side_read_cb`.
    let s = unsafe { &mut *(op as *mut SideIo) };
    if whence == AVSEEK_SIZE {
        return s.size;
    }
    if s.stop.is_set() {
        return -1;
    }
    let target = match whence {
        SEEK_SET => offset,
        SEEK_CUR => s.off + offset,
        SEEK_END => s.size + offset,
        _ => return -1,
    };
    if target < 0 {
        return -1;
    }
    let stop = Arc::clone(&s.stop);
    let mut hopped = None;
    let ok = match &mut s.src {
        SideSrc::Socket { hs, origin, path } => {
            match SideIo::follow(&mut **hs, origin, path, Some(target), &stop) {
                Ok((Followed::Socket { origin: o, path: p }, _)) => {
                    *origin = o;
                    *path = p;
                    true
                }
                Ok((Followed::Tls(url), _)) => match open_curl_private(&url, target, &stop) {
                    Ok(cs) => {
                        hopped = Some(cs);
                        true
                    }
                    Err(_) => false,
                },
                Err(_) => false,
            }
        }
        SideSrc::Curl(cs) => cs.seek(target),
    };
    if let Some(cs) = hopped {
        // the socket stream is finished with: unregister it before it is replaced
        s.stop.clear_sock();
        s.src = SideSrc::Curl(cs);
    }
    if !ok || s.stop.is_set() {
        return -1;
    }
    s.off = target;
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use super::super::test_support::*;

    fn pkt(len: usize, salt: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(7).wrapping_add(salt)).collect()
    }

    #[test]
    fn anchor_match_requires_size_head_tail() {
        let a = pkt(5000, 1);
        let anchor = Anchor::of(1_000, &a);
        assert!(anchor.matches(&a));
        // same size and head, different tail
        let mut b = a.clone();
        *b.last_mut().unwrap() ^= 0xff;
        assert!(!anchor.matches(&b), "a tail difference must refuse");
        // same size and tail, different head
        let mut c = a.clone();
        c[0] ^= 0xff;
        assert!(!anchor.matches(&c), "a head difference must refuse");
        // same head and tail, different size (a byte inserted in the middle)
        let mut d = a.clone();
        d.insert(2500, 0);
        assert!(!anchor.matches(&d), "a size difference must refuse");
        // a difference in the middle only is NOT detectable and is accepted: the identity is the
        // three facts, not a hash
        let mut e = a.clone();
        e[2500] ^= 0xff;
        assert!(anchor.matches(&e));
        // a packet shorter than the fingerprint still compares exactly
        let tiny = Anchor::of(0, &[1, 2, 3]);
        assert!(tiny.matches(&[1, 2, 3]));
        assert!(!tiny.matches(&[1, 2, 4]));
        assert!(!tiny.matches(&[1, 2, 3, 0]));
    }

    #[test]
    fn exact_delta_is_part_time_minus_playhead() {
        // the remux labelled its first keyframe with `offset` (600 s) via disp_base, pts_shift 0
        assert_eq!(delta_exact(598_500_000_000, 0, 0, 600_000_000_000), -1_500_000_000);
        assert_eq!(delta_exact(600_000_000_000, 0, 0, 600_000_000_000), 0);
        // the playhead formula is fed - pts_shift + disp_base
        assert_eq!(playhead_of(5_000, 2_000, 10_000), 13_000);
        assert_eq!(delta_exact(20_000, 5_000, 2_000, 10_000), 7_000);
    }

    #[test]
    fn assumed_delta_uses_keyframe_at_or_before_offset() {
        let kfs = [590_000_000_000, 598_500_000_000, 603_000_000_000, 612_000_000_000];
        let offset = 600_000_000_000;
        let k = keyframe_at_or_before(&kfs, offset).expect("a keyframe precedes the offset");
        assert_eq!(k, 598_500_000_000, "the keyframe AFTER the offset must not be chosen");
        assert_eq!(keyframe_at_or_before(&kfs, 598_500_000_000), Some(598_500_000_000), "at counts");
        assert_eq!(keyframe_at_or_before(&kfs, 1), None);
        assert_eq!(keyframe_at_or_before(&[], offset), None);
        // the remux's first keyframe is stamped 0 and labelled with disp_base = offset
        assert_eq!(delta_assumed(k, 0, 0, offset), -1_500_000_000);
    }

    #[test]
    fn pacing_parks_past_the_window() {
        let play = 100_000_000_000;
        // exactly at the window edge is not past it
        assert!(!pacing_parks(play + WINDOW_NS, 0, play));
        assert!(pacing_parks(play + WINDOW_NS + 1, 0, play));
        // delta moves the packet onto the playhead's clock first
        assert!(!pacing_parks(play + WINDOW_NS + 5_000_000_000, 5_000_000_000, play));
        assert!(pacing_parks(play + WINDOW_NS + 5_000_000_000, 4_000_000_000, play));
        // a packet behind the playhead never parks
        assert!(!pacing_parks(0, 0, play));
        assert!(!pacing_parks(i64::MIN, 1, play), "extreme times saturate rather than wrap");
    }

    /// Open a stream to the listener and return the callbacks' opaque pointer pieces.
    fn open_side(port: u16, stop: &Arc<SideStop>) -> Box<SideIo> {
        SideIo::open(&format!("http://127.0.0.1:{port}/library/parts/1/file.mkv"), stop)
            .unwrap_or_else(|e| panic!("fixture: the side open must succeed: {e:?}"))
    }

    #[test]
    fn stop_makes_read_and_seek_fail_closed() {
        with_counting_listener(|port, accepts, _| {
            let stop = SideStop::new();
            let mut io = open_side(port, &stop);
            assert_eq!(accepts.load(Ordering::Acquire), 1, "fixture: one connection so far");
            let op = &mut *io as *mut SideIo as *mut c_void;
            let mut buf = [0u8; 4];
            assert_eq!(side_read_cb(op, buf.as_mut_ptr(), 4), 4, "fixture: reads work before the stop");
            stop.trigger();
            assert!(io.stopped());
            assert_eq!(side_read_cb(op, buf.as_mut_ptr(), 4), AVERROR_EOF);
            assert_eq!(side_seek_cb(op, 0, SEEK_SET), -1);
            assert_eq!(side_seek_cb(op, 2, SEEK_CUR), -1);
            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "a seek after the stop opened a SECOND connection the stop cannot reach"
            );
            assert_eq!(side_seek_cb(op, 0, AVSEEK_SIZE), 8, "a size query is a field read");
        });
    }

    #[test]
    fn side_io_writes_no_shared_diagnostics() {
        with_counting_listener(|port, _, _| {
            let _g = plx_base::testlock::serial();
            SHARED.dg_net_rx.store(777, Ordering::Relaxed);
            SHARED.file_size.store(888, Ordering::Release);
            SHARED.dg_http_status.store(999, Ordering::Relaxed);
            let stop = SideStop::new();
            let mut io = open_side(port, &stop);
            let op = &mut *io as *mut SideIo as *mut c_void;
            let mut buf = [0u8; 8];
            assert_eq!(side_read_cb(op, buf.as_mut_ptr(), 8), 8);
            assert_eq!(side_seek_cb(op, 0, SEEK_SET), 0, "a seek reopens with a Range");
            assert_eq!(side_read_cb(op, buf.as_mut_ptr(), 8), 8);
            assert_eq!(SHARED.dg_net_rx.load(Ordering::Relaxed), 777, "side reads must not count as the movie's bytes");
            assert_eq!(SHARED.file_size.load(Ordering::Acquire), 888);
            assert_eq!(SHARED.dg_http_status.load(Ordering::Relaxed), 999);
            stop.trigger();
        });
    }
}
