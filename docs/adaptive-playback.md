# Adaptive playback: the risk-aware Auto controller

**What Auto is:** a hybrid throughput/buffer controller with probabilistic risk estimation and
utility-based Original/HLS mode selection. Shorter: a risk-aware stochastic ABR controller.

Everything below lives in `rust-modules/src/abr/` (the policy, host-tested), with three call
sites that own facts rather than decisions: `route.rs` (feasibility, the bootstrap probe, the
main-thread halves of both visible transitions), `ff.rs` (the measurements and the transaction),
and `ui/stats.rs` (the read-out). The protocol constraint this is all built on — one PMS encoder
session has one fixed rendition — is `docs/pms-hls-protocol-probe.md`, and it is what makes a
quality change a **transaction** rather than a request.

The module is split by **decision stage**, in the order the pipeline above runs them: `units.rs`
and `ladder.rs` are the shared vocabulary (`MediaTimeMs`, `Rung`, `LADDER`, the actuator catalog);
`plant.rs` is what the world does (buffer, production ratio, starvation horizon); `estimate.rs` is
what we believe about it (capacity, observation quality, per-segment samples); `viability.rs` and
`mode.rs` are the comparisons; `controller.rs` is the transaction that acts; `bootstrap.rs` is cold
start; `original.rs` is the direct-play path and its watchdog. Everything is re-exported flat from
`mod.rs`, so `abr::Rung` is still the name the rest of the crate uses. `plant.rs` is deliberately
**separate** from `sim.rs`'s copy of the same queue geometry — the two disagreeing is a test, not a
duplication to be folded away.

This note is the design and its reasoning. It is not a status report; `docs/parity-gaps.md` tracks
what is verified on a television.

## 1. The pipeline

Every Auto decision — the first one at startup and every one during playback — runs the same
ordered stages:

```text
feasibility  -> which playback states are technically possible at all
estimation   -> delivery capacity, PMS production, buffer, each with UNCERTAINTY
risk         -> per-candidate starvation horizon + production + buffer stress
utility      -> compare feasible states: quality + features - risk - server - transition
selection    -> argmax utility
validation   -> prime the winner off-screen and grade the actual media
commit       -> or keep the current state, untouched
```

Three properties of that ordering are load-bearing, and each replaced an earlier rule that looked
reasonable and was wrong.

**Feasibility is not a utility term.** A rendition the decoder cannot decode is removed before
anything is scored, so no weight is ever asked to outvote a hardware bound.
`HlsActuatorCatalog::limited_to` takes two facts: the device's own codec table (`devcaps`, which
exists because "4K yes" was once a constant describing one television) and the source raster —
asking PMS to upscale 1080p to 4K buys nothing and costs the measured 2.1x of server work.

**Feasibility is re-asked when the request changes, not only at startup.** Picking a different audio
track or turning on a subtitle can change whether Original is possible at all, and that lives at the
selection commit rather than in this module: `route::commit_audio_selection` re-runs the
direct-playable-audio test and routes the pick to a native stream switch or to a server transcode
accordingly, and both track commits DROP the stored Original candidate while HLS is live — the
recovery declaration captured one exact source/audio pairing, so once the viewer changes it, the old
one must not be resurrected behind their back. A fresh playback establishes a new candidate.

**Measurements reach the decision through ONE risk number per candidate.** Variance, VBR headroom,
buffer level and slope, and PMS cadence all end in `CandidateRisk`. The alternative — one utility
term per telemetry field — is how a utility function becomes untunable, because every new
measurement silently reweights every old one.

**A deficit is not an emergency.** `C < R` says the buffer drains, not that playback stops.

## 2. The arithmetic that replaced the counters

`starvation_horizon(buffer, requirement, capacity)` turns a rate deficit into seconds:

```text
B buffered, requirement R, capacity C, and C < R
    buffer drains at (1 - C/R) seconds per second
    T_starve = B·R / (R - C)
```

