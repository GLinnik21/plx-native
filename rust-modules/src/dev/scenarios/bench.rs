//! The stress-bench oscillators' own state — counted, deterministic twins of `navosc`/`modalosc`
//! (`/tmp/plxnative-pushbench[=<n>[,<ratingKey>]]`, `/tmp/plxnative-modalbench[=<n>]`). Where
//! those two bounce forever so a device FPS scene can sample a settled ramp, a bench runs a FIXED
//! `n` (default 100) push→settle→pop or present→settle→dismiss cycles through the real bridge
//! entry points, rotating a target list, logging one `bench:` event-log line per cycle
//! (`super::bench_frame_tick`'s doc has the wire format), and then stopping — the harness grades
//! the whole run, not a sampled window of an unbounded one.
//!
//! A third bench, [`DeepBench`] (`/tmp/plxnative-deepbench[=<depth>[,<ratingKey>]]`), does not
//! round-trip: it pushes `depth` pages with no pop in between, then pops all the way back to the
//! root one page at a time, so the harness can grade whether a page transition's cost or a
//! session's memory holds flat as the real nav stack goes ninety-plus entries deep rather than
//! the shallow depth-1↔2 churn `PushBench` measures. Each PUSH or POP is its own `Start`/`Settle`
//! pair — one nav op per pair, not a round trip — so it reuses [`BenchClock`]/[`bench_advance`]
//! unchanged with `n = 2 * depth`.
//!
//! **This module is the pure half.** [`BenchClock`]/[`bench_advance`]/[`bench_target_index`]/
//! [`deep_step`] know nothing about `App`, a bridge call, or a target's name — they are driven by
//! a `now: u32` the caller supplies, which is what lets the unit tests below drive a whole run
//! with a fake clock in a few milliseconds instead of a live loop. The impure half — which bridge
//! call each target opens/closes, the frame-time accumulation, the `bench:`/`done` log lines — is
//! `dev::scenarios::push_bench_tick`/`modal_bench_tick`/`deep_bench_tick`/`bench_frame_tick`,
//! which hold the `&mut App` this module deliberately never sees.

/// All three triggers' default cycle count (`DeepBench`'s own `depth`, same number) when `=<n>`
/// is absent or unparseable.
pub(crate) const DEFAULT_BENCH_N: u32 = 100;

/// One 60 Hz panel refresh — the unit every frame-accounting field below is graded in.
pub(crate) const REFRESH_MS: f64 = 1000.0 / 60.0;

/// Before its FIRST cycle a bench waits until the screen has presented nothing for this long:
/// boot's own landings (the hub fetch, the first poster wave, font and shader first use) are over
/// and the root page is at rest. Without it cycle 1 fired 1400 ms after the first frame, before
/// Home's hubs had even arrived, and graded boot as if it were a push (measured 2026-09-28: the
/// `hubs: landed` line printed INSIDE cycle 1). Every other FPS scene excludes boot through
/// `warmup_s`; a bench, which grades every cycle, has to exclude it here.
pub(crate) const SETTLE_QUIET_MS: u32 = 1000;
/// …but a root page that never goes still (a perpetual animation) must not park the bench
/// forever: past this long after arming it starts anyway, and says so.
pub(crate) const SETTLE_CAP_MS: u32 = 30_000;

/// One bench's own phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BenchPhase {
    /// Armed, waiting for boot to settle — see [`SETTLE_QUIET_MS`].
    Settling,
    /// Between cycles: nothing is measured.
    Waiting,
    /// The OPEN half: from the press to the settle point.
    Measuring,
    /// The CLOSE half of a round-trip bench: from the close to the report. A one-way bench (deep)
    /// never enters it.
    Closing,
}

/// One half-cycle's presented-frame accounting.
///
/// **Why `missed`, and not the largest Top→Swap, is the drop count.** On this driver a presented
/// frame blocks for a free buffer inside its own first framebuffer-0 command (`clear` in the
/// `FRAMEDROP` spans), so a frame's Top→Swap includes the wait for the next vsync: a steady 60 fps
/// animation reads ~16.7 ms per frame by construction, and a frame whose own work is 5 ms reads
/// 16.7 + 5 − (the previous frame's work) — 20–23 ms — without the panel ever showing a picture
/// twice. What the panel sees is how many refreshes passed between two presents: an interval of
/// `k` refreshes showed the earlier picture `k − 1` extra times. Each interval is rounded to the
/// nearest refresh on its own, so anything past 1.5 refreshes (25 ms) counts — a catch-up frame
/// that follows a late one never cancels it out — and a frame that took two refreshes counts
/// whether its time went to the CPU or the GPU: a GPU-bound frame delays the next buffer release,
/// so the CPU's own present cadence carries it.
///
/// The first frame after an idle gap has no interval — the panel was holding a still picture. Its
/// own Top→Swap (no vsync wait in it: a buffer is free after idle) is the latency before the first
/// new picture, rounded the same way: a first frame that took two refreshes of work is a late
/// response and counts as one. `first_ms` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HalfStats {
    pub(crate) frames: u32,
    /// Top→Swap of this half's FIRST presented frame — the frame that commits the nav op.
    pub(crate) first_ms: f64,
    pub(crate) worst_ms: f64,
    /// 1-based index (within the half) of the frame that set `worst_ms`.
    pub(crate) worst_at: u32,
    /// The longest present-to-present interval inside the half.
    pub(crate) iv_max_ms: f64,
    /// Refreshes that repeated a picture or arrived late (see the type's doc).
    pub(crate) missed: u32,
    /// Loop iterations the capture fence deferred BEFORE this half's first presented frame: the
    /// input-to-first-frame latency (one capture per modal open, by design), reported here and
    /// kept out of `missed`. A deferral AFTER the first present is not counted here: the
    /// heartbeat keeps the present-to-present interval across it, so it is in `iv_max_ms` and
    /// `missed` already.
    pub(crate) defer_first: u32,
}

