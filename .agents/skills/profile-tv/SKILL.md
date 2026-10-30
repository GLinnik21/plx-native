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

## Styled-subtitle (libass) cost

With an ASS/SSA track rendering, the event log carries `assperf:` lines (`window_ms` the span the counts cover, render and whole-job p95/max, bytes
published and uploaded in that window, Normal+Ahead `jobs`, `ahead_hit`, `nwait`, and `worker tid=`) and `FRAMEDROP spans=` gains `assup` (the
UI-thread GL upload; needs `/tmp/plxnative-framedrop` armed). Read these before attributing a late frame to subtitles; follow the worker
with `tools/tv-sched-trace.sh --tid <tid>`. See `docs/ass-subtitles.md`.

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
   serialize the pipeline, so never quote FPS from this leg. The same goes for late-frame counts:
   with HWCNT armed, every frame of a busy scene ran 24–27 ms on the T820 (2026-10-17, Watchlist
   Remove), so a ">25 ms" count from this leg was 15x inflated (76 against 0–5). Use it to compare
   phases and builds in the same leg; take true late counts from a FRAMEDROP or
   `tools/tv-sched-trace.sh` run with HWCNT off.

If layer 1 says frames are slow, use `/tmp/plxnative-framedrop` and then
`/tmp/plxnative-cpuprof` to localize render-thread wall time. Use asynchronous
`/tmp/plxnative-profile` for GPU phase time. Arm only one profiler mode per launch. The HWCNT and
GL-timer triggers invalidate production pacing even when their output looks plausible.

## Why a back buffer is late (kernel trace)

