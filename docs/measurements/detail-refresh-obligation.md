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

## Atomic visible start follow-up (2026-09-13)

Review found a second ordering hole in the first fix: `refresh_content` queued
`DetailRestore(Requested)` before the `AppFx::Store(RequestDetail)` that started its request. The
dispatcher could therefore expose Requested while the addressed metadata status still described
the previous settled request. A queued Tick or StoreChanged could consume the obligation before
the replacement existed.

`visible_refresh_starts_before_queued_tick_or_store_change_can_consume_it` starts with cached A and
an addressed `Some(false)` status, queues the production DetailRestore with Tick and StoreChanged
already waiting, and checks the next dispatcher effect boundary. Against `c86acb9e` it failed:

```text
assertion `left == right` failed: Requested must not be visible before its reconciliation request exists
  left: Some(false)
 right: Some(true)
test result: FAILED. 0 passed; 1 failed; 0 ignored
```

The visible DetailRestore handler now calls the existing synchronous Metadata compatibility
command first and stores Requested second. Covered pages still store Deferred; Enter uses the same
start-and-arm operation. The queued AppFx copy was removed because RequestDetail is
non-idempotent. Recording/replay observability remains at the actual resource boundary:
`bootstrap::stores::admit` records and replays the one request identity and spawn answer. The new
dispatcher regression runs that controlled admission path and asserts exactly one resource request.
It then performs the prior Related-B supersession sequence, rejects stale A/B landings, retries A,
terminates the obligation, and proves no duplicate request. The logical state shape and screen pin
do not change in this follow-up.

Follow-up verification completed:

- Focused content dispatcher tests: 10 passed; Detail-related tests: 156 passed.
- Full `make check`: 3,225 default-feature and 3,257 hostsim tests passed, with one existing
  ignored test in each configuration; the 305-case harness and all other host gates passed.
- Shipping `--no-default-features` check and `make FLAVOR=debug` ARM cross-build passed.
- No device, stable install, release, or push.
