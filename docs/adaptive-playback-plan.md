# Adaptive playback: amended plan of record

**Status: PLAN OF RECORD. Supersedes the mathematical rework plan reviewed on 2026-08-26.**
Where this document and that one disagree, this one governs. Sections of the earlier plan that
the design board ruled against are **VOID**, not caveated — they are listed as void in §0.2 and
must not be implemented from the older text.

**Baseline SHA: `5a8ef2ef`** (`playback: risk-aware adaptive controller, model read-out and
shaped-link test tier`). Every "today" in this document means that commit. **Every `abr.rs:NNNN`
citation below is a line number IN THAT COMMIT**, and `abr.rs` has since been split into
`rust-modules/src/abr/` (Phase 1). The citations are deliberately NOT rewritten: they are evidence
about a baseline, and repointing them at a moving tip would make them unverifiable. To read one,
`git show 5a8ef2ef:rust-modules/src/abr.rs`. **No change to `abr.rs`
decision behaviour lands before step M2 of §4 has recorded that baseline's device behaviour** — see
§4's preamble for why I0 and I1 are exempt by construction rather than exceptions to it.

**Scope.** `rust-modules/src/abr.rs` policy and mathematics; the data `ff.rs` passes into mode
selection; the small amount of `route.rs`/session state the corrected temporal model needs;
`ui/stats.rs` diagnostics; the host and device test tiers; `docs/adaptive-playback.md`. Transport,
demux, the transaction machinery and the estimator's overall shape are **not** in scope and are
not to be rewritten.

**Two decisions this document defers by omission, and §5 must not be executed until they are made
in writing.** They are called out here because they are the only two places where an implementer
would have to invent policy: (1) N5 now specifies how `r_net`/`r_prod` compose into
`CandidateRisk::score`, but the *acceptance* of that composition's endpoint changes is a product
call; (2) M4 requires a rung-pin dev trigger that does not exist and is new scope in I0.

---

## 0. What changed, and why

### 0.1 The one correction that reorders everything

The previous plan sized its buffer policy against a reserve this pipeline cannot hold.

The AU queues are **byte**-capped — `AQ_VIDEO_BYTES = 8 MiB`, `AQ_AUDIO_BYTES = 1 MiB`
(`player/engine.rs:84-92`) — and the feed pump throttles the video lane to `MAX_FEED_AHEAD_NS =
1.6 s` ahead of the presented position (`player/engine.rs:1051`), the audio lane to
`MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS = 3.6 s` (`player/engine.rs:1052`, `:1280`). The quantity the
controller calls `buffered_ms` is stamped at `aq_push` (`ff.rs:2581-2586`) and read at
`abr.rs:892-899`, so it is bounded above by

```
B_max(R_video, R_audio) ≈ min( 1.6 s + 67_108.864 kbit / R_video_kbps ,
                               3.6 s +  8_388.608 kbit / R_audio_kbps )
```

The minimum is not decorative: `buffered_ms` is `audio.min(video_tail)` (`abr.rs:895`) and
`aq_push` **blocks the single demux thread** on either lane's cap (`aq.rs:108-110`). At 192 kbps
audio the audio ceiling is **47.3 s**, so it binds below ~1 468 kbps of video ES — P240 and P480
only. Everywhere else the video term binds, and

**the reachable reserve is inversely proportional to the bitrate: it is thinnest exactly where the
bitrate is highest.** Derived from nameplate wire rates (video ES taken as wire − 192 kbps audio;
the real ES rate is what step M4 measures):

| rung | wire kbps | video ES | `B_max` |
|---|---:|---:|---:|
| P240 | 320 | 128 | ~47 s (audio-bound) |
| P480 | 720 | 528 | ~47 s (audio-bound) |
| P720Low | 2 000 | 1 808 | ~38.7 s |
| P720 | 4 000 | 3 808 | ~19.2 s |
| P1080M6 | 6 000 | 5 808 | ~13.2 s |
| P1080 | 8 000 | 7 808 | ~10.2 s |
| P1080M10 | 10 000 | 9 808 | **~8.44 s** |
| P1080M12 | 12 000 | 11 808 | ~7.28 s |
| P1080M14 | 14 000 | 13 808 | ~6.46 s |
| P1080M16 | 16 000 | 15 808 | **~5.85 s** |
| P1080M18 | 18 000 | 17 808 | ~5.37 s |
| P1080High | 20 011 | 19 819 | **~4.99 s** |
| Uhd | 20 895 | 20 703 | ~4.84 s |

> **Recorded discrepancy, to be closed by M4.** The board report quotes "~45 s at 480p"; it derived
> that from the *video* form with a ~1.5 Mbps 480p ES assumption (`67 109/1 500 ≈ 45`), not from the
> audio lane. The two land in the same band for different reasons and neither is measured. Do not
> propagate either as fact. **Every `B_max` at P720Low and above is a lower bound**, because the one
> device datum in the file — a buffer "sitting flat at 11 918 ms" on the 10 Mbps rung
> (`abr.rs:1022-1029`) against a nameplate 8.44 s — implies the real ES rate is ~0.66 of nameplate,
> which would move P1080High to ~6.7 s. **The true top-rung value lies somewhere in 5.0–6.7 s** and
> both collisions in §0.3 sit inside that band.

### 0.2 Disposition of the previous plan, section by section

| prev § | ruling | where it lives now |
|---|---|---|
| §1 units | **adopted** | §1 |
| §2 estimation / no anonymous margin | **adopted with amendment** — the 0.8 is *named*, not deleted | N7, §6 |
| §3 starvation mathematics | **adopted** | N2 |
| §4 refill budget | **adopted, superseded on `B*`** — `B* = 10 s` is VOID | N3 |
| §5 production independence | **adopted** (as a filter; the per-candidate ρ prediction is deferred with §6) | N6, §7.D |
| §6 thirteen-way argmax | **VOID for now — BLOCKED** | §7.A |
| §7 log-concave quality from catalog rasters | **VOID as specified** — the inputs are bounding boxes | §7.B |
| §8 risk drives HLS decisions | **adopted, predicate amended** | N4, N5 |
| §9 `tau/(T+tau)` | **VOID** — replaced by a piecewise-linear ramp with no new parameter | N5 |
| §10 `g(remaining)·Q` on rung selection | **VOID** — sign error; incumbent clause adopted separately | N4, N18, §7.C |
| §10 "rung switches are invisible" | **VOID as a premise** | N15 |
| §11 wholesale counter deletion | **superseded** by the itemised N8–N12 and N21 | N8–N12, N21 |
| §12 doc half | **adopted** | N17 |
| §12 runtime `R_runtime ≈ R_avg` | **VOID** — no demand-side variance exists to carry the burst risk | §7.E |
| §13 Original exits, elapsed-time persistence | **adopted** | N13 |
| §14 real HLS in recovery | **adopted, widened** — also `worth_probing` and `original_utility` | N14 |
| §15 mode utility / feature split | **adopted with amendment** (DV/Atmos split mandatory) | N16 |
| §16 remaining-playback scale | **adopted as a confirmed no-op** — already implemented | N18 |
| §17 `exp(−dt/tau)` hysteresis | **deferred**; its three-line prerequisite adopted | N12, §7.F |
| §18 probing / value of information | **adopted** | N14 |
| §19 validation, naming the `800` | **adopted (naming only)**; resolving 800→750/1100 deferred | N19 |
| §20 hard guards vs policy | **adopted** | N20 |
| §21 tests | **rule adopted; four of the twelve VOID** | §8.6 |
| §22 documentation | **adopted** | N17 |
| §23 scope | **adopted**; its closing "do not optimize for green tests" instruction **amended** | §8.5 |

### 0.3 Three collisions that must be fixed before the controller is used as a tuning baseline

Both are consequences of §0.1 and neither was in the previous plan.

1. **Every Auto HLS playback proposes a downshift on its first segment.** At the first `observe`
   the video tail is one segment and `playpos_ns` is ~0, so `buffered_ms` ≈ 1 958–2 000 ms.
   `buffer_bad = buffered < segment || starving()` (`abr.rs:2142`) with `starving()` = `buffered_ms
   <= 2_000` (`abr.rs:1035`) is **true** at the 2 s target duration the client requests. The
   downshift block (`abr.rs:2143-2166`) sits *above* the counter wall at `abr.rs:2171`, so it fires
   at `samples_on_rung == 1`, and `candidate_ready(Down)` needs only `buffered >= segment`
   (`abr.rs:2230-2232`, arm at `:2234`). **P480 commits to P240 on segment 1 of a 40 Mbit/s link,
   then arms `cooldown = 8`.**
   *No host test drives the **first** `observe` with a one-segment reserve.* The closest,
   `abr.rs:2678-2687`, reaches 2 000 ms only after five prior samples, by which point
   `samples_on_rung` and the slope history are established and the decision is correct. The
   bootstrap case is uncovered.
2. **Two thresholds sit at the physical ceiling of the reserve.** The upshift gate `buffered >=
   segment * 3` (`abr.rs:2204`) is 6 000 ms at 2 s segments, so it is satisfiable only while the
   video ES rate is ≲15 252 kbps — the top three rungs are reachable only by a *jump* from below,
   never by a walk. And `starving()`'s second arm (`buffered <= 6_000 && draining_samples >= 2`,
   `abr.rs:1035-1037`) has a 6 000 ms band that **exceeds the reachable ceiling above ~15 252 kbps
   of video ES** — the same threshold the upshift gate crosses — so at P1080M16 and above two
   consecutive draining samples force an emergency downshift with a *full* reserve in hand. Below
   that rate the band is reachable only by a genuine drawdown; the arm is never structurally inert
   at any rung.
   Both thresholds are multiples of an `EXT-X-TARGETDURATION` **requested by the client as
   `secondsPerSegment=2` at every construction site** (`route.rs:705`, `plex/transcoder.rs:275`),
   honoured or not by the server, and parsed back rather than assumed (`hls.rs:491`) — the app has
   never observed anything but 2 s. The guard is still denominated in a quantity this client does
   not control, which is the point.

3. **The Original path abandons direct play on its FIRST measurement window** — found on a real
   4K Dolby Vision + Atmos film 2026-08-26, and the most expensive of the three.
   `auto: Original -> HLS ImminentStarvation measured=42365kbps safe=21182kbps need=34106kbps
   buf=85ms slope=113ms/s starve=0 windows=1`. The link carried 1.24x the requirement; the
   first-sample uncertainty floor halved it to 21 182, manufacturing a deficit; with `buf=85ms`
   one second into playback — and RISING at +113 ms/s — the horizon is ~0 and
   `OriginalExit::ImminentStarvation` fires, a hard guard with no utility veto and no persistence
   requirement. `original_fallback_rung` then divides the already-discounted rate by the same 1.35
   again: 42 365 -> x0.5 -> 21 182 -> /1.35 -> 15 690 -> rung 14 000, **3.0x below measured**.
   Recovery afterwards needed 1.69x the source and never fired.
   Full account and the redacted log: `docs/measurements/orig-first-window-fallback.md`.

**All three share one root: the first measurement window is taken while the buffer is definitionally
near-empty, and a hard guard reads that as an emergency.** (1) costs one rung; (3) costs a playback
MODE and takes Dolby Vision and Atmos with it.

**Normative consequence: any measurement of any other change, taken before these are fixed,
measures these instead.** (1) and (2) are increments I3 and I3b and gate everything downstream;
(3) has no increment yet and is recorded as evidence only.

---

## 1. The model, in physical units

Every quantity below carries its unit in its name at the point of storage. No expression may add
or subtract quantities of different dimension; the review that produced this plan found exactly
that defect at `abr.rs:1531-1534` (milliseconds subtracted from kilobits per second).