A frame whose first framebuffer command waits a tick (8.33 ms) or more longer than usual missed a
compositor pass; which side made it miss (the app's CPU, the app's GPU job, or the compositor) is
answered by a kernel trace, not by a count of late frames. During an fps leg armed with
`--extra-trigger plxnative-framecb --extra-trigger plxnative-framering=17 --save-logs DIR` (framecb
adds `wait_at=` to the FRAMEDROP lines; the ring writes the slow frames and their neighbours; it
needs framedrop armed, see the doc), take 6 s windows with
`tools/tv-sched-trace.sh --app --secs 6 --out w1.gz` (`--app [FLAVOR]` finds that install's pid by
the suffix of `/proc/<pid>/exe`, jail prefix and all; the file opens with a `# plx-task` thread table),
then `tools/analyze-sched-trace.py DIR/fps-<scene>.log w1.gz ... --tid <pid> --latch`. It sorts every
long wait into CPU-late, GPU-late, edge or compositor-side, in 8.33 ms osd ticks, and ends with the
on-time margin (GPU done minus the compositor's pass, over the frames the ring wrote): the number
to quote. Procedure, how to read
the classes, a worked example and two traps (tracing loads the set, so never quote a window's rates;
a wakeup by `<idle>-0` is the cross-CPU IPI path, not a waker) are in `docs/player-submenus.md`
(step 1 of the 2026-10-02 capture, "Kernel scheduler trace", and its `--latch` paragraph). Run it
with HWCNT off.

## Mali T820 facts for reading the counters

Measured facts for reading HWCNT, `plxnative-drawmask` and bench output. For `plxnative-drawmask`
itself (the draw classes, `all`, `glass`, the comma list) see `docs/backdrop-blur-profiling.md`.

**The GPU clock is fixed at 672 MHz (dev set, measured 2026-10-24).** The TV's `mali_kbase.ko`
(Midgard r12p0, `lg1314_platform_config`) sets `gpu_freq_khz_max = 672000` and
`gpu_freq_khz_min = 300000` in `kbase_platform_device_probe`. It imports `clk_get`/`clk_get_rate`/
enable/disable but no `clk_set_rate`, regulator, OPP or devfreq symbol. Its `dvfs_callback` ->
`kbase_pm_get_dvfs_action` only computes utilisation. The kernel clock tree (`clk/clk_summary`)
holds only `xtal`, `busclk`, `cpupll` and `cpuclk`, with no Mali clock. So there is no GPU DVFS to
blame.

- `GPU_ACTIVE` converts to time at 672 MHz: one 60 Hz refresh is about 11.2M GPU cycles.
- That budget is shared with the system compositor, because `GPU_ACTIVE` is GPU-global.
- The highest `GPU_ACTIVE`-per-elapsed-ns rate seen in HWCNT legs was 649 MHz, under that ceiling.

**The driver reports GPU busy % without HWCNT's `glFinish` serialization.** Debugfs is built into
the kernel but not mounted. Mounting it is a device-state change, so do these steps in order under
the TV lock, on the TV's busybox `sh` (POSIX sh only). The state they share lives in files, so they
work as one script or as separate `tools/tv-ssh ssh tv '...'` calls; repeat the first line in each:

```sh
D=/sys/kernel/debug; OUT=/tmp/gpu-util.log; PIDF=/tmp/gpu-util.pid; MNTF=/tmp/gpu-util.mounted

# 1. Mount debugfs only if it is not already mounted, and remember whether THIS run did.
if grep -q " $D debugfs " /proc/mounts; then echo 0 > $MNTF
else mount -t debugfs none $D && echo 1 > $MNTF; fi

# 2. One-shot read; prints "mali0 utilization: N".
cat $D/mali0/gpu_utilization

# 3. Sampled run only: start a 10 Hz loop in the background (its only fork is `usleep`), writing
#    to the absolute $OUT, and record its pid. All three fds are redirected so an ssh call returns.
: > $OUT
while :; do
  read up rest < /proc/uptime
  read a b c < $D/mali0/gpu_utilization
  echo "$up $c" >> $OUT
  usleep 100000
done > /dev/null 2>&1 < /dev/null &
echo $! > $PIDF

# 4. Run the scene, then stop the sampler by killing that pid. $OUT now holds the samples; read it
#    back, then `rm -f $OUT`.
kill $(cat $PIDF); rm -f $PIDF

# 5. Unmount only if step 1 mounted it, before handing the set back.
[ "$(cat $MNTF 2>/dev/null)" = 1 ] && umount $D; rm -f $MNTF
```

- `N` is a percent over the last `dvfs_period` (100 ms), for the whole GPU, compositor included.
  Each line of `$OUT` is the uptime in seconds and `N`.
- **The sampler is not out of the measurement for misses: in one run (22 vs 9) it roughly doubled
  them.** With it running, a grid-pong control with the panel off missed 22 refreshes in cycles 2-60
  and a cards-off run missed 8. The same panel-off control without it missed 9 (panel-on controls
  that day missed 8, 13 and 14, measured 2026-10-24). That is one run each way, so the size of the
  effect is not established. Its utilization figures are valid, but never grade misses from a run
  that had the sampler running; take misses from a separate run without it.
- A Home grid-pong control run read 86-90 % and a cards-off run 50-63 %. That matches HWCNT at
  672 MHz (the control: 9.95M cycles/frame x 60 / 672 MHz).

**CPU and thermal.** LG's power manager governs the CPU only.

- Its files are `/proc/lg/pm/status` (its `temp:` line is the SoC temperature), `/proc/lg/pm/tune/`
  (min 600 MHz, max 1008 MHz), `/proc/lg/pm/frequency` (the current CPU frequency) and
  `/proc/lg/pm/temperature` (the SoC temperature, 39-61 °C on 2026-10-24). Its GPU thermal
  parameters `ts_gpu_aparam`/`ts_gpu_bparam` read 0.
- There is no cpufreq, devfreq or thermal-zone sysfs.
- The owner's Glasshouse dashboard on the TV's port 8080 serves `/api/stats` with SoC `temp`, CPU
  `mhz` and the foreground `app_id`. Its `gpuMhz` field is null on this set.
- A control run at a flat 39-42 °C with the CPU pinned at 1008 MHz showed the normal miss rate, so
  neither thermal throttling nor CPU frequency drives the misses.

**Reading `plxnative-herobench` misses.**

- (a) A cycle-1 `swap` of 13-16 ms on the frame after the event-log line
  `egl surface (after 120 presents)` is the one-shot `EGL_BUFFER_AGE` query in
  `plx_gfx::egl::late_probe()` (`rust-modules/gfx/src/egl.rs`). A build from before #980 ran it on
  every launch, about 2 s in, so older logs show that block in cycle 1. On a current build it runs
  only with the `eglage` dev trigger armed (`docs/egl-partial-update-and-damage.md`, section 7), so
  an unarmed run has neither the stall nor the line, and a cycle-1 miss there is something else: a
  first-time upload (`prepare=` with a large `px=`; #980's runs showed one, a 921,600-px upload with
  `prepare=15.0`) or first-time text raster (`textx4`). Count cycle 1 separately from cycles 2-60.
- (b) On Home `hero-pong` and `grid-pong`, misses in cycles 2-60 rose sharply above about 8M
  `GPU_ACTIVE` p50 per frame (about 70 % busy at 672 MHz):
  - 5-14 misses at 8.1M or more;
  - 1-5 at 7.9M or less;
  - 0-3 at the 3.1M floor (`plxnative-drawmask=all`).

  These are single runs per mask. The control spread in cycles 2-60 was 7-15: `hero-pong` 14, 7
  and 9 and `grid-pong` 15, 11 and 9 in #980's device runs (2026-10-24), and `grid-pong` 8, 9, 13
  and 14 in the controls above. Late frames wait in `clear` (back-buffer release) while the CPU is
  idle. That is the GPU queue, not the UI thread.
- (c) `plxnative-drawmask=glass` also skips the glass source pass, an early post-swap FBO
  submission. A `-glass` leg is therefore not a clean measurement of glass cost.
- (d) The Home p95 was bimodal: the ground probe's 1-in-`GROUND_SAMPLE_EVERY` kick frame added
  1.7-1.9M cycles while the top bar's probe copied framebuffer 0 mid-page. The bar's copy is now made
  at the frame boundary from its band image and `hero-pong` no longer shows the second mode; the
  measurement, and the cost that moved into the inter-frame gap (which `GPU_ACTIVE` does not see),
  are in `docs/glass-hardware-budget.md`, "What to index on instead". The Hero row's controls probe
  (`sample_control_ground`) still copies framebuffer 0 on its call and was not measured separately.

**Home pong census** (release build, 2026-10-24; `GPU_ACTIVE` saved per frame by removing one draw
class with `plxnative-drawmask`):

- hero: ramp bands up to 2.4M (upper bound; it includes about 35 chrome rects), photos 2.1M, wedge
  1.1M, glass + source + blur 1.05M, floor 3.1M.
- grid: cards 3.6M (the focus program 1.8M of that), background wash 1.8M.
- Normal frames run about 10.3M (hero) and 9.9M (grid).

## What counts as verification

The simulator cannot verify frame rate, the Mali driver, compositor behavior or live Linux stacks.
Run host selftests first, then verify the claim on the television. Report the selected flavour,
scene/phase, production-vs-instrumented leg, sample count and bundle path.
