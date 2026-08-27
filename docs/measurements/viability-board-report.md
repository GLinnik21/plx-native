# CHAIR'S RULING — the viability-based ABR control law

---

## 1. THE RULING

The diagnosis is better than what ships; the law is not ready to specify. Its central reading is correct and valuable: `production_ratio_pm` (`abr.rs:1359-1363`) already **is** `1000·A/D` — the two `1_000`s cancel — so the module has been measuring the claim's physical quantity all along, spelled as three unrelated constants (750 at `abr.rs:1486`, a hard-coded 800 at `abr.rs:2334`, 1100). And `A_i <= D_i -> STAY` structurally removes a real, measured, viewer-visible defect: every playback opens at `buf=2000ms` = D, `starving()` tests `buffered_ms <= 2_000` (`abr.rs:1076`), and six of six M2 runs downshifted on segment 1 (`m2-verbose-rerun.txt:90,191,293,395,500,631`).

**The single most important correction: §5's budget is not too small — it is too large, and it has a fixed point.** With `T = B − A_i` spent and nothing fed until commit (`ff.rs:3392-3394`), the post-transaction reserve is `A_i`; one current segment restores it to **exactly `D`, from any starting `B`**. One rejected experiment converts a settled 24 835 ms reserve at the 4000 pin into 2000 ms, and at the top rung moves §11's own collapse boundary from 7.3 Mbps to **21.5 Mbps** — above the media rate. §5 drives the system to the state §11 calls unrecoverable.

---

## 2. WHAT THE CLAIM GETS RIGHT

**Strongest insight — the units.** `abr.rs:1359-1363` divides `total_fetch_us * 1_000` by `media_duration_ms * 1_000`. That is `A/D` in per mille. `total_us` is stamped at `ff.rs:2530` from `request_started` (`ff.rs:2344`), *before* `hls_feed_segment` (`ff.rs:3072`) blocks on `aq_push` (`aq.rs`), so queue backpressure genuinely is outside it — and the device confirms it without reading code: settled `prod` is 247-253 / 225-235 / 313-318 pm with `buf` pinned flat at the ceiling, i.e. the demuxer *was* backpressured every segment; wall time would read ~1000 pm. §6's `A_j <= D_j` is therefore `production_ratio_pm() <= 1000`, and the current test is `<= 800` (`abr.rs:2334`). That reframing is worth landing on its own merits.

**Deletions that are genuinely justified:**

| row | ruling |
|---|---|
| **12** — ms subtracted from kbps (`abr.rs:1586-1589`) | **Unconditional delete.** At B = 0 it subtracts 2500 **kbps**, most of the budget at the low rungs. No defence exists. |
| **3** — `minimum_buffer_ms` (`abr.rs:1425,1486`) | **Delete with Row 12** — `hls_safe_budget` is its only consumer. |
| **9** — `stable_samples` (`abr.rs:2306-2310`) | **Right in principle.** Three consecutive successes give a 95% lower bound on P(A≤D) of 0.000. It is a delay, not a measurement. Conditional on a replacement (§3 below). |
| **10** — the `samples_on_rung < 2` *gate* (`abr.rs:2267`) | **Delete the gate only.** The field also carries the cold-start weight at `abr.rs:2186`; the table does not say so. |
| **11** — reject cooldown (`abr.rs:2361`) | **Already inert.** The decrement at `abr.rs:2200-2201` precedes the gate at `abr.rs:2267`, so `cooldown = 1` is consumed before it can block anything. Deleting it changes nothing — it is not evidence for or against §7. |
| **6** — `candidate >= 2*segment` | **Right structure.** One pre-condition `B > T + resume_cost` implies today's post-condition instead of asserting affordability twice in two currencies. |

**Also right:** §11's observability limit is correct arithmetic and correctly attributed to architecture. And the closing instruction — measurement-only next, no policy in the same commit — is more disciplined than the deletion table it sits beside.

---

## 3. WHAT IS WRONG OR UNDER-SPECIFIED, BY CONSEQUENCE

### 3.1 §5 has a fixed point at `B = D`, and it is a stall attractor *(blocking)*

**Defect.** `T = B − A_i`, nothing fed until commit, so after a rejected experiment `B' = B − T = A_i`; the next current segment drains `A_i` and adds `D`, giving `B' = D` **exactly, independent of B**.