/// A deferral is a loop iteration that wanted to present but was held by the capture fence
/// (`run.rs`'s `Frame::deferred`), unlike an idle skip, which had nothing to show.
impl HalfStats {
    pub(crate) fn note_defer(&mut self) {
        if self.frames == 0 {
            self.defer_first += 1;
        }
    }
}

/// Refreshes beyond the first that `span_ms` covered: 0 up to 1.5 refreshes, 1 up to 2.5, …
pub(crate) fn missed_refreshes(span_ms: f64) -> u32 {
    ((span_ms / REFRESH_MS).round() as u32).saturating_sub(1)
}

impl HalfStats {
    /// One presented frame: `total_ms` its Top→Swap, `interval_ms` its present-to-present interval
    /// (`None` for the first frame after an idle gap).
    pub(crate) fn note(&mut self, total_ms: f64, interval_ms: Option<f64>) {
        self.frames += 1;
        if self.frames == 1 {
            self.first_ms = total_ms;
        }
        if total_ms > self.worst_ms {
            self.worst_ms = total_ms;
            self.worst_at = self.frames;
        }
        // A first frame that followed a deferral measured its wait as latency (`defer_first`); its
        // interval, if the heartbeat carried one, is not a drop.
        let interval_ms = if self.frames == 1 && self.defer_first > 0 { None } else { interval_ms };
        if let Some(iv) = interval_ms {
            self.iv_max_ms = self.iv_max_ms.max(iv);
        }
        self.missed += missed_refreshes(interval_ms.unwrap_or(total_ms));
    }

/// `first:<ms>,worst:<ms>@<i>,iv:<ms>,missed:<n>,defer_first:<n>` — one half's attribution on a `bench:` line.
    pub(crate) fn field(&self) -> String {
        format!(
            "first:{:.1},worst:{:.1}@{},iv:{:.1},missed:{},defer_first:{}",
            self.first_ms, self.worst_ms, self.worst_at, self.iv_max_ms, self.missed,
            self.defer_first
        )
    }
}

/// The clock and accumulators shared by every bench — see the module doc. `cycle` counts cycles
/// COMPLETED; the cycle in flight is `cycle`, not `cycle + 1`, so a `bench:` line for "the cycle
/// that just finished" logs `cycle + 1` (1-based, matching `/n`).
pub(crate) struct BenchClock {
    pub(crate) n: u32,
    pub(crate) cycle: u32,
    pub(crate) phase: BenchPhase,
    /// Does a cycle's close (a pop, a dismiss) belong to the cycle and get measured? True for the
    /// push and modal benches, whose cycle IS a round trip; false for deep, whose every step is
    /// one nav op measured on its own.
    pub(crate) round_trip: bool,
    /// The clock reading `bench_advance` last acted on — the half-period timer.
    pub(crate) last: u32,
    /// The clock reading the in-flight cycle's press happened at, for `dur_ms`.
    pub(crate) cycle_start: u32,
    /// First `bench_advance` reading, and the latest presented frame's — the settle gate's inputs.
    pub(crate) armed_at: Option<u32>,
    pub(crate) last_present: Option<u32>,
    pub(crate) open: HalfStats,
    pub(crate) close: HalfStats,
    /// Set once all `n` cycles are done; `bench_advance` becomes a permanent no-op, which is how
    /// the oscillator "stops and the screen goes idle" (spec) — nothing schedules the next press.
    pub(crate) done: bool,
}

impl BenchClock {
    /// A one-way bench (every cycle one measured op): `deep`.
    pub(crate) fn new(n: u32) -> Self {
        Self {
            n,
            cycle: 0,
            phase: BenchPhase::Settling,
            round_trip: false,
            last: 0,
            cycle_start: 0,
            armed_at: None,
            last_present: None,
            open: HalfStats::default(),
            close: HalfStats::default(),
            done: false,
        }
    }

    /// A round-trip bench (open, settle, close, settle — both halves measured): push, modal.
    pub(crate) fn round_trip(n: u32) -> Self {
        Self { round_trip: true, ..Self::new(n) }
    }

    /// The cycle's worst Top→Swap over both halves (the `worst_ms=` field).
    pub(crate) fn worst_ms(&self) -> f64 {
        self.open.worst_ms.max(self.close.worst_ms)
    }

    /// Presented frames over both halves (the `frames=` field).
    pub(crate) fn frames(&self) -> u32 {
        self.open.frames + self.close.frames
    }

    /// The graded tail every `bench:` line carries after its fixed prefix:
    /// `first_ms=<max of the halves' first frames> missed=<both halves> open=<…> [close=<…>]`.
    pub(crate) fn fields(&self) -> String {
        let mut s = format!(
            "first_ms={:.1} missed={} open={}",
            self.open.first_ms.max(self.close.first_ms),
            self.open.missed + self.close.missed,
            self.open.field()
        );
        if self.round_trip {
            s.push_str(&format!(" close={}", self.close.field()));
        }
        s
    }
}

