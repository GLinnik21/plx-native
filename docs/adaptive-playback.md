# Adaptive playback: current contract

Status: implemented, host-tested, television verification required for the 2026-08-30 network
changes.

Auto contains two different decisions and they must not be collapsed into one bitrate ladder:

1. HLS rung control is a conservation problem over measured acquisitions.
2. Original versus HLS is a utility decision about a visible pipeline reload, source-only
   features and recurring server work.

The first has no product-tuned network margin. The second necessarily contains explicit product
values: network measurements cannot derive how objectionable a black frame is or how much a viewer
values Dolby Vision.

## 1. Why the displayed download rate is not link capacity

An HLS rendition asks PMS for a finite amount of data. If the current rendition needs about
4 Mbit/s, a healthy 25 Mbit/s path can still appear as 4–6 Mbit/s: the request did not offer enough
work to saturate the path. Therefore

```text
bytes / active_read_time
```

for the current response is a lower bound on service obtained by that response, not an upper bound
on the path. It may establish that the current operating point works. It cannot veto a larger
request or identify the best untried rendition.

No continuously parallel speed-test stream is used. It would consume the same path as playback and
change the quantity it tries to measure. The de-obfuscated official Chromium/webOS client confirms
there is no special PMS speed-test endpoint behind its “Checking connection speed” notice: before
`player.open()` it performs an ordinary raw Part GET and measures source bytes. PlxNative uses the
same physically uncapped object, but bounds it to one finite Range response. At cold start that
request runs before playback; at runtime it is serialized between HLS acquisitions, never beside
one.

## 2. Exact finite-episode physics

For every repeatable completed acquisition `i` admitted to the current HLS operating-point episode:

- `A_i` is end-to-end acquisition time, including open, server production, probe and body read;
- `D_i` is playable media duration credited only after that acquisition completes;
- `B` is the currently playable A/V reserve.

The active operating point is sustainable on the observed finite episode exactly when

```text
Σ A_i <= Σ D_i
```

For the chronology that actually occurred, let

```text
R_o = max_i( Σ_(j<i) (A_j - D_j) + A_i )
```

`R_o` is the exact starting reserve needed to replay that ordered current-rung history once. It is
the current-rung stay/down certificate: it follows the measured order and makes no claim that an
unseen response will repeat it.

For discretionary exploration, the rollback obligation must survive any ordering of the completed
episode. Its exact worst-permutation starting reserve is

```text
R_s = Σ max(A_i - D_i, 0) + max_i min(A_i, D_i)
```

The last term is load-bearing. Even when `A_i == D_i`, playback needs `A_i` of starting reserve
because the next `D_i` is unavailable until the acquisition finishes. `R_s` is a retrospective
stress boundary, not a forecast for an unseen response. On an alternating sequence of slow and fast
segments it can grow with the number of historical slow segments even while `R_o` says the actual
ordered stream is survivable. Keeping the two named prevents that adversarial exploration price
from becoming the current-rung emergency trigger.

The live certificate covers every repeatable completed acquisition since the
operating-point/service-episode reset. The structurally marked first object of a fresh HLS cursor
is the one exception: its one-time setup cost still updates delivery, production and reserve, but
does not enter the repeatable acquisition episode. Once that boundary object is past, the
certificate does not become a last-64-sample rule when diagnostics storage wraps. Production folds
an associative constant-space summary carrying `n`, `ΣA`, `ΣD`, `delta = Σ(A-D)`, `R_o`,
`Σ max(A-D,0)`, `max min(A,D)` and the largest per-sample fixed overhead. For adjacent chunks `x`
then `y`, the only order-sensitive composition is

```text
R_o(x ++ y) = max(R_o(x), delta(x) + R_o(y))
```

all sums add, and sample maxima take `max`. Every addition and unit conversion is checked; an
overflow is an absorbing refusal, never a saturated pair of totals that can accidentally compare
equal and authorize exploration. The separate 64-entry ring remains only for retired/offline
order-statistic diagnostics.

A failed discretionary experiment is followed by the exact next ordinary current-rung acquisition
before its media can be credited. Let `D_next` be that object's parsed `EXTINF`, and `L` the balance
left after the experiment. Surviving every unseen rollback response that is still sustainable
requires `L >= D_next`. Once it completes, `B' = L - A + D_next >= L`, so restoring the stress
boundary requires `L >= R_s`. These obligations lie on opposite sides of the same media credit, so
their exact joint requirement is the larger one:

```text
E = max(B - max(R_s, D_next), 0)
```