**Evidence.** Feed only after commit: `ff.rs:3388-3394`; every reject path `continue`s at `ff.rs:3373`. Measured: at the 4000 pin B settles at 24 918 ms (`raw-4000.txt`), so one experiment costs 22.9 s of reserve where today's fixed 4600 ms costs 4.6 s. At the top rung, with `overhead = A − active ≈ 284 ms`, §11's boundary `C_crit = R·D/(B − overhead)` moves from `36 912 000 / 5051 = 7308 kbps` at B = 5335 to `36 912 000 / 1716 = 21 510 kbps` at B = 2000 — **above the media rate of 18 456**, i.e. at the fixed point no capacity is observably recoverable.

**Amendment.** Change the conservation identity from "one more current segment" to "one more *observable control step*": `T <= B − A_i − D`. One term in the claim's own algebra; it makes §5 consistent with §11 instead of adversarial to it. And cap the fraction of reserve one experiment may consume — the payback period `P = T/(D − A_i)` under §5 is 3.5 segments at the top rung but **39 segments (78 s) at the 720 rung**, whereas the fixed 4600 ms costs ~3 segments at every rung. §5 makes a failed experiment cheapest where the reserve is smallest.

### 3.2 §5's guarantee is unenforceable: four legs of the transaction have no deadline *(blocking)*

**Defect.** "The buffer remaining after the transaction is guaranteed sufficient" is not a property the transport can deliver.

**Evidence.** The deadline reaches only body reads (`ff.rs:1496`, `1505`, `1512`), and is passed to `hls_input` at `ff.rs:2363` — *after* the `NotReady` retry loop bounded by its own `retry_budget = clamp(3d+2s, 3s, 15s)` = **8 s at d = 2 s** (`ff.rs:2345-2354`), independent of the deadline argument. Outside any deadline: `control.prime` (`ff.rs:3216` → `route.rs:465` `transcode_decision` on `net::API = {connect_s: 8, total_s: 25}`, `net.rs:460`), `hls_cursor_open` (`ff.rs:3233`), both `hls_cursor_next` calls (`ff.rs:3242`, `3308`). `control.abandon` on the reject path is a **synchronous** `transcode_stop` on the demux thread (`route.rs:511-517`) — note `retire` right above it spawns (`route.rs:495`) and `abandon` does not. The code states this at `ff.rs:3251-3256`; the claim asserts a guarantee over it.

**Amendment.** A plumbing increment first: thread one `Instant` through `hls_fetch_text` / `hls_open_source` / `hls_cursor_open` / `hls_cursor_next`, make the `NotReady` budget `min(retry_budget, deadline)`, give `prime` a `Timeouts` override, and make `abandon` non-blocking. Until then, the enforceable bound is the two media-segment legs, and the law must say so.

### 3.3 §6's "gather evidence until the deadline" is unbounded RAM on a 32-bit TV *(blocking)*

`candidate_outputs` (`ff.rs:3299`, pushed at `3362`) owns `Vec<HlsAu>` with a `Vec<u8>` per AU; nothing is released until the post-commit feed at `ff.rs:3392-3394`. Today it is capped at 2 by construction (warm-up, plus a graded segment gated on `Direction::Up` at `ff.rs:3307`). Under §5's budget at the 4000 pin (~24.4 s) with a top-rung candidate at 4.6 MB/segment, that is **~12 retained segments ≈ 55 MB** beside the 8 MiB video queue (`engine.rs:91`). Both host tiers are blind: the plant models the transaction as a scalar (`sim.rs:319-321`) and allocates nothing.

**Amendment.** Any time budget carries an explicit segment-count cap, `max_candidate_segments = 2` preserved. More evidence than two segments requires feed-and-discard, which is a different design and its own increment.

### 3.4 The acceptance floors are already inert in production — so Rows 7/8 delete the *only* live bound *(blocking; P1 upheld)*

`hls_segment_sample` always builds from `hls_buffer_snapshot(Some(output))` (`ff.rs:2766`), which folds `candidate.video_tail_ns` (`ff.rs:2708`) — the **staged** timeline end (`ff.rs:2520`), while `SHARED.hls_video_tail_ns` advances only inside `hls_feed_segment` (`ff.rs:2583`). So the acceptance sample reads `B_true + 2D` (Up) / `B_true + D` (Down), and `buffered < segment` (`abr.rs:2326`) and `buffered >= segment*2` (`abr.rs:2335`) are arithmetically unfailable. Three consequences the board must carry:

1. The livelock documented at `abr.rs:110-121` is a property of `sim.rs`'s snapshot convention (`sim.rs:333-341` passes post-drain `buf_ms`), not of the television. **The doc comment is wrong in its stated mechanism.**
2. But `PIN_MIN_RESERVE_SEGMENTS = 6` (`abr.rs:122`) stays. Its production analogue is not a benign loop — it is **commit-after-stall**: the reserve runs to zero mid-transaction and the controller then commits a rung it has no evidence it can hold, because the only test that could have vetoed it was reading its own staged content. Rows 9/10/11 get no relief from this.
3. **The fix is not measurement-only and the obvious spelling is a trap.** De-folding the shared helper makes the Up floor honest (correct) *and* makes the Down floor live — and the Down trigger is already `buffered < segment` (`abr.rs:2238`), with no transaction deadline (`abr.rs:2007`, `2026`), so an honest floor would reject essentially every buffer-triggered downshift and leave the session on the failing rung. That is the livelock, manufactured in production, on the recovery path. Move the floor inside the direction match: delete it for `Down`, keep it for `Up` reading honest `B_true`.

Do **not** touch the observe-path call at `ff.rs:3088`: its fold is the documented ~1-frame `segment_end` correction (`ff.rs:2516-2519`) and is why the traces read `buf=2000` rather than `buf=1958`.

### 3.5 `T_down` has no value when it is first needed, and nothing in the repo measures it *(major)*

Both budget functions return `None` for `Direction::Down` (`abr.rs:2007-2008`, `2026-2027`); `ff.rs:3307` gates the graded segment on `Up`, so a downshift fetches one cold segment with no time box. A fresh `Controller` is built per playback and per seek (`ff.rs:3033-3035`). The only figure in the project is `sim.rs:121`'s `transaction_ms = 4_600`, whose own comment says "Derived, not measured" — and it is a sum of two **upshift** deadlines describing a two-segment shape a downshift does not have. §3's central comparison currently has no data on either side. **`T_down` is a design variable, not a constant of the plant:** the answer is to give the fail-safe a deadline, not to accept an unbounded one.

### 3.6 A bare sign test on `A − D` re-opens a dated device finding *(major)*

`BufferEstimate::draining` is a **magnitude** test against `DRAIN_EPS_MS_PER_S = 50` (`abr.rs:1071-1083`) because the sign test failed on device — a reserve flat at 11 918 ms reporting −16, −12, −9, −6, −4 ms/s parked Auto on the 10 Mbps rung for the rest of the film. `A_i > D_i` is a bare sign test on the same per-segment difference, and it silently deletes `&& !draining` (`abr.rs:2301`), which the table does not list. Measured dispersion: lag-1 autocorrelation of `prod` is −0.36 / +0.01 / +0.10, crossing its own median 64% / 48% / 45% of samples. **And 213/213 corpus samples sit at A/D ≤ 0.607 — there is no device evidence anywhere near the boundary the law is keyed on.**

### 3.7 Row 13 is not executable, and the board record had the sign backwards *(major — chair correction)*

There is no HLS-only risk weight: `policy.risk_weight` (`abr.rs:1499`) is read by both sides, `abr.rs:1747` and `abr.rs:1796`. **Settled at `abr.rs:1752`: `total: quality - risk_cost - server - transition`.** Deleting the HLS term **raises** `hls_utility.total` and biases `choose_mode` toward **HLS**. The viewer seat has the direction right; the regression seat and the audit it cites have it backwards. Original is the only mode that direct-plays the source, so the loss is Dolby Vision and Atmos, unrecoverable by any rung. Rescope Row 13 to the rung ladder; name `abr.rs:1547` as retained when scoping Row 4.

### 3.8 Row 1 deletes three things by naming one *(major)*

`network_bad` (`abr.rs:2232`) is the trigger (`2239`), the collapse **target selector** (`2243-2248`, whose comment at `2241-2242` is its derivation — a measured collapse must not walk the ladder one oversized encoder at a time), and the reason code (`2255-2256`). Deleting the selector costs N full transactions of unrefilled playback on a real collapse. §4's "if F(B) is empty, best-effort recovery" likewise replaces an existing **total** rule — `unwrap_or(Rung::P240).min(current.below())` — with prose. Keep the expression as `collapse_target`.

### 3.9 Row 14 removes the last admission margin on a justification the claim itself defers *(major)*