| symbol | meaning | unit |
|---|---|---|
| `C` | estimated network delivery capacity | kbps |
| `C_safe` | **final** conservative delivery capacity — nothing is discounted after it | kbps |
| `R_j` | candidate *j*'s delivery requirement (planning wire rate) | kbps |
| `B` | buffered media duration, as measured at `abr.rs:892-899` | ms |
| `B_max(R)` | the **physically reachable** reserve at rate `R` (§0.1) | ms |
| `B*(R)` | desired reserve at rate `R` (N3) | ms |
| `D_j` | `max(0, B*(R_j) − B)` — the deficit against candidate *j*'s own target | ms |
| `H` | buffer refill horizon | ms |
| `E_tx` | expected cost of an **upshift** transaction, in unrefilled playback | ms |
| `E_tx_down` | the same for a **downshift** — a different number (N4) | ms |
| `ρ` | PMS acquisition time ÷ media duration; `ρ > 1` means the server is falling behind | per-mille |
| `T` | starvation horizon | s |

`C_safe = C_hat · (1 − u)` with `u = uncertainty_pm/1000` clamped at 500 pm
(`abr.rs:283-287`, `MAX_UNCERTAINTY_PM` at `:449`). This is a **downside-biased point estimate**,
not a probability, and no document produced by this project may describe it as one (N17).

**A clock is a unit.** Three distinct clocks exist in this pipeline and the previous plan conflated
them: **wall clock**; **active body-read time** (the 750 ms Original window, `abr.rs:640-641`);
and **per-request elapsed** (`total_fetch_us`, `abr.rs:1266`). The last two are duty-cycled — they
run slow exactly when the reserve is full and the byte cap is idling the demux worker
(`aq.rs:108-110`). Every duration in this plan states its clock at the definition site.

---

## 2. Normative decisions

Each of these is a decision, not a suggestion. Where one supersedes the previous plan, that is
stated. Where one is blocked on a measurement, the gate is named.

**Document-wide rule for every deletion instruction:** *quote the expression beside the line
number.* I1 moves line numbers before I4 runs, and the first draft of this plan ordered the
deletion of `abr.rs:2205` — which is `&& !draining`, a hard guard whose derivation is a device
finding — because it inherited a line number from a review.

### N1 — `C_safe` is final

No discount may be applied to `C_safe` after it is computed, and no unnamed inline factor may
appear anywhere on the admission path. Every safety margin is a named `AbrPolicy` field with a
physical meaning, a value, and an entry in the calibration ledger (§6).

### N2 — the starvation equation and the deficit principle

Preserved exactly:

```
C >= R           →  T = ∞
C <  R           →  T = B·R / (R − C)        [B in ms → T in ms; report in s]
drain_ms_per_s   =  1000 · (R − C) / R       [when C < R]
```

**`C < R` is not an emergency.** It means the reserve is shrinking. Whether that matters is
decided by `T`, by `B`, and — separately — by whether the reserve can afford the recovery action
at all (N4). This principle is already stated at `abr.rs:36-38` and `docs/adaptive-playback.md:53`
and is currently applied to Original and not to HLS; N4 closes that.

`StarvationHorizon::drain_per_s` (`abr.rs:1049`) stores milliseconds-to-starvation under a name
reciprocal to its value and is read by no caller anywhere in the repository. **Rename it
`starvation_ms`**, and represent the no-deficit case as *unbounded* rather than the current `0`
(`abr.rs:1058`), which is backwards under either name. If a true drain rate is ever wanted, add
`drain_ms_per_s` as a second field with the formula above.

### N3 — buffer policy derives from the reachable ceiling

**Supersedes the previous §4 entirely on `B*`. `B* = 10 s` is void.**

```
B_max_est(R_v, R_a)  =  min( MAX_FEED_AHEAD_NS/1_000_000               + (AQ_VIDEO_BYTES · 8) / R_v ,
                             (MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS)/1e6  + (AQ_AUDIO_BYTES · 8) / R_a )
                                                                                              [ms]
   (kbps IS bits-per-millisecond, so bits / kbps is ALREADY ms — there is no scale factor here)

B*(R)   =  min( buffer_target_ms , buffer_reserve_fraction_pm/1000 · B_max_est(R) )            [ms]
D_j     =  max(0, B*(R_j) − B)                                                                 [ms]
R_max_j =  C_safe · H / (H + D_j)                                                            [kbps]

candidate j is admissible  ⟺  R_j <= R_max_j
```

*Worked, to pin the scale:* at `R_v = 19 819` (P1080High), `1 600 + 67 108 864/19 819 = 4 986 ms`,
matching §0.1's ~4.99 s. The audio term at 192 kbps is `3 600 + 8 388 608/192 = 47 290 ms`, so the
video term binds, as it does everywhere above ~1 468 kbps.

Four things are normative here:

1. **`B_max_est` is computed, not assumed — and two of its three inputs need plumbing.**
   `aq_caps()` is already `pub(crate)` and re-exported (`player/engine.rs:84`,
   `player/mod.rs:426`). `MAX_FEED_AHEAD_NS` (`player/engine.rs:1051`) and `AUDIO_SLACK_NS`
   (`:1052`) are **private module consts** and must be made `pub(crate)` and re-exported — one line
   each, in **I1**. The segment's measured media rate has **no accessor**: `SegmentSample.bytes` is
   private (`abr.rs:1264`) and the impl exposes only `network_kbps`, `media_duration_ms` and
   `production_ratio_pm` (`abr.rs:1292-1308`). Add `SegmentSample::media_kbps()` and carry the last
   measured value on the `Controller` and in `ControllerTelemetry` — the same quantity I0 needs for
   the log line, so land both in **I0**. Only once that exists is `B_max_est` computed from a
   measurement; until then it is computed from the candidate's planning wire rate, which N17
   requires be labelled a request rather than a measurement at the site. Where the audio rate has
   not been measured, `assumed_audio_kbps` (§6.2) supplies it.
2. **The refill budget is a per-candidate FILTER, not a scalar budget.** `R` appears on both sides
   of the previous plan's algebra, and the code compares one scalar against every candidate — that
   is the ambiguity. It is resolved in favour of evaluating `D_j` and `R_max_j` per candidate. This
   is a feasibility filter and needs no argmax; it composes with §7.A remaining blocked.
   **Proof obligation, because the predicate is not trivially monotone.** `B_max_est` is decreasing
   in `R`, so `D_j` is decreasing in `R_j`, so `R_max_j` is *increasing* in `R_j` — both sides of
   `R_j <= R_max_j` move the same way, which is the shape that can produce a *scattered* admissible
   set rather than a prefix of the ladder. It is well-posed today only because `B*(R) <=
   buffer_target_ms = 2 500` pins `R_max_j` into `[0.8·C_safe, C_safe]`, a band narrow enough that
   the set is always a prefix. **§8.1 carries a test asserting the admissible set is a prefix of the
   ladder over the §3.3 magnitude sweep**, which is what protects the property when
   `buffer_target_ms` moves after M4.
3. **`buffer_target_ms` stays at 2 500** — today's `minimum_buffer_ms` (`abr.rs:1431`) unchanged.
   At that value `B*(R) = 2 500` at every rung (see item 4), so `D_j = max(0, 2 500 − B)` is zero
   whenever the reserve exceeds one and a quarter segments — which is why it is the safe value to
   land the corrected formula at, **independently of what I3b decides about `abr.rs:2204`**.
   Raising it is a separate decision requiring device evidence from M4. Landing `B* = 10 s` against
   the byte cap would make `D` permanently positive at the top rungs, i.e. `R_max` between
   **0.67·C and 0.71·C** depending on rung (0.667·C at P1080High) — *a larger, permanent,
   rung-dependent haircut than the 0.8 this plan exists to make explicit*, installed in the same
   change.
4. **`buffer_reserve_fraction_pm` (α) ≤ 500.** At α = 500 and `buffer_target_ms = 2 500` the
   ceiling binds only above **~19 738 kbps of video ES** — i.e. at **P1080High and Uhd alone** —
   and the 2 500 ms target binds on the other eleven rungs. That makes α inert almost everywhere at
   today's target, which is the intended shape *while `buffer_target_ms` stays at 2 500*: the plan
   lands the corrected formula without moving any expected value, and M4 decides whether either
   number moves. If `buffer_target_ms` is raised after M4, α becomes the live term and its device
   validation (§6.2) is no longer optional.

**A host test must derive the admissible range from `aq_caps()`, `MAX_FEED_AHEAD_NS` and
`AUDIO_SLACK_NS` — taking the minimum over both lanes — and assert that no gate anywhere in
`abr.rs` requires a reserve the byte caps cannot supply.** That test is the guard against this
class of defect returning; it belongs to §8.1 and lands in **I1**, beside §8.7's re-parameterisation
of the eight existing tests that assert unreachable reserves, so the two cannot land in
contradiction.

Two limitations to state in the code rather than fix by mixing dimensions:

- `C` is measured over `active_fetch_us` (`abr.rs:1265`, `:1292-1295`), which **excludes PMS
  production time**, while "recover `D` within `H`" is a wall-clock promise. The refill guarantee
  therefore over-promises by exactly the factor N6 forbids folding in. Document it.
- The production discount currently folded into the same function (`abr.rs:1525-1530`) is removed
  from the network budget by N6.

### N4 — HLS downshift becomes risk-driven, and affordability is a separate question

**Supersedes the previous §8's predicate.** Today `network_bad = immediate_network <
current_candidate.expected_wire_kbps` (`abr.rs:2136`) is a bare disjunct (`abr.rs:2143`), while
`current_risk.starvation_seconds`, computed eight lines earlier, is discarded unread.

Delete `network_bad` **as a trigger**. An **immediate** downshift may fire only on a labelled
emergency:

```
EMERGENCY  ⟺   regime collapse detected                                  (CapacityObservation)
           ∨   T = B·R/(R − C) <= starvation_fallback_secs               (C < R; else ∞)
           ∨   B < E_tx_down                       ← AFFORDABILITY, a hard guard in its own right
           ∨   (B <= emergency_buffer_ms  ∧  buffer is draining)
           ∨   route/path regime replacement
           ∨   candidate media invalid
