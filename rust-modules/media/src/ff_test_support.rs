//! Shared fixtures and helpers for the `ff` test modules split out below.

use plx_play_feed::shared::{FEED_SHARED, FEED_TX};
use super::*;

struct FixtureDemuxObserver;

impl DemuxObserver for FixtureDemuxObserver {
    fn attempt(&self) -> plx_playback_contract::AttemptId {
        plx_playback_contract::AttemptId::new(0, 0)
    }
    fn with_active(&self, f: &mut dyn FnMut()) -> bool { f(); true }
    fn playpos_ns(&self) -> Option<i64> { Some(FEED_SHARED.playpos_ns.load(Ordering::Relaxed)) }
    fn caption_retention(&self) -> Option<plx_playback_contract::subtitle::CaptionRetention> {
        let seeking = FEED_SHARED.seeking.load(Ordering::Acquire);
        let target = FEED_SHARED.seek_display_ns.load(Ordering::Relaxed);
        Some(plx_playback_contract::subtitle::CaptionRetention {
            playhead_ns: Some(FEED_SHARED.playpos_ns.load(Ordering::Relaxed)),
            seek_target_ns: (seeking && target >= 0).then_some(target),
        })
    }
    fn hold_epoch(&self) -> Option<u64> {
        Some(FEED_SHARED.hls_internal_hold_epoch.load(Ordering::Acquire))
    }
    fn rebuffering(&self) -> Option<bool> {
        Some(FEED_SHARED.hls_rebuffering.load(Ordering::Acquire))
    }
    fn audio_expected(&self) -> Option<bool> {
        Some(FEED_SHARED.hls_audio_expected.load(Ordering::Acquire))
    }
    fn duration_ns(&self) -> Option<i64> {
        Some(FEED_SHARED.duration_ns.load(Ordering::Relaxed))
    }
    fn declared_video_code(&self) -> Option<u8> {
        Some(FEED_SHARED.dg_load_v.load(Ordering::Relaxed))
    }
    fn declared_audio_code(&self) -> Option<u8> {
        Some(FEED_SHARED.dg_load_a.load(Ordering::Relaxed))
    }
    fn paused(&self) -> Option<bool> { Some(FEED_TX.paused.load(Ordering::Acquire)) }
    fn pause_clock(&self) -> Option<plx_playback_contract::demux::PauseClock> {
        let (at, elapsed) = FEED_TX.pause_clock_sample();
        Some(plx_playback_contract::demux::PauseClock { at, elapsed })
    }
    fn hls_buffer_ms(&self) -> Option<Option<i64>> {
        Some(hls_buffer_fact_at(self.hls_buffer_clocks()?, None).buffered_ms())
    }
    fn hls_buffer_clocks(&self) -> Option<plx_playback_contract::demux::HlsBufferClocks> {
        Some(plx_playback_contract::demux::HlsBufferClocks {
            playhead_ns: FEED_SHARED.playpos_ns.load(Ordering::Relaxed),
            video_tail_ns: FEED_SHARED.hls_video_tail_ns.load(Ordering::Acquire),
            audio_tail_ns: FEED_SHARED.hls_audio_tail_ns.load(Ordering::Acquire),
            display_base_ns: FEED_SHARED.disp_base.load(Ordering::Relaxed),
        })
    }
    fn remaining_playback_ms(&self) -> Option<i64> {
        let duration = FEED_SHARED.duration_ns.load(Ordering::Relaxed);
        Some(if duration <= 0 { 0 } else {
            (duration - FEED_SHARED.playpos_ns.load(Ordering::Relaxed).max(0)).max(0) / 1_000_000
        })
    }
    fn user_sequence(&self) -> Option<u64> { Some(FEED_SHARED.hls_user_sequence()) }
    fn publish(&self, effect: DemuxCallbackEffect) -> bool {
        match effect {
            DemuxCallbackEffect::NetworkBytes(bytes) => {
                FEED_SHARED.dg_net_rx.fetch_add(bytes, Ordering::Relaxed);
            }
            DemuxCallbackEffect::HttpStatus(status) =>
                FEED_SHARED.dg_http_status.store(status, Ordering::Relaxed),
            DemuxCallbackEffect::FileSize(size) =>
                FEED_SHARED.file_size.store(size, Ordering::Release),
            DemuxCallbackEffect::DurationNs(ns) =>
                FEED_SHARED.duration_ns.store(ns, Ordering::Relaxed),
            DemuxCallbackEffect::TransportFailure { rc, age_ms, reopen_failed } => {
                FEED_SHARED.dg_transport_rc.store(rc, Ordering::Release);
                FEED_SHARED.dg_transport_age_ms.store(age_ms, Ordering::Release);
                FEED_SHARED.dg_transport_reopen_failed.store(reopen_failed, Ordering::Release);
                FEED_SHARED.demux_io_failed.store(true, Ordering::Release);
            }
            DemuxCallbackEffect::IoFailed =>
                FEED_SHARED.demux_io_failed.store(true, Ordering::Release),
            DemuxCallbackEffect::RequestTerminalHold => {
                FEED_SHARED.hls_rebuffer_request_tail_ns.store(
                    FEED_SHARED.hls_video_tail_ns.load(Ordering::Acquire), Ordering::Release,
                );
                FEED_SHARED.hls_rebuffer_requested.store(true, Ordering::Release);
            }
        }
        true
    }
}

