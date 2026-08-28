# J3 — the §4 admission rule deciding, on a device

**What ran.** `Controller::candidate_ready` admitting an upshift by the acquisition window, with
selection clamped to the same rule. Debug install, 2026-08-27, TV lock held.

Two runs, and the split between them matters when reading the numbers:

| run | build | what it establishes |
|---|---|---|
| A — all 15 `pipe_abr` cases | before `graded_bytes=` reached the wire | **behaviour** |
| B — 5 of 15 | with it | **arithmetic**, replayable |

Run B was cut short when the television was needed. Its five cases each ran to completion and are
valid; the other ten simply did not run, and no claim below rests on them.

---

## 1. Behaviour: 15 of 15

```
pipe_abr_slow_start_then_fast   320 -> 720 -> 2000 -> 14000   settled 14000   0 stalls
pipe_abr_steady_modest_link     720 -> 2000                   settled  2000   0 stalls
pipe_abr_brief_dropout          8000 -> 2000 -> 16000 -> 14000 settled 14000  0 stalls
pipe_abr_oscillating_link       4000 -> 720 -> 2000           settled  2000   0 stalls
pipe_abr_pin_{320,720,2000,4000,10000,16000,20000}            pinned, <=1 s stall
pipe_abr_band_{4000,20000} / reject_up_4000 / down_collapse    pass
```

Three of those are the increment's whole point.

* **`slow_start_then_fast` climbs off the emergency floor and then jumps 2000 → 14000 in one
  move** — a 7× step, three raster changes, no stall. That is `largest_admissible` behaving as
  designed: it walks *down* from the budget's choice to the highest rung the window supports, so it
  is a bound rather than a rung-walking rule, and on a link that supports it the bound is far away.
* **`oscillating_link` settles rather than flapping.** A link flipping 20/3 Mbit/s produces three
  rung changes across 57 segments and lands at 2000 with no stall.
* **`brief_dropout` recovers all the way.** Down to 2000 under a 3 s near-stall, back to 16000, then
  a correction to 14000.

**The emergency-floor limitation is real but did not bind on this pipeline.** §4 records that the
bound licenses a climb off rung 320 only while production is faster than `ratio_pm ≈ 313`; PMS's
production for these fixtures is well inside that, so `slow_start_then_fast` escapes the floor
normally. The host test pins both sides of the boundary so a change that moves it fails loudly.

**One transient, twice, and it is not the app.** `pin_10000` failed `pos_climb` in run A and
`oscillating_link` in run B — both with a sample count far below the cap (37 of 120, 29 of 90),
i.e. a run that ended early rather than a player that ran slowly. `pin_10000` re-ran alone and
passed with 115 samples and a 113 s climb. Two different cases across two runs points at the
`run-stream` ssh tail rather than at playback, and nothing here diagnoses it — it needs a
television and is recorded as open.

## 2. Arithmetic: 140 graded lines, 0 disagreements

```
case                                lines  graded  fill  cand  reset  disagree
pipe_abr_brief_dropout                 41       9    31     1      1         0
pipe_abr_oscillating_link              40       8    31     1      1         0
pipe_abr_pin_320                       91      71    20     0      1         0
pipe_abr_slow_start_then_fast          41      20    18     3      0         0
pipe_abr_steady_modest_link            51      32    18     1      0         0
```

`cand` and `reset` are the two things a replayer cannot see for itself, and both are now on the
wire. Six candidate observations and three resets were replayed exactly.

**That column exists because of a false alarm this grader raised on run A.** Every one of its 54
disagreements read `have=N but this file's own segments give N−1` — the graded candidate segment
entering the window through `observe_candidate`, which produces no `abr: window` line of its own.
The app was right and the instrument was blind, which is the same failure the `reset=` counter was
added for a day earlier. `graded_bytes=` on `abr: tx` completes it: paired with the `graded=` this
line already carried, it makes the one observation a transaction contributes replayable, and its
placement is exact rather than approximate — the transaction runs inline on the demux worker, so no
current-stream segment is acquired while it is in flight.

The check it must not weaken is tested: a `have` that jumps with no transaction to explain it is
still a disagreement.

## 3. What is not established here

* Run B covers 5 of 15 cases. The band cases, the collapse and `reject_up_4000` have a *behavioural*
  result from run A and no replayable arithmetic on this build.
* Nothing compares this against the previous controller under matched conditions. `max_commits` is
  not a grader — the same binary scored 7 then 3 on identical inputs — so a real comparison is the
  closed-loop simulator over frozen traces with a stall disqualifier. ~~which has not been run.~~
  **It RUNS, as of 2026-08-28, and it runs inside `make check`.** `abr/sim.rs` closes the loop over
  the device-calibrated plant with a stall disqualifier in three legs —
  `the_controller_never_rebuffers_on_a_link_that_can_carry_the_ladder` (the disturbance matrix),
  `the_device_collapse_case_descends_to_a_sustainable_rung_and_stops_stalling` (this document's own
  collapse case) and `a_link_under_the_floor_rung_drives_the_controller_to_the_floor`. It was
  blocked because `run()` refuses an unmeasured transaction leg and all four were `None`;
  `TransactionModel::measured()` unblocked it. **The A/B half is still owed** — grading BOTH
  parameter sets needs the `abr_policy` switch (I0 deliverable (h)), which does not exist, so what
  runs today grades the shipped controller and not the comparison this bullet asks for.
* The floor limitation and the collapse silence are both recorded as *bounds on what the rule can
  say*, not as things this run measured the cost of.