/// What [`bench_advance`] wants the impure caller to do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BenchStep {
    Nothing,
    /// Boot has settled (or the cap ran out: `.1`) after `.0` ms — log it once; the first press
    /// follows one half-period later.
    Settled(u32, bool),
    /// Press the target for 0-based cycle `.0` — open/present it and start measuring.
    Start(u32),
    /// Cycle `.0`'s open half is over. One-way: log the line. Round trip: close/dismiss the
    /// target — the close half is measured and logged at [`BenchStep::Report`].
    Settle(u32),
    /// A round trip's cycle `.0` is over, both halves measured — log its line. The next cycle's
    /// press follows on the next frame.
    Report(u32),
    /// Every cycle ran — log the `done` line and stop.
    Done(u32),
}

/// Is a half-period over? `wrapping_sub` matches every other oscillator's clock arithmetic in
/// this file (`nav_osc_tick`'s `now.wrapping_sub(...) > 1400`) — the loop's `now` is a millisecond
/// counter that is free to wrap on a long-enough soak.
fn bench_due(last: u32, now: u32, period_ms: u32) -> bool {
    now.wrapping_sub(last) > period_ms
}

/// Fold one PRESENTED frame into the clock: every present feeds the settle gate, and a frame inside
/// a measured half feeds that half ([`HalfStats::note`]).
pub(crate) fn bench_note_frame(clock: &mut BenchClock, now: u32, total_ms: f64, interval_ms: Option<f64>) {
    clock.last_present = Some(now);
    match clock.phase {
        BenchPhase::Measuring => clock.open.note(total_ms, interval_ms),
        BenchPhase::Closing => clock.close.note(total_ms, interval_ms),
        BenchPhase::Settling | BenchPhase::Waiting => {}
    }
}

/// Fold one DEFERRED iteration (the capture fence held a present the loop wanted) into the half in
/// flight. Only deferrals before the half's first present need recording; later ones are in the
/// next present's interval.
pub(crate) fn bench_note_defer(clock: &mut BenchClock) {
    match clock.phase {
        BenchPhase::Measuring => clock.open.note_defer(),
        BenchPhase::Closing => clock.close.note_defer(),
        BenchPhase::Settling | BenchPhase::Waiting => {}
    }
}

/// The pure state transition, driven once per frame. `period_ms` is the half-period (the existing
/// oscillators' own 1400/1500 ms). A one-way cycle is Start → Settle; a round trip is Start →
/// Settle (close) → Report, the next Start one frame after the Report.
pub(crate) fn bench_advance(clock: &mut BenchClock, now: u32, period_ms: u32) -> BenchStep {
    if clock.done {
        return BenchStep::Nothing;
    }
    if clock.phase == BenchPhase::Settling {
        let armed = *clock.armed_at.get_or_insert(now);
        let waited = now.wrapping_sub(armed);
        let quiet = clock.last_present.is_some_and(|p| now.wrapping_sub(p) >= SETTLE_QUIET_MS);
        let capped = waited >= SETTLE_CAP_MS;
        if !(quiet || capped) {
            return BenchStep::Nothing;
        }
        clock.phase = BenchPhase::Waiting;
        clock.last = now;
        return BenchStep::Settled(waited, !quiet);
    }
    if !bench_due(clock.last, now, period_ms) {
        return BenchStep::Nothing;
    }
    clock.last = now;
    match clock.phase {
        BenchPhase::Settling => unreachable!("handled above"),
        BenchPhase::Waiting => {
            if clock.cycle >= clock.n {
                clock.done = true;
                BenchStep::Done(clock.n)
            } else {
                clock.phase = BenchPhase::Measuring;
                clock.cycle_start = now;
                clock.open = HalfStats::default();
                clock.close = HalfStats::default();
                BenchStep::Start(clock.cycle)
            }
        }
        BenchPhase::Measuring => {
            let cycle = clock.cycle;
            if clock.round_trip {
                clock.phase = BenchPhase::Closing;
            } else {
                clock.phase = BenchPhase::Waiting;
                clock.cycle += 1;
            }
            BenchStep::Settle(cycle)
        }
        BenchPhase::Closing => {
            let cycle = clock.cycle;
            clock.phase = BenchPhase::Waiting;
            clock.cycle += 1;
            // Due again on the very next frame: the report IS the end of the close half's settle,
            // so a further half-period of nothing would only lengthen the run.
            clock.last = now.wrapping_sub(period_ms);
            BenchStep::Report(cycle)
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
    pub(crate) person: Option<(plx_plex::plex::ServerId, String, String, String, String)>,
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
            plx_base::eventlog::log(
                "bench: pushbench has no ratingKey (pushbench=<n>,<rk> or navosc=<rk>) — \
                 rotating Library only, Detail/Person skipped",
            );
            vec![PushTarget::Library]
        } else {
            vec![PushTarget::Detail, PushTarget::Person, PushTarget::Library]
        };
        Self {
            clock: BenchClock::round_trip(n),
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
            plx_base::eventlog::log(
                "bench: modalbench has no ratingKey (modalbench=<n>,<rk> or reuse navosc=<rk>) \
                 — item menu skipped from rotation",
            );
        } else {
            targets.insert(2, ModalTarget::ItemMenu);
        }
        plx_base::eventlog::log(
            "bench: modalbench skips library menu (needs a live, mounted Library page instance) \
             and filmography (needs a live Person page, itself gated on Detail cast data) — both \
             are push-navigation dependencies a modal-only bench should not carry, see ModalBench::new's doc",
        );
        Self { clock: BenchClock::round_trip(n), rk, targets }
    }
}

// =================================================================================================
// deep bench (`/tmp/plxnative-deepbench`)
// =================================================================================================

/// One step's direction in [`DeepBench`]'s single walk down and back up the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeepDir {
    Push,
    Pop,
}

impl DeepDir {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Pop => "pop",
        }
    }
}