pub(super) fn demux_observer() -> Arc<dyn DemuxObserver> {
    Arc::new(FixtureDemuxObserver)
}

pub(super) fn with_demux_session<R>(run: impl FnOnce(Box<dyn DemuxSession>) -> R) -> R {
    let epoch = FEED_SHARED.begin_native_session().expect("fixture owns native epoch");
    struct Retire(u32);
    impl Drop for Retire {
        fn drop(&mut self) { FEED_SHARED.retire_native_session(self.0); }
    }
    let _retire = Retire(epoch);
    run(crate::player::demux_session::capture(
        plx_playback_contract::AttemptId::new(0, u64::from(epoch)), epoch,
    ))
}

/// Short, explicit silence budget for loopback AVIO fixtures. Production AVIO
/// captures its own 15-second budget when the worker is constructed.
pub(super) fn fixture_body_silence() -> std::time::Duration {
    std::time::Duration::from_millis(400)
}

// -- the AVIO callbacks under teardown -------------------------------------------------
//
// These drive `read_cb`/`seek_cb` directly, which works on the host precisely because neither
// one touches FFmpeg: they are plain `extern "C"` fns over an `AvioState`, whose every field
// (an HttpStream, an AuQueue, two CStrings, three integers) is ordinary Rust. So long as a
// test stays off `av_*`, the callbacks link and run here exactly as they do on the TV.

pub(crate) use crate::http_test_support::{serve_file, with_counting_listener};

