# J3a — the §4 admission rule, shadowed on a device

**What ran.** `abr/window.rs` computed on every segment of a real playback and written to the event
log as `abr: window`, reading into no decision. Three cases, 2026-08-27, debug install, TV lock
held.

Logs: `docs/measurements/j3a-window-logs/`. Grader: `tools/abr-window-grade.py`.

---

## 1. The arithmetic is the specification's

`tools/abr-window-grade.py` re-derives the whole rule in Python from the app's own lines — `bytes`
and `dur` off `abr: window`, the acquisition off the `abr: sample` written for the same segment —
and compares term by term.

```
case                                lines  graded  fill  reset  disagree
pipe_abr_band_20000                    49      31    18      0         0
pipe_abr_band_4000                     55      37    18      0         0
pipe_abr_down_collapse                 23       0    23      1         0

68 graded lines, 0 disagreements with the specification
```

**There is no tolerance anywhere in that comparison.** `prod` is a truncated per-mille, so a logged
value at duration `D` admits an acquisition anywhere in `[p·D, p·D + D)` microseconds; the grader
propagates that as an interval through both sums and reports a disagreement only when the app's
number falls outside what its own logs admit. A fudge factor there would have hidden exactly the
off-by-one the comparison exists to find.

What this does and does not establish: that the integer arithmetic which ran on an ARM television
agrees with the rule as written down, on real segments. It says nothing about whether the rule is
the right rule — that is `tools/abr-transfer-bound.py`'s question, and a different one. A correct
rule computed wrongly and a wrong rule computed correctly both produce a log full of plausible
numbers.

## 2. The run reached the band that had never been observed

The plan's meta-finding was that **0 of 366 corpus samples sat in `A/D ∈ [0.80, 1.05]`** — no
evidence anywhere near the boundary the entire law is keyed on. The two band cases sit in it:

```
case                   admit  refuse  load min    mean     max  exc>0  exc max
pipe_abr_band_20000        7      24      0.60    1.00    1.26     31  10915ms
pipe_abr_band_4000        30       7      0.41    0.81    1.11     33   5505ms
```

`load` is `demand/supply`, i.e. `Σ T_i / n·D`, which is the window's A/D. Both means land inside the
band; both runs cross it in both directions. The verdict discriminates rather than being degenerate
— it is not "admit everything" nor "refuse everything" in either case.

## 3. The two conditions are not redundant, and this is the first observation of it

Condition (1) is sustainability (`Σ T_i ≤ n·D`); condition (2) is survivability
(`B ≥ Σ(T_i − D)⁺`). They were kept as separate fields on the argument that a single boolean — or a
single `4/5` haircut — cannot express the state where a rung is affordable on the average and still
unsurvivable in the peak. That was an argument. Here it is, counted:

```
pipe_abr_band_20000        pipe_abr_band_4000
  7  admit  sus=1 sur=1     30  admit  sus=1 sur=1
 17  refuse sus=0 sur=0      7  refuse sus=0 sur=1
  7  refuse sus=1 sur=0     18  filling
 18  filling
```

**Every refusal at rung 4000 is `sus=0 sur=1`.** The reserve is 17–18 s and the excess is 3–6 s, so
survivability is never in question; what fails is that the link is not gaining. The correct action
there is "do not upshift", and it is emphatically not "downshift" — a rule that collapsed both
conditions into one boolean would have read those seven segments as distress.

**Seven segments at rung 20000 are `sus=1 sur=0`** — the converse, and the state the argument was
about. Demand under `n·D` on the average, against a 2.2 s reserve that cannot absorb an 8–9 s
excess. `B_max(20000)` is ~5.4 s, so that reserve is not a transient: the rung's ceiling is below
what its own peaks require.

## 4. What the shipped controller did at the same moments

`pipe_abr_band_20000` settled at 20000 kbps with **0 rung changes**, `min_buf_ms=2000`, and
**14 s of stall over 94 beats**. Across that stretch the shadow refused 24 of 31 graded segments,
with `demand` climbing to ~48 000 ms against 38 000 ms of supply as delivered throughput fell to
16–17 Mbit/s under a 20–21 Mbit/s media rate.

The shadow saw the failure the shipped estimators did not act on. That is one case and it is not a
controlled comparison — it is the reason the next increment moves the decision, not evidence that
the move will help.

## 5. [FINDING] The admission rule is SILENT through a collapse, and cannot be the collapse response

`pipe_abr_down_collapse` graded **zero** segments. It runs 23 segments with one window reset at
segment 13, so the window holds 12 and then 11 — and `n = 19` at ε = 50‰, k = 1. The rule never
accumulates a full window, so it has no verdict at all through exactly the event it would most
obviously be wanted for.

This is not a defect in the implementation and it is not fixable by tuning. `n = k/ε − 1` is forced
by the SLO, `n = 19` at a 2 s segment is **38 s of media**, and a collapse resolves faster than
that. Raising `k` makes `n` larger; lowering `n` raises ε, which is the guarantee being sold.

**The consequence is structural and it confirms §5's split rather than contradicting it.** Trigger,
target and deadline are three different mechanisms. The admission rule is the *trigger and target*
— it answers "is this rung sustainable, and which rung should we be on" from a window of evidence.
It is the wrong instrument for a *deadline*, which has to fire from the current reserve and the
in-flight segment alone, with no window at all. A design that routed the collapse response through
this rule would be silent for 38 s of media, and this run is what that looks like.

The one thing the collapse case does exercise here is the reset path, and it does so exactly once —
which is also how the grader knows to replay it. Before `reset=` was on the wire, the same run
reported 11 spurious disagreements, because a window clearing itself and a window losing its
history for no reason are the same two lines in a captured log.

## 6. What this run cannot say

* Nothing about a **candidate** rung. The query is the segment's own byte count throughout, so
  every number here is current-rung sustainability. A candidate needs `σ·W_j·D/8000`, and `σ` has a
  usable ceiling only at rungs ≥ 4000.
* Nothing about `E_tx`. The transaction charge is not in the module yet.
* Nothing about whether refusing would have been **better**. The shadow decides nothing, by
  construction; grading that needs the closed-loop simulator over frozen traces, with a stall
  disqualifier, and `max_commits` is not a grader. **That simulator exists and runs in `make check`
  as of 2026-08-28** (`abr/sim.rs`, three stall-disqualified legs — see `j3-decides.md`), so this
  bullet's blocker is half lifted: what it still cannot do is the A/B, which needs the `abr_policy`
  switch to run both parameter sets over one trace.
* The regime-change exposure was measured on the earlier build of the same two cases and is not
  re-quoted here as a number, because that run's logs were replaced: after the shaper leg ended and
  throughput jumped ~4×, the verdict stayed `refuse` for a further 20 segments while the window
  still held slow ones. That is `n` behaving as specified, and it is the same 38 s that §5 above is
  about.
