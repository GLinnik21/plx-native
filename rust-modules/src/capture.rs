//! Dev/testing live UI capture stream: the app's own GLES frames, GPU-downscaled and
//! encoded to ONE client per wire mode (`tools/stream-screen.py --source app`).
//! Two hello-selected modes: **MPEG1-in-MPEG-TS** (raw TS, encoder in ff.rs's venc
//! section) and **JPEG** (`PXFR`-framed, NEON libjpeg-turbo). Both run ~30fps at
//! 960x540 — the MIN_GAP_MS cadence cap, with the UI still at 60fps — vs the ~3fps
//! external capture service. UI PLANE ONLY either way (the hardware video overlay is
//! invisible to glReadPixels; watching real playback still needs the service path).
//! GL work lives in `gfx::cap_cycle`; this module owns the trigger, the threads, the
//! single-slot mailbox, and the wire protocol.
//!
//! Enabled by `/tmp/plxnative-capture` (content: optional port, default 8910), read
//! once at boot like every dev trigger — and excluded from `automated_boot`'s DIAG
//! list (app.rs) so attaching a live view to an interactive session doesn't suppress
//! the who's-watching picker. With no client connected, `tick()` is one relaxed
//! atomic load; GL resources are allocated lazily on the first captured frame.
//!
//! Threading (the posters.rs pattern: statics + Condvar + join on shutdown):
//! - main/GL thread: `tick()` — capture into the mailbox (newest-wins, never blocks;
//!   a still-occupied slot or a <33ms gap just skips the frame, which self-paces
//!   capture to encoder throughput).
//! - `caplisten`: accept loop. Hello: `PXRQ w:u16 h:u16` (8B LE, legacy jpeg; 0,0 =
//!   default 480x270, quantized to 480x270/960x540 by width) or `PXR2 w h kind:u8
//!   rate:u8 pad:u16` (12B; kind 1 = MPEG1-in-TS at rate*100kbps, raw unframed TS on
//!   the socket — self-syncing 188B packets, encoder in ff.rs's venc section). TWO
//!   slots keyed by kind, LAST-WINS per slot: a new client displaces the old (after
//!   standby the old socket is a half-open zombie that would otherwise lock the real
//!   client out). The listener only `shutdown()`s the displaced fd — capenc is the
//!   SOLE closer of client fds, so a send in flight can never race a close and write
//!   into a recycled fd (a live PMS socket!). An mpeg client forces the 960x540 chain.
//! - `capenc`: waits on the mailbox, encodes JPEG q70 for the jpeg slot, sends one
//!   `PXFR` frame; MPEG1-encodes + TS-muxes the same frame for the mpeg slot.
//!   Encode is the NDK's NEON libjpeg-turbo (`libturbojpeg.so.0`, deployed next to
//!   the binary by `make deploy` and dlopen'd here) — tjCompress2 eats the RGBA
//!   buffer directly and TJFLAG_BOTTOMUP swallows the bottom-up parity, so there is
//!   no CPU strip/flip pass at all. If the .so is missing the slow pure-Rust
//!   encoder (RGBA->RGB strip + `image` crate) takes over — the stream still works,
//!   just at the old ~6fps@960. On a 5s idle timeout it resends the last frame
//!   (seq unchanged) as a keepalive so the host's deadness timer doesn't trip while
//!   the player route (no UI swaps) is up. MSG_NOSIGNAL on every send — SIGPIPE
//!   would kill the app. A stats line (fps, encode/read/cycle ms, KB/frame) logs
//!   every ~5s while frames flow.
//!
//! Wire formats, TV -> client (no client->server traffic after the hello):
//! - jpeg slot, LE: `"PXFR" | payload_len:u32 | seq:u32 | ticks_ms:u32 | <JPEG>`; a
//!   client resyncs by scanning for the magic.
//! - mpeg slot: raw MPEG-TS, no framing at all (188-byte packets are self-syncing on
//!   0x47, which is what jsmpeg's demuxer expects).

