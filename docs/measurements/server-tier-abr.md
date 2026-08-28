# The ABR path against a REAL PMS — the tier every other measurement here lacked

**Device + real Plex Media Server, 2026-08-28. `./tests/run.py --server`: 27 passed, 0 failed.**

Every other ABR measurement in this repository — including all of this session's — was taken
against `tests/serve_fixtures.py`, a static file server. The board's meta-finding lists that as a
reason the corpus cannot identify a controller: *"static file server | zero PMS JIT, control plane
6 ms; neither is true in deployment."* This is the first ABR characterisation with a real
just-in-time encoder in the loop.

Six Auto cases exercise the controller here: `auto_baseline`, `auto_link_squeeze`,
`auto_original_squeeze`, `auto_pin_and_back`, `auto_hls_pin_and_back`, `original_then_auto`.

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

`candidate_warmup_budget(Up, ..)` is `3/2 * media_duration` — 3000 ms at this pipeline's 2 s
segments, and a derived quantity rather than an invented one. Against a real PMS it is **marginal**:

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

* It is an **UPSHIFT**, so this session's abort rule correctly does not arm — and the reason it
  should not is visible on the same line: the reserve fell 46335 -> 42502, i.e. **3833 ms across a
  3877 ms transaction**, exactly real time. The picture kept playing throughout, which is the
  premise `candidate_warmup_is_guarded` turns on. Aborting early would have bought nothing.
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
* This says nothing about `tools/netcond.py`'s failure modes — stall, blackhole, reject, shaped
  rate against a REAL server — which remain unrun.