`D_next` is read without consuming the current cursor. It is a physical media object, not a tuned
network margin or the duration of the preceding sample. The parser retains exact nanoseconds; on
the controller's whole-millisecond lattice credited media rounds down and reserve obligations
round up. Reusing the preceding `D` is unsound for a variable-`EXTINF` playlist; adding
`R_s + D_next` would charge the same rollback completion twice.

These expressions are deterministic statements about completed observations. They are not a
prediction of an unseen segment and not a probability guarantee.

An incomplete or abandoned response is right-censored. Its prefix never enters the acquisition
episode, the capacity estimator, or a commit decision.

## 3. HLS actuation

### Staying and moving down

At a segment boundary, the current rung stays while its finite episode is sustainable and
`B >= R_o`. This is the exact replay certificate for the chronology actually observed, not a claim
that the next response is drawn from that episode.

If the completed episode is not sustainable but `B >= R_o`, the controller uses only measurements
that episode actually supplied — conservative delivery and reserve refill — to prime the highest
lower actuator their existing conjunction supports. That actuator remains an experiment: a complete
intermediate response must be funded and sustainable before commit. If it is unsustainable, descent
continues; if no lower point passes the model, the smallest feasible response is the bounded
fallback. This does not invert the current demand-capped rate into an unseen link capacity.

If `B < R_o`, the active response cannot replay even the measured chronology without starving. The
controller therefore primes the smallest feasible response, which minimizes worst-case
time-to-picture instead of spending the remaining reserve on a
quality-preserving guess. An abandoned/censored acquisition has the same floor fallback because it
supplied no completed media quantum with which to order lower responses; a displaced live actuator
remains direct rollback evidence and is tried first. The controller carries
`ReservePolicy::TerminalFloor` into that exact candidate transaction: although `B` may still be
positive, the premise for protecting a guaranteed old-cursor replay is absent. The floor downshift
therefore runs to an actual transport result instead of repeatedly spending that remnant on the
measured abort/rollback/retry cycle.

At the ladder floor there is no lower actuator. The runtime clock hold described below still
protects the picture, but if even the floor is physically unsustainable, uninterrupted playback is
impossible until the path or server recovers.

### Exploring upward

If `E > 0` and there is no failed response-size endpoint at that budget, the controller may excite
the highest feasible unclassified rung. The candidate is a separate PMS fixed-rendition encoder
session. The old session remains the rollback actuator.

After a high excitation establishes a scheduling endpoint, adjacent descent is the worst possible
search schedule: it pays for every intervening encoder session. The controller instead splits the
remaining ordinal actuator interval. A completed but unfunded response supplies that endpoint from
its observed size. A deadline-censored attempt retains the requested actuator as an *operational*
endpoint after its hard budget block is released: no response size is claimed, but one additional
millisecond of reserve must not buy the identical maximum transaction again. This is minimax search
over a finite ordered set, not a conversion of the current rung's download rate into an estimate of
hidden link capacity.

A deadline-censored transaction produced no complete candidate media quantum. A completed response
with no Pareto gain produced media, but PMS answered with a different demand-capped object; its
bytes do not say that a lower request ceiling would fare better. In the former case the serial PMS
decision/start/playlist/body path did not finish inside `E`; in the latter the no-gain response
completed for that exact cost. Let `E_f` be the disposable reserve armed at its start. Changing the
requested rung is not new budget evidence in either case and may leave several physical PMS
encoders overlapping. The next quality excitation therefore requires

```text
E > E_f
```

The transaction's own drawdown already lowered `E` while it ran. Returning from that endpoint to
`E_f` replaces the media it consumed; requiring `E_f` plus the drawdown again would charge the
same debt twice and can put the release point above a physically full queue. Strictly exceeding
`E_f` therefore proves both refill and new disposable reserve. A no-gain response then retries the
highest informative request because it ordered no actuator. A censored/unfunded endpoint instead
selects the greater of (a) the ordinal midpoint below the lowest retained endpoint and (b) the
highest eligible actuator admitted by the existing conservative delivery and refill
equations. The midpoint preserves minimax search; the modeled term may move farther upward or cross
an old endpoint when genuinely stronger service evidence supports it. Candidate commit still
requires that candidate's own completed `A <= D` and `B_post >= A` observation. This introduces no
poll interval, bitrate margin or probe-
count constant.

