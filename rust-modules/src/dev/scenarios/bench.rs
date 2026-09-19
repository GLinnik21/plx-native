//! The stress-bench oscillators' own state — counted, deterministic twins of `navosc`/`modalosc`
//! (`/tmp/plxnative-pushbench[=<n>[,<ratingKey>]]`, `/tmp/plxnative-modalbench[=<n>]`). Where
//! those two bounce forever so a device FPS scene can sample a settled ramp, a bench runs a FIXED
//! `n` (default 100) push→settle→pop or present→settle→dismiss cycles through the real bridge
//! entry points, rotating a target list, logging one `bench:` event-log line per cycle
//! (`super::bench_frame_tick`'s doc has the wire format), and then stopping — the harness grades
//! the whole run, not a sampled window of an unbounded one.
//!
//! **This module is the pure half.** [`BenchClock`]/[`bench_advance`]/[`bench_target_index`] know
//! nothing about `App`, a bridge call, or a target's name — they are driven by a `now: u32` the
//! caller supplies, which is what lets the unit tests below drive a whole run with a fake clock in
//! a few milliseconds instead of a live loop. The impure half — which bridge call each target
//! opens/closes, the frame-time accumulation, the `bench:`/`done` log lines — is
//! `dev::scenarios::push_bench_tick`/`modal_bench_tick`/`bench_frame_tick`, which hold the `&mut
//! App` this module deliberately never sees.

/// Both triggers' default cycle count when `=<n>` is absent or unparseable.
pub(crate) const DEFAULT_BENCH_N: u32 = 100;

/// One bench's own phase: waiting for the next cycle's press, or mid the settle window a cycle
/// measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BenchPhase {
    Waiting,
    Measuring,
}

/// The clock and accumulators shared by both benches — see the module doc. `cycle` counts cycles
/// COMPLETED; the cycle in flight (if `phase == Measuring`) is `cycle`, not `cycle + 1`, so a
/// `bench:` line for "the cycle that just settled" logs `cycle + 1` (1-based, matching `/n`).
pub(crate) struct BenchClock {
    pub(crate) n: u32,
    pub(crate) cycle: u32,
    pub(crate) phase: BenchPhase,
    /// The clock reading `bench_advance` last acted on — the half-period timer.
    pub(crate) last: u32,
    /// The clock reading the in-flight cycle's press happened at, for `dur_ms`.
    pub(crate) cycle_start: u32,
    pub(crate) worst_ms: f64,
    pub(crate) frames: u32,
    /// Set once all `n` cycles are done; `bench_advance` becomes a permanent no-op, which is how
    /// the oscillator "stops and the screen goes idle" (spec) — nothing schedules the next press.
    pub(crate) done: bool,
}

impl BenchClock {
    pub(crate) fn new(n: u32) -> Self {
        Self {
            n,
            cycle: 0,
            phase: BenchPhase::Waiting,
            last: 0,
            cycle_start: 0,
            worst_ms: 0.0,
            frames: 0,
            done: false,
        }
    }
}

/// What [`bench_advance`] wants the impure caller to do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BenchStep {
    Nothing,
    /// Press the target for 0-based cycle `.0` — open/present it and start measuring.
    Start(u32),
    /// Cycle `.0`'s settle window is over — log the accumulated line, then close/dismiss it.
    Settle(u32),
    /// Every cycle ran — log the `done` line and stop.
    Done(u32),
}

/// Is a half-period over? `wrapping_sub` matches every other oscillator's clock arithmetic in
/// this file (`nav_osc_tick`'s `now.wrapping_sub(...) > 1400`) — the loop's `now` is a millisecond
/// counter that is free to wrap on a long-enough soak.
fn bench_due(last: u32, now: u32, period_ms: u32) -> bool {
    now.wrapping_sub(last) > period_ms
}

/// The pure state transition, driven once per frame. `period_ms` is the half-period (the existing
/// oscillators' own 1400/1500 ms), so a full cycle (press, settle, close, settle) takes two calls
/// that return non-`Nothing`, `2 * period_ms` apart.
pub(crate) fn bench_advance(clock: &mut BenchClock, now: u32, period_ms: u32) -> BenchStep {
    if clock.done || !bench_due(clock.last, now, period_ms) {
        return BenchStep::Nothing;
    }
    clock.last = now;
    match clock.phase {
        BenchPhase::Waiting => {
            if clock.cycle >= clock.n {
                clock.done = true;
                BenchStep::Done(clock.n)
            } else {
                clock.phase = BenchPhase::Measuring;
                clock.cycle_start = now;
                clock.worst_ms = 0.0;
                clock.frames = 0;
                BenchStep::Start(clock.cycle)
            }
        }
        BenchPhase::Measuring => {
            let cycle = clock.cycle;
            clock.phase = BenchPhase::Waiting;
            clock.cycle += 1;
            BenchStep::Settle(cycle)
        }
    }
}

