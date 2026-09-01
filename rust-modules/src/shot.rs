//! Simulator screenshots — the host analogue of `tools/capture-screen.sh`.
//!
//! An agent driving the simulator needs to LOOK at the result, and it has no television to point a
//! capture service at. This reads the frame back off the GL front-of-swap buffer and writes a PNG.
//!
//! PNG specifically, and not the BMP/PPM that would need no encoder: an agent's file-reading tool
//! renders PNG and JPEG, and a format it cannot display is a file nobody looks at. The encoder is
//! already a dependency — `image` is in the crate for poster DECODING, and its `png` feature
//! carries the encoder too, so this costs no new crate.
//!
//! Deliberately not a general capture subsystem. `capture.rs` is that, it streams over TCP, and it
//! stays the answer for a live view. This is the one-shot an automated run needs: boot, settle,
//! write a file, optionally exit.
//!
//! Environment, read ONCE at first use (the whole app reads its dev config at boot by contract —
//! see `dev.rs` — and this runs before every swap, so re-reading it per frame would be both
//! against that convention and pointless work):
//!   PLXNATIVE_SHOT=<path>          where to write (default: `shot.png` in the instance root)
//!   PLXNATIVE_SHOT_FRAME=<n>       ALSO capture automatically at presented frame n (default: no
//!                                  automatic capture — only the `shot` token fires one)
//!   PLXNATIVE_SHOT_EXIT=1          exit(0) after an automatic capture — the headless one-shot mode
//!
//! The `shot` token on the remote FIFO captures on demand instead, which is what an interactive
//! agent session uses: drive the UI, then ask for the frame. Those are NUMBERED (`shot-1.png`,
//! `shot-2.png`, …) so a sequence of them does not overwrite one file and race whoever is reading
//! it, and they never exit the process — the caller is still driving.

use std::os::raw::{c_int, c_uint, c_void};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

extern "C" {
    fn glReadPixels(
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        format: c_uint,
        ty: c_uint,
        pixels: *mut c_void,
    );
}
const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;

/// Where shots go and when, resolved once.
struct Cfg {
    path: PathBuf,
    /// `Some(n)` arms an automatic capture at presented frame n. There is deliberately NO default:
    /// a frame number that nobody asked for silently drops an extra, un-numbered file next to the
    /// numbered ones an agent was told to read — which is exactly what a default of 150 did to the
    /// `ui-sim` skill's own interactive recipe.
    frame: Option<u32>,
    exit: bool,
}

fn cfg() -> &'static Cfg {
    static C: OnceLock<Cfg> = OnceLock::new();
    C.get_or_init(|| Cfg {
        // Defaulting the path is what lets the `shot` token work in ANY session, including
        // `make sim-run`, rather than only in one that happened to export PLXNATIVE_SHOT.
        path: std::env::var_os("PLXNATIVE_SHOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::paths::in_runtime_dir("shot.png")),
        frame: std::env::var("PLXNATIVE_SHOT_FRAME")
            .ok()
            .and_then(|s| s.parse().ok()),
        exit: std::env::var_os("PLXNATIVE_SHOT_EXIT").is_some(),
    })
}

/// Presented frames so far. Counted here rather than read from the heartbeat because the heartbeat
/// is once per second and a shot wants frame granularity.
static FRAMES: AtomicU32 = AtomicU32::new(0);

/// Set by the `shot` remote token; captures the next presented frame regardless of the count.
static ON_DEMAND: AtomicBool = AtomicBool::new(false);

/// Ask for a capture of the next frame. The remote-FIFO entry point (`app.rs`'s token dispatch).
///
/// Deliberately one-shot and idempotent: two `shot` tokens in a row produce one file, because the
/// second arrives before anything has repainted and would otherwise overwrite the first with the
/// identical frame.
pub(crate) fn request() {
    ON_DEMAND.store(true, Ordering::Relaxed);
    // **Required, not defensive.** The capture happens on the way to a swap, and a settled screen
    // does not swap: `ui::idle` skips `glViewport`…`SDL_GL_SwapWindow` wholesale once nothing is
    // moving. So a `shot` token sent to a UI that has come to rest — which is exactly when an
    // agent wants one, after driving and waiting — would set this flag and then wait forever for a
    // frame that never comes. Observed as a token that logged nothing at all.
    crate::ui::idle::invalidate();
}