use libc::{c_char, c_int, c_uchar, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::log;

const TRIGGER: &str = "/tmp/plxnative-capture";
const DEFAULT_PORT: u16 = 8910;
const MIN_GAP_MS: u32 = 33; // ~30fps capture cadence cap

pub(crate) struct Frame {
    seq: u32,
    ticks: u32,
    w: c_int,
    h: c_int,
    flip: bool, // rows are bottom-up; encoder walks them in reverse
    rgba: Vec<u8>,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);
// TWO client slots, keyed by the hello's kind: legacy PXFR/JPEG and raw-TS MPEG1
// (PXR2 kind=1). Last-wins displacement PER SLOT; capenc is the sole closer of both.
static FD_JPEG: AtomicI32 = AtomicI32::new(-1); // current jpeg client (-1 = none)
static FD_MPEG: AtomicI32 = AtomicI32::new(-1); // current mpeg/ts client (-1 = none)
static LISTEN_FD: AtomicI32 = AtomicI32::new(-1);
static JPEG_960: AtomicBool = AtomicBool::new(false); // the jpeg client's requested size
static MPEG_RATE: AtomicU32 = AtomicU32::new(2_500_000); // bps, from the PXR2 hello
static MPEG_960: AtomicBool = AtomicBool::new(true);     // mpeg client's requested size
static MPEG_OFF: AtomicBool = AtomicBool::new(false); // latched on venc bring-up failure
static SEQ: AtomicU32 = AtomicU32::new(0);
static LAST_MS: AtomicU32 = AtomicU32::new(0);

static MAILBOX: Mutex<Option<Frame>> = Mutex::new(None);
static CV: Condvar = Condvar::new();
static POOL: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new()); // retired RGBA buffers (2-3 circulate)
static HANDLES: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

// whole cap_cycle main-thread cost (copy+downscale submission + readback), folded
// into capenc's periodic stats line together with gfx::CAP_READ_US
static CYC_US: AtomicU32 = AtomicU32::new(0);
static CYC_N: AtomicU32 = AtomicU32::new(0);

/// Boot init (main thread, next to `posters_init`): no-op unless the trigger file exists.
pub(crate) fn init() {
    let Ok(content) = std::fs::read_to_string(TRIGGER) else {
        return;
    };
    let port: u16 = content.trim().parse().unwrap_or(DEFAULT_PORT);
    let mut hs = HANDLES.lock().unwrap();
    hs.push(std::thread::spawn(move || caplisten(port)));
    hs.push(std::thread::spawn(capenc));
    ENABLED.store(true, Ordering::Release);
    log(&format!("capture: enabled, listening on :{port}"));
}

/// App-exit teardown (main thread, next to `posters_shutdown`). `shutdown(2)` — not
/// `close` — wakes a blocked `accept()`/`send()` on Linux; each thread then closes
/// its own fds and exits, and we join.
pub(crate) fn shutdown() {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    QUIT.store(true, Ordering::Release);
    CV.notify_all();
    let lf = LISTEN_FD.load(Ordering::Acquire);
    if lf >= 0 {
        unsafe { libc::shutdown(lf, libc::SHUT_RDWR) };
    }
    for slot in [&FD_JPEG, &FD_MPEG] {
        let cf = slot.load(Ordering::Acquire);
        if cf >= 0 {
            unsafe { libc::shutdown(cf, libc::SHUT_RDWR) }; // break a parked send
        }
    }
    for h in HANDLES.lock().unwrap().drain(..) {
        let _ = h.join();
    }
}

