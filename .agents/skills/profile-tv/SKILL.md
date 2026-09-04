---
name: profile-tv
description: >
  Diagnose a live PlxNative process that is slow, janky, frozen or consuming unexpected CPU/GPU
  on the rooted LG TV. Use for stack samples, a missing heartbeat while the PID still exists,
  frame pacing, Mali IRQ activity, HWCNT attribution, or deciding which graphics profiler explains
  a regression. If the process died, use crash-triage instead.
---

# Profile a live app on the TV

This is opt-in developer tooling. None of it runs in PlxNative unless a command or dev trigger was
explicitly armed, and the stack/IRQ helpers are temporary root processes that never enter an ipk.

## First split: dead or alive

Resolve the selected install through `make -s print-appdir print-rundir print-eventlog FLAVOR=<f>`
and find its process with `fuser <appdir>/plxnative`, never `pidof plxnative` (stable and debug have
the same executable name).

- No matching PID: use **`crash-triage`**.
- PID exists but `loop=` stopped, input is dead, or the UI stalls: use the stack sampler here.
- PID and heartbeat are healthy but pixels/rate are wrong: use the graphics path here.

Every command below drives or stops threads on the one television. Take **`tv-lock`** first; the
tools also wrap themselves in `tv-lock.sh with` when invoked alone.

## Live stacks

```bash
tools/plxnative-sample snapshot                         # all LWPs once
tools/plxnative-sample profile --seconds 5 --hz 10      # render/main statistical sample
tools/plxnative-sample profile --all-threads            # invasive, when ownership is unclear
tools/plxnative-sample watch --stall-ms 2500             # foreground opt-in watchdog
```

`watch` captures three all-thread snapshots for each heartbeat-stall episode and rearms only after
two consecutive heartbeats return. It never kills, restarts or uploads. Hand back the generated local bundle:
`report.txt`, `stacks.folded`, `samples.jsonl`, `maps.txt`, `metadata.json`.

Read `binary identity` before quoting a source line. On mismatch, target-derived function names
and raw module+offsets remain useful, but local file:line is deliberately disabled rather than
symbolizing against the wrong build. `wchan` distinguishes a blocked
sleep/lock from an on-CPU stack; frequency is statistical evidence, not proof that a waiting frame
consumed CPU.

## Three graphics layers

For an already-running screen, observe its current present heartbeat and active Mali IRQ rate:

```bash
tools/profile-graphics --seconds 30
```

Attach mode does not close the selected install, so it has no IRQ baseline. It checks the three
render-profiler triggers and marks pacing invalid if one is armed; only call its FPS production pacing
when the report says those triggers were absent. Use the deterministic command below for all three
layers and the selected-install-closed baseline.

For a reproducible scene, collect the full profile through the FPS harness:

```bash
./tests/run.py --fps --only <scene> --graphics-profile --profile-phase frame.ui
```

The bundle has three deliberately different meanings:

1. **Production present pacing:** unarmed `fps=`/`loop=`. This is the only leg whose FPS may be
   quoted as application pacing.
2. **Passive Mali IRQ rate:** virtually no app overhead, but global to the whole GPU. Compare the
   selected-install-closed baseline and active leg; never call it process utilization.
3. **Mali HWCNT:** phase-attributable JM/tiler/shader/L2 counters. Its `glFinish` boundaries
   serialize the pipeline, so never quote FPS from this leg.

If layer 1 says frames are slow, use `/tmp/plxnative-framedrop` and then
`/tmp/plxnative-cpuprof` to localize render-thread wall time. Use asynchronous
`/tmp/plxnative-profile` for GPU phase time. Arm only one profiler mode per launch. The HWCNT and
GL-timer triggers invalidate production pacing even when their output looks plausible.

## What counts as verification

The simulator cannot verify frame rate, the Mali driver, compositor behavior or live Linux stacks.
Run host selftests first, then verify the claim on the television. Report the selected flavour,
scene/phase, production-vs-instrumented leg, sample count and bundle path.
