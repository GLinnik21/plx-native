# Auto on a LOCAL server has no starvation watchdog at all

**Measured 2026-08-27, on the dev television, through `tools/netcond.py`.**
Reported by the maintainer as *"automatic bitrate cannot downgrade the quality and stays on a very
slow frame rate of the film"*. It reproduces on demand, the mechanism is structural rather than a
tuning failure, and **every assertion this harness had passed while it happened.**

## 1. The observation

`auto_original_squeeze`: `movie_h264_ac3_1080p` (source measured at **10 634 kbps**, 1918x802,
h264/ac3 mkv — direct-playable), quality **Auto**, on the **local** PMS. The link runs unshaped for
30 s and is then held at **2 500 kbps** and never released: a fourfold deficit against the source.

```
route: Auto Original — source 10634kbps 1918x802; no video encode
...
pos=118s play=984pm  vtick=5 vgap=201ms
pos=121s play=246pm  vtick=3 vgap=401ms
pos=121s play=166pm  vtick=3 vgap=400ms
pos=122s play=81pm   vtick=1 vgap=601ms
pos=124s play=83pm   vtick=1 vgap=2003ms
```

The reserve built during the unshaped opening carries the film for about 90 s at full speed. When
it runs out the picture collapses to **8–25 % of real time** — roughly two to six frames per second
on a 24 fps source — and stays there for the rest of the run. It never recovers and never falls
back.

**`grep -c "abr:" auto_original_squeeze.log` is `0`.** Not one adaptive line in 150 s: no
controller, no watchdog, no sample, no decision. There is nothing to downgrade the quality,
by construction.

`docs/measurements/local-original-blind-logs/` holds three captures:
`auto_original_squeeze.log` (before), `auto_original_squeeze-fixed.log` (after) and
`auto_link_squeeze.log` (the control, taken after).

## 2. Why: the watchdog is gated on the SERVER BEING REMOTE

`route::auto_original_watch()` — the sole constructor of the Original starvation watchdog — returns
`None` unless `Session::cur_auto_remote_original`. That field has exactly **one reader**, this
function, so its entire purpose is "should the Original watchdog run". It has four writers and they
do not agree:

| writer | condition |
|---|---|
| `apply_plan` (`route.rs:2373`) | `Auto` **and `Location::Remote`** and original-feasible |
| `set_quality` (`route.rs:1417`) | `Auto` **and `Location::Remote`** and original-feasible |
| `recover_auto_to_original` direct (`route.rs:777`) | `Auto` |
| `recover_auto_to_original` remux (`route.rs:802`) | `Auto` |

So an Original reached by *recovering* from HLS is watched on any link, and an Original chosen at
*play* time on a LAN is not. The two paths reach the same playback state and disagree about whether
it is supervised — which is what makes this an oversight rather than a design.

The justification in `set_quality`'s comment is that a Local link "needs no throughput proof". That
is true of the **pre-flight** question — whether to spend a probe before choosing Original — and
false of the **runtime** one. `Location::Local` is a statement about topology, decided from the
address shape (`probe::configured_tier`), and topology does not imply throughput: Wi-Fi, powerline,
a busy switch, a second stream in the house, or this conditioner all produce a LAN that cannot
carry a 10 Mbps source.

## 3. The prose that says the opposite

`plex/probe.rs`'s `configured_tier` doc argues that guessing `Local` from an address is affordable:

> a wrong `Local` starts Original, and `OriginalModeController` measures the very first 750 ms
> window and leaves on the starvation horizon — the mechanism that exists for a link which turns
> out not to carry the source. […] Neither can strand a playback

**That is false today**, and this run is the counterexample: `OriginalModeController` is never
constructed on a Local link, so it measures no window and leaves on nothing. `route.rs`'s own test
records the fact as intended behaviour — `assert!(auto_original_watch().is_none(), "HLS and Local
Original do not use this watchdog")`.

## 4. What was blind, and why

Every existing assertion passed:

```
[PASS] video_bound      [PASS] timeline_climb: 0s..125s over 148 samples
[PASS] timeline_post    [PASS] no_error
```

`timeline_climb` grades that the film got somewhere, never how long it took. `no_error` is right:
nothing failed — the pipeline is starved, not broken. And every buffer-derived metric is blind for
a structural reason that is worth stating once: **a reserve is media time measured against the
playhead, so when the playhead slows the reserve stops draining** and `min_buf_ms`, `slope` and
`draining()` all read healthy at exactly the moment the picture is worst. `max_stall_s` is blind
from the other side — it grades the clock stopping, and this is the clock advancing too slowly.

`playback_rate` (`tests/run.py`) is the metric that sees it, and the app now prints the same
quantity directly as `play=<pm>` on the heartbeat. On this run: mean **850 pm**, worst 10 s window
**111 pm**.

## 5. The control

`auto_link_squeeze` is the same experiment on `movie_av1_no_dp_audio`, where Original is infeasible
so Auto must run Fixed HLS. There the controller behaves, over the whole profile:

```
abr: committed Up   to 2000kbps 1280x720      unshaped leg
abr: committed Up   to 4000kbps 1280x720
abr: committed Down to  720kbps  854x480      the 2 500 kbps squeeze, UnsafeCurrentState
abr: committed Up   to 2000kbps 1280x720      the link returns
```

