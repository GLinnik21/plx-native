//! How a direct-play seek re-anchors the pipeline, and the per-session state that says so: the one
//! enum the executor and the player's pump both read (there used to be two, `SeekMode` in the media
//! player and an executor-side copy of it, converted back and forth through hooks).
//!
//! The statics are plain atomics beside [`crate::shared::FEED_SHARED`]: the player's pump reads them
//! without an Engine borrow in hand, the executor's `feed_stream` downgrades them, and the player
//! selects and clears them around a session.

use plx_platform::tv::sink::{VideoSink, VP_ACB, VP_EXPORTED};
use std::os::raw::c_int;
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
///   ([`old_abi_seek_tested`]).
/// - [`Reload`](SeekMode::Reload) — a full fresh Starfish `Load` per seek (the player's `reload_at`),
///   built out of Load/Play alone and assuming no layout at all: the fallback both of the others
///   drop to, and every layout-unknown library `NativeSeek` is not tested on (webOS 2, an old-ABI
///   4.x build, an unknown release) or a set with no video plane.
///
/// SCOPE: this is a **per-session mode, not a device-capability latch**, and
/// the executor's `launch_native_open` re-selects it ([`select_seek_mode_for_load`]) for every new session. The one exception is a refused
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
/// `launch_native_open` is the sole constructor and the mode is only read while a session is live.
///
/// The enum itself is contract vocabulary (`plx_playback_contract::feed::SeekMode`), so the player's
/// pump names it without naming this crate; the static below stores it as a byte.
pub use plx_playback_contract::feed::SeekMode;

const fn seek_mode_to_u8(mode: SeekMode) -> u8 {
    match mode {
        SeekMode::Segment => 0,
        SeekMode::NativeSeek => 1,
        SeekMode::Reload => 2,
    }
}

const fn seek_mode_from_u8(v: u8) -> SeekMode {
    match v {
        0 => SeekMode::Segment,
        1 => SeekMode::NativeSeek,
        _ => SeekMode::Reload,
    }
}

static SEEK_MODE: AtomicU8 = AtomicU8::new(seek_mode_to_u8(SeekMode::Segment));

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
    seek_mode_from_u8(SEEK_MODE.load(Relaxed))
}

/// Set this session's direct-play seek mode (`launch_native_open` selects it, `feed_stream` downgrades it).
pub fn set_seek_mode(mode: SeekMode) {
    SEEK_MODE.store(seek_mode_to_u8(mode), Relaxed);
}

/// Select this session's seek mode from the device and the installed sink, and publish it. The
/// executor calls this once per Load ([`crate::native_executor::launch_native_open`]), with the
/// composition's fenced sink: the mode is a fact about the StarfishMediaAPIs object that Load is
/// about to construct.
pub fn select_seek_mode_for_load(sink: &dyn VideoSink) {
    let major = plx_platform::tv::device::info().major;
    let layout_known = inplace_layout_known(major, sink.feed_abi_is_cow());
    let old_abi_seek = old_abi_seek_tested(major);
    let mode = session_seek_mode(layout_known, old_abi_seek, sink.window_mode(), dev_seek_mode());
    plx_base::eventlog::log(&format!(
        "seek mode: {mode:?} (layout_known={layout_known} old_abi_seek={old_abi_seek})"));
    set_seek_mode(mode);
}

/// Whether the decompiled StarfishMediaAPIs offsets (`src/starfish.c`'s `sf_pipeline`)
/// may be trusted for in-place seek. The old-ABI `Feed` is the primary gate: it proves
/// libplayerAPIs is the pre-C++11 build, so 3.9.x is excluded without listing releases. A known
/// release below 4 is refused too; `major == 0` (release unknown) defers to the Feed evidence.
pub fn inplace_layout_known(major: u32, feed_abi_is_cow: bool) -> bool {
    !feed_abi_is_cow && (major == 0 || major >= 4)
}