```

**The horizon test runs on the true `B`, not on `B − E_tx`.** The two questions are separate and
must stay separate: *"does the do-nothing path starve?"* (`T`) and *"can this reserve afford the
recovery transaction at all?"* (`B < E_tx_down`). Folding the second into the first by clamping
`B_eff` to zero re-creates §0.3(2): with `E_tx ≈ 4 600 ms` against a top-rung `B_max` of
4.84–4.99 s, a **full** reserve at Uhd gives `B_eff ≈ 240 ms`, and `T_eff` would reach
`starvation_fallback_secs = 20` at a **5 % rate deficit** where the true horizon is 97 s. That is
the hair-trigger of §0.3(2), reinstalled by the fix for it.

**`E_tx_down` is not `E_tx`.** `E_tx ≈ 2.3·d ≈ 4.6 s` at a 2 s target duration is the **upshift**
budget: `candidate_warmup_budget` (`abr.rs:1967-1977`) and `candidate_prime_budget`
(`abr.rs:1948-1958`) both open with `if proposal.direction == Direction::Down { return None; }`
(`:1952-1954`, `:1971-1973`, deliberately, per `abr.rs:1966`). **The downshift — the fail-safe
itself — has no deadline at all today.** N4 gives it one: `downshift_deadline_ms`, bounded, named,
classified as an operational guard (§6.2), and `E_tx_down` is that value.

Why a transaction cost belongs in the predicate at all: the candidate transaction runs inline on
the demux worker's own loop (`ff.rs:2966`, prime arm `ff.rs:3107-3306`), the current stream is not
read while it runs, and candidate segments are fed only after `control.commit`
(`ff.rs:3292-3294`). `ff.rs:3151-3155` states outright that the existing budgets do not cover
control-plane or playlist latency.

**`network_bad` has three uses, and only the first is deleted.** Keep the other two under a new
named predicate, `collapse_target` = `immediate_network < current_candidate.expected_wire_kbps`,
retained **solely** to select the downshift target at `abr.rs:2147` — `best_for_budget(
conservative_kbps())` bounded below `current.below()` — and the `HlsReason::UnsafeCurrentState`
code at `abr.rs:2159`. The doc comment at `abr.rs:2145-2146` ("A measured link collapse must not
walk the ladder one oversized encoder at a time") is the reason, and it survives N4 intact. Delete
the trigger without this and a link collapse walks the ladder one oversized encoder at a time,
which is the behaviour that comment forbids.

Outside an emergency, a rate deficit **narrows `safe_budget` — which is a reason not to climb, not
a reason to descend — and raises the mode-comparison risk term. It does not by itself move the
rung.** The current rendition stays in the feasible set even when `C_safe < R_current`, because a
reserve that is deep relative to the deficit is safe for a long time. **Keeping a state you are
already buffered into and admitting a new one are different decisions** — the refill filter of N3 is
an *admission* constraint on moving up; it does not evict the incumbent. This is the previous §10's
incumbent clause, adopted; the rest of §10 is void (§7.C).

`starving()`'s second arm (`abr.rs:1035-1037`) is a **hard guard** in N20's sense, not a counter,
and N8–N12 must not delete it. It is re-derived against `B_max_est` and labelled in **I3b**, not
here — see §0.3(2).

The doc comment at `abr.rs:2650-2654` states the opposite intent in one over-broad sentence ("One
slow sample is acted on immediately"). It is **edited, not deleted**, and the two tests it sits
above are not touched — see §8.6.

### N5 — risk is continuous, and introduces no new parameter

**The previous §9's `r_net = tau/(T+tau)` is void.** It is *globally* rather than locally sensitive
to an unstated `tau` (`dr/dtau > 0` for every finite `T`; `r ≈ tau/T` in the tail), and its `1/T`
tail overcharges long horizons: at `T = 60 s` it charges 20–40 points on today's scale where the
bucket ladder charges 1 — contradicting the deficit principle of N2 that the same plan asserts.

Normative form, reusing horizons that already have names and product meanings:

```
r_net(T) =  0                                                  if T = ∞ or T >= T_safe
         =  (T_safe − T) / (T_safe − T_fallback)               if T_fallback < T < T_safe
         =  1                                                  if T <= T_fallback

T_safe     = starvation_safe_secs      = 60   (abr.rs:1437)
T_fallback = starvation_fallback_secs  = 20   (abr.rs:1436)

r_prod(ρ)  = clamp( (ρ_pred − production_safe_pm)
                    / max(1, production_max_pm − production_safe_pm), 0, 1 )
