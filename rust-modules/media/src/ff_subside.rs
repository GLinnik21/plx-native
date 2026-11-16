//! The **side reader**'s transport and presentation runner: a second demux of the film's ORIGINAL Part, reading only
//! subtitle packets, while the player plays a server remux of that same film (PMS cannot put a
//! subtitle in a remux, so the app draws it). The lower codec job owns all FFmpeg objects and
//! returns owned raw or bitmap observations; this module owns transport, clock and delivery.
//!
//! Three pieces, from the bottom up:
//!
//! - [`SideIo`]: the AVIO source. The main demuxer's `read_cb` cannot be reused — it writes
//!   `FEED_SHARED`'s transfer diagnostics and `curlio` keeps ONE `ACTIVE` abort handle for the movie —
//!   so this one owns its transport (a plain socket or a private curl transfer), writes no
//!   diagnostic of the main stream, and has its own [`SideStop`], which every callback consults and
//!   which wakes a read blocked in the socket or in `curl_multi_wait`.
//! - the clock mapping ([`Anchor`], [`delta_exact`], [`delta_assumed`]): the remux and the Part
//!   disagree about time by a constant, because PMS snaps `offset` to a keyframe while the app
//!   labels that keyframe `offset`. An EMBEDDED track's cues are moved by `-delta`
//!   ([`SubCue::shifted`]); a SIDECAR (file time, fetched by the app) is not read here at all —
//!   a [`ReadCfg::clock_only`] pass measures the same `delta`, publishes it only when the keyframe
//!   matched byte for byte, and `player::sidecar_clock_ns` adds it to the sidecar's render clock.
//! - the pacing rule ([`pacing_parks`]): the reader stays
//!   [`plx_playback_contract::subtitle::WINDOW_NS`] ahead of the playhead.
//!
//! No URL, host, token or session identifier is ever logged from here.

use crate::player::subtitle_cue::{begin_sources, SideTracks, SubCue};
use plx_playback_contract::subtitle::CaptionRetention;
use plx_media_codecs::subtitle_extract::{Job, JobOpenError, ReadOutcome, ReadSeek};
use plx_net::stream::HttpStream;
use plx_playback_contract::subtitle::{ReadCfg, RunEnd, SideStop, SCAN_NS};
use std::os::raw::c_int;
use std::sync::Arc;

// Clock evidence is shared with presentation; this FFmpeg reader only supplies bytes and times.
pub(crate) use plx_playback_contract::subtitle::{Anchor, delta_exact, playhead_of};
use plx_playback_contract::subtitle::{delta_assumed, judged_clock, pacing_parks};
#[cfg(test)]
use plx_playback_contract::subtitle::WINDOW_NS;

/// How long it parks per decision.
pub(super) const PARK_MS: u64 = 250;

// ---- the clock mapping ----------------------------------------------------------------------