/// **The pure half of a DEEP step**: which direction 0-based `cycle` is, and the stack depth it
/// leaves behind assuming every push really adds one entry and every pop really removes one — which
/// is exactly what [`DeepBench`]'s own `stack: Vec<PushTarget>` gives it regardless of which target
/// a `Person` step actually opened (a cast-data fallback changes WHAT was pushed, never how deep).
/// A pure test can therefore check the whole depth trajectory without a live `App`.
pub(crate) fn deep_step(depth: u32, cycle: u32) -> (DeepDir, u32) {
    if cycle < depth {
        (DeepDir::Push, cycle + 1)
    } else {
        let pop_index = cycle - depth;
        (DeepDir::Pop, depth - 1 - pop_index)
    }
}

pub(crate) struct DeepBench {
    /// `n = 2 * depth` — one `Start`/`Settle` pair per PUSH (cycles `0..depth`) and one per POP
    /// (cycles `depth..2*depth`). Unlike `PushBench`'s cycle (an open-then-close round trip), each
    /// cycle here performs exactly ONE nav op and settles it — see the module doc.
    pub(crate) clock: BenchClock,
    /// How many pages deep the push half goes before the walk turns around. `0` when the trigger
    /// carried no ratingKey — see [`Self::new`]: neither `Detail` nor `Person` can open without
    /// one, and `Library` (see `targets`' doc) cannot stand in for them here.
    pub(crate) depth: u32,
    pub(crate) rk: String,
    /// The push half's rotation — **Detail and Person only.** `Library`, `PushBench`'s third leg,
    /// is deliberately excluded: it opens through `app::bridge::nav_tab` → `nav_peer` →
    /// `NavOp::SelectTab`, whose `NavStack::apply` arm retires EVERY entry above the root and
    /// mints at most one new one over it — a peer swap, not a stack push (`ui/containers/stack.rs`).
    /// Rotating it into a walk that is supposed to grow by one entry every step would not deepen
    /// the stack at all past that step: it would silently collapse whatever this bench had built
    /// back to depth <= 2, and every entry that swap retired leaves `NavStack::entries` for good —
    /// so the pop half's later `nav_pop` calls would not even be popping the pages this bench
    /// thinks it pushed. `PushBench` can afford the peer swap only because it closes back to depth
    /// 1 every cycle regardless of which leg ran; a bench whose whole point is NOT popping in
    /// between cannot.
    pub(crate) targets: Vec<PushTarget>,
    pub(crate) person: Option<(plx_plex::plex::ServerId, String, String, String, String)>,
    /// Logged once, the first time a `Person` step falls back to re-pushing `Detail` for want of
    /// cast data (mirrors `PushBench::person_fallback_logged`; the fallback target differs because
    /// `Library` is not a safe fallback here — see `targets`' doc).
    pub(crate) person_fallback_logged: bool,
    /// What was ACTUALLY pushed, in push order. The pop half's `Vec::pop()` is the same LIFO
    /// `NavStack::entries` itself keeps, so a pop step always names and closes the right target
    /// without re-deriving it from the (by-then-stale) push rotation index.
    pub(crate) stack: Vec<PushTarget>,
    /// The most recent step's direction and the target it opened/closed, latched at `Start` and
    /// read back at `Settle` — same shape as `PushBench::opened`.
    pub(crate) dir: DeepDir,
    pub(crate) opened: PushTarget,
}

impl DeepBench {
    pub(crate) fn new(depth: u32, rk: String) -> Self {
        let empty_rk = rk.is_empty();
        let depth = if empty_rk { 0 } else { depth };
        if empty_rk {
            plx_base::eventlog::log(
                "bench: deepbench has no ratingKey (deepbench=<depth>,<rk> or reuse navosc=<rk>) \
                 — Library cannot deepen the stack (its entry point is a peer swap, \
                 NavOp::SelectTab, not a push — see DeepBench::targets's doc), so there is \
                 nothing left to rotate; running zero cycles",
            );
        }
        let targets = if empty_rk { Vec::new() } else { vec![PushTarget::Detail, PushTarget::Person] };
        Self {
            clock: BenchClock::new(2 * depth),
            depth,
            rk,
            targets,
            person: None,
            person_fallback_logged: false,
            stack: Vec::new(),
            dir: DeepDir::Push,
            opened: PushTarget::Detail,
        }
    }
}

// =================================================================================================
// hero bench (`/tmp/plxnative-herobench`)
// =================================================================================================

/// Default half-period of the hero bench, ms: a flip every 600 ms, faster than the carousel's own
/// 8 s auto-advance and faster than a slide settles, so a flip lands while the last one is still
/// travelling — the shape of someone paging back and forth to compare titles.
pub(crate) const DEFAULT_HERO_PERIOD_MS: u32 = 600;
/// The hero carousel refuses a second flip inside its own cooldown (`HERO_FLIP_CD`, 0.35 s), so a
/// shorter half-period would measure refusals, not flips.
pub(crate) const MIN_HERO_PERIOD_MS: u32 = 400;

/// The direction of a hero bench cycle's two flips: both halves of one cycle go the same way,
/// and successive cycles alternate, so the carousel walks 0 → 1 → 2 → 1 → 0 → 1 → … over THREE
/// titles, every one of which is left and returned to (the A-B-A case a cache has to survive).
pub(crate) fn hero_flip_dir(cycle: u32) -> i32 {
    if cycle % 2 == 0 { 1 } else { -1 }
}

/// What a hero bench flips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PongTarget {
    /// The billboard carousel: `HomeCmd::Flip`.
    Hero,
    /// Focus across the first shelf's first three cards: the real Left/Right key (pair with
    /// `plxnative-grid`, which seats the first card).
    Grid,
}