```

Continuous, monotone, bounded, zero cliffs, and **zero new free parameters**. `r_net = 1` below
`T_fallback` is consistent by construction, because that region is an emergency under N4 and is
decided by a hard guard rather than by utility.

**Composition, normative.** `CandidateRisk::score` becomes

```
score = round_half_up(40 · r_net) + round_half_up(20 · r_prod) + (30 if buffer_risk)
```

The three coefficients are **not new parameters**: they are today's own worst-case values at
`abr.rs:1500`, `:1503` and `:1506`, so `score_max` stays 90 and every existing ratio to
`visible_switch_cost` is unchanged at the endpoints. Two endpoint consequences are deliberate and
must be asserted in §8.1: `r_net(T ≥ T_safe) = 0` scores **0** where the ladder charged 1 (a
comfortable horizon is now free), and `r_net(T ≤ T_fallback) = 1` scores 40, unchanged.
`buffer_risk` stays the labelled boolean hard guard of N20 and is **not** normalised. **No `λ` is
introduced** anywhere by this decision.

**Scope.** `candidate_risk` is called from both paths, but `score` is read only by `hls_utility`
(`abr.rs:1692`), the panel (`ff.rs:2808` → `ui/stats.rs:673-674`) and one test (`abr.rs:3159`). The
rung path (`abr.rs:2136-2143`) reads only `production_risk`. **So this change moves mode decisions
and the read-out, not rung selection** — which is why I5's rung-tier device legs cannot grade it and
I7a's Original-recovery leg must.

**`original_utility`'s own ladder (`abr.rs:1716-1729`) is NOT in scope of N5** and keeps its
2/10/25/60 shape — plus `+20` for no measurement (`:1723-1725`) and `+4` per deficit window
(`:1729`) — until §7.A unblocks. Stated explicitly, or it will be changed by analogy.

Three requirements on the arithmetic:

- **Round toward MORE risk.** The proxies are integer divisions that truncate toward *less* risk —
  the opposite of every existing truncation in the module, all of which round toward safety
  (`abr.rs:283-287`, `:1060-1062`, `:1644-1650`). `round_half_up` in the composition above is the
  normative form.
- **`.max(1)` on every new divisor.** `AbrPolicy` derives `Default` (`abr.rs:1354`), so
  `production_max_pm − production_safe_pm` is `0` under `Default::default()`.
- **Render with the scale.** `ui/stats.rs:673-674` prints `" · risk {}"` bare on the surface whose
  purpose is being photographed by somebody diagnosing a television. **"Render a percentage" means
  `score / 90`.**

**Also in this decision, because it is the same defect on the diagnostic surface:**
`ui/stats.rs:518` paints *any* finite starvation horizon as a fault (`.fault(d.abr_starve_secs >=
0)`), including a 1 200-second one. Fix it in **I0**.

### N6 — network and production are independent constraints

Production pressure must not be expressed as a reduction in network capacity. Remove the
production fold from the network budget (`abr.rs:1525-1530`); `hls_safe_budget` becomes purely the
refill filter of N3.

Production remains a **feasibility filter and a risk term**: a candidate predicted to acquire at or
behind real time is not admissible for an upshift however fast the link is. The per-candidate
prediction `ρ_j = overhead + work_current · load_j / load_current` is retained as the shape, but its
inputs — the thirteen `production_load_pm` values — are **not identifiable today** (§7.A), so until
M3 the filter uses the measured current ρ and the two named thresholds only, and does not
extrapolate across the ladder.

### N7 — the upshift admission margin is named and kept

**Supersedes the previous §2's instruction to delete the `4/5`.** The archaeology settles it: in
v1 (`ddb7a62e`) an upshift required **two** independent 1.35× margins, one at proposal and one at
commit. At HEAD the commit-side twin is already gone — `abr.rs:2237` is a bare
`sample.network_kbps() >= candidate.expected_wire_kbps`, 1.0×, inside `candidate_ready`'s
`Direction::Up` arm — and the proposal-side margin survives only as the 1.25 implied by
`safe_budget * 4 / 5` at `abr.rs:2180`. **It is not an accidental fourth discount: it is the last
remaining admission headroom in the module, in either place.** Deleting it would leave zero at both
proposal and validation for the first time in this file's history.

Add `upshift_admission_headroom_pm = 800` to `AbrPolicy`, applied at `abr.rs:2180` and at its
telemetry mirror `abr.rs:2069`, documented with the provenance above. The previous plan's own
escape clause permits this verbatim: *"a named policy term with a physical/product interpretation,
not an inline `4 / 5`."*

Then delete the now-tautological **budget conjunct** at `abr.rs:2202` —
`safe_budget >= target_candidate.expected_wire_kbps` — which can never fail once the candidate has
been selected from `safe_budget · upshift_admission_headroom_pm / 1000` at `abr.rs:2179-2184`
(the filter is `candidate.expected_wire_kbps <= budget` at `abr.rs:1250`), and which is what
disguised the margin's existence. **Do not touch `abr.rs:2204`** (the reserve gate — §0.3(2) and
I3b) **or `abr.rs:2205`** (`&& !draining`, a hard guard under N20, whose derivation is the
2026-08-25 device finding at `abr.rs:1022-1032`).

**Two other `4 / 5` expressions in this file are NOT this one and must not be touched:**
`abr.rs:1920` (`measured_kbps.saturating_mul(4) / 5` in `startup_rung`, justified in the comment at
`:1916-1917` — no buffer exists yet) and `abr.rs:1955`
(`media_duration.as_micros().saturating_mul(4) / 5` in `candidate_prime_budget`, a transaction
deadline). Note a bare `grep "4 / 5"` returns only N7's two targets, because those two are spelled
`saturating_mul(4) / 5`; grep for both spellings.

Three host tests pin this margin — `abr.rs:3044`, `abr.rs:3086` leg 4, and `abr.rs:2619` via the
shared `prime_up` helper. Under N7 they are **policy-choice tests** (§8.3) and their expectations
are preserved **through I4**. Two of them — `abr.rs:2619` (via `prime_up`, four `observe`s) and
`abr.rs:3044` (six) — must be **re-derived at I6**, not by N7 but by N9's deletion of the
`samples_on_rung` gate: the uncertainty floor is itself a sample-count ladder (`abr.rs:264-268`,
§6.3(2)), so proposing earlier means proposing at a 500 or 300 pm floor instead of 200, and the
admitted budget falls by a third to a half. `abr.rs:3086` leg 4 goes through `settle_link`'s
80-sample loop and is unaffected; it is the only host expectation in the file traceable to an
LG-checklist device session, and `docs/lg-self-checklist.md:87` already concedes it was never
re-measured after `5a8ef2ef`. Retiring the margin is a device question (M-D6), not a host one.

### N8 — `stable_samples` is deleted as policy

`abr.rs:2210-2214` is pure counting layered on a model that has already passed every risk, budget,
buffer and production condition (`abr.rs:2202-2209`), reset at seven separate sites. Delete it and
all seven resets. It is the dominant term in the opening-seconds cost: counter spacing today is
exactly 5 segments between successive upshifts (2 cooldown + 3 stable) and 10 after a downshift.

### N9 — `samples_on_rung` is deleted as an adaptation gate, and kept as an estimator input

Delete the gate at `abr.rs:2171`. **Keep the field**: `abr.rs:2107` uses it as the production
estimator's cold-start flag (`let cold_start = self.samples_on_rung == 0;`), feeding a 1-vs-3
sample weight at `abr.rs:926` for a measured PMS reason. This is the "retain whatever sample count
the production estimator genuinely needs" requirement, and it is the only sample count that
survives anywhere in HLS policy. It is also the predicate I3 uses.

### N10 — sample-count cooldown becomes a wall-clock operational guard

`cooldown` (`abr.rs:2250-2255`, `Down => 8`, `Up => 3`) counts **segments**, and segment duration is
a client request the server may ignore, so today's guard is an unbounded amount of wall time.
Replace it with `upshift_dwell_ms`: **wall clock**, applied to the **UP path only**, explicitly
labelled an *encoder-lifecycle operational resource guard* under N20 — not ABR policy, and not
permitted to express a quality preference.

It needs a clock the controller does not have today. `SegmentSample::total_fetch_us`
(`abr.rs:1266`) is per-**request** elapsed (`ff.rs:2529-2530`) and is a duty-cycled clock that runs
slow exactly when the reserve is full and the byte cap is idling the demux worker
(`aq.rs:108-110`) — the same substitution N13 identifies as a defect in `ORIGINAL_DEFICIT_WINDOWS`.
Thread a monotonic `Instant` from the demux worker instead, in the same way N12 fixes
`advanced_by(0)`, and pass elapsed milliseconds into `observe`. **`upshift_dwell_ms`,
`reject_backoff_ms` and `sustained_unsafe_deficit_ms` are all wall clock and say so at the
definition site.**

Delete `cooldown = 1` on reject (`abr.rs:2265`) outright: the decrement at `abr.rs:2120-2122`
precedes the check at `abr.rs:2171`, so `K` blocks `K−1` segments and **`K = 1` has never blocked
anything**. Its real function is covered by N11.

*Recorded for the ledger:* v1's cooldown for a Down commit was **1**; HEAD's is **8**. An eightfold
increase in downshift stickiness landed in `5a8ef2ef` with no test naming it.

### N11 — a failure-driven reject/backoff guard (new; the previous plan had nothing here)

`reject` (`abr.rs:2259-2267`) records **nothing about what failed**, so any stateless cost — a
`PrimeCost` term included — re-proposes the identical rung on the very next segment. Each failed
prime costs ≥4.6 s of unrefilled reserve against ~3.6 s of refill, so a repeating reject is a
self-inflicted drain.

Record the rejected rung and the reason, and refuse to re-prime that rung until either the capacity
estimate has moved materially or `reject_backoff_ms` of **wall clock** has elapsed. Pin it on the
host by asserting that a reject loop's buffer trajectory is non-decreasing.

This guard is the reason N8 is safe to land; the regression-risk seat's dissent (§9) is that it
should land and be observed *first*. The sequence in §5 honours that only partially — N8 and N11
land in the same increment, gated on a device A/B — and the dissent is recorded rather than
resolved.

### N12 — `on_resume` clears everything, and the visible-switch decay actually advances

`on_resume` (`abr.rs:2050-2054`) resets `stable_samples` and neither `cooldown` nor
`samples_on_rung`, so a pause leaves a stale lifecycle guard running against an estimate that was
just deliberately aged. Whatever survives N9/N10 is cleared there. Note it also resets
`self.buffer = BufferEstimate::default()` (`abr.rs:2052`), which is why I3's regression test must
cover the first `observe` after a resume as well as after a bootstrap.

Separately, and worth more than the whole of the previous §17: `TransitionHistory::advanced_by`
(`abr.rs:1558`) has exactly one call site in the crate — `ff.rs:2946`, called with **`0`**, under a
comment saying "The worker's own clock takes over from here". It does not. With
`visible_switch_cost = 15` and `visible_switch_penalty = 15` (`abr.rs:1438-1439`), two visible
switches give `transition_cost = 45` against Original's structural advantage of 40, so **on an
ordinary non-DV item, after two mode switches, Auto can never return to Original for the rest of
the film**, while the two-minute decay designed to prevent exactly that never executes. Pass real
elapsed wall time. Three lines.

The `exp(−dt/tau)` redesign of the previous §17 is **deferred** until the decay is observed to run;
it is ungradable on a television before that. When it returns, implement it as a decayed scalar in
`route::note_visible_switch` reusing the existing `>> halvings` with its load-bearing `.min(16)` —
no `exp()`, no libm. Note `visible_switch_decay_ms` is documented as a **half-life**
(`abr.rs:1404-1405`), so a literal reading of the previous plan's formula changes the rate by 1.44×
through notation alone.

### N13 — Original persistence and probe spacing become elapsed wall time

`ORIGINAL_DEFICIT_WINDOWS = 6` (`abr.rs:647`) and the `persistent_deficit_windows * 4` term
(`abr.rs:1729`) are counts of a window that is **750 ms of active body-read time, not wall clock**
(`abr.rs:640-641`, `:771-773`) — so under backpressure, the healthy full-buffer case, one window
spans unbounded wall time. Two doc comments already disagree about the unit: `abr.rs:645` says six
windows is "four and a half seconds of real transfer" (correct) and `abr.rs:1726` reads the same
counter as "about nine seconds" of wall clock.

Replace the count with `sustained_unsafe_deficit_ms`, **wall clock**, accumulated only while the
unsafe condition holds. The 750 ms window may remain an implementation detail; the **policy** may
not be expressed in windows. The conversion from six active-read windows is not 1:1 and M2 records
the observed ratio.

`ORIGINAL_PROBE_SPACING = 3` (`abr.rs:213`) is not an Original window at all — it counts **HLS
segments** (`abr.rs:210`, `:599-600`, called per `SegmentSample` from `ff.rs:3082`). Replace it with
`probe_spacing_ms` in wall or media time. The two constants live in two unrelated clocks behind a
shared `ORIGINAL_` prefix; the rename is part of the fix.

The three Original exits keep their distinct semantics and their classification:
`ImminentStarvation` (**hard guard**, no utility veto), `SustainedDeficit` (**policy**, utility may
decide), `EmergencyLowBuffer` (**hard guard**, estimator fail-safe).

### N14 — every mode comparison scores real alternatives

**Widened from the previous §14, which named only one of the three fabrication sites.**

1. **`observe_probe`** (`abr.rs:629-631`) builds the entire HLS side of the argmax from
   `HlsActuatorCatalog::measured().candidate(Rung::P1080High)` and defaults the
   `ProductionEstimate` (`abr.rs:566`). Both go. All six required inputs are live on the demux
   worker's stack within 25 lines of the call — `remaining_ms` at `ff.rs:3008`, `telemetry`
   (carrying `current`, `optimal`, `production`) at `ff.rs:3014`, `delivery()`/`buffer()` already
   passed at `ff.rs:3027-3032`, and `catalog().candidate(controller.current())` already spelled out
   on the adjacent branch at `ff.rs:3077`. **Nothing crosses a thread** for this site.
2. **`worth_probing`** (`abr.rs:536-543`) passes the real `current` as *both* `current_hls` and
   `best_hls` and also defaults production, so the value-of-information gate scores a different
   alternative than the decision it gates — the app spends real source probes on questions the
   decision has already settled differently. Same fix.
3. **`original_utility`** (`abr.rs:1730-1735`) computes Original's own quality as
   `original_quality_bonus + hls_quality_score(catalog.candidate(Rung::P1080High))` — a constant
   116 regardless of what it is being compared against. So Original's structural advantage is +40
   against P1080High, +76 against P720 and +116 against P240, while the policy comment at
   `abr.rs:1398-1401` reasons about 40 throughout. **Fixing sites 1 and 2 without site 3 makes
   recovery easier, not more correct.**
   Original's quality must be scored from the **actual source**, never from a synthetic HLS
   reference. **Only one of the two inputs is in hand.** The average bitrate is
   `ModeInputs::source_kbps` (`abr.rs:1616`). The source raster is **not** — `ModeInputs`
   (`abr.rs:1613-1639`) has no raster field, and `abr.rs:1141` is the catalog's *unset* `source:
   (0,0)` default (declared `:1111`, set only by `limited_to` at `:1157`), which `original_utility`
   (`abr.rs:1703`) never sees. Add `source_raster: (u16, u16)` to `ModeInputs`, sourced from
   `session().cur_src` (`route.rs:532-536`) and threaded through `HlsAbrControl` to the worker —
   **that one does cross a thread**, unlike site 1's six inputs. Score it as `hls_quality_score`
   evaluated at `min(source_kbps, planning ceiling)` with `source_raster` as a cap, so the value is
   pinnable by a host test, and say in the code that it is conservative.

Two ordering constraints: consume `best_sustainable` directly rather than `telemetry.optimal`,
whose value moves through the margin named in N7; and note that the site-3 fix and the site-1 fix
push the recovery decision in *opposite* directions, so they must land together or the intermediate
state is worse than either.

Also normative, from the previous §18 and uncontested: probe only when the reserve is safely above
emergency and not materially draining, HLS evidence suggests spare capacity, `probe_spacing_ms` has
elapsed, a successful probe *could* change the decision, and enough playback remains for Original
to repay a visible switch. Do not require the top rung. A truncated probe is **not** a completed
low-capacity sample.

### N15 — a rung change is not universally invisible

**The previous §10's premise "HLS rung switches are invisible to the viewer" is void as stated.**
The device evidence establishes *acceptance*, not invisibility:
`docs/pms-hls-protocol-probe.md:154-155` records that a 720p→1080p→720p fixture proved the pipeline
**accepts** in-band raster changes on one Load. The repo's own manifest says the opposite in product
language — `tests/manifest.json:1591`: "five to ten visible quality changes to the person watching".

Eight of thirteen rungs share 1920×1080 (`abr.rs:129-141`) and are genuinely eventless; four of
twelve adjacent steps cross a raster band and are a different class of event. **Any future
transition or prime cost must be raster-aware** — a two-line predicate on `Rung::raster()`. Until
such a cost exists, N10's dwell guard is the operational stand-in and the raster asymmetry is
recorded here so that it is not rediscovered later.

Instrumentation is nearly free: `RE_ABR_COMMIT` already parses width and height
(`tests/run.py:1227`) and `tests/run.py:1256-1258` already discards them. A `raster_changes_max`
assertion in `a_abr_shape` lands in **I0**, so M2's baseline includes a raster-crossing count.

### N16 — mode utility keeps one framework; the feature bonus is split

Adopted from the previous §15, with the DV/Atmos split made **mandatory rather than optional**:
`route.rs:571-577` returns `candidate.dovi.profile > 0 || candidate.immersive` as one boolean worth
a flat `original_feature_bonus = 25` (`abr.rs:1442`), so an Atmos-only film buys two visible reloads
for a benefit inaudible on TV speakers, priced identically to a Dolby Vision panel-mode change.
Split them, keep the **ordering** load-bearing rather than the magnitudes (§6), and constrain where
the feature term may act: the non-emergency arms only, never `ImminentStarvation` or
`EmergencyLowBuffer`.

Original's server cost for video encode is 0 and stays 0.

### N17 — the documentation states what the code does

Four sentences are false today and are corrected in **I1**:

- `docs/adaptive-playback.md:1-4` — "probabilistic risk estimation" / "risk-aware stochastic ABR
  controller". There is no random variable, distribution, quantile or confidence interval anywhere
  in `abr.rs`; this is the only occurrence of the claim in the repository, and the module's own doc
  never makes it. **The controller is described as an uncertainty-aware, risk-aware
  throughput/buffer ABR controller.**
- `docs/adaptive-playback.md:69` (and `abr.rs:1374-1376`, and a test name at `abr.rs:3459`) — "a
  whole-file average is a lower bound on demand". False: over a full transfer the average *is* the
  demand. The defensible claim is the next clause's — it is not an **upper** bound on
  **short-horizon** demand, and the buffer horizon is a short-horizon question.
- `docs/adaptive-playback.md:135-136` — "stores the request beside the measured output". True for
  2 of 13 rows (`abr.rs:1126-1138`); eleven are the request by construction. The sibling field is
  already scrupulous about exactly this (`abr.rs:1074-1075`: "Two values are measured and the rest
  are an ordering assumption"). Rename `expected_wire_kbps` → `planning_wire_kbps` and split the
  struct: `request_kbps` / `planning_wire_kbps` / `observed_wire_kbps: Option<u32>`.
- `docs/adaptive-playback.md:145-148` — "the best actuator that fits **it**" omits the margin named
  in N7.

Two further prose corrections, because they misattribute what a device case grades:
`tests/manifest.json:1591` and `tests/run.py:1243-1245` both claim `max_commits` grades "the
decaying transition penalty". It cannot — `transition_cost(Hls, Hls)` is **0** (`abr.rs:1591-1593`,
pinned at `abr.rs:3402`) and `pipe_abr_oscillating_link` sets `source_kbps: 60000` so Original is
infeasible. That case grades the sample counters and nothing else, which matters because it is the
**only** device coverage the counters have.

Where the repository already holds a measurement that contradicts the catalog, say so rather than
silently keeping the request: `docs/pms-hls-protocol-probe.md:18-19` records 720 → ~425 kbps and
12 000 → ~11.356 Mbps.

### N18 — remaining-playback scale: §16's clamp already exists (no-op); the scale then had to
### be applied to the recurring terms on BOTH sides of the argmax, not one

`benefit_scale_pm` (`abr.rs:1644-1651`) already *is* the previous §16's linear clamp against
`benefit_horizon_ms = 120_000` (`abr.rs:1443`), and no `if remaining < X` cliff exists anywhere in
`abr.rs`. Confirmed no-op; recorded so it is not re-implemented.

**But the previous §10's `g(remaining)·Q` on rung selection is void** (§7.C). It scales exactly one
recurring term and leaves risk, server cost and production unscaled, making effective risk aversion
inversely proportional to remaining playback. Reproduced in the shipped integers: at 6 s remaining
the argmax is P720; at 2 s it is a tie between P240 and P480.

The same defect exists at `abr.rs:1741` in `original_utility` **and is fixed there by scaling all
recurring terms by `benefit_scale_pm`** — `quality` (`:1730`) and `features` (`:1736`) already are;
`risk_cost` (`:1741`) is not and becomes so. One-off costs, i.e. `transition` (`:1740`), stay
outside the scale. **Lands in I7a.**

### N19 — validation thresholds are named, not resolved

`candidate_ready`'s hardcoded `production_ratio_pm() <= 800` (`abr.rs:2238`) is a third unnamed
production constant sitting between `production_safe_pm = 750` and `production_max_pm = 1_100`
(`abr.rs:1428-1429`). **Name it at its current value** as `candidate_accept_pm = 800`. **Do not
resolve it to either neighbour**: that is a 6 % tightening or a 37 % loosening and no verification
tier can currently tell them apart. Flagged in §6.3(3) as a design defect — three constants, one
question — and unblocked by M3.

Candidate timing budgets likewise derive from named transaction policy, not from inline fractions.
A warm-up segment may legitimately carry a larger budget because encoder start-up is real; that is
**transaction policy**, not ABR mathematics, and is labelled as such.

### N20 — hard guards are labelled, in code and in prose

Every rule is exactly one of: **hard safety guard**, **ABR policy**, or **operational resource
guard**. The classification is written at the definition site.

Hard guards (each one in the file gets a label): technically impossible raster or codec; malformed
candidate media; emergency-low buffer while draining; `T <= starvation_fallback_secs`;
`B < E_tx_down` (affordability); route/path regime replacement; a prime whose elapsed cost would
itself threaten playback; `starving()`'s low-buffer arm (`abr.rs:1035-1037`); **`&& !draining` on
the upshift path (`abr.rs:2205`)**, whose derivation is the 2026-08-25 device finding at
`abr.rs:1022-1032`.

Operational resource guards: `upshift_dwell_ms`, `reject_backoff_ms`, `downshift_deadline_ms`,
`candidate_warmup_budget`, `candidate_prime_budget`.

Everything else flows through estimation → risk → utility. **No policy may be disguised as a
lifecycle counter**, which is the failure mode this whole plan exists to correct.

### N21 — `production_bad` loses its persistence requirement, deliberately

`production_bad = current_risk.production_risk && self.buffer.draining_samples >= 8`
(`abr.rs:2137-2138`) requires **eight consecutive** draining segments — ~16 s at 2 s segments —
before a server falling behind may move the rung, while `starving()` (`abr.rs:1036`) treats two as
enough. Replace the count with the magnitude predicate `&& self.buffer.draining()`
(`abr.rs:1030-1032`), whose derivation is the 2026-08-25 device finding at `abr.rs:1022-1029`.

**State it as what it is: dropping the persistence requirement entirely — an 8× increase in
sensitivity on an immediate-downshift arm — not a 4× reconciliation of two counters.** If it proves
too eager, the fallback is `>= 2`, matching `starving()`. Lands in **I6**, with a host differential,
because the only device grading available (`max_commits: 8` on `pipe_abr_oscillating_link`) has two
commits of headroom and cannot see one or two extra production-driven downshifts.

---

## 3. Numerical safety (normative, and testable)

`C` is **not bounded** by anything today: `clamped_to_evidence` clamps **Weak** samples only
(`abr.rs:418-425`), and `abr.rs:410` records a real device reading of **865 Gbit/s**. Every new
expression in this plan multiplies a capacity by a time.

1. **Widen, then narrow explicitly.** Every capacity × time or capacity × per-mille expression is
   computed in `u64`/`i64` and narrowed with `.min(u64::from(u32::MAX)) as u32`, exactly as
   `abr.rs:284-287` already does. `C_safe · H` with `H = 10_000` overflows `u32` at any capacity
   above ~430 Mbps, which this project has measured in the field.
2. **Host and device must not disagree silently.** `[profile.release]` sets only `opt-level = 2`
   (`rust-modules/Cargo.toml:112-113`), so `overflow-checks` is **off** on the television and
   **on** under `cargo test`. The same expression panics on the host and wraps on the device. No
   new expression may rely on either behaviour: all arithmetic on the decision path is explicitly
   `checked_*`/`saturating_*`/widened, so the two configurations are indistinguishable by
   construction.
3. **A mathematical-invariant test asserts it.** Drive every public entry point on the decision
   path with the extreme magnitude set — capacities `{0, 1, 320, 22_000, 865_000_000, u32::MAX}`,
   buffers `{0, 1, 2_500, 120_000, i64::MAX/2}`, ρ `{0, 250, 1_100, u32::MAX}` — and assert no
   panic, no wrap, monotonicity where the model claims it, and **that the admissible set is a
   prefix of the ladder** (N3 point 2). This test is the reason the configuration difference stops
   mattering.
4. **`.max(1)` on every divisor**, because `AbrPolicy` derives `Default` (`abr.rs:1354`).
5. **Round toward safety.** Truncation direction is part of the specification, not an artefact:
   capacity rounds down, requirement rounds up, risk rounds up (N5's `round_half_up`).

---
## 4. Measurement-first sequence

**No change to `abr.rs` DECISION BEHAVIOUR may land before M1 and M2 are recorded.** I0 and I1 are
exempt by construction and are **prerequisites of M2, not violations of it**: I0 touches only
`tests/run.py`, log lines, dev triggers and `#[cfg(test)]` code, and I1 changes no expected value.
The M2 baseline is therefore taken on **`5a8ef2ef` + I0 + I1**, which is behaviour-identical to
`5a8ef2ef` and provably so (`make check` green with zero expected-value changes). **Record both
SHAs in `tests/README.md`.** This is a refinement of the requested order, not a departure from it:
step 5's instrumentation half necessarily precedes steps 2 and 4, because those steps are measured
with it.