PMS encoder lifecycle is an independent physical condition. A successful universal-transcoder
`/stop` only queues cleanup, and PMS owns the physical `session=` encoder separately from the
logical `X-Plex-Session-Identifier` Streaming Resource charged by the bandwidth governor. Before
another upward HLS or Original experiment, the client checks the stopped physical key with the
matching `/ping`; `200` proves it remains and `404` proves only that physical entry is gone. It
then synchronously closes the exact logical identity through `POST /status/sessions/close`; `2xx`
means it was terminated and authenticated `404` means it was already absent. Only that two-part
certificate releases the cleanup barrier. Checks are coalesced and driven by completed active HLS
quanta, not sleep, retry-count or elapsed-time policy. Emergency descent remains available while
cleanup is pending.

For the initial phase of an upward experiment, one playhead-funded reserve clock, bounded by `E`,
covers:

- PMS transcode decision and session creation;
- master playlist;
- media-playlist refresh and its wait;
- the first complete candidate segment: open, headers, any production wait, probe and body.

If that structurally unique boundary object has `A > D`, its `A` also contains one-time
decision/session/JIT work and does not identify the running encoder's repeatable cadence. The
complete object remains staged, and the same original `E` clock may fund one ordinary object from
the now-running candidate encoder. Neither object enters the playback queues before the final
verdict. Thus the optional second acquisition is conditional but cannot enlarge its own grant by
crediting media from an actuator which has not committed.

Before each blocking control or playlist leg, its remaining `E - Δplayhead` is projected to an
absolute transport deadline; the media AVIO retains the clock itself and refreshes that projection
on every open, wait and read. If Pause or a naturally stopped native clock moves an issued
projection, idempotent GET legs retry with the unchanged reserve. A timed-out transcode decision is
not replayed blindly because PMS may have registered it; exact cleanup is queued. If its projection
moved while the playhead reserve remained unspent, the outcome is a circumstance rather than a
censored-rung observation; `Δplayhead >= E` remains censored reserve. Synchronous firmware DNS may
still exceed the projection on builds without an interruptible resolver; the main-thread reserve
floor remains active during that call.

A downshift cannot arm that same end-to-end clock before registration: its exact media obligation
depends on the candidate playlist's actual `EXTINF` and delivered rendition, facts which do not
exist yet. Its control and playlist legs therefore retain their typed transport-liveness bounds.
Once those facts exist, the media leg receives the exact recovery budget computed at that boundary.
That clock spends elapsed time through involuntary starvation and an internal clock hold, but
subtracts every native-accepted user Pause interval, including a complete Pause→Resume cycle hidden
inside one blocked read. A terminal-floor recovery remains unarmed because abandoning the only
remaining actuator cannot restore a stronger rollback guarantee.

The HLS GET legs also retain an independent wall-clock transport-inactivity deadline. Plaintext PMS
control has the same rolling inactivity shape: complete headers and actual body bytes begin a fresh
epoch. HTTPS PMS control instead retains its existing 25-second whole-request API cap, intersected
with the current reserve projection; progress does not renew that total cap. In every case an
obsolete reserve wake renews neither form of transport liveness. Classification reads the owning
clocks: spent reserve is censored evidence, expired transport liveness is a circumstance, and
neither means the projection merely moved.

A completed upward candidate is accepted exactly when

```text
A <= D  and  B_post >= A
```

A downshift is a recovery transaction: it must leave one complete decodable segment funded
(`B_post >= D`), and an unsustainable intermediate response (`A > D`) is rejected so descent can
continue. The terminal floor is the derived no-rollback exception. No cheaper actuator exists
there, so any complete demuxable floor response commits even when `A > D`, `B_post < D`, or PMS
exceeds the requested rung box; otherwise the only possible result is retrying the same floor
while retaining a known-losing higher route. That verdict means “best available”, not “stable”. A
successful commit atomically changes the actuator and seeds a fresh
operating-point bag with that candidate sample. Old-rung acquisitions never become new-rung
evidence, in either direction. It also publishes the physical encoder id, URL and rung as one route
state; a seek or mode rollback therefore rebuilds the stream that actually won, not the bootstrap
request that created the worker.

The candidate-media ownership protocol has these externally meaningful phases:

- `Primed`: the transaction owns a candidate encoder; route, controller and playback queues still
  belong to the current actuator.
- `Staged`: one boundary object, and only when structurally necessary one ordinary object, belong
  solely to the candidate. The old cursor and playback queues remain untouched. A rejected
  candidate discards all staged media and retires its encoder.
- `MediaPending`: only after the complete candidate verdict is `Commit`, immediately before the
  first candidate AU may cross into a playback queue, the worker makes a structural generation
  gate unstable. Every staged object is then fed under that one ownership transition.
- `Committed`: while serialized against queue abort and route replacement, the process route
  publishes encoder/URL/rung/observation and its callback moves the controller plus the
  worker-local encoder id. After those locks release, the worker promotes the matching cursor and
  publishes its runway. If an internal hold is active, releasing the structural gate first starts
  a fresh new-route recovery epoch.
