//! The media half of the **side subtitle reader**: the reader's thread, state and clock anchor are the
//! Feed's (`plx_play_feed::side_reader`); this module is what the common player contributes to them.
//! [`spec_for`] builds the reader's [`Spec`] from a route's [`crate::route::SideReaderTarget`] (route
//! and Plex facts), [`JOB`] is the common caption runner the thread calls back into (`subtitle_job`
//! runs the transport and presentation loop over a codec-owned extraction job, `desired_sub_idx` /
//! `request_subtitle` are the viewer's selection, `sub_bitmaps` the bitmap store), the route's
//! request to match the running reader to the selection is a readout event the pump hands the Feed
//! as a `FeedCommand::SyncSideReader`, and [`dev_armed_ordinal`] reads the diagnostic trigger
//! `plxnative-subside`.
//!
//! See `plx_play_feed::side_reader` for what the reader does, when it starts and stops, and how a
//! SIDECAR gets it clock-only (`plx_playback_contract::subtitle::SIDE_READER_CLOCK_ONLY`).

use plx_playback_contract::subtitle::{Anchor, CaptionRetention, ReadCfg, RunEnd, SideReaderJobPort, SideStop, Spec};
use std::sync::Arc;

/// The common caption runner the reader thread calls back into.
pub const JOB: &dyn SideReaderJobPort = &MediaSideReaderJob;

struct MediaSideReaderJob;

impl SideReaderJobPort for MediaSideReaderJob {
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
    ) -> RunEnd {
        crate::subtitle_job::read_once(cfg, stop, anchor, delta, resume_part_ns, playpos, clock, retention, on_exact_delta)
    }

    fn desired_sub_idx(&self) -> i32 { super::desired_sub_idx() }

    fn request_subtitle(&self, ordinal: i32) { super::request_subtitle(ordinal) }

    fn clear_bitmaps(&self) {
        super::SHARED.sub_bitmaps.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// The subtitle ordinal `plxnative-subside` asks to draw, or `None` when the trigger is absent or
/// holds no non-negative number. Read at each playback start (a live trigger: no relaunch needed).
/// Always `None` in a build without the dev triggers.
pub fn dev_armed_ordinal() -> Option<i32> {
    plx_base::devtrig::read("subside")?.trim().parse::<i32>().ok().filter(|n| *n >= 0)
}

/// The reader's [`Spec`] for a route's [`crate::route::SideReaderTarget`]: the fresh-session URL
/// shape PMS serves a second reader of the Part (the live session's own identifier is refused while
/// the remux runs). It carries the token and is handed to the reader, never logged. `None` when the
/// target's server is gone.
///
/// `offset_ns` is where the remux was started in the film (the feed's display base at the start): the
/// start path reads it from [`capture_offset_ns`], a reader begun mid-play from the live sample.
pub fn spec_for(target: &crate::route::SideReaderTarget, offset_ns: i64) -> Option<Spec> {
    let client = plx_plex::plex::client_for(target.sid)?;
    let su = client.direct_play_url(&target.part, &format!("{}-subs", target.session));
    Some(Spec {
        url: format!("{}{}", su.origin.base(), su.path),
        offset_ns,
        part_kbps: target.part_kbps,
        ordinal: target.ordinal,
        from_playhead: false,
        delta: None,
    })
}

/// Where the remux a start is about to open begins in the film, for the reader captured with it: the
/// display base the player last commanded the feed to open with. A restart's `Rebase` was sent after
/// the old session's reset idled the readout, and the attempt this start mints has not sampled yet,
/// so the feed's own `disp_base` is not in `raw` here; reading it from there would hand a resumed
/// remux's reader offset 0.
pub(super) fn capture_offset_ns(ps: &crate::route::PlaybackSession) -> i64 {
    ps.readout().open_disp_base_ns()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_play_feed::shared::FEED_SHARED;
    use plx_play_feed::side_reader::{active, failures, start, stop};
    use std::sync::atomic::Ordering;

    /// A listener that accepts connections and never answers: a stalled server. Held connections
    /// are dropped when the test ends.
    fn stalled_server() -> (std::net::TcpListener, u16) {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        (srv, port)
    }

    fn spec(port: u16) -> Spec {
        Spec { url: format!("http://127.0.0.1:{port}/library/parts/1/file.mkv"), offset_ns: 0, part_kbps: 0, ordinal: 0, from_playhead: false, delta: None }
    }

    /// `stop` returns and the reader is gone even when the server never answers a single byte,
    /// whether the reader is parked waiting for the anchor or blocked reading the open's headers.
    #[test]
    fn start_then_stop_joins_promptly_with_a_stalled_server() {
        let _g = plx_base::testlock::serial();
        plx_base::eventlog::with_private_log(|| {
            let (srv, port) = stalled_server();
            srv.set_nonblocking(true).unwrap();
            // 1. no anchor yet: the reader parks in its anchor wait
            *FEED_SHARED.side_anchor.lock().unwrap() = None;
            assert!(start(spec(port), JOB));
            assert!(active());
            assert!(FEED_SHARED.side_subs_owner.load(Ordering::Acquire));
            stop("test-no-anchor");
            assert!(!active());
            assert!(!FEED_SHARED.side_subs_owner.load(Ordering::Acquire));
            // 2. an anchor exists: the reader opens the Part and blocks in the stalled server
            *FEED_SHARED.side_anchor.lock().unwrap() = Some(Anchor::of(0, &[1, 2, 3]));
            assert!(start(spec(port), JOB));
            // wait until the reader has actually connected, so the stop lands on a blocked read
            let mut conn = None;
            while conn.is_none() {
                match plx_base::testnet::accept(&srv) {
                    Ok((s, _)) => conn = Some(s),
                    Err(_) => std::thread::yield_now(),
                }
            }
            stop("test-stalled");
            assert!(!active());
            assert_eq!(failures(), 0, "a stop is not a failure");
            drop(conn);
            let log = std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default();
            assert!(log.contains("subside: stop reason=test-stalled"), "{log}");
            for banned in ["http://", "127.0.0.1", "token", "Token", "/library"] {
                assert!(!log.contains(banned), "the log names {banned:?}: {log}");
            }
            *FEED_SHARED.side_anchor.lock().unwrap() = None;
        });
    }

    #[test]
    fn an_unarmed_trigger_asks_for_no_track() {
        plx_base::devtrig::with_private_triggers(|| {
            assert_eq!(dev_armed_ordinal(), None, "no file, no reader");
            std::fs::write(plx_base::devtrig::path("subside"), "2\n").unwrap();
            assert_eq!(dev_armed_ordinal(), Some(2));
            std::fs::write(plx_base::devtrig::path("subside"), "-1").unwrap();
            assert_eq!(dev_armed_ordinal(), None);
            std::fs::write(plx_base::devtrig::path("subside"), "").unwrap();
            assert_eq!(dev_armed_ordinal(), None, "an empty trigger names no track");
        });
    }
}
