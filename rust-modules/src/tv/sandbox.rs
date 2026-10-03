//! The jail's verdict on the video path, and the user-confirmed repair of it. The webOS port
//! probes `/dev/rtkmem` at boot and publishes the verdict here; the player gates on it.
use std::sync::OnceLock;

/// The three outcomes of the boot-time probe, cached for the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The SoC did not match the affected family — `/dev/rtkmem` was never probed.
    NotApplicable,
    /// The SoC matched and `/dev/rtkmem` is readable.
    Ok,
    /// The SoC matched and `/dev/rtkmem` is missing (or unreadable) from this jail.
    Missing,
}

static VERDICT: OnceLock<Verdict> = OnceLock::new();

/// The boot probe's verdict, once per boot. First write wins, like the `OnceLock` it lands in.
pub(crate) fn publish(verdict: Verdict) {
    let _ = VERDICT.set(verdict);
}

/// TEST ONLY: force [`blocks_native_video`] to report blocked, without touching the process-wide
/// `VERDICT` `OnceLock` — a real boot sets that exactly once via the port's probe, and no test can
/// re-init it to exercise the blocked path. Mirrors `ffi_host.rs`'s `FORCE_*` controls. There is no
/// separate "release" call: `false` is the default, so a test that sets this to `true` must reset
/// it to `false` before returning, under `crate::testlock::serial()`.
#[cfg(test)]
pub(crate) static FORCE_BLOCKED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True only when this session must not attempt a native video Load: the SoC matched the
/// affected family AND `/dev/rtkmem` was missing from this jail. The community-tier evidence it is
/// built on is in the webOS port's `probe_jail` section doc. `player::mod` calls this before the
/// first Load of every session.
///
/// Reads with a plain `get()` (never `get_or_init`) so a call that races ahead of the port's
/// probe sees `NotApplicable` for itself but leaves the cell EMPTY — a later [`publish`] can still
/// `set()` the real verdict, and every subsequent call observes it. `get_or_init` would instead
/// latch `NotApplicable` permanently on that first early call, silently discarding the real
/// probe's `set()` and leaving the `devjail: … rtkmem=missing` log line contradicted by a gate
/// that reports not-blocked forever.
pub(crate) fn blocks_native_video() -> bool {
    #[cfg(test)]
    if FORCE_BLOCKED.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    blocks(&VERDICT)
}

/// PURE half of [`blocks_native_video`]'s verdict, taking the cell as a parameter — so the
/// early-read-vs-later-write race it exists to guard against can be tested against a throwaway
/// local `OnceLock`, rather than by writing into the real process-wide `VERDICT` (which a test can
/// never un-set, and which every later test's `blocks_native_video()` call also reads).
fn blocks(cell: &OnceLock<Verdict>) -> bool {
    matches!(
        cell.get().copied().unwrap_or(Verdict::NotApplicable),
        Verdict::Missing
    )
}

/// The closed-enum sandbox fact for every telemetry event — `ok` / `missing` / `n/a` — read from
/// the SAME cached verdict [`blocks_native_video`] gates playback on, never a second probe of
/// `/dev/rtkmem`. An unset cell (a call racing ahead of boot's probe, the same race
/// [`blocks_native_video`]'s doc describes) reads as `n/a` — the same fallback that function uses,
/// so a telemetry event and the gate it would have been diagnosing this attempt's failure against
/// can never disagree about what this jail carries.
pub(crate) fn context() -> &'static str {
    match VERDICT.get().copied().unwrap_or(Verdict::NotApplicable) {
        Verdict::NotApplicable => "n/a",
        Verdict::Ok => "ok",
        Verdict::Missing => "missing",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    StartFailed,
    HbcUnavailable,
    NotRoot,
    CommandFailed,
    // The simulator has no LS2 timeout; development fixtures and tests still construct it.
    // `devtriggers` is irrelevant here: hostsim's `call_hbc` stub never constructs this variant
    // either way, so it is dead outside `#[cfg(test)]` in every hostsim build, not only when
    // `devtriggers` happens to be off.
    #[cfg_attr(all(feature = "hostsim", not(test)), expect(dead_code))]
    Timeout,
    Unreadable,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Running,
    Repaired,
    Failed(Failure),
}

/// Called only by the PlayerAdapter worker after the owner accepts explicit confirmation.
pub(crate) fn repair() -> Result<(), Failure> {
    (super::port().repair_sandbox)()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for `rtkmem-verdict-defeated-by-get-or-init-default`: an early call to
    /// `blocks_native_video()` (before the port's probe has ever run) must NOT permanently latch
    /// `NotApplicable` into the cell. With the old `get_or_init(|| NotApplicable)` reader this
    /// first call would win the race and every later `VERDICT.set(Missing)` from the real probe
    /// would be silently dropped (`OnceLock::set` returns `Err` once already initialized) — the
    /// gate would report "not blocked" forever while the boot log said `rtkmem=missing`.
    ///
    /// Exercised against a throwaway local cell via `blocks`, not the process-wide `VERDICT`
    /// static — that `OnceLock` can never be un-set, so writing into the real one here would
    /// permanently latch `blocks_native_video()` to `true` for every other test in this process,
    /// un-serialized. See finding
    /// `rtkmem-probe-test-permanently-sets-the-process-wide-oncelock`.
    #[test]
    fn early_read_does_not_defeat_a_later_missing_verdict() {
        let cell: OnceLock<Verdict> = OnceLock::new();
        assert!(!blocks(&cell), "cell must start empty/unset");
        cell.set(Verdict::Missing)
            .expect("cell was still empty, so this must be the first, winning set()");
        assert!(
            blocks(&cell),
            "a verdict set after an earlier read must still take effect"
        );
    }
}