/// **Is LG's public `Seek` device-tested where the private layout is not known?** Only on a KNOWN
/// webOS 3 release: 80 seeks on platform release 3.9.3 (`docs/webos5-port.md`, "The public
/// `Seek`"). It reads the release alone, not the Feed ABI: every 3.x library seen has the old
/// (COW) one, and a release-3 set with the cxx11 Feed would get `Seek` too. webOS 2, an old-ABI
/// 4.x build and an unknown release (`major == 0`) keep reloading, because an accepted `Seek` that
/// anchors wrong is caught by no net ([`select_seek_mode`]).
pub fn old_abi_seek_tested(major: u32) -> bool {
    major == 3
}

/// [`select_seek_mode`] for a session that is starting: a `NativeSeek` this playback already had
/// REFUSED stays `Reload` (the latch `feed_stream` sets, which `reload_at` preserves and a fresh
/// play clears), instead of paying reopen + keyframe + refused `Seek` + full `Load` on every seek.
pub fn session_seek_mode(
    layout_known: bool,
    old_abi_seek: bool,
    window_mode: c_int,
    dev: Option<SeekMode>,
) -> SeekMode {
    match select_seek_mode(layout_known, old_abi_seek, window_mode, dev) {
        SeekMode::NativeSeek if native_seek_refused() => SeekMode::Reload,
        mode => mode,
    }
}

/// How a direct-play seek re-anchors the pipeline in a new session (see [`SeekMode`]).
///
/// - `VP_EXPORTED` (webOS 5+) -> [`SeekMode::NativeSeek`]: LG's PUBLIC `StarfishMediaAPIs::Seek`.
///   The Kodi in-place path (`Segment`) is NOT used there: it reaches LG-private C++ objects
///   through two decompile-derived offsets (`StarfishMediaAPIs::player` at object+0x4c, then
///   `AbstractPlayer::pipeline` at +0x04, plus `MEDIA_CUSTOM_CONTENT_INFO`'s `ptsToDecode` at
///   +0x28) read off a webOS 4.5 binary, which nothing in a symbol table can confirm on another
///   firmware: a field added to StarfishMediaAPIs by any 5.x build moves +0x4c and the very next
///   seek dereferences whatever now lives there. `Seek` is an exported symbol, present on every
///   firmware 1.2 to 11.2, so it assumes no layout at all.
/// - A known layout without `VP_EXPORTED` (webOS 4.x) -> [`SeekMode::Segment`], unchanged.
/// - NOT `Segment` when the library is the pre-C++11 build (`inplace_layout_known` false): its
///   StarfishMediaAPIs is a different object from the 4.5 one, and the first fast-forward on a
///   3.4.0 set crashed through those offsets. The old-ABI Feed (`sf_feed_abi_is_cow`) is the
///   direct evidence, and it covers 3.9.x too. A KNOWN release below 4 is refused as well; an
///   UNKNOWN one (`major == 0`: neither `os_info.json` nor `/usr/lib/os-release` named a release)
///   with the C++11 Feed keeps in-place seek, as before #578, because that Feed says the library
///   is 4.x or later.
/// - A layout-unknown library on the ACB plane in a KNOWN webOS 3 release
///   ([`old_abi_seek_tested`]) -> [`SeekMode::NativeSeek`] too. `Seek` takes a `const char*`, so
///   its symbol and calling convention are the same on both C++ ABIs; that it also behaves the
///   same rests on the device test in `docs/webos5-port.md` ("The public `Seek`"), not on the
///   decompile, which is of 4.5.
///   The nets are the ones webOS 5+ has: a refused `Seek` drops the playback to `Reload`
///   (`native_seek_refused`), and a seek that does not anchor within `SEEK_STUCK_MS` is retried,
///   then reloaded. Neither catches a `Seek` that is accepted and then anchors wrong (the
///   stale-segment stall, about 48 s later), which is why the release is gated to the one tested.
/// - Any other layout-unknown library (webOS 2, an old-ABI 4.x build, an unknown release) and no
///   video plane (`VP_NONE`) -> [`SeekMode::Reload`], as before.
/// `dev` is the `plxnative-seekmode` dev trigger and wins when present, so a webOS 4.5 set can
/// exercise `NativeSeek`.
pub fn select_seek_mode(
    layout_known: bool,
    old_abi_seek: bool,
    window_mode: c_int,
    dev: Option<SeekMode>,
) -> SeekMode {
    if let Some(forced) = dev {
        return forced;
    }
    if window_mode == VP_EXPORTED {
        SeekMode::NativeSeek
    } else if layout_known {
        SeekMode::Segment
    } else if old_abi_seek && window_mode == VP_ACB {
        SeekMode::NativeSeek
    } else {
        SeekMode::Reload
    }
}