| # | step | where | blocks |
|---|---|---|---|
| **M1** | **Freeze the baseline.** Record `5a8ef2ef` and the `+I0+I1` SHA in `tests/README.md` as the adaptive-playback baseline pair, with the exact build configuration used for M2. | desk | everything |
| **M2** | **Record baseline device behaviour.** Run the four `pipe_abr_*` cases on the baseline pair with the new assertions armed (`min_buf_ms`, `dip_max_kbps`, `max_stall_s`, `raster_changes_max`) and write the results into `tests/README.md`. **There is no recorded device result for any of these cases anywhere in the repository today**, and every increment that lands first makes the "before" unrecoverable. Also record the observed ratio of active-read time to wall clock, which N13's conversion needs. Take M4 and M-D6 in the same lease. | television, 1 lease | I3 onward |
| **M-D6** | **The 17.5 Mbit/s leg.** A new flat shaped case at 17 500 kbps (`tools/netcond.py --rate`) with settle bounds, run against the M2 baseline binary. *Falsifies:* settling on P1080M10 confirms `upshift_admission_headroom_pm = 800` costs a rung at 57 % utilisation; settling on P1080M14 says it does not. Closes `docs/lg-self-checklist.md:87`, which records the leg as never re-measured after `5a8ef2ef`. | television, same lease as M2 | N7's retention of the margin |
| **M3** | **Extend `tools/pms-hls-probe.py` to measure production cost per actuator.** For each of the thirteen request ceilings, against one 4K and one 1080p source, fetch ≥10 consecutive segments and record `total_fetch_us / media_duration_ms` (ρ), cold and warm. **Runs on the dev Mac. No television, no lock, no `make`.** Compare on the **residual**, as `predicted_ratio_pm` does (`abr.rs:977-983`): compute `load_j ∝ (ρ_j − ρ_floor)` normalised so the P1080High point reads 1000, with `ρ_floor` from the lowest rung's measured ρ. Run **both** a back-to-back leg and a `--pace 2.0` leg (`tools/pms-hls-probe.py:1013`) and state which the catalog is meant to match; the paced one is what the app experiences. *Falsifies:* residual loads within ±15 % of `abr.rs:1126-1138` uphold the "inert argmax" finding and keep §7.A closed; any mid-ladder load off by >25 % means the deferred argmax is a re-parameterisation on fresh numbers and must be argued on those. **Record the raw ρ per rung as well**, because `abr.rs:1074-1083`'s own anchor (0.21 at P1080High) sits *below* `production_floor_pm = 250`, which makes `predicted_ratio_pm` from the top rung return the measured ratio unchanged for every candidate — a property M3 should confirm or refute rather than inherit. | host | §7.A, N19, N6's extrapolation |
| **M4** | **Measure the settled `buf=` at high rungs and at more than one segment duration.** *Prerequisites:* I0's `abr: sample` line (media kbps + buf, on every segment) **and** I0's rung-pin trigger. Unshaped LAN ≥25 Mbit/s so the AQ cap is the only limiter; one ≥40-minute 1080p item forcing a video transcode. Arm `/tmp/plxnative-abrrung=<kbps>` for each of 720, 4 000, 10 000, 16 000, 20 000 in turn for ≥3 minutes, discarding the first 30 s. **The playback-quality selector CANNOT be used for this**: `route::hls_abr_control()` returns `None` for any non-Auto quality (`route.rs:581`), and the quality ladder has no mid-1080p rungs (`plex/session.rs:190-212`). Record the audio ES rate beside the video one. *Falsifiers, split by which lane binds:* a **video-bound leg** (10 000 and above) must track `1 600 + 67 108 864/R_video` within ±20 %; an **audio-bound leg** (720) must track `3 600 + 8 388 608/R_audio`. **The single most valuable byte: is the settled `buf=` at 20 000 above or below 6 000 ms?** The 4 s `EXT-X-TARGETDURATION` leg needs **new 4 s fixture clips** in `make fixtures-pipeline` and a `#EXTINF:4.0` playlist — `tests/serve_fixtures.py:141-146` hard-codes 2 s — and the pipeline tier cannot substitute for the PMS legs at all, because `serve_fixtures.py:71-78` serves the same `pipe_abr_1080p.ts` for every rung from 6 000 to 20 000. | television, same lease as M2 | N3's `buffer_target_ms` and α, N4's guards, §0.3(2) |
| **M5** | **Instrumentation and the closed-loop host simulator.** See I0. No policy change. | host | I3 onward |
| **M6** | **Land the zero-risk correctness fixes.** See I1. | host | — |
| **M7** | **Fix the confirmed control-law defects, without the deferred argmax.** I3–I7b. | host + device | — |
| **M8** | **Re-run the device traces** against the M2 baseline, case for case, with the same assertions. Follows I7b. | television | — |
| **M9** | **Only then revisit utility-based HLS selection and quality scoring**, on measured production loads (M3) and observed output raster (§7.B). | — | — |

---

## 5. Implementation sequence

Each increment is independently landable and independently verifiable. **★ marks a policy-surface
change** — those can only be trusted after a device session, **no two ★ increments may land in one
commit, and a single ★ increment may contain only one policy surface.**