/// Which rotation slot a 0-based cycle lands on. `targets_len == 0` cannot happen for either
/// bench (both always have at least one target — the harness-only fallback of a Library-only
/// push bench), but reads as slot 0 rather than panicking if it ever did.
pub(crate) fn bench_target_index(targets_len: usize, cycle: u32) -> usize {
    if targets_len == 0 {
        0
    } else {
        (cycle as usize) % targets_len
    }
}

// =================================================================================================
// push bench (`/tmp/plxnative-pushbench`)
// =================================================================================================

/// The push bench's rotation, in the order it cycles. `Detail`/`Person` are only ever in the
/// rotation when a ratingKey is available — see [`PushBench::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushTarget {
    Detail,
    Person,
    Library,
}

impl PushTarget {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Detail => "detail",
            Self::Person => "person",
            Self::Library => "library",
        }
    }
}

pub(crate) struct PushBench {
    pub(crate) clock: BenchClock,
    /// The Detail leg's ratingKey — `pushbench=<n>,<rk>`, or `navosc`'s own value reused when the
    /// bench's own trigger carried none (spec: "reuse the navosc rk trigger").
    pub(crate) rk: String,
    pub(crate) targets: Vec<PushTarget>,
    /// The Person target's data, opportunistically refreshed from whichever Detail item is
    /// current (`dev::scenarios::push_bench_refresh_person`) — Person has no ratingKey-shaped
    /// trigger of its own, so this is the only door onto it.
    pub(crate) person: Option<(crate::plex::ServerId, String, String, String, String)>,
    /// Which target the MOST RECENT `Start` actually opened — may differ from the rotation's
    /// nominal pick at that cycle when `Person` was chosen but no cast data has landed yet (falls
    /// back to `Library` for that one cycle). `Settle` reads this rather than recomputing the
    /// nominal target, so the logged `target=` always names what really opened.
    pub(crate) opened: PushTarget,
    /// Logged once, the first time a `Person` cycle falls back to `Library` for want of cast data.
    pub(crate) person_fallback_logged: bool,
}

impl PushBench {
    pub(crate) fn new(n: u32, rk: String) -> Self {
        let targets = if rk.is_empty() {
            crate::log(
                "bench: pushbench has no ratingKey (pushbench=<n>,<rk> or navosc=<rk>) — \
                 rotating Library only, Detail/Person skipped",
            );
            vec![PushTarget::Library]
        } else {
            vec![PushTarget::Detail, PushTarget::Person, PushTarget::Library]
        };
        Self {
            clock: BenchClock::new(n),
            rk,
            targets,
            person: None,
            opened: PushTarget::Library,
            person_fallback_logged: false,
        }
    }
}

// =================================================================================================
// modal bench (`/tmp/plxnative-modalbench`)
// =================================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModalTarget {
    Settings,
    AccountMenu,
    ItemMenu,
    About,
}

impl ModalTarget {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::AccountMenu => "account-menu",
            Self::ItemMenu => "item-menu",
            Self::About => "about",
        }
    }
}

pub(crate) struct ModalBench {
    pub(crate) clock: BenchClock,
    /// The item menu leg's ratingKey. `plxnative-modalbench=<n>,<rk>` carries it directly; an
    /// empty value falls back to `navosc`'s own ratingKey exactly as the push bench's Detail leg
    /// does, via `app::boot::boot` — see `modalbench_value`'s doc for why a scene that wants the
    /// item menu without navosc's own competing bounce should prefer the direct form.
    pub(crate) rk: String,
    pub(crate) targets: Vec<ModalTarget>,
}