`* 4 / 5` at `abr.rs:2276` (mirrored at `2148`) is the only remaining admission headroom; the commit-side twin is already gone (`abr.rs:2333` is bare 1.0×). Its stated replacement is §10's `A⁺`, which §10 explicitly does not build. Deleting a margin is conditioned on the bound that replaces it existing.

### 3.10 Assertions about the code that are not true

- "**4.6 s accidentally nearly coincides with the physically derived 4.70 s**" compares incommensurable quantities: 4.6 s is a sum of two *deadlines* on media segments only, and `ff.rs:3251-3256` states the control plane is excluded from both. Even taken at face value the settled worst case is **4643 ms against 4600 — a +43 ms, 0.9% margin**, not a coincidence.
- §5's "A ≈ 0.44-0.48 s at 4 Mbps": the rung labelled 4000 delivers **3183 kbps** and measures 420-480 ms, median 450; 17% of samples fall below the stated window.
- §11's `R·D/C > B` omits acquisition overhead, which is **44% of A at the top rung** (`A − active` = 281 ms of 640). Corrected `C_critical` is **7.31 Mbps**, not 6.9 — the limit is worse than claimed.
- "prod ≈ 0.315-0.329": measured minimum is **310 pm**.
- `B_max ≈ 5.335 s` is a **median** of a five-valued, frame-quantized distribution (5293…5460, spaced 41-42 ms = one frame at 24 fps), not a ceiling.

### 3.11 Struck from the record

Audit 2's "the one decision in the corpus the proposed law would reverse" (`raw-720.txt:12`) is **the dev pin**, not the controller: `observe` short-circuits at `abr.rs:2205-2216` waiting for `PIN_MIN_RESERVE_SEGMENTS = 6` × 2000 = 12 000 ms, the line sits at `buf=12501ms`, and it prints `reason=None` — which the `network_bad` path cannot produce, since it sets `last_reason` at `abr.rs:2255-2256`. All three census traces are pinned; **every graded decision in 213 samples is `Stay` from the pin arm.** They are evidence about state and dispersion only. The control seat's use of the same line falls with it.

---

## 4. THE SIMULATOR DEFECT — ruled separately

**The claim is right, and the file disagrees with itself.** `sim.rs:239` computes `wall_ms = acquire_ms.max(blocked_until)` and `sim.rs:268` passes it as `total_fetch_us`; the candidate site 65 lines later passes `fetch_ms + overhead_ms` (`sim.rs:333`). Production passes acquisition. Fix: `sim.rs:268` → `acquire_ms`. Note the module doc at `sim.rs:47-49` also becomes true only after the fix — because line 239 is `max` and not `+`, `overhead_ms` currently vanishes from `total_fetch_us` in exactly the backpressured regime.

**Yes, it invalidates conclusions already drawn.** In the settled state `blocked_until = D` exactly, so the plant reports `A == D`, `production_ratio_pm = 1000 pm` at every rung and every link speed — against a device reading 225-318 pm. Downstream that permanently fails the upshift gate (`abr.rs:2299`, `<= 750`), permanently haircuts `hls_safe_budget` by 25% (`abr.rs:1582-1585`), and filters the incumbent out of its own feasible set. **The plant structurally vetoes upshifting out of a settled reserve — the exact behaviour the new law claims to fix.** Any A/B run today scores the law well for a reason that does not exist on the television. It also makes rule 1's verdict at the ceiling a matter of `<=` versus `<`, not of physics.

**The one-token fix is not sufficient.** Three further defects, all biased the same way:

1. `Plant::overhead_ms: 120` flat (`sim.rs:118`) against a measured, byte-dominated residual: 37.6 / 63.9 / 281.1 ms at the three census points (`overhead ≈ 18 + 57 µs/kB × kB`, R² ≈ 0.99999). Wrong by −57% at the top rung and +219% at 720. Source it per rung from M4.
2. `transaction_ms` is charged as a constant with no term reading `trace.capacity_kbps`, identically for Up and Down and for commit and reject (`sim.rs:319-321`). So `T_down` growing on a collapsing link — the centre of the control seat's case — **cannot occur in the plant at all**, and a downshift is charged a two-segment upshift budget it never spends. Split it into `transaction_ms_up` / `transaction_ms_down` before simulating any transaction policy.
3. `b_max_ms` is called with the catalog's `expected_wire_kbps` (`sim.rs:228`, `346`), which is 8.4% high at P1080High (20 011 declared vs 18 456 measured) and 92% *low* at P480 (720 vs 1381). `b_max(20 011) = 4986` against a device 5335; at the measured rate it is 5274. Pass delivered media rate and assert against the three census medians.