- `Discarded`: no candidate AU crossed into playback, so the old cursor, timeline, route and
  recovery epoch do not move. There is nothing to realign.
- `Terminal`: abort, a concurrent route replacement, or an impossible commit precondition after
  media publication leaves the gate latched until teardown clears it with the queues.

Cleanup never runs inside the queue-abort/route publication locks. A losing transaction retains and
retires its candidate after those locks release; a successful commit releases the structural gate
and any trial-reserve gate before retiring the previous encoder. An I/O completion is likewise
reduced to one typed event at the boundary where control returns: caller abort, deliberate
active-stream stall, reserve expiry, transport expiry, HTTP response and parse failure are distinct
causes. A later scheduler delay cannot relabel one as another.

An upward commit additionally requires the candidate's observed output to strictly Pareto-dominate
the output it would replace:

```text
w_candidate >= w_current
h_candidate >= h_current
declared_candidate >= declared_current
and at least one inequality is strict
```

The first two values are decoded raster; the third is the candidate master playlist's declared
bandwidth. More bits at the same raster can improve an encode. More pixels at fewer declared bits
is not objectively ordered without a product weight, so it is rejected rather than called an
upgrade. A larger request ceiling by itself is never evidence of higher picture quality.

The active state therefore retains both the requested actuator and the observed master/raster.
If the largest request returns geometry that is provably below both its bounding box and the
known source, it is not terminal `AtBestRung`. A fresh encoder at the same actuator becomes
eligible only when the live conservative service bound rises strictly above the completed-service
observation attached to that response. That is the first proof that the environment differs from
the one which produced the active underfill.

If a fresh encoder completes but returns no Pareto gain, its response is still demand-capped and
cannot identify dormant path capacity or order other request ceilings. The completed transaction
instead records the exact common refill frontier `E_f` defined above. Quality
exploration may run again only after disposable reserve grows strictly past that frontier, and then
retries the most informative highest request instead of walking every lower tier. Thus a filling
real buffer can safely buy one later retry only after replacing the media the previous transaction
spent; an unchanged reserve cannot poll PMS and a full reserve eventually makes the frontier
terminal. There is no timer, bitrate margin or inferred capacity in either release. A seek/reload
carries the requested/observed pair together, so rebuilding the worker cannot promote the request
back into a delivered-quality claim.

The first segment of a *candidate transaction* is never discarded merely because it contains
setup. If it satisfies the conservation law, it decides the transaction immediately. If it is
complete, raster-valid, Pareto-improving and funded but `A > D`, it is instead a setup-bearing
boundary and may be followed by one ordinary observation. Both remain staged and spend the same
initial exploration clock. They are queued together only after the ordinary observation validates
the candidate; otherwise both are discarded. This keeps one media owner on the decoder timeline
and prevents a rejected encoder from advancing the active cursor by proxy.

A full player Load or seek is a different boundary: the carried rung and link estimate already
exist, while exactly one active-cursor object also contains one-off encoder/session setup. That
object is credited to the queues and exposed in telemetry, but it is not inserted into the
repeatable acquisition bag and cannot immediately demote the carried rung. The next completed
active object is the first steady observation. This is keyed to a structural session boundary,
not to a dwell timer or sample-count confidence rule.

Handing an existing playback back to Auto is likewise not a cold start. Let `F` be the feasible
source/device catalog, `W(r)` its calibrated wire demand, `r_c` the fixed rung currently being
replaced (when it belongs to `F`), and `C_p` the carried HLS posterior's conservative capacity.
Auto opens at

```text
r_p     = arg max { W(r) : r in F and W(r) <= C_p }       when a posterior exists
r_start = arg max W(r) over { ordinary unknown fallback, r_c, r_p }
```

The `r_c` term is continuity of the control point, not a claim that a demand-capped progressive
response measured spare link capacity. The posterior term is the controller's own completed HLS
evidence. Its values and observation instant are one synchronized snapshot; a pause, app
background or reload interval with no segment observations widens it before the new controller
consumes it. With neither, the ordinary unknown-link fallback remains. Consequently `fixed 4 Mbps
→ Auto` cannot first become `720 kbps`, and a previous settled Auto session may immediately
reclaim more; either route may still move down after the first repeatable post-Load acquisition
proves the current point unsustainable.

There is no dwell timer, stable-sample counter, bitrate headroom multiplier, fixed exploration
spacing or passive “optimal capacity” above a demand-capped response.

## 4. Failure frontier

