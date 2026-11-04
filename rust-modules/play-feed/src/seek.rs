//! How a direct-play seek re-anchors the pipeline, and the per-session state that says so: the one
//! enum the executor and the player's pump both read (there used to be two, `SeekMode` in the media
//! player and an executor-side copy of it, converted back and forth through hooks).
//!
//! The statics are plain atomics beside [`crate::shared::FEED_SHARED`]: the player's pump reads them
//! without an Engine borrow in hand, the executor's `feed_stream` downgrades them, and the player
//! selects and clears them around a session.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering::Relaxed};

/// How a direct-play seek re-anchors the pipeline for THIS session (issue #568).
///
/// - [`Segment`](SeekMode::Segment) — Kodi's in-place seek: flush + reopen + `av_seek`, then on the
///   first post-seek keyframe `setTimeToDecode` / `setContentInfo` + the private `sendSegmentEvent`.
///   No reload, no decoder re-init, so no HDR-mode popup and no A/V-resync glitch. It reaches
///   LG-private C++ objects through decompile-derived offsets (`src/starfish.c`'s `sf_pipeline`:
///   object+0x4c, then player+0x04, plus `MEDIA_CUSTOM_CONTENT_INFO`'s `ptsToDecode`), proven on
///   webOS 4.5 only, so it is used only where `inplace_layout_known` holds and the window is not
///   `VP_EXPORTED`. On webOS<11 (this 4.5) `setTimeToDecode` returns 0, so `feed_stream` falls
///   back to the content-info path (`loadSpi_getInfo` + `setContentInfo(ptsToDecode)`).
/// - [`NativeSeek`](SeekMode::NativeSeek) — LG's PUBLIC `StarfishMediaAPIs::Seek(const char*)`
///   (`_ZN17StarfishMediaAPIs4SeekEPKc`, present on every firmware 1.2 to 11.2): a flushing
///   `gst_element_seek` whose segment starts at the millis passed. No private offset, no
///   `flush()` (Seek flushes itself, and a `flush()` after it re-anchors the segment at 0 — the
///   stale-segment stall), no decoder re-init. This is what `VP_EXPORTED` (webOS 5+) uses, and
///   a known webOS 3 release on the ACB plane, whose old-ABI library `Segment` cannot assume
///   (`engine::old_abi_seek_tested`).
/// - [`Reload`](SeekMode::Reload) — a full fresh Starfish `Load` per seek (`engine::reload_at`),
///   built out of Load/Play alone and assuming no layout at all: the fallback both of the others
///   drop to, and every layout-unknown library `NativeSeek` is not tested on (webOS 2, an old-ABI
///   4.x build, an unknown release) or a set with no video plane.
///
/// SCOPE: this is a **per-session mode, not a device-capability latch**, and
/// the player's `engine::start_bufferfeed` re-selects it for every new session. The one exception is a refused
/// native `Seek`: that is remembered for the rest of the SAME playback (`native_seek_refused`,
/// preserved across `reload_at`, cleared by a fresh play), so a firmware or file that refuses
/// does not pay a refused `Seek` plus a full `Load` on every seek. A downgrade to `Reload` (a
/// `sendSegment` that found no pipeline, a refused `Seek`) is a fact about the StarfishMediaAPIs
/// object, which `sf_load` constructs and teardown's `sf_destroy` destructs once per session — not
/// about the TV. `sendSegmentEvent` itself returns void, so its 0 NEVER means "the segment event
/// was rejected", only "there was nothing to call it on": a liveness/timing condition by
/// construction. Latching it for the process was a bug with a very long tail (one teardown-window
/// race downgraded every later seek of every later item to a ~1 s reload until restart).
/// Re-selecting per session is self-healing rather than oscillating, too: `reload_at` builds a
/// fresh Starfish object, so the condition that produced the downgrade cannot survive into the
/// session that re-selects. It remains a static rather than an `Engine` field only because
/// `pump.rs` reads it without an Engine borrow at hand; its LIFETIME is the Engine's, since
/// `start_bufferfeed` is the sole constructor and the mode is only read while a session is live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SeekMode {
    Segment,
    NativeSeek,
    Reload,
}

impl SeekMode {
    const fn to_u8(self) -> u8 {
        match self {
            SeekMode::Segment => 0,
            SeekMode::NativeSeek => 1,
            SeekMode::Reload => 2,
        }
    }
    const fn from_u8(v: u8) -> SeekMode {
        match v {
            0 => SeekMode::Segment,
            1 => SeekMode::NativeSeek,
            _ => SeekMode::Reload,
        }
    }
}

static SEEK_MODE: AtomicU8 = AtomicU8::new(SeekMode::Segment.to_u8());

/// Set once a native `Seek` was REFUSED during the current playback, and preserved by `reload_at`
/// (whose fresh session would otherwise re-select `NativeSeek` and pay a refused `Seek` plus a full
/// `Load` on every seek). A fresh play (`start_bufferfeed_tracked` with no live Engine) clears it.
static NATIVE_SEEK_REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether this playback's native `Seek` was refused (see `NATIVE_SEEK_REFUSED`).
pub fn native_seek_refused() -> bool {
    NATIVE_SEEK_REFUSED.load(Relaxed)
}

pub fn set_native_seek_refused(refused: bool) {
    NATIVE_SEEK_REFUSED.store(refused, Relaxed);
}

/// This session's direct-play seek mode.
pub fn seek_mode() -> SeekMode {
    SeekMode::from_u8(SEEK_MODE.load(Relaxed))
}

/// Set this session's direct-play seek mode (`start_bufferfeed` selects it, `feed_stream` downgrades it).
pub fn set_seek_mode(mode: SeekMode) {
    SEEK_MODE.store(mode.to_u8(), Relaxed);
}