impl ModalBench {
    /// **Library menu and Filmography are not in the rotation, deliberately.** Both present only
    /// over a page this bench does not otherwise visit: the library menu needs a live, MOUNTED
    /// Library page instance to hang its `SectionAddress`/`InstanceId` off — a push concern, the
    /// push bench's Library leg's, not a modal-only bench's — and Filmography needs a live Person
    /// page, itself gated on a Detail item's cast data exactly as the push bench's own Person leg
    /// is. Folding either dependency chain in here would make "the modal ramp" measure page setup
    /// cost as well as the present/dismiss transition the bench exists to grade. Both are named in
    /// the spec as "if reachable"; this is the call that neither is, cleanly, from a modal-only
    /// bench — see the AGENTS.md report for the full account.
    pub(crate) fn new(n: u32, rk: String) -> Self {
        let mut targets = vec![ModalTarget::Settings, ModalTarget::AccountMenu, ModalTarget::About];
        if rk.is_empty() {
            crate::log(
                "bench: modalbench has no ratingKey (modalbench=<n>,<rk> or reuse navosc=<rk>) \
                 — item menu skipped from rotation",
            );
        } else {
            targets.insert(2, ModalTarget::ItemMenu);
        }
        crate::log(
            "bench: modalbench skips library menu (needs a live, mounted Library page instance) \
             and filmography (needs a live Person page, itself gated on Detail cast data) — both \
             are push-navigation dependencies a modal-only bench should not carry, see ModalBench::new's doc",
        );
        Self { clock: BenchClock::new(n), rk, targets }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a `BenchClock` with a fake, hand-stepped clock and returns every non-`Nothing` step
    /// in order — the shape both state-machine tests below share.
    fn run(n: u32, period_ms: u32, iterations: u32) -> Vec<BenchStep> {
        let mut clock = BenchClock::new(n);
        let mut now = 0u32;
        let mut steps = Vec::new();
        for _ in 0..iterations {
            now += period_ms + 1;
            let step = bench_advance(&mut clock, now, period_ms);
            if step != BenchStep::Nothing {
                steps.push(step);
            }
        }
        steps
    }

    #[test]
    fn a_bench_clock_emits_exactly_n_start_settle_pairs_then_one_done_and_stops() {
        let steps = run(3, 1400, 12);
        assert_eq!(
            steps,
            vec![
                BenchStep::Start(0),
                BenchStep::Settle(0),
                BenchStep::Start(1),
                BenchStep::Settle(1),
                BenchStep::Start(2),
                BenchStep::Settle(2),
                BenchStep::Done(3),
            ],
            "n=3 must emit exactly 3 Start/Settle pairs, in cycle order, then one Done"
        );
        // Nothing schedules another press once done — the oscillator has genuinely stopped, not
        // just refused to log, which is the "screen goes idle" half of the spec.
        let mut clock = BenchClock::new(1);
        let mut now = 0u32;
        // Start(0), then Settle(0), then the Waiting-phase call that discovers cycle >= n and
        // turns into Done — three half-periods for n=1, matching `run`'s own count above.
        for _ in 0..3 {
            now += 1401;
            bench_advance(&mut clock, now, 1400);
        }
        assert!(clock.done);
        for _ in 0..50 {
            now += 1401;
            assert_eq!(bench_advance(&mut clock, now, 1400), BenchStep::Nothing);
        }
    }

    #[test]
    fn bench_clock_never_fires_early_and_never_double_fires_inside_one_half_period() {
        let mut clock = BenchClock::new(5);
        // Repeated calls inside the SAME half-period must not advance the phase twice.
        assert_eq!(bench_advance(&mut clock, 100, 1400), BenchStep::Nothing);
        assert_eq!(bench_advance(&mut clock, 1400, 1400), BenchStep::Nothing, "exactly at the boundary is not yet due");
        let started = bench_advance(&mut clock, 1401, 1400);
        assert_eq!(started, BenchStep::Start(0));
        assert_eq!(bench_advance(&mut clock, 1500, 1400), BenchStep::Nothing, "already measuring this half-period");
        assert_eq!(bench_advance(&mut clock, 2801, 1400), BenchStep::Nothing, "boundary again");
        assert_eq!(bench_advance(&mut clock, 2802, 1400), BenchStep::Settle(0));
    }

    #[test]
    fn push_bench_targets_rotate_through_every_listed_target_in_order() {
        let targets = [PushTarget::Detail, PushTarget::Person, PushTarget::Library];
        let got: Vec<PushTarget> = (0..9)
            .map(|cycle| targets[bench_target_index(targets.len(), cycle)])
            .collect();
        assert_eq!(
            got,
            vec![
                PushTarget::Detail, PushTarget::Person, PushTarget::Library,
                PushTarget::Detail, PushTarget::Person, PushTarget::Library,
                PushTarget::Detail, PushTarget::Person, PushTarget::Library,
            ]
        );
    }

    #[test]
    fn push_bench_without_a_ratingkey_rotates_library_only() {
        let bench = PushBench::new(10, String::new());
        assert_eq!(bench.targets, vec![PushTarget::Library]);
        for cycle in 0..10 {
            assert_eq!(bench.targets[bench_target_index(bench.targets.len(), cycle)], PushTarget::Library);
        }
    }

    #[test]
    fn push_bench_with_a_ratingkey_rotates_all_three_targets() {
        let bench = PushBench::new(10, "12345".into());
        assert_eq!(bench.targets, vec![PushTarget::Detail, PushTarget::Person, PushTarget::Library]);
    }

    #[test]
    fn modal_bench_targets_rotate_through_every_listed_target_in_order() {
        let bench = ModalBench::new(8, "12345".into());
        assert_eq!(
            bench.targets,
            vec![ModalTarget::Settings, ModalTarget::AccountMenu, ModalTarget::ItemMenu, ModalTarget::About]
        );
        let got: Vec<ModalTarget> = (0..8)
            .map(|cycle| bench.targets[bench_target_index(bench.targets.len(), cycle)])
            .collect();
        assert_eq!(
            got,
            vec![
                ModalTarget::Settings, ModalTarget::AccountMenu, ModalTarget::ItemMenu, ModalTarget::About,
                ModalTarget::Settings, ModalTarget::AccountMenu, ModalTarget::ItemMenu, ModalTarget::About,
            ]
        );
    }

    #[test]
    fn modal_bench_without_a_ratingkey_skips_item_menu() {
        let bench = ModalBench::new(8, String::new());
        assert_eq!(bench.targets, vec![ModalTarget::Settings, ModalTarget::AccountMenu, ModalTarget::About]);
        assert!(!bench.targets.contains(&ModalTarget::ItemMenu));
    }
}