/// `dir/stem-N.ext`, N counting up per on-demand shot within this process.
fn numbered(base: &std::path::Path) -> std::path::PathBuf {
    static NTH: AtomicU32 = AtomicU32::new(0);
    let n = NTH.fetch_add(1, Ordering::Relaxed) + 1;
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shot".into());
    let ext = base
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "png".into());
    base.with_file_name(format!("{stem}-{n}.{ext}"))
}

/// Grab the frame about to be presented, if this is the one asked for.
///
/// **Must be called before `SDL_GL_SwapWindow`.** After the swap the back buffer's contents are
/// undefined by specification, and on a real driver they are whatever the compositor left there —
/// a screenshot taken after would be intermittently blank, which is worse than never working.
pub(crate) fn maybe_capture(vx: c_int, vy: c_int, vw: c_int, vh: c_int) {
    let n = FRAMES.fetch_add(1, Ordering::Relaxed);
    let on_demand = ON_DEMAND.swap(false, Ordering::Relaxed);
    let cfg = cfg();
    if !on_demand && cfg.frame != Some(n) {
        return;
    }
    if vw <= 0 || vh <= 0 {
        crate::log("shot: viewport is empty — nothing to capture");
        return;
    }

    // The viewport rect, not the whole window: `surface::probe` letterboxes the logical canvas
    // into the drawable, so the bars around it are not part of the interface and only make the
    // image harder to compare against a device capture.
    let (w, h) = (vw as usize, vh as usize);
    let mut buf = vec![0u8; w * h * 4];
    unsafe {
        glReadPixels(
            vx,
            vy,
            vw,
            vh,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            buf.as_mut_ptr() as *mut c_void,
        )
    };

    // Flip and drop alpha in one pass.
    //
    // GL's origin is bottom-left and every image format's is top-left, hence the row flip.
    //
    // **Alpha is discarded on purpose, and keeping it was wrong.** This app renders a deliberately
    // NON-OPAQUE UI plane so the hardware video plane composites through it (`system.rs`'s
    // set_opaque_region NULL) — so the framebuffer's alpha channel is not "how opaque this pixel
    // is", it is an instruction to the television's compositor. Only about a third of a Home frame
    // is fully opaque. Written into a PNG, that instruction gets re-interpreted by whatever views
    // the file: over a white page the whole interface blooms, which reads as a rendering fault and
    // is not one.
    //
    // The RGB channels are already the composited answer — everything the app drew, blended over
    // its own clear colour — which is exactly what the panel shows when no video is playing. So an
    // opaque RGB image is the FAITHFUL screenshot, and the one that compares to a device capture,
    // where the TV's compositor has likewise already flattened the two planes.
    let src_stride = w * 4;
    let dst_stride = w * 3;
    let mut rgb = vec![0u8; dst_stride * h];
    for y in 0..h {
        let src = (h - 1 - y) * src_stride;
        for x in 0..w {
            let s = src + x * 4;
            let d = y * dst_stride + x * 3;
            rgb[d..d + 3].copy_from_slice(&buf[s..s + 3]);
        }
    }

    let out = if on_demand {
        numbered(&cfg.path)
    } else {
        cfg.path.clone()
    };
    match image::save_buffer(&out, &rgb, w as u32, h as u32, image::ColorType::Rgb8) {
        Ok(()) => crate::log(&format!("shot: wrote {}x{} to {}", w, h, out.display())),
        Err(e) => crate::log(&format!("shot: could not write {}: {e}", out.display())),
    }

    if cfg.exit && !on_demand {
        // Flush by leaving `log` alone (it appends unbuffered) and go. A clean exit here is the
        // whole point of the headless mode: the caller wants a file, not a window.
        std::process::exit(0);
    }
}