Also reconcile the snapshot convention (§3.4): after the acceptance-sample fix the plant's post-drain `buf_ms` becomes the correct convention, and every sim reject count taken before that must be re-baselined.

---

## 5. THE PROPOSED NEXT INCREMENT

Right shape, wrong content. Amended list:

**Keep:** `sim.rs:268`; transaction start/end, warm-up, graded, commit/reject, buffer at both ends, current `A_i`, candidate acquisition + duration.

**Add (missing):**
- The **control-plane leg** timed separately — `control.prime`, master playlist, both `hls_cursor_next` calls. It is the leg no deadline covers and the one that decides whether §5 is enforceable at all.
- **`T_down`**, split by leg and by rung distance, on the downshift path. This is the single number that decides §3, and nothing else can.
- `not_ready` retry consumption attributed to the transaction (already logged at `ff.rs:2545`, not attributed).
- **`ttfb_us`** on `SegmentTransfer` — one timestamp at the first successful body read — so `A − active` splits into client demux versus PMS JIT. Without it §6's three-way logging split is not computable.
- **Both** buffer values at acceptance: the honest `hls_buffer_snapshot(None)` and the folded value the test consumed. After the §3.4 fix, the first device run is otherwise uninterpretable against the I1 baseline.
- Per-lane demuxed ES bytes, for `B_max_est`.
- `achievable_eps_pm = 1000/(k+1)` logged beside `prod=` (see §6.4).

**Unnecessary:** nothing on the list is wasted, but "buffer at transaction start" is already free via `hls_buffer_snapshot(None)` — the `None` arm today has **no caller** (`hls_buffer_snapshot(` appears twice in `ff.rs`: the definition at `2704` and the call at `2766`).

**Cannot be observed today without a code change the claim does not mention:**
- No sample is emitted between proposal and commit — the prime arm runs inline inside the one-sample-per-iteration loop (`ff.rs:3065`, `3207`), so **the transaction drawdown is structurally invisible**. `min_buf_ms = 2000` in the M2 results is not evidence of no drawdown.
- The census traces are pin-masked; grading any decision needs an **unpinned** shaped leg.
- ~~`A` at every rung above ~11 Mbit/s of media is unmeasured, because `PIN_MIN_RESERVE_SEGMENTS = 6` cannot be satisfied there.~~ **Both clauses closed 2026-08-27.** The constant no longer gates a downshift, and `A` is measured at seven rungs across an 18x range including 16000 (n = 54) and 20000 (n = 39). The code change this bullet says the claim does not mention is the directional split of that constant — `docs/measurements/p2-census.md`.
- The ABR fixtures are a static file server, so **every A in the corpus has zero PMS JIT production**. At the top rung the slack `B − A_i − 4600` is 95 ms; 95 ms of JIT inverts §5's own worked example.

**Free win to take first:** `abr: sample` already carries `prod=` on every current-stream segment (`ff.rs:3115`). Replaying the event logs of the four shaped M2 profiles answers "how many of the 7/6/7/3 commits would rule 1 have suppressed" with **no code change and no television**.

---

## 6. PRECONDITIONS BEFORE THE LAW CAN BE SPECIFIED

| # | Precondition | What settles it | TV? |
|---|---|---|---|
| 6.1 | Acceptance sample reads honest `B_true`, with the floor moved inside the direction match (Down: deleted; Up: honest) | Code change + three host tests; then one device run to re-baseline M2 commit counts | host, then **yes** for re-baseline |
| 6.2 | Plant fixed: `acquire_ms`, per-rung `overhead_ms`, `transaction_ms` split Up/Down and made capacity-dependent, `b_max_ms` on delivered rates | `make check` + assert `b_max` against the three census medians within 5% | no |
| 6.3 | `T_down` measured, per leg, per rung distance, with its conditional distribution under a degrading link | I2 instrumentation + one shaped device run | **yes** |
| 6.4 | Reliability is an **output**, not an SLO parameter: `ε >= 1/(k+1)` with k the graded candidate segments, and k capped at 2 by §3.3 — so **ε >= 0.5 at the top rung** | Arithmetic; log it | no |
| 6.5 | Real transaction wall cost `E_tx` including the control plane | I2, same run as 6.3 | **yes** |
| 6.6 | `B_max_est` plumbed into `abr.rs` (`MAX_FEED_AHEAD_NS` / `AUDIO_SLACK_NS` are private, `engine.rs:1051-1052`) and the reachability invariant landed: no gate may require a reserve `B_max_est` cannot supply | host test | no |
| 6.7 | Host corpus re-parameterised — `settle_link`/`prime_up` hand the controller 10-12 s reserves at 20 Mbps where the device measures 5335 max. Under §5, B *generates* the budget, so those tests would confirm a 2× over-grant | `make check`; derive `buffered_ms` from `b_max_ms` so an unreachable reserve is inexpressible | no |
| 6.8 | A at rungs above ~11 Mbit/s, and against a real PMS rather than the fixture server | pin at the route's starting ceiling (I0 follow-up) + `tools/pms-hls-probe.py` | **yes** for the first |