Completed response evidence is per rung only when the response supplies an ordinal endpoint for
that actuator. A failure at one rung cannot erase stronger evidence retained for another, and a
completed blocked top rung cannot hide an eligible lower experiment. Deadline-censored and
completed no-gain underfill results are additionally global because neither response orders the
requested actuator set.

The retained facts are different:

- deadline-censored: store the largest exact common refill frontier `E_f`; run no quality
  excitation at that budget or less, and retain the failed actuator as an operational scheduling
  endpoint after a larger budget releases the hard block;
- completed but unfunded: store the largest actually executed `E` for that actuator; its completed
  response size keeps lower ordinal experiments meaningful;
- completed response with no quality gain: store the same common refill frontier; disposable
  reserve strictly beyond it may buy another fresh PMS session at the highest informative request,
  while the demand-capped response rate is never promoted into an ordinal or capacity bound;
- completed unsustainable (`A > D`): more reserve cannot make the operating point sustainable, so
  buffer growth does not release it. The certificate also retains the live HLS distribution's
  recent estimate at failure; a later live distribution whose conservative bound is strictly
  above that old-regime estimate authorizes one new excitation;
- PMS refusal: structurally excludes that exact actuator for this controller;
- raster larger than a rung's bounding box: excludes that rung and smaller boxes, while a larger
  box remains eligible;
- origin/session/transport/parse failure: says nothing about the rung and creates no certificate.

Pause duration and wall time release none of these facts. The completed-unsustainable release is
new end-to-end service evidence, not time or reserve standing in for it. Until the two delivery
distributions are confidence-separated, the certificate carries across segments as the
piecewise-stationary fact it measured. A new playback/controller also discards the frontier.

Recovery is separate from this quality search. If an upshift has just displaced a lower actuator,
that actuator is direct rollback evidence and is tried first. Otherwise, once the active response
cannot replay its observed chronology, the smallest feasible HLS response minimizes worst-case
time-to-picture; walking down through adjacent quality rungs only multiplies transaction latency.
After the floor produces media, the ordinary ordinal exploration above restores as much quality as
the measured reserve can fund. If that recovery has held the native playback clock, its completed
media is allowed to resume the clock before any private upshift transaction starts; otherwise an
unrelated quality experiment adds its whole latency to the visible rebuffer. Downshifts remain
eligible because they are the recovery edge itself. This ordering uses no duration threshold or
bitrate margin.

## 5. Smooth rebuffer instead of freeze and catch-up

The demux worker can block in DNS, HTTP, FFmpeg probing or an AU queue. The main thread therefore
owns the runtime safety actuator.

The full stress boundary `R_s` is never compared with a partially spent buffer on every pump tick.
Doing that is dimensionally wrong: after a safe start, both the buffer and the remaining cost of
the in-flight acquisition fall with playhead time, whereas a static `R_s` does not. It caused the
measured Play→Pause chatter at the ladder floor.

An active candidate transaction still owns its explicitly reserved balance, but the two directions
use different physical clocks. Upward exploration spends `Δplayhead`, so a user Pause or a
naturally stopped native clock spends no playable reserve. Downshift media spends elapsed time
minus native-accepted user Pause: involuntary starvation and an internal HLS hold remain recovery
cost even while the playhead is stopped. Crossing a retrospective balance does not independently
stop a running clock.

The native clock, durable user hold, recovery epoch and automatic actuator owner are one
mutex-protected authority. They are orthogonal fields rather than independently polled atomics: at
most one of `QualityUp(token)`, `QualityDown(token)` or `Original(token)` owns an automatic
boundary. Blocking PMS, HTTP and native work necessarily runs outside that mutex, but carries its
monotone lease token and the accepted user-clock sequence through the result. Completion may only
take the explicit commit edge if both still own the compatible boundary; a complete
Pause→Resume is therefore not mistaken for the earlier identical-looking `Running` state.
Otherwise the result is retained evidence or discarded work.
Session reset never restarts the token sequence, so a late destructor from a retired worker cannot
release the next session's lease. Hot-path atomics are diagnostics/projections of this machine, not
a second transition authority.

User Pause remains an orthogonal event and can therefore win while an automatic request is in
flight. An active user hold prevents an Original result from committing; a safety downshift may
continue to fill queues under an internal rebuffer hold without clearing the viewer's intent. A
complete Pause→Resume interval is carried by a cumulative event sequence, so a worker blocked for
both edges ages its evidence once instead of observing the same surrounding `false` boolean.