/// Per-frame hook (GL thread, after the last UI draw, before the swap). One atomic
/// load when idle; captures at most every MIN_GAP_MS and only when the encoder has
/// consumed the previous frame (mailbox empty) — never blocks, never queues.
pub(crate) fn tick(now: u32) {
    if !ENABLED.load(Ordering::Relaxed)
        || (FD_JPEG.load(Ordering::Relaxed) < 0 && FD_MPEG.load(Ordering::Relaxed) < 0)
    {
        return;
    }
    if now.wrapping_sub(LAST_MS.load(Ordering::Relaxed)) < MIN_GAP_MS {
        return;
    }
    {
        // encoder still holds / hasn't taken the previous frame -> skip (self-pacing)
        let Ok(slot) = MAILBOX.try_lock() else { return };
        if slot.is_some() {
            return;
        }
    }
    let mut buf = POOL.lock().unwrap().pop().unwrap_or_default();
    let t0 = Instant::now();
    // One shared GL downscale chain serves both slots, so the output size is the max
    // any CONNECTED slot needs — derived per frame, never a latched hello (a departed
    // client must not keep pinning the size). The mpeg encoder is fixed at 960x540.
    let want_960 = (FD_MPEG.load(Ordering::Relaxed) >= 0 && MPEG_960.load(Ordering::Relaxed))
        || (FD_JPEG.load(Ordering::Relaxed) >= 0 && JPEG_960.load(Ordering::Relaxed));
    match crate::gfx::cap_cycle(want_960, &mut buf) {
        Some((w, h, flip)) => {
            let f = Frame { seq: SEQ.fetch_add(1, Ordering::Relaxed), ticks: now, w, h, flip, rgba: buf };
            if let Ok(mut slot) = MAILBOX.try_lock() {
                if let Some(old) = slot.replace(f) {
                    POOL.lock().unwrap().push(old.rgba); // displaced unencoded frame (shouldn't happen)
                }
                CV.notify_all();
            }
        }
        None => POOL.lock().unwrap().push(buf), // primed the first chain, or latched off
    }
    CYC_US.fetch_add(t0.elapsed().as_micros() as u32, Ordering::Relaxed);
    CYC_N.fetch_add(1, Ordering::Relaxed);
    LAST_MS.store(now, Ordering::Relaxed);
}

// ---------------------------------------------------------------- caplisten --

fn caplisten(port: u16) {
    unsafe {
        let lfd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if lfd < 0 {
            log("capture: socket() failed");
            return;
        }
        let one: c_int = 1;
        libc::setsockopt(lfd, libc::SOL_SOCKET, libc::SO_REUSEADDR,
                         &one as *const c_int as *const c_void, std::mem::size_of::<c_int>() as u32);
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = port.to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY.to_be();
        // The tmp+mv deploy / SAM stale-running window can leave the OLD instance's
        // listener alive briefly — SO_REUSEADDR doesn't cover a live LISTEN, so retry.
        let mut bound = false;
        for _ in 0..5 {
            if libc::bind(lfd, &addr as *const _ as *const libc::sockaddr,
                          std::mem::size_of::<libc::sockaddr_in>() as u32) == 0 {
                bound = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if !bound || libc::listen(lfd, 1) != 0 {
            log(&format!("capture: bind/listen :{port} failed — capture off"));
            libc::close(lfd);
            ENABLED.store(false, Ordering::Release);
            return;
        }
        LISTEN_FD.store(lfd, Ordering::Release);

        loop {
            let fd = libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut());
            if QUIT.load(Ordering::Acquire) {
                if fd >= 0 {
                    libc::close(fd);
                }
                break;
            }
            if fd < 0 {
                // EINVAL after shutdown() = exiting; anything else transient -> retry.
                // last_os_error() rather than a raw errno deref: `__errno_location` is a
                // glibc symbol, and reading errno portably is what lets this crate compile
                // (and therefore host-test) off-device. Must stay adjacent to the failed call.
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            libc::setsockopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY,
                             &one as *const c_int as *const c_void, std::mem::size_of::<c_int>() as u32);
            let tv = libc::timeval { tv_sec: 2, tv_usec: 0 };
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO,
                             &tv as *const _ as *const c_void, std::mem::size_of::<libc::timeval>() as u32);
            let rtv = libc::timeval { tv_sec: 0, tv_usec: 500_000 };
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO,
                             &rtv as *const _ as *const c_void, std::mem::size_of::<libc::timeval>() as u32);

            // hello: "PXRQ" w:u16 h:u16 (8B, legacy jpeg) or "PXR2" w:u16 h:u16
            // kind:u8 rate:u8 pad:u16 (12B; kind 0=jpeg 1=mpegts, rate = video bitrate
            // in 100kbps units, 0=default). Timeout/short/bad magic = jpeg defaults.
            let recv_exact = |fd: c_int, buf: &mut [u8]| -> bool {
                let mut got = 0usize;
                while got < buf.len() {
                    let n = libc::recv(fd, buf[got..].as_mut_ptr() as *mut c_void, buf.len() - got, 0);
                    if n <= 0 {
                        return false;
                    }
                    got += n as usize;
                }
                true
            };
            let mut hello = [0u8; 8];
            let ok8 = recv_exact(fd, &mut hello);
            let magic = &hello[..4];
            let w = u16::from_le_bytes([hello[4], hello[5]]);
            let mut kind = 0u8;
            if ok8 && magic == b"PXR2" {
                let mut ext = [0u8; 4];
                if recv_exact(fd, &mut ext) {
                    kind = ext[0];
                    if kind == 1 {
                        let bps = if ext[1] == 0 { 2_500_000 } else { ext[1] as u32 * 100_000 };
                        MPEG_RATE.store(bps, Ordering::Relaxed);
                        MPEG_960.store(w == 0 || w >= 720, Ordering::Relaxed);
                    }
                } // short trailing read: fall back to a jpeg client (kind 0)
            }

            if kind == 1 {
                if MPEG_OFF.load(Ordering::Relaxed) {
                    log("capture: mpeg client refused (venc latched off)");
                    libc::close(fd);
                    continue;
                }
                // TS chunks ride bursts; give the tunnel more slack than the jpeg slot
                // before a parked send tears the client down (browser rejoin costs a GOP).
                let mtv = libc::timeval { tv_sec: 5, tv_usec: 0 };
                libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDTIMEO,
                                 &mtv as *const _ as *const c_void, std::mem::size_of::<libc::timeval>() as u32);
                log(&format!("capture: mpeg client connected (fd={fd}, {} @{}bps)",
                             if MPEG_960.load(Ordering::Relaxed) { "960x540" } else { "480x270" },
                             MPEG_RATE.load(Ordering::Relaxed)));
                let old = FD_MPEG.swap(fd, Ordering::AcqRel);
                if old >= 0 {
                    libc::shutdown(old, libc::SHUT_RDWR); // displace only; capenc closes it
                }
            } else {
                // 0 = default; else nearer of the two supported widths
                let want960 = ok8 && (magic == b"PXRQ" || magic == b"PXR2") && w != 0 && w >= 720;
                JPEG_960.store(want960, Ordering::Relaxed);
                log(&format!("capture: client connected (fd={fd}, {})",
                    if want960 { "960x540" } else { "480x270" }));
                let old = FD_JPEG.swap(fd, Ordering::AcqRel);
                if old >= 0 {
                    // displace only: capenc observes the slot changed and closes the old fd
                    libc::shutdown(old, libc::SHUT_RDWR);
                }
            }
        }
        libc::close(lfd);
        LISTEN_FD.store(-1, Ordering::Release);
    }
}

