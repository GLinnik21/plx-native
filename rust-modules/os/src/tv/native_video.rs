//! Whether this set may start a native video Load, and the user-confirmed repair of a refusal. The
//! platform's boot probe publishes the verdict here and the player gates on it. webOS answers it
//! by probing `/dev/rtkmem`; a platform with nothing to ask publishes nothing, and the verdict is
//! then `NotApplicable`.
use std::sync::OnceLock;

/// What the boot-time probe found, cached for the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The platform probed and the video path is there. On webOS: the SoC matched the affected
    /// family and `/dev/rtkmem` is readable.
    Available,
    /// The platform probed and the video path is not usable here, so a Load must not be attempted.
    /// The reason is a constant word the platform names; it is printed, in the log and as the
    /// report's context ([`context`]), and no shared code branches on it. On webOS: the SoC matched
    /// the affected family and `/dev/rtkmem` is missing (or unreadable) from this jail, reason
    /// `"missing"`.
    Blocked(&'static str),
    /// There is nothing to ask: nothing was probed, or the platform's probe found the set outside
    /// the family it asks about. On webOS: the SoC did not match, so `/dev/rtkmem` was never probed.
    NotApplicable,
}

static VERDICT: OnceLock<Verdict> = OnceLock::new();

/// The boot probe's verdict, once per boot. First write wins, like the `OnceLock` it lands in.
pub fn publish(verdict: Verdict) {
    let _ = VERDICT.set(verdict);
}

/// TEST ONLY: force [`blocked`] to report blocked, without touching the process-wide `VERDICT`
/// `OnceLock` — a real boot sets that exactly once via the platform's probe, and no test can
/// re-init it to exercise the blocked path. Mirrors `ffi_host.rs`'s `FORCE_*` controls. There is no
/// separate "release" call: `false` is the default, so a test that sets this to `true` must reset
/// it to `false` before returning, under `plx_base::testlock::serial()`.
#[cfg(any(test, feature = "test-support"))]
pub static FORCE_BLOCKED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True only when this session must not attempt a native video Load: the platform's probe found
/// the video path unusable ([`Verdict::Blocked`]). On webOS that is the SoC matching the affected
/// family AND `/dev/rtkmem` missing from this jail; the community-tier evidence it is built on is
/// in the webOS port's `probe_jail` section doc. `player::mod` calls this before the first Load of
/// every session.
///
/// Reads with a plain `get()` (never `get_or_init`) so a call that races ahead of the platform's
/// probe sees `NotApplicable` for itself but leaves the cell EMPTY — a later [`publish`] can still
/// `set()` the real verdict, and every subsequent call observes it. `get_or_init` would instead
/// latch `NotApplicable` permanently on that first early call, silently discarding the real
/// probe's `set()` and leaving the `devjail: … rtkmem=missing` log line contradicted by a gate
/// that reports not-blocked forever.
pub fn blocked() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if FORCE_BLOCKED.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    blocks(&VERDICT)
}

/// PURE half of [`blocked`]'s verdict, taking the cell as a parameter — so the
/// early-read-vs-later-write race it exists to guard against can be tested against a throwaway
/// local `OnceLock`, rather than by writing into the real process-wide `VERDICT` (which a test can
/// never un-set, and which every later test's `blocked()` call also reads).
fn blocks(cell: &OnceLock<Verdict>) -> bool {
    matches!(verdict_of(cell), Verdict::Blocked(_))
}

/// What `cell` holds, or [`Verdict::NotApplicable`] when nothing has been published: the one
/// fallback [`blocked`] and [`context`] share, so the gate and the report cannot disagree about a
/// call that raced ahead of the probe.
fn verdict_of(cell: &OnceLock<Verdict>) -> Verdict {
    cell.get().copied().unwrap_or(Verdict::NotApplicable)
}

/// The closed word for a verdict: `ok` for [`Verdict::Available`], `n/a` for
/// [`Verdict::NotApplicable`] and the platform's own reason for [`Verdict::Blocked`] (`missing` on
/// webOS). The vocabulary the telemetry schema and `PRIVACY.md` document.
fn word(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Available => "ok",
        Verdict::NotApplicable => "n/a",
        Verdict::Blocked(reason) => reason,
    }
}