/// The last keyframe at or before `offset_ns`, from keyframe times in any order.
#[cfg(test)]
fn keyframe_at_or_before(keyframes: &[i64], offset_ns: i64) -> Option<i64> {
    keyframes.iter().copied().filter(|k| *k <= offset_ns).max()
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

/// The owned network source of the side reader. The codec job boxes it behind an AVIO callback
/// state whose address stays stable until libavformat closes.
pub(crate) struct SideIo {
    src: SideSrc,
    stop: Arc<SideStop>,
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
    match crate::curlio::CurlSource::open_private_with(url, at, &|a| {
        let abort = Arc::clone(a);
        stop.publish_curl(move || abort.signal());
    }) {
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
            // The wake owns the address, not the stream: `SideIo`'s drop clears it (under the
            // stop's lock) before the stream is freed, so the address is never woken once gone.
            let sock = &mut *hs as *mut plx_net::stream::HttpStream as usize;
            stop.publish_sock(move || plx_net::stream::http_shutdown(sock as *mut plx_net::stream::HttpStream));
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
        Ok(Box::new(SideIo { src, stop: Arc::clone(stop), size }))
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

}

enum Followed {
    Socket { origin: plx_plex::plex::Origin, path: String },
    Tls(String),
}

/// The transport half of the subtitle job. AVIO owns this boxed source and checks the same
/// cancellation flag around every call; `SideStop` wakes a blocked socket or curl read.
impl ReadSeek for SideIo {
    fn read(&mut self, dst: &mut [u8]) -> ReadOutcome {
        if self.stop.is_set() { return ReadOutcome::End; }
        if dst.is_empty() { return ReadOutcome::Bytes(0); }
        let n = c_int::try_from(dst.len()).unwrap_or(c_int::MAX);
        let r = match &mut self.src {
            SideSrc::Socket { hs, .. } => plx_net::stream::http_read(&mut **hs, dst.as_mut_ptr(), n),
            SideSrc::Curl(cs) => cs.read(dst),
        };
        if self.stop.is_set() { ReadOutcome::End }
        else if r < 0 { ReadOutcome::Failed }
        else if r == 0 { ReadOutcome::End }
        else { ReadOutcome::Bytes(r as usize) }
    }

    fn seek(&mut self, target: i64) -> bool {
        if self.stop.is_set() || target < 0 { return false; }
        let stop = Arc::clone(&self.stop);
        let mut hopped = None;
        let ok = match &mut self.src {
            SideSrc::Socket { hs, origin, path } => {
                match SideIo::follow(&mut **hs, origin, path, Some(target), &stop) {
                    Ok((Followed::Socket { origin: o, path: p }, _)) => {
                        *origin = o;
                        *path = p;
                        true
                    }
                    Ok((Followed::Tls(url), _)) => match open_curl_private(&url, target, &stop) {
                        Ok(cs) => { hopped = Some(cs); true }
                        Err(_) => false,
                    },
                    Err(_) => false,
                }
            }
            SideSrc::Curl(cs) => cs.seek(target),
        };
        if let Some(cs) = hopped {
            self.stop.clear_sock();
            self.src = SideSrc::Curl(cs);
        }
        ok && !self.stop.is_set()
    }

    fn size(&self) -> i64 { self.size }
    fn cancelled(&self) -> bool { self.stop.is_set() }
}

// ---- the reader -----------------------------------------------------------------------------

/// Open the optional extractor over the transport owned by this reader. The lower codec job
/// owns FFmpeg objects and yields only owned raw/bitmap observations; the common adapter installs
/// ASS sources and gives the job the presentation dialects and bounded copy limits.
fn open_side(cfg: &ReadCfg, stop: &Arc<SideStop>) -> Result<(Job, Option<SideTracks>, u64), RunEnd> {
    let io = match SideIo::open(cfg.url, stop) {
        Ok(io) => io,
        Err(SideOpenErr::Stopped) => return Err(RunEnd::Stopped),
        Err(SideOpenErr::Failed) => return Err(RunEnd::Failed),
    };
    // The main demuxer publishes the anchor only after the same bundled-FFmpeg ABI gate. Keep
    // the backstop here in case the gate was refused before this reader was admitted.
    if !plx_media_codecs::ffi::abi_ok() { return Err(RunEnd::Unusable); }
    let mut job = Job::open(io, !cfg.clock_only).map_err(|error| match error {
        JobOpenError::NoVideo => RunEnd::Unusable,
        JobOpenError::Failed if stop.is_set() => RunEnd::Stopped,
        JobOpenError::Failed => RunEnd::Failed,
    })?;
    if cfg.clock_only { return Ok((job, None, 0)); }
    let tracks = SideTracks::new(job.tracks());
    let count = tracks.len();
    job.configure(tracks.decode_plan());
    let manifest = job.manifest(crate::player::ass_source::MAX_HEADER_BYTES);
    let generation = begin_sources(cfg.url, tracks.kinds(), manifest);
    plx_base::eventlog::log(&format!("subside: open tracks={count} ordinal={} kbps={}", cfg.ordinal, cfg.kbps));
    if cfg.ordinal >= count { return Err(RunEnd::Unusable); }
    Ok((job, Some(tracks), generation))
}

/// The `subside: anchor …` line. Numbers and labels only.
pub(super) fn anchor_line(
    matched: Option<i64>,
    anchor_pts_ns: i64,
    disp_base: i64,
    pts_shift: i64,
    delta: i64,
    assumed: Option<i64>,
    scan_bytes: i64,
    scan_ms: u128,
) -> String {
    let ms = |ns: i64| ns / 1_000_000;
    format!(
        "subside: anchor match={} part_kf_ms={} remux_kf_ms={} disp_base_ms={} pts_shift_ms={} delta_ms={} assumed_delta_ms={} scan_bytes={scan_bytes} scan_ms={scan_ms}",
        if matched.is_some() { "exact" } else { "assumed" },
        matched.map_or_else(|| "none".to_owned(), |t| ms(t).to_string()),
        ms(anchor_pts_ns),
        ms(disp_base),
        ms(pts_shift),
        ms(delta),
        assumed.map_or_else(|| "none".to_owned(), |d| ms(d).to_string()),
    )
}

/// How far past the playhead the reader must have read before an image set that started at or
/// before it, and whose end it has not seen, is taken to be still on screen. A muxer interleaves
/// the subtitle stream with the video by a second or so, so the CLEAR of a set can sit slightly
/// behind a video packet already read.
const OPEN_SET_SLACK_NS: i64 = 2_000_000_000;

/// Hands the side reader's cues to the render stores.
///
/// The reader starts reading BEHIND the playhead (a keyframe scan, a reopen's lead, the start of a
/// pick on a live remux), so it delivers cues the viewer has already passed. Text and ASS cues
/// carry their end, so the stores' time lookup never selects one that ended; an image set carries
/// none — it stays open until the next set or CLEAR is read — and so, delivered at once, a stale set
/// is the active cue (and drawn) for as long as it takes the reader to reach its CLEAR, one flash per
/// set in the order they were read. So an image set that started at or before the judged clock is
/// held until its end is known: a later set or CLEAR at or before the clock retires it unseen, one
/// after the clock lets it through (it is the cue on screen now), and so does the reader's own read
/// position passing the clock by [`OPEN_SET_SLACK_NS`] with no end seen.
pub(super) struct Delivery {
    /// The held set, already shifted onto the remux clock.
    held: Option<SubCue<'static>>,
}

impl Delivery {
    pub(super) fn new() -> Self {
        Delivery { held: None }
    }

    /// Deliver one cue stamped in Part time. `clock_ns` is [`judged_clock`].
    pub(super) fn deliver(&mut self, cue: SubCue<'_>, delta_ns: i64, clock_ns: i64, generation: u64, retention: CaptionRetention) {
        match cue.shifted(-delta_ns) {
            SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects } => {
                self.settle(pts, clock_ns, generation, retention);
                let set = SubCue::BitmapSet { track, pts, cw, ch, rects, total_rects };
                if pts > clock_ns {
                    set.push(generation, retention);
                } else {
                    self.held = Some(set);
                }
            }
            SubCue::BitmapClear { track, pts } => {
                self.settle(pts, clock_ns, generation, retention);
                SubCue::BitmapClear { track, pts }.push(generation, retention);
            }
            other => other.push(generation, retention),
        }
    }

    /// The reader has read a packet whose remux-clock time is `read_ns`.
    pub(super) fn advance(&mut self, read_ns: i64, clock_ns: i64, generation: u64, retention: CaptionRetention) {
        if self.held.is_some() && read_ns >= clock_ns.saturating_add(OPEN_SET_SLACK_NS) {
            self.release(generation, retention);
        }
    }

    /// The Part ended: no later cue will say otherwise, so a held set is the one on screen.
    pub(super) fn flush(&mut self, generation: u64, retention: CaptionRetention) {
        self.release(generation, retention);
    }

    fn release(&mut self, generation: u64, retention: CaptionRetention) {
        if let Some(set) = self.held.take() {
            set.push(generation, retention);
        }
    }

    /// The held set's end is now known to be `end_ns`: it goes to the store only if that is after
    /// the clock (the store then closes it at the next push), else it is dropped unseen.
    fn settle(&mut self, end_ns: i64, clock_ns: i64, generation: u64, retention: CaptionRetention) {
        if end_ns > clock_ns {
            self.release(generation, retention);
        } else {
            self.held = None;
        }
    }
}

/// One pass of the side reader: open the Part, map its clock onto the remux's (the first pass only:
/// `delta` is kept across a reopen), then decode the drawn track's cues from `resume_part_ns()` on,
/// staying [`plx_playback_contract::subtitle::WINDOW_NS`] ahead of the playhead. Returns when stopped or on failure; a Part that
/// simply ends idles until the stop.
///
/// `playpos` is the remux playhead, `clock` its `(pts_shift, disp_base)`, and
/// `on_exact_delta` publishes only a byte-exact keyframe match to the caller. A
/// [`ReadCfg::clock_only`] pass returns [`RunEnd::Measured`] after the keyframe scan (or at once if
/// handed a delta) instead of decoding anything. Never panics on bad input: every FFmpeg value is
/// checked before use.
pub(crate) fn read_once(
    cfg: &ReadCfg,
    stop: &Arc<SideStop>,
    anchor: &Anchor,
    delta: &mut Option<i64>,
    resume_part_ns: &dyn Fn(i64) -> i64,
    playpos: &dyn Fn() -> i64,
    clock: &dyn Fn() -> (i64, i64),
    retention: &dyn Fn() -> CaptionRetention,
    on_exact_delta: &dyn Fn(i64),
) -> RunEnd {
    let (mut job, tracks, generation) = match open_side(cfg, stop) {
        Ok(v) => v,
        Err(end) => return end,
    };
    if delta.is_none() {
        let started = std::time::Instant::now();
        let scan = match job.scan_keyframes(cfg.offset_ns, SCAN_NS, || stop.is_set(), |bytes| anchor.matches(bytes)) {
            Some(scan) => scan,
            None => return if stop.is_set() { RunEnd::Stopped } else { RunEnd::Failed },
        };
        let (pts_shift, disp_base) = clock();
        let assumed = scan.k_le.map(|k| delta_assumed(k, anchor.pts_ns, pts_shift, disp_base));
        let chosen = match (scan.matched, assumed) {
            (Some(t), _) => delta_exact(t, anchor.pts_ns, pts_shift, disp_base),
            (None, Some(a)) => a,
            (None, None) => 0,
        };
        plx_base::eventlog::log(&anchor_line(
            scan.matched, anchor.pts_ns, disp_base, pts_shift, chosen, assumed,
            scan.bytes, started.elapsed().as_millis(),
        ));
        *delta = Some(chosen);
        if scan.matched.is_some() {
            // Only byte-exact matching gives the sidecar's render clock a correction.
            on_exact_delta(chosen);
        }
        if cfg.clock_only { return RunEnd::Measured; }
    }
    if cfg.clock_only { return RunEnd::Measured; }
    let delta_ns = delta.unwrap_or(0);
    if !job.seek_video(resume_part_ns(delta_ns)) {
        return if stop.is_set() { RunEnd::Stopped } else { RunEnd::Failed };
    }
    let tracks = tracks.expect("embedded extractor has tracks");
    let mut delivery = Delivery::new();
    let judged = || {
        let (pts_shift, disp_base) = clock();
        let anchor_playhead = playhead_of(anchor.pts_ns, pts_shift, disp_base);
        judged_clock(playpos(), anchor_playhead, crate::player::subtitle_offset_ns())
    };
    loop {
        if stop.is_set() { return RunEnd::Stopped; }
        let packet = match job.next(cfg.ordinal, crate::player::desired_sub_idx() >= 0, crate::player::SUB_BITMAP_BUDGET) {
            Ok(Some(packet)) => packet,
            Ok(None) => {
                if stop.is_set() { return RunEnd::Stopped; }
                delivery.flush(generation, retention());
                while !stop.is_set() {
                    std::thread::park_timeout(std::time::Duration::from_millis(PARK_MS));
                }
                return RunEnd::Stopped;
            }
            Err(()) => return if stop.is_set() { RunEnd::Stopped } else { RunEnd::Failed },
        };
        if let Some(raw) = packet.cue {
            delivery.deliver(tracks.decode(raw), delta_ns, judged(), generation, retention());
        }
        if let Some(t) = packet.time_ns {
            delivery.advance(t.saturating_sub(delta_ns), judged(), generation, retention());
            while pacing_parks(t, delta_ns, playpos()) {
                if stop.is_set() { return RunEnd::Stopped; }
                std::thread::park_timeout(std::time::Duration::from_millis(PARK_MS));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use plx_play_feed::shared::FEED_SHARED;
    use super::*;
    use crate::http_test_support::{serve_file, with_counting_listener};
    use crate::player::SHARED;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

    /// A one-pixel image set on `track` stamped `pts` (Part time).
    fn set(track: i32, pts: i64) -> SubCue<'static> {
        let rect = crate::player::SubRect { x: 0, y: 0, w: 1, h: 1, index: vec![1], palette: Box::new([[255; 4]; 256]) };
        SubCue::BitmapSet { track, pts, cw: 1920, ch: 1080, rects: vec![rect], total_rects: 1 }
    }

    fn clear(track: i32, pts: i64) -> SubCue<'static> {
        SubCue::BitmapClear { track, pts }
    }

    const S: i64 = 1_000_000_000;
    /// Part time minus remux playhead.
    const DELTA: i64 = 5 * S;

    fn retention(playhead_ns: i64) -> CaptionRetention {
        CaptionRetention { playhead_ns: Some(playhead_ns), seek_target_ns: None }
    }

    /// The key (display-clock start) of the set the renderer would draw at `playhead`.
    fn shown_at(playhead: i64) -> Option<i64> {
        crate::player::active_bitmap_key(playhead)
    }

    /// Feed what a scan from an earlier keyframe delivers, in file order: image sets that ended
    /// before the playhead (`PLAYHEAD`), then the one still current at it. `clock_at_start` is
    /// the clock the reader is given while the player's own has not advanced. At every step,
    /// whatever the renderer would draw at the playhead must not be a set that ended before it.
    fn replay_earlier_scan(playpos_at_start: i64, clock_at_start: i64) {
        const PLAYHEAD: i64 = 111 * S;
        let _g = plx_base::testlock::serial();
        SHARED.sub_bitmaps.lock().unwrap().clear();
        crate::player::request_subtitle(0);
        let mut d = Delivery::new();
        // Part-time cues: A 100..103 s and B 106..108 s end before the playhead (Part 116 s);
        // C starts at 110 s remux time, is current at 111 s, and is cleared at 113 s.
        let file: [(SubCue<'static>, i64); 6] = [
            (set(0, 100 * S), 100 * S),
            (clear(0, 103 * S), 103 * S),
            (set(0, 106 * S), 106 * S),
            (clear(0, 108 * S), 108 * S),
            (set(0, 115 * S), 115 * S),
            (clear(0, 118 * S), 118 * S),
        ];
        for (n, (cue, part_t)) in file.into_iter().enumerate() {
            let before = shown_at(playpos_at_start);
            assert_eq!(before, None, "step {n}: shown before the playhead moved");
            d.deliver(cue, DELTA, clock_at_start, 0, retention(playpos_at_start));
            d.advance(part_t - DELTA, clock_at_start, 0, retention(playpos_at_start));
            // the playhead lands; whatever is shown must be set C (key 110 s) or nothing
            let at = shown_at(PLAYHEAD);
            assert!(
                at.is_none() || at == Some(110 * S),
                "step {n}: a set that ended before the playhead is drawn: key {at:?}"
            );
        }
        crate::player::request_subtitle(-1);
        SHARED.sub_bitmaps.lock().unwrap().clear();
    }

    #[test]
    fn a_scan_from_an_earlier_keyframe_never_presents_a_set_that_ended_before_the_playhead() {
        // the player's clock has not advanced (start / seek reload): playpos 0, the anchor's
        // playhead is 111 s
        replay_earlier_scan(0, 111 * S);
    }

    #[test]
    fn a_pick_on_a_live_remux_and_a_seek_reload_judge_against_the_real_playhead() {
        // the player's clock is already at the playhead (a pick), or the anchor says where it
        // will be (a reload): the same scan, the same answer
        replay_earlier_scan(111 * S, 111 * S);
        assert_eq!(judged_clock(0, 111 * S, 0), 111 * S, "before the first frame: the anchor's playhead");
        assert_eq!(judged_clock(40 * S, 111 * S, 0), 111 * S, "a stale pre-seek position is behind it");
        assert_eq!(judged_clock(130 * S, 111 * S, 0), 130 * S, "the live playhead once it has advanced");
        assert_eq!(judged_clock(130 * S, 111 * S, 3 * S), 127 * S, "a delay keeps cues due that long");
    }

    #[test]
    fn the_set_on_screen_at_the_playhead_still_appears_once_its_end_is_known_or_unseen() {
        const PLAYHEAD: i64 = 111 * S;
        let _g = plx_base::testlock::serial();
        SHARED.sub_bitmaps.lock().unwrap().clear();
        crate::player::request_subtitle(0);
        let mut d = Delivery::new();
        // C: 110 s, started before the playhead and not over (cleared at 113 s)
        d.deliver(set(0, 110 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), None, "held while the reader is still behind the playhead");
        d.advance(110 * S, PLAYHEAD, 0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), None, "reading at 110 s says nothing of 111 s");
        d.advance(113 * S + 1, PLAYHEAD, 0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), Some(110 * S), "the reader is past the playhead and saw no end");
        d.deliver(clear(0, 113 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), Some(110 * S));
        assert_eq!(shown_at(113 * S), None, "closed at its clear");

        // a clear read after the playhead lets the held set through
        SHARED.sub_bitmaps.lock().unwrap().clear();
        let mut d = Delivery::new();
        d.deliver(set(0, 110 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        d.deliver(clear(0, 112 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), Some(110 * S));
        assert_eq!(shown_at(112 * S), None);

        // the Part ending with a set held: it is the one on screen
        SHARED.sub_bitmaps.lock().unwrap().clear();
        let mut d = Delivery::new();
        d.deliver(set(0, 110 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        d.flush(0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), Some(110 * S));

        // a set superseded by another that starts at or before the playhead is never stored
        SHARED.sub_bitmaps.lock().unwrap().clear();
        let mut d = Delivery::new();
        d.deliver(set(0, 108 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        d.deliver(set(0, 110 * S + DELTA), DELTA, PLAYHEAD, 0, retention(PLAYHEAD));
        d.flush(0, retention(PLAYHEAD));
        assert_eq!(shown_at(PLAYHEAD), Some(110 * S));
        assert_eq!(shown_at(109 * S), None, "the superseded set never reached the store");
        crate::player::request_subtitle(-1);
        SHARED.sub_bitmaps.lock().unwrap().clear();
    }

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

    /// **The counterexample the assumed mapping cannot survive.** Measured on one real file (GOP
    /// 2.002 s, first remux frame at 0.083 s): a remux asked for 917 s started on the Part keyframe
    /// at 914.914 s, although 916.916 s is the keyframe at or before the offset (0.084 s earlier).
    /// "The last keyframe at or before the offset" is therefore NOT what PMS starts on, and
    /// [`delta_assumed`] is 2.002 s wrong here; only the byte-exact match
    /// ([`Anchor::matches`] over the scanned keyframes) names the right one. Do not fit an epsilon
    /// to this one data point.
    #[test]
    fn sidecar_clock_at_917_the_assumed_delta_is_two_seconds_wrong_and_only_the_exact_match_is_right() {
        const MS: i64 = 1_000_000;
        let keyframes = [912_914 * MS, 914_914 * MS, 916_916 * MS, 918_918 * MS];
        let offset = 917_000 * MS;
        let first_frame = 83 * MS;
        let at_or_before = keyframe_at_or_before(&keyframes, offset).unwrap();
        assert_eq!(at_or_before, 916_916 * MS, "the keyframe at or before 917 s");
        let assumed = delta_assumed(at_or_before, first_frame, 0, offset);
        let exact = delta_exact(914_914 * MS, first_frame, 0, offset);
        assert_eq!(exact, -2_169 * MS, "the app clock led the content by 2.169 s");
        assert_eq!(assumed, -167 * MS);
        assert_eq!(assumed - exact, 2_002 * MS, "one GOP wrong");
    }

    #[test]
    fn the_anchor_line_is_numbers_and_labels_only() {
        let exact = anchor_line(Some(598_500_000_000), 0, 600_000_000_000, 0, -1_500_000_000, Some(-1_500_000_000), 123, 45);
        assert_eq!(
            exact,
            "subside: anchor match=exact part_kf_ms=598500 remux_kf_ms=0 disp_base_ms=600000 pts_shift_ms=0 \
             delta_ms=-1500 assumed_delta_ms=-1500 scan_bytes=123 scan_ms=45"
        );
        let assumed = anchor_line(None, 0, 0, 0, 0, None, 0, 1);
        assert!(assumed.starts_with("subside: anchor match=assumed part_kf_ms=none "), "{assumed}");
        for banned in ["http", "token", "Token", "/library", "session"] {
            assert!(!exact.contains(banned) && !assumed.contains(banned), "names {banned:?}");
        }
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

    /// Open a stream to the listener for the injected AVIO source.
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
            let mut buf = [0u8; 4];
            assert_eq!(io.read(&mut buf), ReadOutcome::Bytes(4), "fixture: reads work before the stop");
            stop.trigger();
            assert!(io.cancelled());
            assert_eq!(io.read(&mut buf), ReadOutcome::End);
            assert!(!io.seek(0));
            assert!(!io.seek(2));
            assert_eq!(
                accepts.load(Ordering::Acquire),
                1,
                "a seek after the stop opened a SECOND connection the stop cannot reach"
            );
            assert_eq!(io.size(), 8, "a size query is a field read");
        });
    }

    #[test]
    fn side_io_writes_no_shared_diagnostics() {
        with_counting_listener(|port, _, _| {
            let _g = plx_base::testlock::serial();
            FEED_SHARED.dg_net_rx.store(777, Ordering::Relaxed);
            FEED_SHARED.file_size.store(888, Ordering::Release);
            FEED_SHARED.dg_http_status.store(999, Ordering::Relaxed);
            let stop = SideStop::new();
            let mut io = open_side(port, &stop);
            let mut buf = [0u8; 8];
            assert_eq!(io.read(&mut buf), ReadOutcome::Bytes(8));
            assert!(io.seek(0), "a seek reopens with a Range");
            assert_eq!(io.read(&mut buf), ReadOutcome::Bytes(8));
            assert_eq!(FEED_SHARED.dg_net_rx.load(Ordering::Relaxed), 777, "side reads must not count as the movie's bytes");
            assert_eq!(FEED_SHARED.file_size.load(Ordering::Acquire), 888);
            assert_eq!(FEED_SHARED.dg_http_status.load(Ordering::Relaxed), 999);
            stop.trigger();
        });
    }

    /// **End to end through the bundled FFmpeg**: a generated 5 s MKV with one SRT cue at 1.0 s is
    /// side-read over a local HTTP listener with the clock `offset 2 s`, `disp_base 1 s`, so the
    /// assumed mapping gives `delta = +1 s` and the cue must land at 0.0 s in the store.
    ///
    /// Needs the host build of the bundled FFmpeg and a system `ffmpeg` to generate the fixture.
    /// `make check-ffmpeg` stages its libraries and runs this test.
    #[test]
    #[ignore = "needs the bundled host FFmpeg and a system ffmpeg; run make check-ffmpeg"]
    fn side_reader_draws_a_shifted_cue_from_a_generated_mkv() {
        let _g = plx_base::testlock::serial();
        let _ffmpeg = plx_media_codecs::host_test_support::bind_host_ffmpeg();
        let dir = std::env::temp_dir().join(format!("plx-subside-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("t.srt"), "1\n00:00:01,000 --> 00:00:02,500\nHello\n\n").unwrap();
        let made = std::process::Command::new("ffmpeg")
            .current_dir(&dir)
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=64x64:r=10:d=5", "-i", "t.srt"])
            .args(["-map", "0:v", "-map", "1:0", "-c:v", "mpeg4", "-g", "10", "-c:s", "srt", "out.mkv"])
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("SKIPPED: no system ffmpeg to generate the fixture");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let body = std::fs::read(dir.join("out.mkv")).unwrap();
        static STOP_SERVER: AtomicBool = AtomicBool::new(false);
        STOP_SERVER.store(false, Ordering::Release);
        let (port, server) = serve_file(body, &STOP_SERVER);

        crate::player::ass_source::reset();
        crate::player::request_subtitle(0);
        let exact_delta = Arc::new(AtomicI64::new(i64::MIN));
        let stop = SideStop::new();
        let url = format!("http://127.0.0.1:{port}/library/parts/1/out.mkv");
        let reader = {
            let stop = Arc::clone(&stop);
            let exact_delta = Arc::clone(&exact_delta);
            std::thread::spawn(move || {
                let cfg = ReadCfg { url: &url, offset_ns: 2_000_000_000, ordinal: 0, kbps: 0, clock_only: false };
                let anchor = Anchor::of(0, &[0]); // matches no real keyframe: the assumed mapping
                let mut delta = None;
                let end = read_once(
                    &cfg,
                    &stop,
                    &anchor,
                    &mut delta,
                    &|_| 0,
                    &|| 0,
                    &|| (0, 1_000_000_000),
                    &|| retention(0),
                    &|ns| exact_delta.store(ns, Ordering::Release),
                );
                (end, delta)
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let cue = loop {
            // The reader registered the file's text track and its cue went to libass's source.
            let held = crate::player::ass_source::selected(0).and_then(|source| match &source.content {
                crate::player::ass::Content::Embedded { events, .. } => events.first().map(|e| (e.start_ms, e.duration_ms)),
                _ => None,
            });
            if let Some(c) = held {
                break Some(c);
            }
            if std::time::Instant::now() > deadline || reader.is_finished() {
                break None;
            }
            std::thread::yield_now();
        };
        stop.trigger();
        let (end, delta) = reader.join().expect("the reader must not panic");
        assert_eq!(delta, Some(1_000_000_000), "assumed delta = keyframe at 2 s - (0 + disp_base 1 s)");
        assert_eq!(
            exact_delta.load(Ordering::Acquire),
            i64::MIN,
            "the assumed delta must never be published"
        );

        // A clock-only pass over the same Part (what a sidecar over this remux gets): it scans for
        // the anchor and reads no cue at all — no subtitle track is opened, so the ASS source store
        // stays as empty as `reset` left it — and returns `Measured` instead of decoding.
        crate::player::ass_source::reset();
        let stop2 = SideStop::new();
        let url2 = format!("http://127.0.0.1:{port}/library/parts/1/out.mkv");
        let mut clock_delta = None;
        let clock_end = read_once(
            &ReadCfg { url: &url2, offset_ns: 2_000_000_000, ordinal: 0, kbps: 0, clock_only: true },
            &stop2,
            &Anchor::of(0, &[0]),
            &mut clock_delta,
            &|_| 0,
            &|| 0,
            &|| (0, 1_000_000_000),
            &|| retention(0),
            &|ns| exact_delta.store(ns, Ordering::Release),
        );
        assert_eq!(clock_end, RunEnd::Measured, "a clock-only pass ends after its scan");
        assert_eq!(clock_delta, Some(1_000_000_000), "the same mapping the track reader measured");
        assert!(
            crate::player::ass_source::selected(0).is_none(),
            "a clock-only pass installs no ASS source: nothing of it is drawn"
        );
        assert_eq!(exact_delta.load(Ordering::Acquire), i64::MIN, "no byte-exact match here: nothing is published");

        STOP_SERVER.store(true, Ordering::Release);
        let _ = server.join();
        crate::player::request_subtitle(-1);
        crate::player::ass_source::reset();
        let _ = std::fs::remove_dir_all(&dir);
        let cue = cue.unwrap_or_else(|| panic!("no cue arrived; reader ended {end:?}, delta {delta:?}"));
        assert_eq!(delta, Some(1_000_000_000), "assumed delta = keyframe at 2 s - (0 + disp_base 1 s)");
        assert_eq!(cue, (0, 1_500), "1.0 s .. 2.5 s shifted by -delta: starts at 0 and lasts 1.5 s");
        assert_eq!(end, RunEnd::Stopped);
    }
}