// --------------------------------------------------------------- turbojpeg --
// The NDK sysroot's NEON libjpeg-turbo (2.1.4), deployed next to the binary as
// libturbojpeg.so.0 and dlopen'd at capjpeg startup (dlopen, not DT_NEEDED: the
// app must still boot from a deploy/ipk that lacks the .so — encode then falls
// back to the pure-Rust path). Constants verified against the NDK's turbojpeg.h.

const TJPF_RGBA: c_int = 7;
const TJSAMP_420: c_int = 2;
const TJFLAG_BOTTOMUP: c_int = 2;
const TJFLAG_NOREALLOC: c_int = 1024;
const TJFLAG_FASTDCT: c_int = 2048;

#[allow(clippy::type_complexity)]
struct TurboJpeg {
    handle: *mut c_void, // tjInitCompress handle; lives as long as the thread
    compress2: unsafe extern "C" fn(*mut c_void, *const c_uchar, c_int, c_int, c_int, c_int,
                                    *mut *mut c_uchar, *mut c_ulong, c_int, c_int, c_int) -> c_int,
    buf_size: unsafe extern "C" fn(c_int, c_int, c_int) -> c_ulong,
    err_str: Option<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    out: Vec<u8>, // TJFLAG_NOREALLOC output buffer, tjBufSize-grown per resolution
}

