# Detail reconciliation after directional navigation (2026-09-13)

Base: `8dc55b14b9bab010bd123b7be41bb528f690fdf0`.

The production dispatcher regression
`app::content::library_publication_tests::directional_related_navigation_preserves_the_refresh_obligation_after_back`
was added and observed failing before production code changed. A has cached metadata and a
Requested reconciliation. DOWN navigates from its episode text to Related B, cancelling focus
restoration; OK down/up activates B through the press dispatcher. The emitted push opens B,
which supersedes A's request. BACK emits the return request before B lands.

Command used for the historical RED:

```sh
CARGO_INCREMENTAL=0 cargo +nightly test --manifest-path rust-modules/Cargo.toml --lib \
  directional_related_navigation_preserves_the_refresh_obligation_after_back -- --nocapture
```

Observed failure (not simulated):

```text
assertion `left == right` failed: Back starts one replacement for the requested reconciliation B superseded
  left: 5
 right: 6
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3220 filtered out
```

`DetailScreen.refresh` now owns the server obligation independently of `restore_intent`.
It is always serialized by `LogicalState`, including when user input has cleared restoration.
The screen schema pin changes from `824916e7058b2ada` to `3c0b74a77a48169c`.

| Obligation on Enter | Addressed request status | Result |
|---|---|---|
| Deferred | Any | Supersede the pre-write request; become Requested |
| Requested | None | Retry once |
| Requested | Some(true) | Wait for the current request |
| Requested | Some(false) | Consume completion without retry |
| None | Any | Ordinary fetch only for missing detail with no request in flight |

The expanded regression rejects old A, pre-write A, and B landings, asserts no duplicate retry,
then moves RIGHT to Related C before A's reconciliation settles. Focus remains on C
and the obligation terminates with no restore intent. The prior regression retains its original
episode-restoration and terminal-failure path. Additional screen tests cover the full visibility
truth table with and without cached metadata, memory precedence, independent hashing, and both
successful and failed reconciliation after directional cancellation.

The comment/prose audit updated Detail's ownership comments, the registry's schema explanation,
and the UI reference's return-navigation description. Searches for restore intent and
Detail refresh/reconciliation claims found no other contradictions caused by this change.

Verification completed:

- Focused Detail tests: 156 passed; content dispatcher tests: 9 passed; schema pin: passed.
- Full `make check`: 3,224 default-feature and 3,256 hostsim tests passed, with one existing
  ignored test in each configuration; lint, additional feature checks, and all host self-tests passed.
- `CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path rust-modules/Cargo.toml --lib --no-default-features`: passed.
- `make FLAVOR=debug`: ARM cross-build passed. No device, stable install, release, or push.

The first shipping check overlapped the harness's temporary negative source fixtures and failed
on the injected Browse declaration. After the full harness restored the source and passed,
the shipping check passed sequentially. The RED above predates implementation and is unrelated
to that verification overlap.