**Landing order I will defend:** (1) 6.2 + 6.1 + 6.7, host only; (2) rule 1 alone, as a *suppressor* of the three down triggers at `abr.rs:2238-2240` — never the pseudocode's literal `if sustainable { Stay }`, which sits ahead of the upshift arm and would freeze the ladder on its starting rung — with a retained guard at the **stall boundary** (`buffered_ms < A_i`) rather than at one segment; (3) I2 instrumentation; (4) §5, only with §7's certificate, a segment-count cap, and the `− D` term.

On §7's certificate: its stated re-arm condition does not work. "A larger transaction budget appeared" is true on **every** segment during fill-in, since B climbs ~790 ms/segment for ~30 segments on every trace — so it never suppresses at the low rungs; at the top rung B is pinned flat and it suppresses everything, including a retry that has become correct. The only regime detector available is `is_regime_change` with `REGIME_FACTOR = 4`, documented as too coarse to see ordinary variance. **Rows 9/10/11 need a computed probe spacing** — `n = ceil(T_tx / (D − A_i))` segments — not the certificate. (The four seats' proposals are one quantity in two units; both give 3.4 segments at the top rung.)

---

## 7. RECORDED DISSENT

**Control theory.** "Build §5 first and defer §3: a wasted experiment is bounded and recoverable, a stall is not." *Chair: overturned.* In this code the asymmetry is inverted — a downshift has no deadline (`abr.rs:2007`, `2026`), the current stream's own fetch is called with `deadline: None` (`ff.rs:3070`) and can absorb 8 s of `NotReady` retries inside a single `A_i` (`ff.rs:2345-2354`), while the upshift experiment is the one leg that *is* time-boxed. The incumbent's recovery path is the unbounded one. §5 also moves the state to the boundary by construction; §3 rule 1 merely tolerates being there.

**Regression risk.** "Keep `starving()` arm 1 unconditionally as a labelled hard guard — it is the only detector that survives the estimator being wrong." *Chair: partially overturned.* The guard's most reachable firing state is post-transaction drawdown on a healthy link — today's admission needs 6000 ms, a rejected transaction burns up to 4600 ms plus an unbudgeted control plane, leaving 1400 ms, which trips `buffered < segment` and produces a second false downshift of the same family. Retain a guard at the **stall boundary** (`buffered_ms < A_i`, the degenerate `T_down = A_i` case), not at one segment.

**Measurement & statistics.** "§10 is blocking, not a later refinement: achievable ε is queue geometry, worst at the top rung, and it cannot be chosen." *Chair: upheld and strengthened.* With §3.3's two-segment cap the top rung yields k = 1, so **ε >= 0.5** — the acceptance test is a coin flip precisely where the ladder is tightest. Log `achievable_eps_pm` in the measurement increment.

**Implementation.** "The 20-line pseudocode is ~3% of the change; 700-1000 lines across seven files and three device leases." *Chair: recorded without dissent,* with one addition — any replacement controller must emit the same `abr: sample` / `abr: steady` field set, or `tests/run.py`'s regexes report "no samples", which reads exactly like a total regression.

**Viewer experience.** "Deleting all dwell while keying a lexicographic order on an injective quality function makes the tie-break unreachable — HLS rung changes carry no transition cost at all (`transition_cost` returns 0 for `from == to` and its only callers are the *mode* comparison at `abr.rs:1746`/`1795`), so the deleted counters **are** the churn policy." *Chair: upheld.* Quantise quality by raster class so the tie-break has a domain, and grade the A/B on `raster_changes`, not `max_commits` alone.