fn tj_load() -> Option<TurboJpeg> {
    unsafe {
        let exe = std::fs::read_link("/proc/self/exe").ok()?;
        let path = std::ffi::CString::new(
            exe.parent()?.join("libturbojpeg.so.0").into_os_string().into_string().ok()?,
        )
        .ok()?;
        let h = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if h.is_null() {
            log("capture: libturbojpeg.so.0 not deployed — pure-Rust JPEG fallback");
            return None;
        }
        let sym = |n: &[u8]| libc::dlsym(h, n.as_ptr() as *const c_char);
        let (init, comp, bufsz) = (sym(b"tjInitCompress\0"), sym(b"tjCompress2\0"), sym(b"tjBufSize\0"));
        if init.is_null() || comp.is_null() || bufsz.is_null() {
            log("capture: libturbojpeg.so.0 lacks tj symbols — pure-Rust JPEG fallback");
            return None; // handle intentionally left open; never dlclose a lib with live code
        }
        let initf: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(init);
        let handle = initf();
        if handle.is_null() {
            log("capture: tjInitCompress failed — pure-Rust JPEG fallback");
            return None;
        }
        log("capture: NEON libjpeg-turbo encoder up");
        let err_str = sym(b"tjGetErrorStr2\0");
        Some(TurboJpeg {
            handle,
            compress2: std::mem::transmute(comp),
            buf_size: std::mem::transmute(bufsz),
            err_str: if err_str.is_null() { None } else { Some(std::mem::transmute(err_str)) },
            out: Vec::new(),
        })
    }
}

impl TurboJpeg {
    /// RGBA -> JPEG q70 4:2:0 straight from the capture buffer; bottom-up frames are
    /// handled by the flag. Returns the encoded length within `self.out`, or None
    /// (logged) on a compress error.
    fn encode(&mut self, f: &Frame) -> Option<usize> {
        unsafe {
            let need = (self.buf_size)(f.w, f.h, TJSAMP_420) as usize;
            if self.out.len() < need {
                self.out.resize(need, 0);
            }
            let mut out_ptr = self.out.as_mut_ptr();
            let mut out_len = self.out.len() as c_ulong;
            let flags =
                TJFLAG_NOREALLOC | TJFLAG_FASTDCT | if f.flip { TJFLAG_BOTTOMUP } else { 0 };
            let rc = (self.compress2)(self.handle, f.rgba.as_ptr(), f.w, f.w * 4, f.h,
                                      TJPF_RGBA, &mut out_ptr, &mut out_len, TJSAMP_420, 70, flags);
            if rc != 0 {
                let msg = self
                    .err_str
                    .map(|e| std::ffi::CStr::from_ptr(e(self.handle)).to_string_lossy().into_owned())
                    .unwrap_or_default();
                log(&format!("capture: tjCompress2 failed ({msg}) — pure-Rust JPEG fallback"));
                return None;
            }
            Some(out_len as usize)
        }
    }
}

// ----------------------------------------------------------------- capjpeg --

/// Write every byte to `fd` (MSG_NOSIGNAL — SIGPIPE would kill the app). Shared with
/// ff.rs's venc AVIO write callback so both halves have one partial-write policy.
pub(crate) fn send_all(fd: c_int, data: &[u8]) -> bool {
    let mut off = 0usize;
    while off < data.len() {
        let n = unsafe {
            libc::send(fd, data[off..].as_ptr() as *const c_void, data.len() - off, libc::MSG_NOSIGNAL)
        };
        if n <= 0 {
            return false;
        }
        off += n as usize;
    }
    true
}