| # | increment | contents | verified by | must not be entangled with |
|---|---|---|---|---|
| **I0** | **Instruments** (M5) | (a) Extend `RE_ABR_STEADY` (`tests/run.py:1226`) to capture `buf=(\d+)ms` and add `min_buf_ms` to `a_abr_shape`. (b) Add `dip_max_kbps` (`min(visited)`, already computed at `tests/run.py:1260`). (c) Add `max_stall_s` from the 1 Hz `pos=` series (`tests/run.py:829`) — **state its resolution: `RE_POS` captures integer seconds off a once-per-second heartbeat, so it is ±1 s and cannot see a sub-second stall.** (d) Add `raster_changes_max` — commits whose `Rung::raster()` differs from the previous commit's; `RE_ABR_COMMIT` already parses width and height (`tests/run.py:1227`). (e) **Make the buffer sample decision-independent**: publish `buf=`, `current=` and the segment media rate on *every* segment as an `abr: sample` line beside `publish_hls_abr_model` (`ff.rs:3015`), because `abr: steady` is emitted only on `Decision::Stay` (`ff.rs:3016-3018`) — until this lands, `min_buf_ms` cannot see the trough and `dip_max_kbps` is an order statistic over a sample the policy under test controls. (f) `SegmentSample::media_kbps()`; carry the last measured media rate on `Controller` and in `ControllerTelemetry`. (g) **A rung-pin dev trigger** `/tmp/plxnative-abrrung=<kbps>`, read through `dev::read`, clamping `observe`'s proposal to that rung while leaving the estimator, buffer model and log lines fully live — **this is what makes M4 executable**. (h) A manifest key `"abr_policy": "legacy"｜"new"` that `case_triggers` turns into a `plxnative-abrpolicy` file — without it the I5/I6 A/B trigger cannot be used with `tests/run.py` at all, because `apply_triggers` (`tests/run.py:795`) wipes every `plxnative-*` in the runtime root before each case. (i) The closed-loop host simulator under `#[cfg(test)]`: a link trace, a byte-capped buffer integrating `C/R − 1`, **and a transaction model — on `Prime`, advance the clock by `E_tx` with zero fill, then deliver the candidate sample** (`ff.rs:3107-3306`, feed only at `:3292-3294`); without the transaction model it reports zero stalls for exactly the regression N8/N11 trade against. (j) **The frozen trace library**, an I0 deliverable: six traces — the four `pipe_abr_*` shaped profiles, one flat 17 500 kbps leg, one monotone decay — each stored as `C(t)` in kbps at segment granularity in a checked-in file, append-only, extended by M2's device traces. (k) Fix `ui/stats.rs:518`. | `make check` + reading | everything |
| **I1** | **Zero-risk correctness** (M6) | N2's rename; N17's four doc sentences and two manifest/harness sentences; N19's naming of `800`; N20's labels; the `planning_wire_kbps` rename and struct split; make `MAX_FEED_AHEAD_NS` and `AUDIO_SLACK_NS` `pub(crate)` and re-export through `player/mod.rs`; the §3.3 numerical-safety invariant test; the N3 `B_max`-derived reachability invariant test (§8.1); **§8.7's re-parameterisation of the eight tests asserting unreachable reserves**. No behaviour change. | `make check` | I2+ |
| **I2** | **Device baseline** (M2, M4, M-D6) | Run I0's assertions against **HEAD + I0 + I1**, whose decision behaviour is identical to `5a8ef2ef`; take the buffer census and the 17.5 Mbit leg; record both SHAs and all results in `tests/README.md`. | television, one lease | any `abr.rs` behaviour change |
| **I3 ★** | **First-segment false downshift** (§0.3(1)) | `abr.rs:2107` already computes `let cold_start = self.samples_on_rung == 0;` — true exactly on the first `observe` of a rung, captured *before* `buffer.update` runs. Gate the whole downshift trigger on it: `if !cold_start && (buffer_bad ǀǀ network_bad ǀǀ production_bad)`. **Both disjuncts of `buffer_bad` must be neutralised**, not only `starving()`: at a ~1 958 ms first reserve against a 2 000 ms segment, `buffered < segment` is true on its own. **`buffer.samples == 0` is NOT a usable predicate** — `BufferEstimate::update` increments it (`abr.rs:1019`) at `abr.rs:2110`, before `abr.rs:2142` reads it. Regression test: the first `observe` of a bootstrap controller must not return `Prime(Down)`, **and** the first `observe` after `on_resume` must not either (`abr.rs:2052` resets the buffer estimate). | host test + one device leg | I3b, I6 |
| **I3b ★** | **Threshold collisions** (§0.3(2)) | (a) `starving()`'s second arm re-derived against `B_max_est` and labelled a hard guard. (b) A ruling on `abr.rs:2204`: express the upshift reserve gate as `min(3·segment, α · B_max_est(R_target))`, **or** state normatively that it stays and the top three rungs are jump-only by design, with the reason written at the site. | host differential + one device leg | I5 — any measurement of I5 before this measures this instead |
| **I4** | **Naming and deletion** | N7's `upshift_admission_headroom_pm` at `abr.rs:2069`/`:2180`; delete the tautological budget conjunct `safe_budget >= target_candidate.expected_wire_kbps` at **`abr.rs:2202`** (NOT `:2204`, NOT `:2205`); delete the ms-from-kbps branch (`abr.rs:1531-1534`); N6's removal of the production fold from the network budget (`abr.rs:1525-1530`); N12's `on_resume` clearing; N12's three-line `advanced_by` fix. | `make check` — **zero expected-value changes, which is the claim that licenses landing this without a device session; if any expectation moves, stop and reclassify** | I5, I6 |
| **I5 ★** | **N3 + N4 + N5** (one surface: the emergency/admission law) | The per-candidate refill filter at `buffer_target_ms = 2 500`; the emergency predicate with the affordability guard separate from the horizon test; `downshift_deadline_ms`; `collapse_target` retained for the target selector and reason code; the piecewise risk and its composition; risk rendered as `score / 90`. | host differential (**including: the emergency predicate must NOT fire at `B = B_max(P1080High)` with a 5 % deficit**) + closed-loop sim + device A/B on `pipe_abr_brief_dropout` and `pipe_abr_oscillating_link`. `min_buf_ms`/`dip_max_kbps` are comparable across legs **only after I0(e)**; state that in the M2 write-up. | I3b, I6 |
| **I6 ★** | **N8–N11 + N21** (one surface: counters → guards) | Delete `stable_samples`; delete the `samples_on_rung` gate, keep the field; wall-clock `upshift_dwell_ms` on the UP path; the reject/backoff guard; N21's `production_bad`. **Re-derive `abr.rs:2619` and `abr.rs:3044` here** (N7, §8.6). | closed-loop sim over the frozen trace library, both parameter sets, **a stall regression in any leg disqualifies** + device A/B on `pipe_abr_oscillating_link` against `max_commits: 8` — the board's simulation gives 6 gated / 13 ungated, two commits of headroom, so **7–9 is inconclusive and must be re-run**. Add a host differential for N21. | I5 |
| **I7a ★** | **N14** (one surface: the mode comparison) | The three fabrication sites together (`observe_probe`, `worth_probing`, `original_utility`'s baseline) + `ModeInputs::source_raster` plumbing and its construction sites + `abr.rs:1741`'s scaled risk term (N18) + log the `ModeUtility` decomposition on the `abr: mode` line (§7.H). | host + a device leg on **Original recovery** | I5, I6, I7b |
| **I7b ★** | **N16 + N13** (one surface: when Original is abandoned, and what it is worth) | The DV/Atmos split; `sustained_unsafe_deficit_ms` and `probe_spacing_ms` in elapsed wall time. | host + a device leg on the **Original→HLS fallback** — the `pipe_abr_*` cases cannot reach it; name the case that can, or state the leg is a hand-driven `netcond` session | I7a; then M8 |
| **I8 ★** | **Seek estimate preservation** (§7.G) | A `CapacityEstimate` snapshot (`slow_kbps`, `fast_kbps`, `uncertainty_pm`, `samples`) stored in session state at teardown and consumed by the new `Controller` as its seed, replacing `auto_prior_kbps` on the seek path. `route.rs:700` keeps writing `auto_prior_kbps` as the **Original-fallback-only** seed and is not otherwise read on the seek path. Reset positional buffer and risk history; reset any pending transaction that no longer describes the seeked position. | host + a device leg: seek twice on a healthy link and assert the rung does not re-ramp | — |
| **I9** | **BLOCKED — and M3 is now DONE, which narrows rather than lifts it** | §7.A's argmax and §7.B's quality scoring. M3 ran 2026-08-28 (`docs/measurements/m3-production-census.md`): `Uhd = 2100` measures 2404 and the whole 1080p block lands within 10.3%, so the two empirical points and the interpolation between them are **upheld**; `P1080M6 = 900` measures 353, off by **60.8%** in both pacing legs, which is past the 25% rule and REFUTES the mid-ladder. The larger finding is that the table is indexed by the wrong VARIABLE: against a 1080p source the ordering INVERTS (58 of 75 rung pairs), because a target below the source raster is downscaling work while a target at it is a near-copy — so no thirteen numbers keyed to the target alone can be right for both a 4K and a 1080p item. **Half of that is now UNBLOCKED (2026-08-28).** The 4K `auto_network` case exists: `serve_fixtures.py` answers rung 22000 with a real 3840x2160 clip, `route::arm_auto_fixture` takes the source raster instead of hardcoding 1080p, and `pipe_abr_uhd_source_admits_4k` declares it. The two were circular — the literal was there because the rung 404'd — so a fixture gap had been standing in for a policy, and the two entries the table calls empirical were the two nothing could reach. What remains is the decision on whether `production_load_pm` becomes a function of (source, target) or is declared correct for one source class only. That decision is a product call and is not made here — **and it should not be made on Result 2's stated cause**, which the census has since narrowed: the ordering is measured, but the same numbers also fit "PMS stopped re-encoding at an unbinding ceiling", and the output column that separates the two was not recorded the first time. It is recorded now; the re-run is owed. | — | — |

---

## 6. Calibration ledger

`identifiable today` means: **is there a measurement this project can actually take that pins the
value**, as opposed to a host test asserting a number somebody already chose. A "no" is a design
defect to be recorded, not a tuning task. Every constant that governs a decision has a row,
including the ones that cannot be identified.

### 6.1 Kept, anchored

| parameter | meaning | unit | value | source | observable | identifiable today | device validation |
|---|---|---|---|---|---|---|---|
| `visible_switch_cost` | cost of one visible reload | utility pts | 15 | **derived** — an indifference statement with a stated unacceptable outcome (`abr.rs:1391-1397`) | mode-switch count per playback | **yes** | no — it is the numeraire |
| `visible_switch_penalty` | added pressure per recent switch | utility pts | 15 | empirical | switches per hour | partly | after I4, with the decay running |
| `visible_switch_decay_ms` | **half-life** of that pressure | ms, wall | 120 000 | empirical | interval between visible switches | no — the decay has never run (N12) | yes, after I4 |
| `starvation_fallback_secs` | emergency horizon | s | 20 | product | must be ≥ `E_tx_down` | partly | **M4** (yields `E_tx`/`E_tx_down`) |
| `starvation_safe_secs` | comfortable horizon | s | 60 | product | is it reachable at all at the top rungs? | partly | **M4** |
| `production_safe_pm` | server real-time margin, comfortable | pm | 750 | measured PMS cadence | ρ per rung | **yes** | **M3** |
| `production_max_pm` | server real-time margin, unacceptable | pm | 1 100 | measured PMS cadence | ρ per rung | **yes** | **M3** |
| `production_floor_pm` | fixed per-segment overhead | pm | 250 | measured | ρ at the lowest rung | **yes** | **M3** — note `abr.rs:1074-1083`'s anchor ρ = 0.21 sits *below* it |
| `emergency_buffer_ms` | fail-safe floor | ms | 2 000 | hard guard | relative to `E_tx_down` | yes | **M4** |
| `stale_half_life_ms` | ageing of an unmeasured gap | ms, wall | 30 000 | empirical | estimate error after a pause | partly | I8's pause leg |
| `original_quality_bonus` | Original's structural advantage | utility pts | 40 | empirical — and today applied against a **fabricated** baseline (N14 site 3) | mode choice vs. source properties | not until I7a lands | yes, I7a's leg |
| `benefit_horizon_ms` | amortisation window | ms, wall | 120 000 | product judgement, correctly so | mode choice as a film ends; `pipe_abr_*` end-of-film leg | judgement, but its *effect* is observable | not required |
| `risk_weight` | risk-to-quality exchange rate | ratio | 2 | empirical | none directly: utility is scale-invariant, only the ratio to `visible_switch_cost` is observable | **no** — §6.3(1) | not required, see §6.3(1) |
| `server_cost_weight` | server-work-to-quality exchange rate | ratio | 4 | empirical | as above | **no** — §6.3(1) | not required, see §6.3(1) |

### 6.2 Introduced, renamed, or re-scoped by this plan

| parameter | meaning | unit | value | source | observable | identifiable today | device validation |
|---|---|---|---|---|---|---|---|
| `upshift_admission_headroom_pm` | **the** admission margin; v1's 1.35 reduced once, its commit-side twin already deleted | pm | 800 | **empirical, with provenance** (`ddb7a62e` → HEAD) | settled rung at a known shaped rate | **yes** | **M-D6** |
| `candidate_accept_pm` | candidate validation ρ threshold | pm | 800 | **temporary** — preserved at its current value | ρ at commit vs. reject | **NO** — 750 vs 800 vs 1 100 is unresolvable on any tier today | **M3** |
| `buffer_target_ms` | `B*` ceiling (was `minimum_buffer_ms`) | ms | 2 500 | derived; **unchanged deliberately** | settled `buf=` per rung | **yes** | **M4** |
| `buffer_reserve_fraction_pm` (α) | fraction of the reachable ceiling we ask for | pm | 500 | derived from the byte caps | `buf=` vs. `B_max` | **yes** | **M4** |
| `buffer_refill_horizon_ms` (H) | how fast a deficit must close | ms, wall | 10 000 | **temporary** | refill slope after a dip (`min_buf_ms` recovery) | **weakly** — inert at today's `B*`, so unobservable until `buffer_target_ms` moves | after M4 |
| `assumed_audio_kbps` | audio ES rate when unmeasured, for `B_max_est` | kbps | 192 | **temporary** | audio ES rate on the `abr: sample` line | **yes** | **M4** |
| `E_tx` | unrefilled playback an **upshift** transaction costs | ms, wall | **~5 200 (2.6·d at d = 2 s)** — this row said 2.3·d, which was right while `candidate_prime_budget` was a literal `4/5·d` and wrong once it started reading `production_max_pm` | **derived, unmeasured** | elapsed between prime and commit | **yes, cheaply** | **M4** |
| `E_tx_down` = `downshift_deadline_ms` | the same for a **downshift**; today unbounded | ms, wall | TBD from M4 | derived | elapsed on the downshift commit line | **yes** | **M4** |
| `upshift_dwell_ms` | encoder-lifecycle guard (**operational**) | ms, wall | **LANDED I6, and as a non-tunable**: it IS `E_tx`, evaluated per segment from the two deadlines the transaction is already held to. No `AbrPolicy` field exists | derived | commits per minute | **yes** | I6's A/B |
| `reject_backoff_ms` | failed-prime rate limit (**operational**) | ms, wall | **LANDED I6, and as a non-tunable**: releases at `refill_time(E_tx) = E_tx·R/(C−R)` or on the estimator's own uncertainty band. No `AbrPolicy` field exists | derived | reject → re-prime interval | **yes** | I6's A/B |
| `probe_spacing_ms` | minimum spacing between source probes | ms, wall | ≈ 6 000 (today's 3 × 2 s segments) | derived from the current count | probe interval in the event log | **yes** | I7b's leg |
| `sustained_unsafe_deficit_ms` | Original persistence threshold | ms, **wall** | ≈ 4 500 — today's six windows of 750 ms *active read*, re-expressed as wall clock; **the conversion is not 1:1 and M2 records the observed ratio** | derived | deficit duration before the exit fires | **yes** | I7b's leg |
| `raster_crossing_multiplier` | a raster change is a different class of event (N15) | ratio | TBD | derived | `raster_changes_max` in `a_abr_shape` | **yes**, cheaply | I6/I7 |
| `dv_bonus` | Dolby Vision preserved by Original | utility pts | split from 25 | product ordering (DV is first) | mode choice on a DV item vs. a plain one | ordering yes, magnitude no | I7b's leg |
| `generation_loss_bonus` | no re-encode | utility pts | split from 25 | product ordering (second) | as above | ordering yes, magnitude no | I7b's leg |
| `atmos_bonus` | Atmos/lossless audio preserved | utility pts | split from 25 | product ordering (last — inaudible on TV speakers) | mode choice on an Atmos-only item | ordering yes, magnitude no | I7b's leg |
| `planning_wire_kbps` (13 entries) | conservative planning ceiling | kbps | request for 11 of 13; measured for `P1080High` (20 011) and `Uhd` (20 895) | **mixed, and now labelled as such** | advertised vs. requested rate | partly — the repo already contradicts two rows (`docs/pms-hls-protocol-probe.md:18-19`) | **M3** |
| `production_load_pm` (13 entries) | relative PMS work per rung | pm | 90 … 2 100 | **empirical for 2, "an ordering assumption" for 11** (`abr.rs:1074-1082`) | residual ρ per rung, normalised (M3) | **NO** | **M3** — this is what blocks §7.A |

### 6.2b Kept, unidentified — no measurement exists and none is planned

| parameter | meaning | unit | value | source | observable | identifiable today | device validation |
|---|---|---|---|---|---|---|---|
| `vbr_allowance_pm` (rename to say **burst**) | short-horizon VBR burst on the DEMAND side | pm | 1 350 | **empirical** | none available: no demand-side variance is modelled anywhere (§7.E) | **NO** — §6.3(4) | none defined |
| `bootstrap_confidence_pm` | cold-start Original admission margin | pm | 1 350 | **empirical** | Original admissions at cold start vs. at recovery | **NO** — §6.3(4) | none defined |
| `candidate_warmup_budget` | warm-up deadline (**operational**) | ms, per-request | 3/2 · d (`abr.rs:1967-1977`) | derived from segment duration | reject rate attributable to timeout | partly | I6's A/B |
| `candidate_prime_budget` | prime deadline (**operational**) | ms, per-request | 4/5 · d (`abr.rs:1948-1958`) | derived from segment duration | as above | partly | I6's A/B |
| uncertainty floor ladder | estimator confidence floor by sample count | pm | 500 / 300 / 200 (`abr.rs:264-268`) | **empirical** | admitted budget vs. sample index | **NO** — §6.3(2) | I6's A/B, which moves it |
| `MAX_UNCERTAINTY_PM` | cap on the confidence discount | pm | 500 (`abr.rs:449`) | **empirical** | as above | **NO** | as above |

### 6.3 Design defects, flagged

1. **`risk_weight`, `server_cost_weight`, and any future `λ_net`/`λ_prod`/`PrimeCost` have no
   identification procedure.** Utility is invariant to a common scale, so only ratios matter, and
   the pairs where a miscalibrated ratio bites (P1080High vs Uhd; adjacent 1080p rungs) are
   precisely the ones no verification tier can reach. **This is the principal reason §7.A is
   deferred rather than amended.** If a utility argmax returns, every weight lands with a written
   indifference statement, a host test asserting it, and a device-observable consequence, expressed
   as a ratio to `visible_switch_cost` as numeraire.
2. **The uncertainty floor is itself a sample-count ladder** (`abr.rs:264-268`; and a three-way
   constant 250/500/250 at `abr.rs:943-949`). The previous plan's instruction that "evidence
   sufficiency should come from estimator uncertainty" is therefore **circular**: it replaces one
   sample count with another, one layer down. Make the floor decay with elapsed measured **wall**
   time. This is the only part of the counter removal that delivers the counter removal's own
   stated principle, the previous plan did not contain it, and **I6 moves two host expectations
   through this mechanism** (N7).
3. **Three constants, one question:** `production_safe_pm = 750`, `candidate_accept_pm = 800`,
   `production_max_pm = 1_100` all answer "is the server keeping up". M3 collapses them or proves
   they are distinct.
4. **Two 1.35s, one question, answered 2× apart.** Cold start (`bootstrap_confidence_pm`, applied
   bare at `abr.rs:203-208`) and recovery (`vbr_allowance_pm` compounded with `conservative_kbps` at
   `abr.rs:625-626`) apply **1.35× versus 1.69–2.70×** to the same question. There is a real reason
   for asymmetric conservatism — **at cold start there is no visible switch to pay for; at recovery
   there is** — and neither the previous plan nor the audit stated it. Document that reason at both
   sites, or unify them; do not leave two undocumented gates 2× apart. The previous plan's
   instruction to drop the allowance from runtime is void (§7.E).

---

## 7. Deferred and blocked, with unblock conditions

**A. The thirteen-way utility argmax (previous §6, §10).** *Blocked.* It is provably inert on
today's catalog over the admitted set — swept across 11 budgets × 4 source shapes × 7 weight pairs,
the only cell that ever moves without wrecking the ladder is Uhd → P1080High — and its inertness
rests on eleven `production_load_pm` values the file itself calls "an ordering assumption"
(`abr.rs:1074-1082`). A ±40 % correction to one entry caps a gigabit link four rungs low. *Inert
today, arbitrary tomorrow, with no measurement in between.* It also overrules a documented design
decision at `abr.rs:1767-1772` that the previous plan neither cites nor rebuts. **Unblocked by:**
M3, **and** a 4K `auto_network` case existing. That second half is DONE as of 2026-08-28 and this
paragraph used to describe the blocker as permanent scenery: `tests/serve_fixtures.py` omitted rung
22 000 and every `auto_network` case used a 1080p fixture, so `admits` deleted Uhd — and
`route::arm_auto_fixture` hardcoded the 1080p source raster *because* the rung 404'd, which made it
circular. The server answers 22 000 with a real 3840x2160 clip, the raster is declared per case, and
`pipe_abr_uhd_source_admits_4k` grades `floor_kbps: 22000`. The previous §21's tests E and F are
reachable on the pipeline tier now; what still blocks I9 is M3's indexing decision alone. **Do not build it without B**: in
the shipped integer scale the argmax at every budget above 20 Mbps is P1080M18, not P1080High
(76 − 3 > 76 − 4), with unresolved ties at 12 000 and 16 000 — §6 alone is worse than what ships.

**B. Quality scoring from catalog rasters (previous §7).** *Void as specified.* The defect it
targets is real — `hls_quality_score` saturates at 76 above 17 000 kbps (`abr.rs:1670-1671`), so
P1080High (20 011) and Uhd (20 895) score **identically**, and P480's catalog 720 scores a bucket
above its measured ~425. But its **inputs are fiction**: `abr.rs:1178-1189` states verbatim that
"**a rung's raster is a BOUNDING BOX, not a target**" and records that reading it as a target
already shipped as a device bug — *"Auto capped at 4 Mbps / 720p on a gigabit LAN, and the log
looked healthy while it did it."* `docs/lg-self-checklist.md:87` measures 720 kbps → 480×200 against
a box of 854×480, a 4.3× pixel error. **Reading a bounding box as a quality input re-commits that
mistake by hand, this time on the side where an over-stated value is anti-conservative.**
*Unblocked by:* scoring on `min(box, source)` and `min(wire, source average)` — the average is
`ModeInputs::source_kbps` (`abr.rs:1616`); the raster needs the `ModeInputs` field N14 site 3 adds
— **and** feeding the observed raster back at commit, which `ff.rs:3265-3270` already reads. Note
the commit line prints `proposal.rung.raster()`, the **catalog** raster (`ff.rs:3302-3308`), which
is why `tests/run.py:1227` currently asserts the catalog against itself.

**C. `g(remaining)·Q` on rung selection (previous §10).** *Void.* See N18.

**D. Per-candidate ρ extrapolation across the ladder (previous §5).** *Deferred with A*, because
`load_j / load_current` is exactly the ratio M3 measures. The decomposition is retained as the
intended shape.

**E. `R_runtime ≈ R_avg` at runtime (previous §12).** *Void.* There is **no demand-side variance
anywhere in the module** to carry the burst risk the allowance expresses:
`CapacityEstimate::uncertainty_pm` (`abr.rs:255-275`) measures **delivery** dispersion, and
`ProductionEstimate::uncertainty_pm` (`abr.rs:943-949`) is a three-way constant. The term would be
deleted, not moved — invisibly, since no test can assert on a quantity the model does not have. It
also breaks `abr.rs:2461` outright (T goes 59 s → 90 s). *Unblocked by:* a named demand-side burst
allowance, ideally a function of reserve depth so cold start and runtime unify by construction. The
documentation half of the previous §12 is adopted in N17.

**F. `exp(−dt/tau)` switch hysteresis (previous §17).** *Deferred.* The defect is the plumbing, not
the formula (N12). Ungradable on a television until the decay is observed to run.

**G. Seek estimate preservation.** *Scheduled as I8, and worth more than its one line in the
previous §11.* An HLS seek routes `route::transcode_seek` (`route.rs:1175-1200`) →
`engine::reload_transcode` → a fresh `Controller` (`ff.rs:2953`). The only survivor is
`session().auto_prior_kbps`, whose writers are the bootstrap and `route.rs:700` — **the
Original→HLS fallback, storing `measured_kbps` at the moment the link failed.** So after one bad
patch, every seek re-seeds from the worst rate the playback ever measured, at `MAX_UNCERTAINTY_PM`
with `samples = 1`, and the ladder re-ramps for five to ten segments: ten to twenty seconds of
visibly softer picture after every skip.

**H. The utility decomposition is computed and discarded.** `choose_mode` returns four values and
all three production callers read one — `abr.rs:539`, `:631`, `:876` are each
`let (mode, _, _, _) = choose_mode(...)`. Two doc comments claim the event log prints the terms
(`abr.rs:1597-1600`; `abr.rs:1754-1755`, *"'why did Auto choose this' is answerable from the event
log alone"*) and `grep ModeUtility` finds no reader outside `abr.rs`. This plan makes that
comparison the centre of every mode decision (N14, N16), so **the decomposition is logged in I7a**
or the change is unobservable after the fact.

---

## 8. Test taxonomy

Every test in `abr.rs` and every device case that grades the controller carries a **primary
category** in a doc comment at its definition site, and may carry a **secondary** one. Where a test
is both 8.2 and 8.3, **8.2 governs** — the stricter rule wins. The category determines what may be
changed about it.

### 8.1 Mathematical-invariant tests

Assert a property of the mathematics that holds independently of any policy choice: dimensional
correctness, monotonicity, saturation, overflow and rounding direction, algebraic identities, the
**reachability invariant** (no gate may require a reserve `B_max` cannot supply — taking the minimum
over both lanes, N3), and the **prefix invariant** (the admissible set is a prefix of the ladder,
N3 point 2). May be added freely; expectations change only if the mathematics is proven wrong.

New in this plan: the §3.3 extreme-magnitude sweep; the reachability test; the prefix test; a
rounding-direction test (capacity down, requirement up, risk up); the piecewise-risk continuity and
boundary test — `r_net(T_safe) = 0`, `r_net(T_fallback) = 1`, monotone between, `∞ → 0`, and the two
deliberate endpoint changes N5 names (a comfortable horizon scores 0 where the ladder charged 1).

### 8.2 Device-finding regression tests

Encode something a television or a real PMS taught this project. **Their expected values may never
be re-fitted to a rewritten controller.** When the surrounding model changes, the assertion is
*re-expressed* with its provenance restated in the doc comment, and the new expectation is derived
from the recorded measurement — not from running the new code and copying the output.

Known members (I1 adds the labels): `abr.rs:3198-3218` (the 28 116 kbps regime-change probe);
`abr.rs:1022-1034` (the Auto-stuck-on-10-Mbps finding and the flat 11 918 ms buffer, which is also
the derivation of `&& !draining` and of N21's magnitude predicate); `abr.rs:3086` leg 4 (the
LG-checklist leg, `docs/lg-self-checklist.md:87`); `abr.rs:410` (the 865 Gbit/s reading); the
`pipe_abr_*` device cases once M2 records their results.

### 8.3 Policy-choice tests

Pin a decision this document makes. They may change **when and only when the corresponding
normative decision changes**, and the changed test must cite the decision by number.

Members after this plan: the `upshift_admission_headroom_pm` trio (`abr.rs:3044`, `:3086` leg 4 —
also 8.2, so 8.2 governs — and `:2619` via `prime_up`); `abr.rs:2656` and `abr.rs:2668` (the
collapse-jump pair); the emergency-predicate tests from N4; the dwell and backoff tests from
N10/N11; N21's differential; the Original exit classification from N13.

### 8.4 Integration and transaction tests

Exercise propose → prime → validate → commit/reject as a lifecycle, including the reject path,
`on_resume`, seek, and the device tiers. The closed-loop simulator and the frozen trace library
live here.

New: the reject-loop buffer-trajectory assertion (N11); the bootstrap and post-resume first-`observe`
assertions (I3); the `on_resume` clearing assertion (N12); the seek-does-not-re-ramp assertion (I8);
the closed-loop stall-and-commit report over the frozen trace library, **both parameter sets, a
stall regression in any leg disqualifying**.

### 8.5 The rule for changing an existing test

**The previous plan's closing instruction — "do not optimize for making the old tests green,
change tests where they encode behaviour the new mathematics explicitly replaces" — is amended.**
As written it licences re-fitting ~20 host expected values in the same pass that rewrites the code
producing them, on the only tier that can see most of this change. The amended rule:

> When an existing test changes, the commit message and the test's doc comment state **(a) its
> category, (b) why the old expectation was invalid — as a statement about the world or the
> mathematics, not about the new code, and (c) how the new expectation was derived.** A category
> 8.2 test may not be changed at all without restating its provenance and deriving the new value
> from the original measurement.

And a positive rule for new tests, which the previous plan's own list mostly failed:

> **Every test added by this change must be differential — impossible to satisfy against
> unmodified code — or structural. Never a value echo.** A test asserting that a formula just
> written returns what that formula computes proves the formula was typed correctly and nothing
> else.

### 8.6 Disposition of the previous plan's twelve tests, and of the tests this plan moves

| test | ruling |
|---|---|
| A (refill math) | **keep, re-parameterise** — `B* = 10 s` is void (N3); assert the per-candidate filter at `buffer_target_ms = 2 500`, and the prefix invariant beside it |
| B (harmless deficit) | **VOID as specified** — it is **green against unmodified code**, because the trigger reads the raw sample (`abr.rs:2127`), not `C_safe`. Rewrite onto a **reachable** state: `B = 5 s`, `T ≈ 100 s`, still not an emergency. The conclusion survives; the 60 s does not |
| C (severe deficit) | **VOID** — duplicates `abr.rs:2656` |
| D (no sample-count requirement) | **keep** — the only item on the previous list differential by construction |
| E (production independence) | **VOID** — duplicates `abr.rs:2997` verbatim, and is unreachable until §7.A unblocks |
| F (high-1080 vs 4K) | **deferred with §7.A/B** |
| G (real recovery comparison) | **keep, widen** to all three fabrication sites, with a pinnable `Q_source` (N14 site 3) |
| H (end of film) | **VOID as a supplement** — duplicates `abr.rs:2817`, which passes by **one utility point** and must be rewritten as a monotone crossover sweep rather than supplemented |
| I (VBR semantics) | **keep the documentation half** (N17); the runtime half is void (§7.E) |
| J (pause/resume) | **keep** (N12), extended to the post-resume first `observe` (I3) |
| K (smooth quality) | **deferred with §7.B** |
| L (no hidden haircut) | **inverted** — under N7 the margin is kept and named, so the test asserts the admission budget is `C_safe · upshift_admission_headroom_pm / 1000` **and that no other factor exists** |
| **`abr.rs:2656`, `abr.rs:2668`** (existing) | **category 8.3; expectations PRESERVED under N4.** Both scenarios give `T ≈ 8–9 s`, inside `starvation_fallback_secs = 20`, so the emergency predicate still fires, and `collapse_target` preserves the jump to the sustainable rung. Without `collapse_target` they would fail on the **target**, not the decision |
| **`abr.rs:2619`, `abr.rs:3044`** (existing) | **category 8.3; moved by I6, not I4, and not by N7.** N9's deletion of the `samples_on_rung` gate makes the proposal arrive at the 500/300 pm uncertainty floor instead of 200 (`abr.rs:264-268`), so the admitted budget falls by a third to a half. **The new expectation is derived from the uncertainty ladder and written into the doc comment, per §8.5(c) — not from running the new code** |

### 8.7 Existing tests asserting unreachable reserves

`abr.rs:2425`, `:2469`, `:2542`, `:2557`, `:2581`, `:3156`, `:3468`, `:3485` all pass 20–60 s
reserves against 28–60 Mbit/s sources, where §0.1 puts `B_max` at 2.7–4.0 s. (`:3156` passes
`buffered_ms: 60_000` to a P1080High candidate whose ceiling is 4.99 s.) **The assertion is
preserved; the buffer value is re-expressed at a reachable magnitude with the same qualitative
conclusion** — the worked example is `B = 5 s`, `T ≈ 100 s`, still not an emergency. Each is
classified at its site (8.2 where it encodes a device finding, 8.3 otherwise) and its doc comment
states why the old magnitude was invalid *as a statement about the pipeline*, per §8.5(b). This is
a prerequisite of §8.1's reachability test, so the two cannot land in contradiction. **Lands in I1.**

---

## 9. Recorded dissent

Carried forward so that a minority position that turns out to be right is findable later.

**Control theory.** Deferring the utility formulation leaves the keep-versus-admit asymmetry
expressed as a hand-written branch instead of as a comparison, and the top-of-ladder threshold
collisions are exactly the class of defect a utility formulation makes visible and a branch hides.
Right about the guesses, wrong about the remedy: measure the loads, do not decline the framework
that would have priced them.

**Implementation.** Confining the `observe_legacy` A/B path to two increments is better than nothing
and worse than the alternative; fifty lines behind a `dev::read` would let both regimes be measured
back-to-back throughout. Also dissents on deferring §7.B's observed-raster plumbing: twelve lines,
and it is the difference between a quality score being a formula and being a measurement.

**Regression risk.** Deleting `stable_samples` is still a net regression for stalls, and N11
concedes it by inventing a replacement guard the previous plan did not have. The honest sequencing
is to land N11 first and observe it, then delete `stable_samples` — not both in one increment.
**There is no tier that can see a rebuffer**, so the trade is being graded on quality metrics alone
(and note `abr: steady` is emitted only on `Decision::Stay`, `ff.rs:3016-3018`, so the grader is one
the change itself can starve — I0(e) is the mitigation, not a cure).
`pipe_abr_steady_modest_link`'s own note says as much: an over-reaching client "rebuffers, recovers,
and passes every other assertion in this file".

**Viewer experience.** Dissents on keeping the 0.8, however well-named: a 17.5 Mbit/s link settles
on P1080M10 at 57 % utilisation instead of P1080M14 at 80 %. The provenance argument proves where
the number came from, not that it is right. *Adopted as to sequencing — M-D6 now sits beside M2 in
the same lease.*

**Verification.** Dissents on allowing I0/I1 to land before the M2 baseline is recorded, and notes
that the closed-loop simulator is only useful with a falsification protocol: both parameter sets
reported over a frozen trace library, and **a stall regression in any leg disqualifies the change
regardless of every quality gain.** *Editorially resolved on the first half only: I0 and I1 land
first because M2 cannot be taken without them, and the baseline is the two-SHA pair in §4. The
falsification protocol is adopted in full (I0(j), I6).*

**Minimalist.** The file is one day old; evidence accretes, policy surfaces do not get
re-litigated. If the argmax is deferred anyway, the honest alternative is to leave the controller
alone for a month and measure it rather than rebuild it in nine increments while nobody can see the
result.

---

## 10. What this plan does not do

- It does not rewrite transport, demux, the transaction machinery, or the estimator's shape.
- It does not introduce a utility argmax over the HLS ladder (§7.A).
- It does not introduce a quality curve over catalog rasters (§7.B).
- It does not claim the model is probabilistic, and no document produced under it may (N17).
- It does not tune any parameter marked unidentifiable in §6.2b or §6.3 from host tests.
- It does not change a device-provenance expectation to make a rewritten controller green (§8.5).
- It does not resolve `candidate_accept_pm` to either neighbouring threshold before M3 (N19).