/// The closed native-video fact for every telemetry event — `ok` / `missing` / `n/a` on webOS — read
/// from the SAME cached verdict [`blocked`] gates playback on, never a second probe of the device.
/// An unset cell (a call racing ahead of boot's probe, the same race [`blocked`]'s doc describes)
/// reads as `n/a` — the same fallback that function uses, so a telemetry event and the gate it
/// would have been diagnosing this attempt's failure against can never disagree about what this
/// jail carries.
pub fn context() -> &'static str {
    word(verdict_of(&VERDICT))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    StartFailed,
    HbcUnavailable,
    NotRoot,
    CommandFailed,
    // The simulator has no LS2 timeout; development fixtures and tests still construct it. The
    // enum is public to the application crate, so the variant is never dead in this crate.
    Timeout,
    Unreadable,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Running,
    Repaired,
    Failed(Failure),
}

/// Called only by the PlayerAdapter worker after the owner accepts explicit confirmation. A platform
/// with no repair (`Services.identity.repair_native_video` is `None`) answers
/// [`Failure::Unsupported`].
pub fn repair() -> Result<(), Failure> {
    match super::services().identity.repair_native_video {
        Some(hook) => hook(),
        None => Err(Failure::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for `rtkmem-verdict-defeated-by-get-or-init-default`: an early call to
    /// `blocked()` (before the platform's probe has ever run) must NOT permanently latch
    /// `NotApplicable` into the cell. With the old `get_or_init(|| NotApplicable)` reader this
    /// first call would win the race and every later `VERDICT.set(Blocked(..))` from the real probe
    /// would be silently dropped (`OnceLock::set` returns `Err` once already initialized) — the
    /// gate would report "not blocked" forever while the boot log said `rtkmem=missing`.
    ///
    /// Exercised against a throwaway local cell via `blocks`, not the process-wide `VERDICT`
    /// static — that `OnceLock` can never be un-set, so writing into the real one here would
    /// permanently latch `blocked()` to `true` for every other test in this process,
    /// un-serialized. See finding
    /// `rtkmem-probe-test-permanently-sets-the-process-wide-oncelock`.
    #[test]
    fn early_read_does_not_defeat_a_later_blocked_verdict() {
        let cell: OnceLock<Verdict> = OnceLock::new();
        assert!(!blocks(&cell), "cell must start empty/unset");
        cell.set(Verdict::Blocked("missing"))
            .expect("cell was still empty, so this must be the first, winning set()");
        assert!(
            blocks(&cell),
            "a verdict set after an earlier read must still take effect"
        );
    }

    /// The three verdicts, mapped: only a blocked one blocks, and an unpublished cell reads as not
    /// applicable for both the gate and the report.
    #[test]
    fn only_a_blocked_verdict_blocks_and_an_unpublished_one_is_not_applicable() {
        for (verdict, blocks_load) in [
            (Verdict::Available, false),
            (Verdict::NotApplicable, false),
            (Verdict::Blocked("missing"), true),
        ] {
            let cell = OnceLock::new();
            cell.set(verdict).unwrap();
            assert_eq!(blocks(&cell), blocks_load, "{verdict:?}");
        }
        let unset: OnceLock<Verdict> = OnceLock::new();
        assert_eq!(verdict_of(&unset), Verdict::NotApplicable);
        assert!(!blocks(&unset));
    }

    /// The words telemetry, the log and `PRIVACY.md` carry for the jail pre-flight. A blocked
    /// verdict reads as the reason its platform named, which on webOS is `missing`.
    #[test]
    fn the_context_words_are_ok_missing_and_not_applicable() {
        assert_eq!(word(Verdict::Available), "ok");
        assert_eq!(word(Verdict::Blocked("missing")), "missing");
        assert_eq!(word(Verdict::NotApplicable), "n/a");
    }
}