No new private quality transaction starts while the user is paused. If Pause races an existing
read, the socket retains both its ordinary liveness deadline and a scheduled re-check of the owning
reserve clock. The re-check spends nothing while user Pause holds, but it is already armed if
Resume makes that clock advance during the blocked read. A user Pause changes the feed gate only
after Starfish accepts `Pause`, and an ordinary Resume does the same after `Play`. Resume during an
internal HLS runway hold is the deliberate exception: the feed gate opens without `Play` or an ACB
state change so recovery media can refill the queues while the native clock stays held; the
measured re-prime later owns `Play` and its ACB mirror. The native playback clock is not restarted
until that recovery transaction ends, so a candidate and the playhead cannot both spend the same
discretionary balance.
There is one terminal exception derived from the actuator set rather than from time. A downshift
candidate at the ladder floor has no robust rollback guarantee left to protect and no cheaper
response to buy when either the completed current bag has `B < R_o`, or the main thread has already
observed `B = 0`. Its media read therefore keeps the ordinary transport liveness bounds but no
longer inherits the reserve deadline; aborting it can only re-enter the same recovery transaction
and would make the measured exhausted state absorbing. The first condition is carried by the
controller's typed `ReservePolicy::TerminalFloor`, because waiting for the later exact-zero sample
was the measured abort/retry loop at a positive 84 ms remnant.

An incomplete ordinary response is right-censored and has no prefix-rate projection. This is not
just a conservative interpretation: PMS 1.43.4 can publish `Content-Length` from the current file
size while the requested HLS segment is still growing, return would-block at the apparent body EOF,
and resume the same HTTP response when the encoder produces more bytes. Prefix time therefore
mixes server production, pacing and network service; neither it nor the advertised remainder
identifies completion time. For an ordinary active response, the worker still acts only at the
coefficient-free physical boundary `B = 0`, observed either by the main thread's internal clock
hold or by actual playhead consumption of the fetch-start reserve. The earlier `B < R_o` exception
applies only to the floor candidate selected by that completed-bag proof; it is not a prefix-rate
projection. At the ladder floor the only useful response continues; above it the still-incomplete
larger object may be abandoned so recovery can fetch a smaller one. An already held recovery clock
arms no second abort loop. In every case network, demux and feeding remain active.

Runtime resume does not use the historical `R_s` at all. A hold starts a fresh ordered recovery
epoch. For complete active-rung acquisitions after that pause,

```text
P_0 = 0
H   = max_i(P_(i-1) + A_i)
P_i = P_(i-1) + A_i - D_i
```

`P` is the acquisition debt in the order that actually occurred and `H` is that epoch's exact
largest prefix cost. The epoch is one mutex-protected publication, so the main thread cannot see a
new completed segment beside an old debt. Candidate media is not credited as recovery evidence;
after a rung commit the epoch restarts and the next ordinary acquisition describes the encoder
that will sustain playback.

Resume requires all of:

- Starfish accepted its ordinary decoder prime;
- at least one complete segment landed in the current recovery epoch;
- `P <= 0` — the epoch has repaid all acquisition time with at least as much media;
- the whole already-playable pipeline (Starfish plus demuxed AU queues) covers `H` on every
  expected A/V lane;
- the user transport is not paused;
- Starfish accepted `Play`.

The epoch is discarded after successful `Play`; old slow acquisitions cannot raise a later
runtime floor. `R_s` remains only a retrospective fresh-Load/seek certificate and ABR diagnostic. A
user Pause/Play cannot bypass an active internal hold. Failed `Pause`/`Play` calls do not mutate
state as though the clock moved.

HTTP bodies with a declared `Content-Length` count as completed only after all declared bytes
arrive. A short body cannot be credited with the segment's full `EXTINF`, which previously allowed
the model to resume on media that did not exist.

## 6. Original is a mode, not the top HLS rung

An HLS rung swap can continue on the same normalized media timeline and is intended to be invisible.
Moving between HLS and Original is different:

- PMS creates or tears down a different stream;
- the native pipeline is re-Loaded;
- the video plane may be black while it rebinds;
- codec and HDR declarations may change;
- Original can restore Dolby Vision, Atmos/lossless audio and avoid generation loss;
- Original has zero recurring PMS video-encode work; every HLS rendition has some.

The mode comparison therefore scores the real Original candidate against the best currently
supportable HLS alternative, not against a fabricated “top rung.”

Let

```text
s = min(remaining_playback / benefit_horizon, 1)
C(m) = 0, if m is the current mode; otherwise visible_transition_cost

U_original = s * (quality_original + source_features - playback_risk_original)
             - C(Original)

U_hls      = s * (quality_hls - playback_risk_hls - recurring_server_cost)
             - C(HLS)
```