fn capenc() {
    // the fds this thread last used; it ALONE closes client fds (both slots)
    let mut my_jfd: c_int = -1;
    let mut my_mfd: c_int = -1;
    let mut turbo = tj_load(); // None (logged) -> pure-Rust fallback; dropped for good on a tj error
    let mut venc: Option<Box<crate::ff::Venc>> = None; // mpeg1/ts session, per client
    let mut last_mpeg_ticks: u32 = 0;
    let mut rgb: Vec<u8> = Vec::new();
    let mut jpg: Vec<u8> = Vec::new(); // fallback-path encode target
    let mut last_pkt: Vec<u8> = Vec::new(); // last sent header+JPEG, for the jpeg keepalive resend

    // rolling stats, logged every ~5s while frames flow
    let (mut st_t0, mut st_n, mut st_bytes) = (Instant::now(), 0u32, 0u64);
    let (mut st_enc_us, mut st_enc_max, mut st_send_us) = (0u64, 0u32, 0u64);

    loop {
        // take a frame, or time out for the keepalive
        let frame = {
            let mut slot = MAILBOX.lock().unwrap();
            loop {
                if QUIT.load(Ordering::Acquire) {
                    drop(slot);
                    for fd in [my_jfd, my_mfd] {
                        if fd >= 0 {
                            unsafe { libc::close(fd) };
                        }
                    }
                    return;
                }
                if let Some(f) = slot.take() {
                    break Some(f);
                }
                let (s, t) = CV.wait_timeout(slot, std::time::Duration::from_secs(5)).unwrap();
                slot = s;
                if t.timed_out() {
                    break None;
                }
            }
        };

        // fd bookkeeping per slot: loaded ONCE per cycle; if displaced, close ours.
        let cur = FD_JPEG.load(Ordering::Acquire);
        if my_jfd >= 0 && cur != my_jfd {
            unsafe { libc::close(my_jfd) };
        }
        my_jfd = cur;
        let cur = FD_MPEG.load(Ordering::Acquire);
        if cur != my_mfd {
            if my_mfd >= 0 {
                unsafe { libc::close(my_mfd) };
            }
            venc = None; // new/lost client: next frame builds a fresh session (seq hdr + I)
        }
        my_mfd = cur;

        match frame {
            Some(f) => {
                // ---------------- jpeg slot (PXFR framing) ----------------
                if my_jfd >= 0 {
                    let enc_t0 = Instant::now();
                    let turbo_len = match turbo.as_mut().map(|t| t.encode(&f)) {
                        Some(Some(n)) => Some(n),
                        Some(None) => {
                            turbo = None; // tj error (logged): permanent fallback, no per-frame spam
                            None
                        }
                        None => None,
                    };
                    let payload: Option<&[u8]> = match (&turbo, turbo_len) {
                        (Some(t), Some(n)) => Some(&t.out[..n]),
                        _ => {
                            // pure-Rust fallback: RGBA -> RGB strip (+ row reversal when the
                            // GL side flagged bottom-up), then the `image` crate encoder
                            let (w, h) = (f.w as usize, f.h as usize);
                            rgb.resize(w * h * 3, 0);
                            for dy in 0..h {
                                let sy = if f.flip { h - 1 - dy } else { dy };
                                let src = &f.rgba[sy * w * 4..sy * w * 4 + w * 4];
                                let dst = &mut rgb[dy * w * 3..dy * w * 3 + w * 3];
                                for x in 0..w {
                                    dst[x * 3] = src[x * 4];
                                    dst[x * 3 + 1] = src[x * 4 + 1];
                                    dst[x * 3 + 2] = src[x * 4 + 2];
                                }
                            }
                            jpg.clear();
                            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 70);
                            enc.encode(&rgb, f.w as u32, f.h as u32, image::ExtendedColorType::Rgb8)
                                .ok()
                                .map(|_| jpg.as_slice())
                        }
                    };
                    if let Some(payload) = payload {
                        let enc_us = enc_t0.elapsed().as_micros() as u64;
                        last_pkt.clear();
                        last_pkt.extend_from_slice(b"PXFR");
                        last_pkt.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                        last_pkt.extend_from_slice(&f.seq.to_le_bytes());
                        last_pkt.extend_from_slice(&f.ticks.to_le_bytes());
                        last_pkt.extend_from_slice(payload);
                        let send_t0 = Instant::now();
                        if !send_all(my_jfd, &last_pkt) {
                            disconnect_slot(&mut my_jfd, &FD_JPEG, "jpeg");
                        }
                        st_n += 1;
                        st_bytes += last_pkt.len() as u64;
                        st_enc_us += enc_us;
                        st_enc_max = st_enc_max.max(enc_us as u32);
                        st_send_us += send_t0.elapsed().as_micros() as u64;
                    }
                }

                // ---------------- mpeg slot (raw TS) ----------------
                // Encodes whatever geometry the shared chain produced. Encode cost scales
                // with macroblock count and MPEG1 has no intra prediction, so a detailed
                // screen costs ~4x more at 960x540 than at 480x270 — the client picks via
                // the hello, and a geometry change rebuilds the session.
                if my_mfd >= 0 {
                    // resume after an idle gap (player route), or a resolution change:
                    // rebuild so a sequence header + I-frame lead the new stream.
                    let geom_changed = venc.as_ref().map_or(false, |v| v.w != f.w || v.h != f.h);
                    if venc.is_some() && (geom_changed || f.ticks.wrapping_sub(last_mpeg_ticks) > 1500) {
                        venc = None;
                    }
                    if venc.is_none() {
                        venc = crate::ff::Venc::open(f.w, f.h, MPEG_RATE.load(Ordering::Relaxed) as i64);
                        if venc.is_none() {
                            // bring-up failed (encoder/muxer missing): latch off so we
                            // don't retry per frame; refuse future mpeg hellos too.
                            MPEG_OFF.store(true, Ordering::Relaxed);
                            disconnect_slot(&mut my_mfd, &FD_MPEG, "mpeg");
                        }
                    }
                    match venc.as_mut().map(|v| v.encode(&f.rgba, my_mfd, f.flip)) {
                        Some(true) => last_mpeg_ticks = f.ticks,
                        Some(false) => {
                            venc = None;
                            disconnect_slot(&mut my_mfd, &FD_MPEG, "mpeg");
                        }
                        None => {}
                    }
                }

                POOL.lock().unwrap().push(f.rgba);

                let dt = st_t0.elapsed().as_secs_f32();
                if dt >= 5.0 && st_n > 0 {
                    let (cyc_us, cyc_n) = (CYC_US.swap(0, Ordering::Relaxed), CYC_N.swap(0, Ordering::Relaxed));
                    let (rd_us, rd_n) = (
                        crate::gfx::CAP_READ_US.swap(0, Ordering::Relaxed),
                        crate::gfx::CAP_READ_N.swap(0, Ordering::Relaxed),
                    );
                    log(&format!(
                        "capture: {} frm/{:.1}s ({:.1}fps) enc {:.1}ms avg (max {:.1}) send {:.1}ms \
                         cyc {:.1}ms read {:.1}ms {}KB/frm [{}]",
                        st_n, dt, st_n as f32 / dt,
                        st_enc_us as f32 / st_n as f32 / 1000.0, st_enc_max as f32 / 1000.0,
                        st_send_us as f32 / st_n as f32 / 1000.0,
                        cyc_us as f32 / cyc_n.max(1) as f32 / 1000.0,
                        rd_us as f32 / rd_n.max(1) as f32 / 1000.0,
                        st_bytes / st_n as u64 / 1024,
                        if turbo.is_some() { "turbo" } else { "rust" },
                    ));
                    st_t0 = Instant::now();
                    (st_n, st_bytes, st_enc_us, st_enc_max, st_send_us) = (0, 0, 0, 0, 0);
                }
            }
            None => {
                // 5s idle: resend the last JPEG frame UNCHANGED — same seq — so the
                // client's deadness timer doesn't trip while no UI frames flow (player
                // route). The unchanged seq is load-bearing: the host detects "seq
                // stalled" and temporarily switches to the service capture (the only
                // view of the video plane) during playback. JPEG SLOT ONLY: the mpeg
                // socket must go byte-silent when idle — re-injecting old TS bytes
                // (stale continuity counters, rewound PTS) would corrupt the stream;
                // the host detects mpeg staleness as a byte-flow stall instead.
                if my_jfd >= 0 && last_pkt.len() > 16 {
                    if !send_all(my_jfd, &last_pkt) {
                        disconnect_slot(&mut my_jfd, &FD_JPEG, "jpeg");
                    }
                }
            }
        }
    }
}

/// Send failed: stop using the fd. If we still own it in the slot (CAS succeeds),
/// close it; if a newer client already displaced us, the next cycle's bookkeeping
/// closes it exactly once.
fn disconnect_slot(my_fd: &mut c_int, slot: &AtomicI32, label: &str) {
    let fd = *my_fd;
    unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
    if slot.compare_exchange(fd, -1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        unsafe { libc::close(fd) };
        *my_fd = -1;
        log(&format!("capture: {label} client disconnected"));
    }
    // CAS failure: displaced — leave *my_fd; the top-of-cycle check closes it.
}