So a 60-second reserve against a 1.7% shortfall is an hour away, and ten seconds against a 12x
shortfall is ten seconds away. Both used to be "the measured rate is below the file's average",
which is the same sentence for both and the reason Auto abandoned Original on two slow windows.

**The requirement is not the file's average.** A whole-file average is a lower bound on demand: the
file contains scenes above it. `source_requirement_kbps` adds `AbrPolicy::vbr_allowance_pm` (1.35),
so a link that merely matches the average is already at risk before the first busy scene.

## 3. Estimation, with uncertainty as a first-class output

`CapacityEstimate` keeps a fast and a slow rate, a dispersion in per-mille, and a sample count.
What every decision consumes is `conservative_kbps()` — the slow estimate discounted by its own
uncertainty — never the mean. Two histories averaging the same number are not the same evidence:

```text
59, 60, 61, 60, 60   ->  tight dispersion, small discount
60, 10, 60, 12, 60   ->  wide dispersion, large discount
```

Three mechanisms keep that honest:

* **Observation quality.** Throughput is a rate, so the size and duration of a transfer decide how
  much it proves. A 40 KiB read that finished in 3 ms honestly reports 100 Mbit/s and proves
  nothing about the next second. `ObservationQuality` is Weak / Normal / Strong and weights the
  update accordingly; a truncated transfer is Weak whatever rate it reports, because it measured a
  floor.
* **Confidence grows with agreement.** A first sample carries the maximum discount and earns
  confidence as later samples agree with it. This is what "two successful probes" became: a probe at
  twice the requirement clears the bar alone, a marginal one has to be confirmed, and the number of
  probes is an output of the rule rather than part of it.
* **Staleness and weak priors.** `age_ms` widens uncertainty over an unmeasured wall-clock gap (each
  half-life closes half the remaining distance to the maximum discount) and past four half-lives
  demotes the estimate to a prior. `demote_to_prior` keeps the value and throws away the confidence;
  it has exactly three callers, each a different reason the history stopped describing the present —
  a bootstrap source probe seeding steady-state HLS (different request, different server work), a
  path change, and a long pause.

**A pause is the only real staleness.** Backpressure with a full buffer stops the reader on purpose
and must never be aged; both workers therefore watch `TX.paused` for the transition rather than
inferring idleness from the clock.

## 4. Two resources, never one budget

Network delivery and PMS production move independently, and the measured 4K point is the proof: the
wire cost rose 4% while the server's work roughly doubled.

| operating point | request | measured output | production ratio |
|---|---:|---|---:|
| 1080p high | 20,000 kbps | 1920x1080, ~20,011 kbps | 0.21 |
| 4K | 22,000 kbps | 3840x2160, ~20,895 kbps | 0.44 |

Both halves of the 4K row are load-bearing. A request of up to 21,750 kbps with a 3840x2160 ceiling
**stays 1080p**; 22,000 flips the output; and every request from 22 to 60 Mbps produced that same
output. So asking for 20,895 does not get 4K, and asking for 22,000 does not get 22 Mbit/s of bits.

`ProductionEstimate` therefore keeps a per-unit-of-work speed beside the raw ratio, and
`predicted_ratio_pm` answers "what would this candidate cost this server". **Only part of the
measurement scales.** The ratio is total ACQUISITION time over content duration, so it contains a
fixed per-segment cost — connection, request, time to first byte, playlist latency — that does not
care how hard the encode was. Extrapolating the whole number by the load ratio reads a LAN's 300 ms
of round trips on a 480p segment as a struggling server and vetoes every upshift out of the opening
rung; measured on the host suite, 480p at 0.4 predicted 1080p at 1.0 and Auto never left 480p on a
7 Mbit/s link. `AbrPolicy::production_floor_pm` is where the measurement is split.

Consequence worth stating: a fast link in front of a loaded server does not get 4K. The network says
yes, the server's own cadence says it would fall behind real time, and the two constraints are
evaluated separately so neither can override the other.

## 5. The actuator catalog