Only the transition that would actually occur pays the last term. All recurring terms scale with
remaining playback; the reload cost is paid once, now. Consequently a return to Original can be
worthwhile near the start of a film and naturally lose near the credits without a hard
“do not switch after N minutes” rule.

The transition cost also grows with recent visible switches and decays with elapsed wall time, so
two rapid Original↔HLS reloads are priced more heavily than one old transition.

Both mode directions currently pay the same base reload cost plus that history penalty. The extra
uncertainty of returning to a source that previously failed is not hidden in another coefficient:
HLS→Original additionally requires a completed source request. HLS rung changes pay no mode cost.

### What is measured and what is a product choice

Measured/structural inputs include:

- actual source bitrate and raster;
- source DV and immersive/lossless-audio flags;
- completed source-probe evidence;
- current end-to-end HLS acquisition cadence;
- current reserve and remaining playback;
- whether Original is technically feasible;
- the HLS candidate's calibrated PMS-work class.

Explicit product choices include:

- the subjective cost of a visible reload/black frame;
- the relative value ordering of DV, no generation loss and Atmos;
- the time horizon over which recurring benefits fully repay a reload;
- the relative price assigned to ongoing PMS encode work.

Those values cannot be derived from network probability and are intentionally named as policy.

DV and Atmos/lossless audio are the source feature flags currently scored explicitly. A generic
HDR declaration change is part of the reload/feasibility boundary, but it has no separate utility
bonus today.

The measured HLS acquisition ratio spans open, PMS wait, pacing and path transfer. It remains useful
telemetry, but those components are not separately identifiable and the ratio is not charged as a
second feasibility or mode gate. The rendition's calibrated PMS-work class prices recurring server
work in the Original/HLS utility comparison. That class is a product calibration, not a live
reading of the server's current load; the implementation must not describe either quantity as one.

## 7. Leaving and returning to Original

Runtime Original observes completed source windows and the playable-buffer derivative. A
whole-file average cannot describe a VBR scene, so a rate deficit alone does not trigger a reload.

There are three exits:

- emergency low buffer: reserve is very low and measurably falling;
- imminent starvation: the observed reserve runway cannot afford further confirmation;
- sustained deficit: a measured drain persists and the mode utility agrees that HLS is worth the
  visible switch.

If the remaining film is already buffered, no exit is possible.

Returning from HLS requires an actual bounded source request. HLS traffic cannot prove source
capacity because its request sizes are capped. PMS 1.43.4 resolves a raw Part by exact Streaming
Resource identity, then token alias, and only then enters AdHoc admission. The old implementation
first stopped/closed HLS and used a fresh `source-N` id; that forced AdHoc admission, whose
`99 341 > 92 000` bandwidth refusal fell through a PMS lexical-cast bug and surfaced as HTTP 500.
The current experiment instead names the exact active HLS encoder and issues no client-side stop,
close or replacement before the read. It runs between ordinary HLS acquisitions, and its result is
discarded if the active identity changes while the request is in flight. That ordering keeps the
client route explicit, but it is not evidence that PMS preserves the old HLS cursor: PMS may rebind
the shared Streaming Resource while serving the raw Part. A successful `Recover` therefore starts
an Original trial on that same completed media boundary and never waits for one more HLS segment.
HLS remains the rollback owner until the exact Load succeeds and a decoded frame confirms the
handoff. After an unsuccessful probe, continuity is established only by the next actual HLS
response; it is not assumed from the absence of a client-side route change.

The probe and the subsequent source publication hold one `Original(token)` automatic-actuator
lease. The finite network request may finish after a user/native transition, but its result cannot
cross the commit edge unless the synchronized clock authority still admits that token. Requesting
the replacement also does not spend anti-flap history: an automatic visible-switch charge travels
with `PendingOriginal` and is committed by the first decoded frame. A failed Load rolls both the
route and that unspent charge back, because the viewer never saw the proposed mode.

The experiment becomes informative when no higher HLS actuator remains unclassified at the current
exploration budget. That does not require the live actuator to equal the largest request: if PMS
maps larger requests to the same or a worse encode, their structural rejections exhaust the HLS
frontier without ever becoming current.

The source body itself still has two separately bounded phases. Connection setup
(DNS/TLS/headers) gets `P_setup`; only after headers arrive does the finite body — one second of
source media within the transport-size clamps — get its derived `P_body`. The body interval is the
throughput evidence; setup is a one-off reload cost and cannot truncate it. The shared
`SourceProbePlan` gives both phases the same derived bound `P`; the exact physical admission floor is