/// A server that honours `Range` and cuts the body: the first `cuts` connections send `per` bytes
/// of what was asked for and then close, so each cut is a transfer that dies mid-body. `reset` ends a
/// cut connection with an RST (`SO_LINGER` 0) instead of a short close. Returns the port, the file
/// served and the `Range` start of every request in arrival order (`None` for a plain GET).
pub(super) fn cutting_server(
    n: usize,
    cuts: usize,
    per: usize,
    reset: bool,
) -> (u16, Vec<u8>, std::sync::Arc<std::sync::Mutex<Vec<Option<usize>>>>) {
    use std::io::{Read, Write};
    let file: Vec<u8> = (0..n).map(|i| (i * 7 + i / 251) as u8).collect();
    let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = srv.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (f2, s2) = (file.clone(), seen.clone());
    std::thread::spawn(move || {
        let mut conn = 0usize;
        for s in srv.incoming() {
            let Ok(mut s) = s else { return };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => buf.extend_from_slice(&tmp[..k]),
                }
            }
            let head = String::from_utf8_lossy(&buf).to_ascii_lowercase();
            let start = head.lines().find_map(|l| {
                l.strip_prefix("range: bytes=")
                    .and_then(|r| r.split('-').next().and_then(|a| a.trim().parse::<usize>().ok()))
            });
            let at = start.unwrap_or(0);
            let cut = conn < cuts;
            let send = if cut { per.min(n - at) } else { n - at };
            s2.lock().unwrap().push(start);
            conn += 1;
            let h = match start {
                Some(_) => format!(
                    "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {at}-{}/{n}\r\nContent-Length: {}\r\n\r\n",
                    n - 1,
                    n - at
                ),
                None => format!("HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Length: {n}\r\n\r\n"),
            };
            let _ = s.write_all(h.as_bytes());
            let _ = s.write_all(&f2[at..at + send]);
            let _ = s.flush();
            if cut {
                // Past the client's header phase, so the death is a BODY failure.
                std::thread::sleep(std::time::Duration::from_millis(200));
                if reset {
                    use std::os::fd::AsRawFd;
                    let l = libc::linger { l_onoff: 1, l_linger: 0 };
                    unsafe {
                        libc::setsockopt(
                            s.as_raw_fd(),
                            libc::SOL_SOCKET,
                            libc::SO_LINGER,
                            &l as *const _ as *const c_void,
                            std::mem::size_of::<libc::linger>() as libc::socklen_t,
                        );
                    }
                }
            }
            drop(s);
        }
    });
    (port, file, seen)
}

/// An open stream plus the aborted video lane behind its AVIO — the state teardown leaves:
/// `engine::teardown` aborts both lanes, `http_shutdown`s this socket, then joins the demuxer.
pub(super) fn opened_stream_with_aborted_lane(
    port: u16,
) -> (Box<HttpStream>, Box<AuQueue>, CString, CString) {
    let ip = CString::new("127.0.0.1").unwrap();
    let path = CString::new("/library/parts/1/file.mkv").unwrap();
    let mut hs = plx_net::stream::http_stream_boxed();
    let rv = plx_net::stream::http_open(
        &mut *hs,
        ip.as_ptr(),
        port as c_int,
        path.as_ptr(),
        std::ptr::null(),
        "GET",
    );
    assert_eq!(rv, 0, "fixture: the first open must succeed");
    let mut aq = plx_play_feed::aq::aq_new(1 << 20);
    plx_play_feed::aq::aq_abort(&mut *aq);
    (hs, aq, ip, path)
}

// -- the same two invariants, with libcurl under the AVIO instead of a socket ------------
//
// The guards above live in `read_cb`/`seek_cb`, ABOVE the dispatch, so they are transport
// independent by construction — which is exactly the kind of claim that stops being true the
// first time somebody moves a check into a branch. These pin it. The listener speaks plain
// HTTP and curl speaks plain HTTP, so no TLS is needed to grade the abort path; what a host
// cannot reach is a real handshake, which is why the PR carries a device recipe for aborting
// during DNS and during TLS instead of pretending these cover it.

/// libcurl bound and both tables live, with the crate-wide lock HELD for the caller's whole
/// test — `curlio`'s one-source registry is a process-global these two contend on with
/// `curlio`'s own suite, in another module, which is exactly what `testlock` is for. `None`
/// on a host with no libcurl at all, where these two would be grading nothing.
pub(super) fn curl_gate() -> Option<plx_base::testlock::Serial> {
    let g = plx_base::testlock::serial();
    if plx_net::net::global_init() && crate::curlio::available() {
        Some(g)
    } else {
        None
    }
}

/// The curl twin of `opened_stream_with_aborted_lane`: a live https-capable source over the
/// counting listener, plus the aborted video lane teardown leaves behind.
pub(super) fn opened_curl_with_aborted_lane(port: u16) -> (Box<crate::curlio::CurlSource>, Box<AuQueue>) {
    let cs = crate::curlio::CurlSource::open(&format!("http://127.0.0.1:{port}/f.mkv"), 0)
        .expect("fixture: the first open must succeed");
    let mut aq = plx_play_feed::aq::aq_new(1 << 20);
    plx_play_feed::aq::aq_abort(&mut *aq);
    (cs, aq)
}