pub(crate) struct HeroBench {
    pub(crate) target: PongTarget,
    /// A round trip: the open half is the cycle's first flip, the close half its second, so every
    /// flip is measured and the next cycle's first flip follows the report on the next frame.
    pub(crate) clock: BenchClock,
    pub(crate) period_ms: u32,
}

impl HeroBench {
    pub(crate) fn new(n: u32, period_ms: u32, target: PongTarget) -> Self {
        Self { target, clock: BenchClock::round_trip(n), period_ms: period_ms.max(MIN_HERO_PERIOD_MS) }
    }
}

// =================================================================================================
// press bench (`/tmp/plxnative-holdbench`)
// =================================================================================================

/// Half-period of a HOLD cycle, ms. The open half has to cover the whole gesture: OK down, the
/// hold fill (`press::LONG_MS`, 500 ms), the item menu opening out of it and its appear spring
/// settling; the close half covers Back and the dismiss spring. 1500 ms clears the first with
/// ~0.4 s to spare, and the quiet gap between halves is what lets each half end on a still frame.
pub(crate) const DEFAULT_HOLD_PERIOD_MS: u32 = 1500;
/// Half-period of a TAP-and-cancel cycle, ms. It must stay under `press::LONG_MS` (500 ms) or the
/// "tap" becomes a hold: OK goes down, 350 ms later a direction key arrives and abandons the press,
/// and the second half lets the spring-back and the focus move settle.
pub(crate) const DEFAULT_TAP_PERIOD_MS: u32 = 350;
/// The ceiling a tap's half-period is clamped to: a margin under the hold threshold.
pub(crate) const MAX_TAP_PERIOD_MS: u32 = 450;
/// A hold's floor: the hold threshold plus a frame budget for the menu to be requested, so the
/// close half never fires before the menu could have opened.
pub(crate) const MIN_HOLD_PERIOD_MS: u32 = 900;

/// What a press bench does with the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressKind {
    /// OK down and KEPT down past `press::LONG_MS`: the hold fill runs, the item menu opens out of
    /// the hold, and Back dismisses it. The key-up is never sent (the dispatcher abandons the
    /// press the moment the hold is answered, and an OK edge while a menu is up would land on its
    /// Play row).
    Hold,
    /// OK down, then a direction key before the hold threshold: the real cancel path
    /// (`input::begin_fresh_press`, "navigation or BACK arrived"). The card dips, springs back
    /// without activating, and focus moves one card; the next cycle moves it back.
    Tap,
}

/// The surface a press bench presses a card of. Named in each `bench:` line's `target=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressHost {
    Home,
    Library,
    Collection,
}

impl PressHost {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Library => "library",
            Self::Collection => "collection",
        }
    }
}

pub(crate) struct PressBench {
    pub(crate) kind: PressKind,
    pub(crate) clock: BenchClock,
    pub(crate) period_ms: u32,
    /// Did the cycle in flight actually press? A cycle with no card to press is reported as a
    /// miss, never papered over.
    pub(crate) in_flight: bool,
    /// Did the menu open in the cycle in flight (hold)?
    pub(crate) menu_opened: bool,
    /// The host the first press found, for the report lines.
    pub(crate) host: Option<PressHost>,
}

impl PressBench {
    pub(crate) fn new(n: u32, period_ms: u32, kind: PressKind) -> Self {
        let period_ms = match kind {
            PressKind::Hold => period_ms.max(MIN_HOLD_PERIOD_MS),
            PressKind::Tap => period_ms.min(MAX_TAP_PERIOD_MS),
        };
        Self {
            kind,
            clock: BenchClock::round_trip(n),
            period_ms,
            in_flight: false,
            menu_opened: false,
            host: None,
        }
    }
}

/// May the press bench send OK-down now? Only onto a focused card with NO menu up: a menu's first
/// row is Play, so an OK edge while one is open starts playback on a real account. This is the one
/// place the bench decides to send OK at all.
pub(crate) fn press_allowed(menu_up: bool, card_focused: bool) -> bool {
    !menu_up && card_focused
}