```text
B >= P_setup + P_body + max(R_s,D_next) = 2P + max(R_s,D_next)
```

There is no client-side stop, close, decision or HLS restart in that path. If HLS is retained, the
balance funds a sustainable next HLS acquisition and restores the replay boundary when that
acquisition credits its media. A successful `Recover` instead starts the Original trial immediately
and consumes no later HLS request; the handoff commits only after the exact Load succeeds and a
decoded frame confirms it. A completed source response at or above source consumption is a lower
bound sufficient for the mode comparison; a truncated body probe is absence of evidence.

Cold Remote preflight uses the durable logical playback id and does not close it, so the selected
initial Original or HLS route can exact-reuse the same resource. Runtime direct recovery binds the
actual Part URL to the active HLS id measured by the probe. Decoded source frames stop only the
physical HLS encoder (`closeResourceSession=0`); its Streaming Resource remains the direct stream's
owner so later Range/seek opens remain valid. Final playback teardown performs the full resource
close. A remux recovery instead registers a distinct replacement encoder; decoded frames close the
held old HLS resource, while a failed remux open restores the client-side HLS route and closes only
the unproven remux. The first actual HLS response after that restoration decides whether PMS kept
or can resume its cursor.
If a cold resolve is cancelled, superseded or refused before it owns an Engine, there is no winning
route to reuse its logical resource; that abandonment path therefore performs the exact full close.

A source request which fails before any body arrives (for example PMS `5xx`, DNS or connect
failure) is an inconclusive request failure, not a zero-rate link sample and not proof that the
Part is unavailable to a later playback open. It does not enter the source capacity estimate and
HLS retains its best observed/requested route.

Neither a failed nor an insufficient experiment is polled alongside every HLS segment. At the
experiment the gate records the greater of the source result and HLS's recent estimate. It rearms
only after live HLS's conservative bound rises strictly above that record. Thus wall-clock time and
an unchanged demand-capped stream cannot cause another request; a confidence-separated link-regime
change can. The probe itself is a finite `Range: bytes=0-(N-1)` request and is accepted only with a
matching `206 Content-Range`, so completing the measurement also completes the HTTP response.

A completed terminal comparison follows the HLS actuator that supplied its counterfactual. An
upward HLS commit retains the exact source lower bound but invalidates the comparison; the first
ordinary object from the new live encoder re-scores it without another source request. A downward
commit is evidence that the prior service regime failed to sustain its operating point, so the old
source lower bound is retired and the fully funded source gate returns to `Fresh`. It may authorize
a new bounded request after HLS restores continuity; the earlier fast result is never projected
across that collapse.

## 8. Instrumentation and verification

Relevant log lines:

```text
abr: sample ... media=... net=... buf=... prod=... decision=... target=... complete=...
abr: window ... have=.../... demand=... supply=... excess=... runway=... sus=... sur=... reset=...
abr: exploration target=... buf=... runway=... budget=...
abr: committed Up|Down to ...
abr: tx Up|Down ... outcome=... decided=... control=... media=... \
    candidate_acq=... candidate_bytes=... candidate_dur=...
hls: auto-rebuffer pause buf=... trial_reserve=... runway=...
primed: v=... a=... runway=... -> Play
primed: v=... a=... recovery_n=... debt=... runway=... -> Play
abr: mode chose=... why=... scale=... win[...] lose[...]
abr: Original probe ... complete=... left=... verdict=...
```

`tools/abr-window-grade.py` independently reconstructs each attributable part of the finite episode
from these lines. A commit is accepted only when the matching `abr: committed` marker and following
transaction agree; rejected candidates never seed the episode. A delivery-collapse reset has no
separate marker, so the reset counter makes the grader flag that discontinuity and leave subsequent
rows ungraded until a marked seed or commit restores attribution; it never invents the missing
episode boundary. Integer-millisecond telemetry is replayed as its exact quantisation interval, and
threshold-straddling `sus`/`sur` checks are reported as ambiguous coverage rather than silently
counted as proof.

The on-screen diagnostics likewise keep request and output separate: `Quality` shows the requested
ceiling, PMS master declaration and decoded raster; the measured media rate is the HLS demand used
in the conservative-budget row. A requested `22 Mbps / 4K` box can therefore never masquerade as
a decoded `720×404` picture.

Host tests prove integer conservation, deadline propagation, failure-frontier ordering, short-body
rejection and state-machine invariants. The host simulator can exercise transport, demux, AU queues
and the synthetic clock sink. It cannot prove LG decoder behaviour, video-plane reload visibility,
HDR transitions, real frame pacing or the native Starfish buffer cap. Those require the television.