PMS accepts a bitrate ceiling; what it does with it is empirical. `HlsActuatorCatalog` stores the
request beside the measured output, and the ladder is 13 operating points: 320 / 720 kbps, 2 / 4 /
6 / 8 / 10 / 12 / 14 / 16 / 18 / 20 Mbps, and the 4K point.

**The "measured output" half of that sentence is now known to be wrong for 12 of the 13, and it is
wrong for a structural reason rather than a stale one: it is a per-ITEM quantity kept as a
per-server constant.** Swept across three library items
(`docs/measurements/p2h-pms-ladder.md`, `tools/pms-rung-sweep.py`), the rate PMS declares is
5%–32% below the request at every rung but the 4K one and moves with the title; rungs 18000 and
20000 turn out to be the same encoder session on a 1080p item, byte-identical in 39 of 40
segments. Every error is an over-estimate, so nothing mis-behaves today — but the replacement is
free, because the transaction already fetches the true declared rate and logs it before it decides
anything, and it is a real bound at rungs **4000 and above**: 0 of **1 440** segments exceeded
0.85x their own declaration, max 0.8456. **Not "above 2000", which is what this sentence said
until a second item was run through the full ladder and refuted it** — rung 2000 puts 9 of 120
segments over, to 0.9175, and the shipped constant is now `sigma = 0.90`: the pooled max scaled by
the largest measured cross-item spread, because 0.85 against a measured 0.8456 is a rounding
artefact of that measurement and not a margin. `docs/measurements/p2h-pms-ladder.md` §2/§2a. Spending it is admission-rule work and has not landed. Six of them are byte-for-byte the
`route::Quality` rungs a user can pick by hand, because Auto arriving at the same operating point
must send the same request.

The six 1080p rungs between 6 and 18 Mbps exist for one reason: **spending a measured link instead
of rounding it down to the next power of two.** A 17.5 Mbit/s link that has to choose between 8 and
20 Mbps spends 12 Mbit/s of itself on nothing.

Selection is a continuous **safe budget** (`hls_safe_budget`: the conservative capacity, discounted
again for a server already behind and for a reserve that needs refilling) and then the best
feasible, production-sustainable actuator that fits it. Never "one rung up" — a jump from 8 Mbps to
a 15 Mbit/s budget primes the 14 Mbps encoder once instead of paying for three encoder creations to
walk 10, 12, 14.

That skipping is bounded by evidence rather than by nerve: extrapolating the server's cost five
raster steps ahead is a guess, so when the production model cannot support the whole jump the
controller takes the step it can justify and re-measures. On the device's 17.5 Mbit/s leg that is
two moves instead of one.

## 6. Original is a mode, not the top rung

Original has benefits no bitrate expresses — no generation loss, source audio, Dolby Vision and
Atmos preserved, and **zero server video encoding** — and costs a visible reload to enter or leave.
So it is a separate `ModeKind`, compared by utility:

```text
U = quality + features - λr·risk - λs·serverCost - λt·transitionCost
```

with two terms that make the comparison behave like a human decision:

* **Benefit accrues over the remaining playback; cost is paid once, now.** Below
  `AbrPolicy::benefit_horizon_ms` the benefit is scaled linearly, which is the whole of "do not
  reload with twenty seconds left" — no threshold, no special case, and it degrades smoothly.
* **Transition cost is asymmetric and decays.** An HLS rung change is a background prime the viewer
  never sees and costs nothing here; a mode change costs `visible_switch_cost`; and each visible
  switch already spent in this playback adds a penalty that halves every
  `visible_switch_decay_ms`. One switch is a decision, a fourth inside two minutes has to buy a
  lot. That is the anti-flapping mechanism, and it is history the model can see rather than a
  sample-count cooldown.

The switch history outlives the workers deliberately: every Original↔HLS transition replaces the
engine, so a counter held by a demux worker would reset to zero on exactly the event it exists to
count. It lives on the route session, is captured into each worker at spawn, and both directions
record it — the penalty prices the ALTERNATION.

## 7. Leaving Original, and returning

**Leaving** (`OriginalModeController::observe`, one 750 ms window of ACTIVE body-read time) has three
exits, and the log names which one fired:

| exit | rule | consults utility |
|---|---|---|
| `ImminentStarvation` | horizon inside `starvation_fallback_secs`, **and the reserve measurably falling** | no — a stall beats any switch |
| `SustainedDeficit` | horizon unsafe for `ORIGINAL_DEFICIT_WINDOWS`, and utility agrees | yes |
| `EmergencyLowBuffer` | reserve under the floor and falling, whatever the estimates say | no |

**Both hard guards require an observed drain, and the first one did not until 2026-08-27.** The
horizon is `T = B·R/(R−C)`, which is a prediction only under the premise that the reserve is being
consumed at `(R−C)/R`; when the measurement beside it says the reserve is flat or growing, `T` is
arithmetic on a discounted rate rather than a forecast. Without that conjunct the guard fired on
measurement window ONE — where `conservative_kbps` is pinned to half the measurement by the
uncertainty floor and `buffered_ms` is the prime remnant — and cost a real film its 4K Dolby Vision
and Atmos for the whole playback (`docs/measurements/orig-first-window-fallback.md`). Both read the
RAW delta rather than `draining()`: at the moment of a *correct* fallback the smoothed slope was
measured at **+8446 ms/s**, still carrying the healthy leg before it.

**Every Auto Original is watched, wherever the server is.** Until 2026-08-27 the watchdog was armed
only for a `Remote` server, on the argument that a Local link needs no throughput proof — true of
the pre-flight probe below, false at runtime, since `Location` is decided from the address shape and
describes topology rather than throughput. A LAN held at 2 500 kbps under a 10 634 kbps source ran
the film at 8–25 % of real time with no `abr:` line in the log at all
(`docs/measurements/local-original-blind.md`).

The third is a **labelled emergency guard**: it should be unreachable when the model works, and its
appearance in a log is a finding about this module rather than about the network. It reads the raw
buffer delta, not the smoothed slope, because a 3:1 EWMA still reads positive through the first
sharp drop.

Two consequences of the arithmetic, both of which used to need special cases:

* A reserve that outlasts the remaining content can never starve, so the closing minutes need no
  rule of their own.
* The replacement state is the best candidate the CURRENT estimate sustains, never the bottom of the
  ladder. The worker hands the main thread its conservative estimate rather than the last window's
  raw rate — one sample of a noisy distribution is the wrong basis for choosing a rung.

**Returning** (`OriginalRecovery`) drops both of the old gate's requirements. It no longer waits for
the top rung: PMS producing 20 Mbit/s of H.264 says the SERVER can encode and says nothing about
whether the link can carry a 60 Mbit/s remux — a set that struggles to transcode may be an ideal
direct-play target, so gating recovery on transcode success measured the wrong resource. And it no
longer counts successful probes; see §3.

A probe reads real media bytes over the link the segments need, so it is not free. Four gates decide
whether to spend one, none of them a rung: a reserve deep enough that the probe cannot cause the
starvation it is looking for, a reserve that is not draining, measurable spare capacity in the HLS
evidence (segments prove a lower bound on the link — the only thing they honestly can), and a
minimum spacing. Then `worth_probing` asks the utility comparison under an assumed-good outcome, so
"twenty seconds left" and "already switched three times" stop the measurement rather than being
discovered after paying for it.

## 8. Bootstrap is a separate decision

At startup every estimator is empty, there is no buffer, and the viewer is looking at a black
screen. `bootstrap()` therefore branches on how much is knowable for free, and its worst case is
"start conservative HLS and let the real controller recover", never "hold the screen black until the
link is proven".

| link | rule | reason code |
|---|---|---|
| Local | Original immediately, no probe — and then WATCHED, §7 | `LocalDirect` |
| Relay | HLS; relay is bandwidth-limited by design | `RelayLimited` |
| Remote | one bounded probe of the actual file | `ProbeSustainable` / `ProbeBelowRequirement` |
| Remote, probe failed or source bitrate unknown | conservative HLS, playback still starts | `ProbeInconclusive` |
| any, Original impossible for this item | HLS | `OriginalInfeasible` |

