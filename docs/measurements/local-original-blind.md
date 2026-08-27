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

`docs/measurements/local-original-blind-logs/` holds both captures.

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
so Auto must run Fixed HLS. There the controller behaves: it climbs 720 → 2000 → 4000 on the
unshaped leg, and when the link drops to 2 500 kbps it commits **Down to 720** with
`reason=Some(Hls(UnsafeCurrentState))` and settles at `prod` 114–175 pm — sustainable. Worst window
900 pm.

So the fault is not in the HLS controller and not in the estimators. It is that on this path
neither exists.

## 6. Still open

* The fix is to make the field mean what its only reader asks — *Auto chose Original* — and drop
  the `Location` conjunct from the two writers that carry it. Cost on a healthy LAN is one
  starvation-horizon computation per 750 ms of active read, on a horizon that is infinite;
  `starvation_safe_secs`, `ORIGINAL_DEFICIT_WINDOWS` and the utility veto are the same guards the
  Remote path already relies on to avoid a spurious visible switch.
* **The 90 s of healthy playback before the collapse is a second finding**, not incidental: the
  reserve built on the fast leg masks the deficit for a minute and a half, so any watchdog that
  waits for the reserve to fall is late by exactly that much. The deficit is knowable from the
  first window (`requirement` 10 634 kbps against a `conservative` estimate of the delivered rate)
  and the horizon test is already written that way — this run cannot say whether it would have
  fired early, because it never ran.
* Whether the collapse is *recoverable* — the profile deliberately never lifts the squeeze.
