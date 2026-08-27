# I3 — the first segment no longer forces a downshift

**Device session 2026-08-27.** LG 49SM9000PLA, webOS 4.10.0,
`com.beb.plxnative.debug`. Binary: `36b96679` (`abr: suppress first-segment false
downshifts`). Host gate at that SHA: 1 327 Rust tests + 152 harness tests green;
`cargo +nightly check --lib --no-default-features` green.

## The differential

The baseline recorded in `i1-abr-baseline.md` opened all four shaped cases by
downshifting on their first segment. In `pipe_abr_steady_modest_link` specifically:

```text
before: buf=2000ms current=2000kbps decision=prime_down target=720kbps
after:  buf=2000ms current=2000kbps decision=stay       target=0kbps
```

The after case ran for the full 75-second cap under the manifest's flat 6 000 kbps
network profile. It passed `abr_shape`, `stream_path`, `load_decl`, `codec`,
`video_bound`, `pos_climb`, `no_error`, and `server_wire`; 53 segments were observed
and none had an unreadable reserve. This is the device leg required by I3: the cold
sample still updates the estimators, but `B = D` by itself no longer changes the
actuator.

## A stale-artifact result that is not evidence

Two earlier launches in the same lease still printed `prime_down`. They are excluded:
the first used the ARM binary from before `36b96679`; the second rebuilt
`pkg/plxnative` but did not deploy it. `tests/run.py` launches the installed debug app
and does not replace its binary. The accepted run followed an explicit `make deploy`;
only that run exercised the commit named above.