The probe is only taken where it can change the answer. Admission uses
`AbrPolicy::bootstrap_confidence_pm` (1.35) as a fixed margin rather than an uncertainty discount,
and that is deliberate: with exactly one sample there is no dispersion to discount, so the margin
has to stand in for the confidence a history would have given.

Whatever the verdict, the measurement is not thrown away. A completed probe becomes an explicitly
weak prior for the live estimator (§3), so the first HLS segment refines a number the app already
paid for instead of starting from nothing — and it picks the opening rung from the same catalog
steady-state selection uses, so a 17 Mbit/s probe on a 60 Mbit/s file opens at a 12 Mbps rendition
rather than at a floor it would spend a minute climbing out of.

## 9. The transaction is unchanged, and it is the reality check

Everything above is a prediction. A quality move is still: propose, register a separately named PMS
encoder, fetch and fully demux its media off-screen, grade the actual segment (in-bounds decoded
raster, decodable IDR, valid audio framing and timestamps, network and production headroom, a
surviving reserve), and only then commit and retire the old encoder. A rejected candidate leaves the
controller's current rung untouched.

This is what makes the empirical table in §4 survivable if another PMS holds a different boundary:
the model chooses what to try, and the media decides what ships.

## 10. What this deliberately does not model

* **Decoder/render health.** This television publishes no trustworthy dropped-frame or
  decoder-starvation counter — the heartbeat's `vtick=`/`vgap=` pair counts a 5 Hz position callback
  and reads flat straight through a visible stutter. A proxy invented here would be an unfalsifiable
  input to every decision above, so candidate feasibility asks the device's codec table (a fact) and
  nothing asks the decoder how it feels.
* **Thermal state.** A throttling SoC or server arrives as what it actually is: production ratio
  drift, delivery drift, buffer slope.
* **Anything learned.** No ML, no online reinforcement learning. Every number is a measurement or a
  policy constant with a product meaning in `AbrPolicy`.
* **PMS's own `autoAdjustQuality`.** The probe established that this server does not change a live
  session's rendition; the client owns the decision.

## 11. Diagnostics

Two log surfaces, both free of names, addresses, titles and tokens — rates, milliseconds and
per-mille only, so a line can be pasted into an issue thread.

```text
abr: steady current=8000kbps safe=17600kbps pending=0kbps fast=22000kbps slow=22000kbps unc=200pm
     n=6 buf=12000ms slope=0ms/s prod=200pm/419pm risk=0 starve=none left=3512s reason=Some(...)
auto: Original -> HLS ImminentStarvation measured=3998kbps safe=3198kbps need=10800kbps buf=2900ms
     slope=-1200ms/s starve=4 windows=1 target=2000kbps
abr: Original probe #2 measured=60321kbps 2048KiB/400ms complete=1 current_safe=1 left=2100s
     verdict=Recover
```

Every field in the steady line was an INPUT to the decision published beside it, and the struct is
assembled by the controller rather than re-read at the log site, so the numbers logged are the
numbers used. `ui/stats.rs` carries the same state as the on-screen read-out for a photograph.

## 12. Where the tests are, and what they cannot see

`rust-modules/src/abr/tests.rs` grades the whole model on the host: the estimators
(dispersion, weighting, staleness, priors), the starvation arithmetic, candidate selection including
the 4K veto and the feasibility filter, all three Original exits, the recovery confidence ladder,
transition hysteresis, the bootstrap table, and the lifecycle resets. It is pure integer arithmetic,
so it runs in milliseconds and needs no television.

What it structurally cannot see: whether the television's decoder accepts a raster change inside one
Starfish Load, whether PMS honours a request the way the table says, and whether any of it looks
right. The synthetic pipeline tier (`./tests/run.py`, `pipe_auto_original_slow_recover`) drives the
whole Original→HLS→Original transaction against generated clips with no Plex anywhere, and the
device is still the only place a frame is decoded.