Worst window **900 pm**, mean 993, no stall, `prod` 28–30 pm at the end.

So the fault is not in the HLS controller and not in the estimators. It is that on this path
neither exists.

## 6. After the fix

Same case, same profile, same item, on the two commits that followed
(`abr: ★ a starvation horizon must observe the drain it claims to be racing` and
`abr: ★ Auto Original is watched wherever the server is`):

```
auto: Original -> HLS ImminentStarvation measured=2529kbps safe=1264kbps need=14355kbps
      buf=14917ms slope=8446ms/s starve=16 windows=1 target=720kbps
auto: Original became unsustainable at 1264kbps; switching to 720kbps 854x480 HLS
```

| | before | after |
|---|---|---|
| worst 10 s window | **111 pm** | **818 pm** |
| mean | 850 pm | 978 pm |
| `abr:` lines in the log | **0** | 83 HLS segments |
| log length | 314 lines | 742 lines |

The squeeze starts at t=30 s; the watchdog leaves Original at t≈50 s, once the reserve has drained
from ~40 s to 14.9 s and the horizon has reached 16 s against a `starvation_fallback_secs` of 20.
The remaining 818 pm window is the fresh `Load` the mode switch costs, and is what the case's
`min_play_rate_pm` floor is sized to admit.

**One detail in that line is worth keeping**, because it decides which of two similar-looking
quantities the new guard may read: `slope=8446ms/s` — the SMOOTHED EWMA — is strongly POSITIVE at
the moment of a correct fallback, because it still carries the healthy opening leg. Only the raw
`last_delta_ms` was negative. A guard written against `draining()` would have blocked this exit;
the one that shipped reads the raw delta, for the reason `EmergencyLowBuffer` already gave beside
it.

## 7. Corroboration from the AU queue

The `feed v#…` lines carry `qbytes`, the video AU queue depth, and they place the collapse exactly:

```
feed v#2700 … fed=112416999999 … qbytes=1361575
feed v#2800 … fed=116582999999 … qbytes=379154
feed v#2900 … fed=120749999999 … qbytes=0
feed v#3000 … fed=124916999999 … qbytes=0
```

It drains monotonically from 3.2 MB and reaches zero at `fed=120.7 s`, which is the beat at which
`play=` fell to 246 pm. So the picture is starved rather than slow to present — the pipeline shows
the frames it has, when it has them — and `vtick` falling 5 → 1 with `vgap` reaching 2003 ms is the
same fact from the video plane's side.

## 8. Still open

* **The fallback is one-way here by construction** — the profile never lifts the squeeze — so
  nothing in this run says whether Original is recovered when the link returns.
  `orig-first-window-fallback.md` §"Why it never came back" prices that gate at 1.69x the source,
  which is a separate open item.
* The replacement rung is **720 kbps 854x480** against a link measured at 2 529 kbps: the same
  double discount that document describes (`conservative_kbps` halves, then
  `original_fallback_rung` divides by `vbr_allowance_pm` again). Correct in direction, and
  three times more conservative than the measurement.
* **The 90 s of healthy playback before the collapse is a second finding**, not incidental: the
  reserve built on the fast leg masks the deficit for a minute and a half, so any watchdog that
  waits for the reserve to fall is late by exactly that much. The deficit is knowable from the
  first window (`requirement` 10 634 kbps against a `conservative` estimate of the delivered rate)
  and the horizon test is already written that way — this run cannot say whether it would have
  fired early, because it never ran.
* Whether the collapse is *recoverable* — the profile deliberately never lifts the squeeze.
* **The control ends sitting on rung 2000 with `safe=117928kbps`** — a 118 Mbps budget under a
  2 Mbps rung — because the admission window needs 19 samples per rung and the case is 150 s long.
  That is the under-reach the plan of record already tracks, not something this finding introduces,
  and it is why neither case carries a rung bound.

## 9. The metric against the whole committed corpus

`playback_rate` over all 73 captured logs under `docs/measurements/` that carry a usable `pos=`
series. Eight fall below the 700 pm floor the two new cases assert, and **every one of them is a
case where slowness is required by construction** — no false positive anywhere else:

| worst 10 s window | logs | why |
|---|---|---|
| 0 | `pipe_abr_down_collapse` x3, `pipe_abr_down_outrun` | the profile collapses the link to **500 kbps**, and the ladder floor is 320 kbps of video plus ~192 kbps of audio. The bottom rung is itself unaffordable, so the film has to run slow — there is nowhere left to go |
| 111 | `auto_original_squeeze` (before) | the defect this document is about |
| 667 | `pipe_abr_band_20000` x3 | `abr_pin: 20000` — the case exists to HOLD an unsustainable rung and sample the `A/D` band nobody had observed |

The four zeros mean a full ten-beat window with no media advance at all, i.e. a hard stall of
≥10 s. Those cases carry `settle_max_kbps` and no `max_stall_s`; whether the floor rung should
refuse a link it cannot fit on (`HlsReason::LadderFloor` is a stated terminal case and does
nothing else) is the open question they pose, and it is not this one.