/// `/tmp/plxnative-seekmode`: `native`, `segment` or `reload`. `None` without the `devtriggers`
/// feature (`devtrig::read` folds away), so a release build cannot be steered.
fn dev_seek_mode() -> Option<SeekMode> {
    match plx_base::devtrig::read("seekmode")?.trim() {
        "native" => Some(SeekMode::NativeSeek),
        "segment" => Some(SeekMode::Segment),
        "reload" => Some(SeekMode::Reload),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::sink::VP_NONE;

    /// The mode a set on release `major` (old-ABI Feed when `cow`) gets from a window of kind
    /// `window_mode`, composed from the same facts [`select_seek_mode_for_load`] composes.
    fn mode_for(major: u32, cow: bool, window_mode: c_int) -> SeekMode {
        select_seek_mode(inplace_layout_known(major, cow), old_abi_seek_tested(major), window_mode, None)
    }

    #[test]
    fn in_place_seek_needs_the_cxx11_feed_and_a_release_that_is_unknown_or_4_plus() {
        assert!(inplace_layout_known(4, false));
        assert!(inplace_layout_known(0, false));
        assert!(inplace_layout_known(11, false));
        assert!(!inplace_layout_known(3, false));
        assert!(!inplace_layout_known(3, true));
        assert!(!inplace_layout_known(4, true));
        assert!(!inplace_layout_known(0, true));
    }

    #[test]
    fn each_firmware_gets_its_seek_mode() {
        assert_eq!(mode_for(4, false, VP_ACB), SeekMode::Segment, "webOS 4.x is unchanged");
        assert_eq!(mode_for(5, false, VP_EXPORTED), SeekMode::NativeSeek, "webOS 5+ is unchanged");
        assert_eq!(mode_for(3, true, VP_ACB), SeekMode::NativeSeek, "webOS 3 (a 3.9.3 set: COW Feed) uses LG's public Seek");
        assert_eq!(mode_for(2, true, VP_ACB), SeekMode::Reload, "webOS 2 is untested and reloads, as before");
        assert_eq!(mode_for(4, true, VP_ACB), SeekMode::Reload, "an old-ABI 4.x build is untested and reloads");
        assert_eq!(mode_for(0, true, VP_ACB), SeekMode::Reload, "an unknown release with the old ABI reloads");
        assert_eq!(mode_for(3, true, VP_NONE), SeekMode::Reload, "no video plane, nothing to seek in place");
        assert_eq!(select_seek_mode(true, false, VP_ACB, Some(SeekMode::NativeSeek)), SeekMode::NativeSeek, "the dev trigger wins");
    }

    /// A webOS 3 playback whose `Seek` was refused stays on the reload path across the reload's
    /// fresh session, as a webOS 5+ one does (`pump`'s refused-Seek test); a fresh play clears it.
    #[test]
    fn a_refused_seek_on_webos3_keeps_the_playback_on_reload() {
        let _serial = plx_base::testlock::serial();
        let webos3 = || session_seek_mode(inplace_layout_known(3, true), old_abi_seek_tested(3), VP_ACB, None);
        set_native_seek_refused(true);
        assert_eq!(webos3(), SeekMode::Reload, "the downgrade survives reload_at");
        set_native_seek_refused(false);
        assert_eq!(webos3(), SeekMode::NativeSeek, "a fresh play offers Seek again");
    }
}
