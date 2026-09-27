//! **The simulator's picture** (`plxnative-simvideo`, with the clock sink armed): the video access
//! units the clock sink already accepts are ALSO piped to a system `ffmpeg` child process, decoded
//! there, and the frame whose presentation time the sink's clock has reached is composited UNDER
//! the finished UI frame — exactly the arithmetic the television's compositor applies to its
//! hardware video plane and our transparent UI plane.
//!
//! **Why it exists.** The documentation's player figure has to show a real picture under the real
//! HUD, and the simulator otherwise shows the HUD over black (`ffi_host.rs`: nothing decodes). It
//! is a screenshot facility, nothing more: every number the clock sink's module doc disowns stays
//! disowned, and nothing here says anything about LG's decoder.
//!
//! **Why a child process, and why only in the simulator.** The host FFmpeg this crate links
//! (`ci/build-ffmpeg.sh`) is built from the television's ONE component list, which has no video
//! decoders — the TV decodes in hardware — and growing that list for a Mac would make the two
//! builds diverge. A system `ffmpeg` binary (`PLXNATIVE_SIM_FFMPEG`, else `ffmpeg` on `PATH`) keeps
//! the decode entirely outside this process and this crate's link line. `hostsim`-only: the
//! television build does not contain this file.
//!
//! **What it costs, and the bound.** One 1920x1080 RGBA frame (8 MiB) crosses a pipe per decoded
//! picture and is uploaded once when it becomes current — about what a poster upload costs, once
//! per video frame, on a Mac. The reader holds at most ONE frame ahead of the clock (it blocks
//! until the clock reaches it), so a paused player holds a frozen picture and does no work, and
//! the render thread only ever sees the latest frame.
//!
//! **Frame ↔ PTS.** The decoder is fed raw Annex-B, which carries no timestamps, and emits frames
//! in presentation order. So the n-th frame out is the n-th smallest PTS fed since the last
//! reset: the pending PTS are kept in a min-heap and popped as frames emerge. A reorder window
//! cannot break that: a frame cannot be output before every frame that precedes it in display
//! order has been fed.

use std::collections::BinaryHeap;
use std::cmp::Reverse;
use std::io::{Read, Write};
use std::os::raw::c_uint;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// The decoded picture's size — the canvas. Letterboxed to it, whatever the stream's shape.
const W: usize = 1920;
const H: usize = 1080;
const FRAME_BYTES: usize = W * H * 4;

/// Is the picture armed? Read once (a trigger is a boot-time fact).
fn armed() -> bool {
    static ONCE: OnceLock<bool> = OnceLock::new();
    *ONCE.get_or_init(|| {
        let on = crate::dev::flag("simvideo");
        if on {
            crate::log(
                "simvideo: ARMED — video AUs are also decoded by a system ffmpeg and composited \
                 under the UI. A screenshot facility: nothing here measures the television.",
            );
        }
        on
    })
}

/// One decode session: a child, the thread feeding it, and the PTS its frames will carry.
struct Session {
    child: Child,
    feed: Sender<Vec<u8>>,
    pending: Arc<Mutex<BinaryHeap<Reverse<i64>>>>,
}

/// The live session, if any. Replaced on every Load/flush, dropped (and the child killed) on stop.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
/// Bumped on every reset, so a reader thread from an older session stops publishing.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The container's codec as the Load payload named it: `true` = HEVC, else H.264.
static HEVC: AtomicBool = AtomicBool::new(false);
/// The clock the frames are presented against — the sink's `Clock::position_ns`.
static CLOCK: OnceLock<fn() -> i64> = OnceLock::new();

/// Every published frame's number, process-wide, so the render thread's texture cache can never
/// mistake a new session's first frame for the last one's.
static FRAME_SEQ: AtomicU64 = AtomicU64::new(0);

/// The frame the render thread should show: `(frame number, rgba)`; `None` = nothing to show.
static LATEST: Mutex<Option<(u64, Arc<Vec<u8>>)>> = Mutex::new(None);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A new stream: remember its codec and clock, and drop whatever the last one left. `payload` is
/// the Load JSON the television would have been given.
pub(crate) fn load(payload: &str, clock: fn() -> i64) {
    if !armed() {
        return;
    }
    let _ = CLOCK.set(clock);
    let lower = payload.to_ascii_lowercase();
    HEVC.store(lower.contains("h265") || lower.contains("hevc"), Relaxed);
    reset(true);
}