/// The key the close half sends, or `None`. NEVER OK, and never anything while the state is not the
/// one the half expects:
/// - hold: Back, and only while the item menu is up (Back with no menu would pop a page);
/// - tap: a direction key, and only while NO menu is up, right then and left next cycle so focus
///   walks 0-1-0-1 over two cards and the same card is never pressed past the shelf's end.
pub(crate) fn press_close_key(kind: PressKind, menu_up: bool, cycle: u32) -> Option<plx_machine::machine::Key> {
    use plx_machine::machine::Key;
    match kind {
        PressKind::Hold => menu_up.then_some(Key::Back),
        PressKind::Tap => (!menu_up).then_some(if cycle % 2 == 0 { Key::Right } else { Key::Left }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hero_bench_walks_three_titles_and_returns_to_each() {
        let mut at = 0i32;
        let mut seen = std::collections::BTreeMap::<i32, u32>::new();
        for cycle in 0..8 {
            for _half in 0..2 {
                at += hero_flip_dir(cycle);
                *seen.entry(at).or_default() += 1;
            }
        }
        assert_eq!(at, 0, "an even number of cycles ends where it began");
        assert_eq!(seen.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2], "never leaves three titles");
        assert!(seen.values().all(|&n| n >= 4), "each title is revisited: {seen:?}");
    }

    #[test]
    fn the_hero_bench_never_flips_faster_than_the_carousel_accepts() {
        assert_eq!(HeroBench::new(10, 1, PongTarget::Hero).period_ms, MIN_HERO_PERIOD_MS);
        assert_eq!(HeroBench::new(10, 700, PongTarget::Grid).period_ms, 700);
    }

    #[test]
    fn a_press_bench_never_sends_ok_onto_an_open_menu_or_an_unfocused_card() {
        assert!(press_allowed(false, true));
        assert!(!press_allowed(true, true), "a menu is up: its first row is Play");
        assert!(!press_allowed(false, false), "no card focused");
        assert!(!press_allowed(true, false));
    }

    #[test]
    fn the_close_key_is_never_ok_and_only_fits_the_state() {
        use plx_machine::machine::Key;
        for kind in [PressKind::Hold, PressKind::Tap] {
            for menu_up in [false, true] {
                for cycle in 0..4 {
                    assert_ne!(press_close_key(kind, menu_up, cycle), Some(Key::Ok));
                }
            }
        }
        assert_eq!(press_close_key(PressKind::Hold, true, 0), Some(Key::Back));
        assert_eq!(press_close_key(PressKind::Hold, false, 0), None, "Back with no menu would pop a page");
        assert_eq!(press_close_key(PressKind::Tap, true, 0), None);
        let walk: Vec<_> = (0..4).map(|c| press_close_key(PressKind::Tap, false, c)).collect();
        assert_eq!(walk, [Some(Key::Right), Some(Key::Left), Some(Key::Right), Some(Key::Left)]);
    }

    #[test]
    fn a_hold_outlasts_the_hold_threshold_and_a_tap_does_not() {
        let long = plx_ui::press::LONG_MS;
        assert!(PressBench::new(1, 1, PressKind::Hold).period_ms > long);
        assert!(PressBench::new(1, 100_000, PressKind::Tap).period_ms < long);
        assert_eq!(PressBench::new(1, DEFAULT_HOLD_PERIOD_MS, PressKind::Hold).period_ms, DEFAULT_HOLD_PERIOD_MS);
        assert_eq!(PressBench::new(1, DEFAULT_TAP_PERIOD_MS, PressKind::Tap).period_ms, DEFAULT_TAP_PERIOD_MS);
    }

    #[test]
    fn a_press_bench_counts_its_cycles_and_reports_each_one() {
        let mut b = PressBench::new(3, DEFAULT_HOLD_PERIOD_MS, PressKind::Hold);
        b.clock.phase = BenchPhase::Waiting;
        let (mut now, mut starts, mut settles, mut reports) = (0u32, 0, 0, 0);
        for _ in 0..40_000 {
            now += 16;
            match bench_advance(&mut b.clock, now, b.period_ms) {
                BenchStep::Start(_) => starts += 1,
                BenchStep::Settle(_) => settles += 1,
                BenchStep::Report(_) => reports += 1,
                BenchStep::Done(n) => { assert_eq!(n, 3); break }
                _ => {}
            }
        }
        assert_eq!((starts, settles, reports), (3, 3, 3));
    }

    /// A clock past its boot-settle gate, for the tests that drive the cycle machine itself.
    fn settled(mut clock: BenchClock) -> BenchClock {
        clock.phase = BenchPhase::Waiting;
        clock
    }

    const P: u32 = 1400;

    #[test]
    fn a_bench_waits_for_the_screen_to_go_still_before_its_first_press() {
        let mut clock = BenchClock::round_trip(3);
        // Boot is presenting every frame: no quiet second, so nothing starts however long it runs
        // short of the cap — the old clock pressed at 1401 ms regardless.
        let mut now = 0u32;
        while now < 10_000 {
            bench_note_frame(&mut clock, now, 16.7, Some(16.7));
            assert_eq!(bench_advance(&mut clock, now, P), BenchStep::Nothing, "at {now}");
            now += 16;
        }
        // Still for just under a second: not yet.
        let last = now - 16;
        assert_eq!(bench_advance(&mut clock, last + SETTLE_QUIET_MS - 1, P), BenchStep::Nothing);
        assert_eq!(
            bench_advance(&mut clock, last + SETTLE_QUIET_MS, P),
            BenchStep::Settled(last + SETTLE_QUIET_MS, false)
        );
        // Settling measured nothing, and the first press is a whole half-period later.
        assert_eq!(clock.frames(), 0);
        assert_eq!(bench_advance(&mut clock, last + SETTLE_QUIET_MS + P, P), BenchStep::Nothing);
        assert_eq!(bench_advance(&mut clock, last + SETTLE_QUIET_MS + P + 1, P), BenchStep::Start(0));
    }

    #[test]
    fn a_root_page_that_never_goes_still_starts_the_bench_at_the_cap_and_says_so() {
        let mut clock = BenchClock::new(2);
        let mut now = 5_000u32;
        let armed = now;
        loop {
            bench_note_frame(&mut clock, now, 16.7, Some(16.7));
            let step = bench_advance(&mut clock, now, P);
            if step != BenchStep::Nothing {
                assert_eq!(step, BenchStep::Settled(SETTLE_CAP_MS, true));
                assert_eq!(now - armed, SETTLE_CAP_MS);
                break;
            }
            now += 1;
        }
    }

    #[test]
    fn a_round_trip_measures_both_halves_and_reports_once_per_cycle() {
        let mut clock = settled(BenchClock::round_trip(2));
        let mut steps = Vec::new();
        let mut now = 0u32;
        for _ in 0..20_000 {
            now += 1;
            // one presented frame per ms tick, 16.7 ms apart in interval terms
            bench_note_frame(&mut clock, now, 5.0, Some(REFRESH_MS));
            match bench_advance(&mut clock, now, P) {
                BenchStep::Nothing => {}
                BenchStep::Report(c) => {
                    // both halves carried frames into the one report
                    assert!(clock.open.frames > 0 && clock.close.frames > 0);
                    steps.push(BenchStep::Report(c));
                }
                step => steps.push(step),
            }
            if clock.done {
                break;
            }
        }
        assert_eq!(
            steps,
            vec![
                BenchStep::Start(0), BenchStep::Settle(0), BenchStep::Report(0),
                BenchStep::Start(1), BenchStep::Settle(1), BenchStep::Report(1),
                BenchStep::Done(2),
            ]
        );
    }

    #[test]
    fn the_next_press_follows_a_report_on_the_very_next_frame() {
        let mut clock = settled(BenchClock::round_trip(2));
        assert_eq!(bench_advance(&mut clock, P + 1, P), BenchStep::Start(0));
        assert_eq!(bench_advance(&mut clock, 2 * P + 2, P), BenchStep::Settle(0));
        assert_eq!(bench_advance(&mut clock, 3 * P + 3, P), BenchStep::Report(0));
        assert_eq!(bench_advance(&mut clock, 3 * P + 4, P), BenchStep::Start(1));
    }

    fn half(frames: &[(f64, Option<f64>)]) -> HalfStats {
        let mut h = HalfStats::default();
        for &(total, iv) in frames {
            h.note(total, iv);
        }
        h
    }

    #[test]
    fn steady_sixty_with_vsync_jitter_misses_nothing_even_when_top_to_swap_reads_over_budget() {
        // The device shape (FRAMEDROP 2026-09-28): the vsync wait sits inside each frame, so a
        // frame after a light one reads 21-23 ms Top->Swap while its neighbour reads ~12, and the
        // panel still got a new picture on every refresh.
        let mut frames = vec![(6.0, None)];
        for i in 0..40 {
            let iv = if i % 2 == 0 { 22.9 } else { 2.0 * REFRESH_MS - 22.9 };
            frames.push((iv, Some(iv)));
        }
        let h = half(&frames);
        assert!(h.worst_ms > 20.0, "Top->Swap alone reads as a drop: {}", h.worst_ms);
        assert_eq!(h.missed, 0);
        assert_eq!(h.first_ms, 6.0);
    }

    /// A clock mid-open-half, the way `bench_frame_tick` / `bench_defer_tick` feed it.
    fn measuring() -> BenchClock {
        let mut c = settled(BenchClock::round_trip(1));
        c.phase = BenchPhase::Measuring;
        c
    }

    #[test]
    fn a_fence_deferral_after_the_first_present_is_missed_refreshes() {
        // The 21 fps menu open: 14 ms of work, 47 ms between presents because the capture fence
        // held two loop iterations back. The heartbeat keeps the interval across a deferral, so
        // the frame reads Some(47).
        let mut c = measuring();
        bench_note_frame(&mut c, 1000, 6.0, None);
        bench_note_defer(&mut c);
        bench_note_defer(&mut c);
        bench_note_frame(&mut c, 1047, 14.0, Some(47.0));
        assert_eq!(c.open.missed, 2);
        assert_eq!(c.open.iv_max_ms, 47.0);
        assert_eq!(c.open.defer_first, 0, "a deferral after the first present is not latency");
    }

    #[test]
    fn a_deferral_before_the_first_present_is_latency_not_missed() {
        let mut c = measuring();
        bench_note_defer(&mut c);
        bench_note_defer(&mut c);
        bench_note_frame(&mut c, 1047, 14.0, Some(47.0));
        bench_note_frame(&mut c, 1064, 14.0, Some(17.0));
        assert_eq!(c.open.defer_first, 2);
        assert_eq!(c.open.missed, 0);
        assert!(c.open.field().contains("defer_first:2"), "{}", c.open.field());
        // the other half is untouched, and a deferral between halves counts for neither
        assert_eq!(c.close.defer_first, 0);
        let mut w = settled(BenchClock::round_trip(1));
        bench_note_defer(&mut w);
        assert_eq!((w.open.defer_first, w.close.defer_first), (0, 0));
    }

    #[test]
    fn an_idle_skip_still_resets_the_interval_so_nothing_is_missed() {
        // An idle skip clears the heartbeat's interval: the next present reads None and grades by
        // its own Top->Swap, however long the gap was.
        let mut c = measuring();
        bench_note_frame(&mut c, 1000, 6.0, None);
        bench_note_frame(&mut c, 3000, 6.0, None);
        assert_eq!(c.open.missed, 0);
        assert_eq!(c.open.iv_max_ms, 0.0);
    }

    #[test]
    fn a_frame_spanning_two_refreshes_is_one_missed_refresh_wherever_its_time_went() {
        let mut frames = vec![(6.0, None)];
        frames.extend(std::iter::repeat_n((REFRESH_MS, Some(REFRESH_MS)), 10));
        frames.push((33.4, Some(2.0 * REFRESH_MS))); // CPU- or GPU-late: the interval carries it
        frames.extend(std::iter::repeat_n((REFRESH_MS, Some(REFRESH_MS)), 10));
        let h = half(&frames);
        assert_eq!(h.missed, 1);
        assert_eq!(h.worst_at, 12);
        // …and an 86 ms hitch is five repeated refreshes, not one
        let h = half(&[(6.0, None), (86.6, Some(86.6)), (REFRESH_MS, Some(REFRESH_MS))]);
        assert_eq!(h.missed, 4);
    }

    #[test]
    fn a_late_frame_is_not_cancelled_by_the_catch_up_frame_after_it() {
        // Device shape: a 30 ms interval followed by a 3 ms one. Their SUM is two refreshes for
        // two frames, yet the panel held the earlier picture across a refresh.
        let h = half(&[(6.0, None), (30.0, Some(30.0)), (3.3, Some(3.3)), (REFRESH_MS, Some(REFRESH_MS))]);
        assert_eq!(h.missed, 1);
        // 1.5 refreshes is the boundary: under it rounds to one refresh, over it to two
        assert_eq!(missed_refreshes(24.9), 0);
        assert_eq!(missed_refreshes(25.1), 1);
    }

    #[test]
    fn a_first_frame_that_took_two_refreshes_of_work_is_a_late_response() {
        let h = half(&[(40.0, None), (REFRESH_MS, Some(REFRESH_MS))]);
        assert_eq!(h.missed, 1);
        assert_eq!(h.first_ms, 40.0);
    }

    #[test]
    fn an_idle_gap_is_never_a_missed_refresh() {
        // Two bursts separated by a skipped present: the gap has no interval, so it cannot count;
        // each burst's first frame is graded on its own work.
        let h = half(&[
            (7.0, None), (REFRESH_MS, Some(REFRESH_MS)), (REFRESH_MS, Some(REFRESH_MS)),
            (9.0, None), (REFRESH_MS, Some(REFRESH_MS)),
        ]);
        assert_eq!(h.missed, 0);
        assert_eq!(h.frames, 5);
        assert_eq!(h.first_ms, 7.0, "the half's FIRST frame, not the second burst's");
    }

    #[test]
    fn the_bench_line_tail_names_both_halves_for_a_round_trip_and_one_for_a_one_way_step() {
        let mut rt = settled(BenchClock::round_trip(1));
        rt.open = half(&[(6.0, None), (REFRESH_MS, Some(REFRESH_MS))]);
        rt.close = half(&[(8.0, None), (40.0, Some(2.0 * REFRESH_MS))]);
        assert_eq!(
            rt.fields(),
            "first_ms=8.0 missed=1 open=first:6.0,worst:16.7@2,iv:16.7,missed:0,defer_first:0 \
             close=first:8.0,worst:40.0@2,iv:33.3,missed:1,defer_first:0"
        );
        assert_eq!(rt.worst_ms(), 40.0);
        assert_eq!(rt.frames(), 4);
        let mut one = settled(BenchClock::new(1));
        one.open = half(&[(6.0, None)]);
        assert_eq!(one.fields(), "first_ms=6.0 missed=0 open=first:6.0,worst:6.0@1,iv:0.0,missed:0,defer_first:0");
    }

    /// Drives a `BenchClock` with a fake, hand-stepped clock and returns every non-`Nothing` step
    /// in order — the shape both state-machine tests below share.
    fn run(n: u32, period_ms: u32, iterations: u32) -> Vec<BenchStep> {
        let mut clock = settled(BenchClock::new(n));
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
        let mut clock = settled(BenchClock::new(1));
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
        let mut clock = settled(BenchClock::new(5));
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

    #[test]
    fn deep_bench_clock_emits_exactly_two_depth_start_settle_pairs_then_one_done() {
        let depth = 4u32;
        let steps = run(2 * depth, 1400, 20);
        let mut expect = Vec::new();
        for i in 0..2 * depth {
            expect.push(BenchStep::Start(i));
            expect.push(BenchStep::Settle(i));
        }
        expect.push(BenchStep::Done(2 * depth));
        assert_eq!(steps, expect, "depth={depth} must emit 2*depth Start/Settle pairs, then Done");
    }

    #[test]
    fn deep_step_pushes_depth_times_then_pops_back_to_the_root_one_at_a_time() {
        let depth = 5u32;
        let got: Vec<(DeepDir, u32)> = (0..2 * depth).map(|cycle| deep_step(depth, cycle)).collect();
        assert_eq!(
            got,
            vec![
                (DeepDir::Push, 1), (DeepDir::Push, 2), (DeepDir::Push, 3), (DeepDir::Push, 4), (DeepDir::Push, 5),
                (DeepDir::Pop, 4), (DeepDir::Pop, 3), (DeepDir::Pop, 2), (DeepDir::Pop, 1), (DeepDir::Pop, 0),
            ],
            "depth must climb 1..=depth on the way down, then descend depth-1..=0 on the way back"
        );
    }

    #[test]
    fn deep_bench_push_rotation_alternates_detail_and_person() {
        let bench = DeepBench::new(6, "12345".into());
        assert_eq!(bench.targets, vec![PushTarget::Detail, PushTarget::Person]);
        let got: Vec<PushTarget> = (0..bench.depth)
            .map(|cycle| bench.targets[bench_target_index(bench.targets.len(), cycle)])
            .collect();
        assert_eq!(
            got,
            vec![
                PushTarget::Detail, PushTarget::Person, PushTarget::Detail,
                PushTarget::Person, PushTarget::Detail, PushTarget::Person,
            ]
        );
    }

    #[test]
    fn deep_bench_never_rotates_library_since_selecttab_would_collapse_the_stack() {
        let bench = DeepBench::new(6, "12345".into());
        assert!(!bench.targets.contains(&PushTarget::Library));
    }

    #[test]
    fn deep_bench_without_a_ratingkey_runs_zero_cycles() {
        let bench = DeepBench::new(100, String::new());
        assert_eq!(bench.depth, 0, "Library cannot stand in for Detail/Person here, so there is nothing to push");
        assert!(bench.targets.is_empty());
        assert_eq!(bench.clock.n, 0);
    }
}
