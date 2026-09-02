# The ABR path against a REAL PMS — the tier every other measurement here lacked

**Device + real Plex Media Server, 2026-08-28. `./tests/run.py --server`: 27 passed, 0 failed.**

Every other ABR measurement in this repository — including all of this session's — was taken
against `tests/serve_fixtures.py`, a static file server. The board's meta-finding lists that as a
reason the corpus cannot identify a controller: *"static file server | zero PMS JIT, control plane
6 ms; neither is true in deployment."* This is the first ABR characterisation with a real
just-in-time encoder in the loop.

Six Auto cases exercise the controller here: `auto_baseline`, `auto_link_squeeze`,
`auto_original_squeeze`, `auto_pin_and_back`, `auto_hls_pin_and_back`, `original_then_auto`.

## CORRECTION — netcond was in the path, and I did not know it when I wrote this

The first version of this document said the server tier ran "against a real PMS" and contrasted it
with the synthetic shaper as though no conditioning were involved. **`tests/run.py` OWNS a
`tools/netcond.py` for the whole `--server` run and steers it per case** (`LinkConditioner`), and it
conditions the link whenever the deployed binary has `PMS_PORT` pointing at the proxy — which this
build does (32499). So every byte below traversed the proxy, and the run started and stopped it
without my noticing: the port was closed again by the time I went looking, which is what made the
log's `<ip>:32499` look inexplicable.

**This also answers the question that prompted the run in the affirmative on both halves.**
`auto_link_squeeze` carries `link_profile: [pass @0s, rate:2500 @50s, pass @105s]` — a real
2.5 Mbit/s squeeze applied to a real PMS mid-playback — and it PASSES. That is the netcond
experiment, and it is part of the default server tier rather than something to be set up by hand.

**What it costs the numbers below**, stated rather than buried: 6 of the 7 transactions come from
`auto_link_squeeze` and were measured while the link was shaped or recovering from it; only one
(`auto_hls_pin_and_back`) is unshaped.

| case | shaping | `control=` | `warmup=` |
|---|---|---|---|
| `auto_hls_pin_and_back` | pass only | 190 | 1420 |
| `auto_link_squeeze` | rate:2500 | 120, 123, 128, 133, 159, 304 | 1155, 1415, 1475, 1894, 2248 |

The single unshaped sample has a HIGHER control cost than the shaped median and a mid-range
warm-up, so the squeeze does not appear to dominate either quantity — but that is one sample, and
the honest reading of the table below is **"real PMS, through a pass-mode proxy, mostly under a
2.5 Mbit/s squeeze"** rather than "real PMS". The 22x control-plane gap is far larger than any
plausible relay overhead and survives; the warm-up comparison is the one to distrust.

## What the synthetic tier gets right, and what it does not

| quantity | real PMS | synthetic | ratio |
|---|---|---|---|
| `control=` p50 | **133 ms** (n=7, 120–304) | 6 ms (n=77, 4–87) | **22x** |
| `warmup=` p50 | **1475 ms** (n=6, 1155–2248) | 616 ms (n=76, 24–2161) | 2.4x |
| `prod=` p50 | 203 pm (n=180) | 193 pm (n=1170) | 1.05x |
| `prod=` min | **57 pm** | 9 pm | a JIT encoder is never free |
| `prod=` p90 | 467 pm | 795 pm | synthetic tail is WIDER |

**The production median transfers and the tails do not.** `prod=` agrees to 5% at the median, which
is better than the meta-finding implies and is worth knowing before dismissing a synthetic
production number. But the synthetic floor is 6x lower — a file read can be nearly free and a JIT
encode cannot — and the synthetic p90 is 1.7x HIGHER, so the fixture shaper's spread is wider than
a real server's in both directions. A conclusion drawn from the synthetic TAIL is not transferable.

**The control plane does not transfer at all.** 133 ms against 6 ms. R8 already said `control=` is
three requests rather than a near-zero-byte transfer; this is the deployment magnitude.

## The finding that has a decision behind it

At the time of this measurement, `candidate_warmup_budget(Up, ..)` was
`3/2 * media_duration` — 3000 ms at this pipeline's 2 s segments. The current controller instead
arms the exact disposable exploration reserve `E = max(B - max(R,D), 0)` end to end, so the budget
line below is historical evidence for why the fixed rule was retired, not a statement of current
policy. Against that real PMS the old budget was **marginal**:

```
warmup observed: 1155 … 2248 ms  (n=6 committed)
budget:          3000 ms
misses:          1 of 7 transactions
```

The one miss is instructive rather than alarming, and it is worth reading in full:

```
tx Up 2000->4000kbps outcome=warmup_deadline decided=3877ms total=3882ms
   control=133ms prime=11ms master=5ms media=117ms warmup_dl=3000ms
   buf_start=46335ms buf_decided=42502ms net=138716kbps
```

* It is an **UPSHIFT**. At the time of this measurement the candidate prefix-abort rule did not arm
  in that direction, and the reason that looked correct is visible on the same line: the reserve
  fell 46335 -> 42502, i.e. **3833 ms across a 3877 ms transaction**, exactly real time. The
  picture kept playing throughout, so aborting early would have bought nothing. The rule was later
  removed in both directions after a real-PMS body proved that prefix rate does not identify the
  unseen completion time; candidate transactions retain their absolute reserve deadline except
  for the already-stalled floor recovery, where no cheaper response exists.
* `net=138716kbps`. The LAN is not the constraint. The miss is **production latency**, which is the
  term the synthetic tier cannot produce at all.

**No change is made on this evidence.** n=7 transactions across six cases on one server and one
library is far too thin to re-size a budget, and re-sizing it to fit seven samples is the move this
plan forbids. What would settle it: the same tier run repeatedly, and on a server that is BUSY —
every measurement here, synthetic and real alike, has been against an idle PMS, which is still the
open item the production gate exists for.

## Scope

* One PMS, one library, one LAN, idle server. Titles and server identifiers are deliberately absent
  and no log from this run is committed.
* `--server` runs as GUEST by default and resets `viewOffset` per case via `/:/unscrobble`.
* `tools/netcond.py`'s **shaped rate** against a real server IS exercised here (`auto_link_squeeze`,
  `rate:2500`). Its other modes — `stall`, `blackhole`, `reject`, and scoped variants like
  `stall@/:/timeline` — are not reached by any case in this tier and remain unrun.