/// A flush (a seek restarts the fed timeline): keep the picture on screen until the next frame.
pub(crate) fn flush() {
    if armed() {
        reset(false);
    }
}

/// Unload/destroy: no stream, no picture.
pub(crate) fn stop() {
    if armed() {
        reset(true);
    }
}

fn reset(clear_picture: bool) {
    GENERATION.fetch_add(1, Relaxed);
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if clear_picture {
        *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = None;
        crate::ui::idle::invalidate();
    }
}

/// One video access unit, Annex-B, at `pts` in the fed timeline. Never blocks the feeder.
pub(crate) fn feed(au: &[u8], pts: i64) {
    if !armed() {
        return;
    }
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = spawn();
    }
    let Some(session) = guard.as_ref() else { return };
    session.pending.lock().unwrap_or_else(|e| e.into_inner()).push(Reverse(pts));
    if session.feed.send(au.to_vec()).is_err() {
        *guard = None;
    }
}

fn ffmpeg_path() -> std::ffi::OsString {
    std::env::var_os("PLXNATIVE_SIM_FFMPEG").unwrap_or_else(|| "ffmpeg".into())
}

fn spawn() -> Option<Session> {
    let generation = GENERATION.load(Relaxed);
    let scale = format!(
        "scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2,format=rgba"
    );
    let spawned = Command::new(ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-f"])
        .arg(if HEVC.load(Relaxed) { "hevc" } else { "h264" })
        .args(["-i", "pipe:0", "-an", "-vf"])
        .arg(scale)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            crate::log(&format!("simvideo: could not start ffmpeg ({e}); no picture this session"));
            return None;
        }
    };
    let (mut stdin, mut stdout) = (child.stdin.take()?, child.stdout.take()?);
    let (tx, rx) = channel::<Vec<u8>>();
    let pending = Arc::new(Mutex::new(BinaryHeap::new()));
    let writer = std::thread::Builder::new().name("simvideo-in".into()).spawn(move || {
        for au in rx {
            if stdin.write_all(&au).is_err() {
                break;
            }
        }
    });
    let pending_r = Arc::clone(&pending);
    let reader = std::thread::Builder::new().name("simvideo-out".into()).spawn(move || {
        let mut buf = vec![0u8; FRAME_BYTES];
        let mut frames = 0u64;
        while stdout.read_exact(&mut buf).is_ok() {
            let Some(Reverse(pts)) = pending_r.lock().unwrap_or_else(|e| e.into_inner()).pop() else {
                continue;
            };
            // Hold this frame until the sink's clock reaches it: a paused player waits here.
            while CLOCK.get().is_some_and(|clock| clock() < pts) {
                if GENERATION.load(Relaxed) != generation {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(4));
            }
            if GENERATION.load(Relaxed) != generation {
                return;
            }
            let seq = FRAME_SEQ.fetch_add(1, Relaxed);
            *LATEST.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((seq, Arc::new(std::mem::replace(&mut buf, vec![0u8; FRAME_BYTES]))));
            frames += 1;
            if frames == 1 {
                crate::log("simvideo: first picture decoded");
            }
            crate::ui::idle::invalidate();
        }
    });
    if writer.is_err() || reader.is_err() {
        crate::log("simvideo: could not spawn the pipe threads; no picture this session");
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    Some(Session { child, feed: tx, pending })
}

/// The render thread's half: upload the current frame if it changed and composite it UNDER what
/// the UI drew this frame. Called once per presented frame, before the capture and the swap.
pub(crate) fn composite_under() {
    thread_local! {
        /// `(texture, frame number it holds)`.
        static TEX: std::cell::Cell<(c_uint, Option<u64>)> = const { std::cell::Cell::new((0, None)) };
    }
    if !armed() {
        return;
    }
    let latest = LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some((n, rgba)) = latest else { return };
    TEX.with(|t| {
        let (mut tex, held) = t.get();
        if held != Some(n) {
            tex = crate::gfx::upload_rgba(tex, W as i32, H as i32, rgba.as_ptr());
            t.set((tex, Some(n)));
        }
        crate::gfx::draw_under(tex, 0.0, 0.0, W as f32, H as f32);
    });
}
