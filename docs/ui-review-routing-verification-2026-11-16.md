# Typed routing verification receipt — 2026-11-16

Uncommitted isolated checkout `ui-review-main-20261116`, base
`990d0439444c7e8180fe6437e95fdcd9136daae9`. This receipt supersedes no earlier source hash;
`4b52d6ce...` describes the previous frame-only checkpoint, not this changed source.

Source SHA-256 after `EffectDisposition` rename: `6ac95351369f3f70fd57c31eb8aa46be8dbf78bda2c25ab14379964ca5e5bb4e`.
2555 paths: all tracked plus nonignored untracked paths, excluding this receipt. Sorted UTF-8 path,
NUL, binary SHA-256 of file bytes (symlink target bytes for links), matching the earlier construction.
Full tracked/untracked Rust/Python/Markdown scan: no CRLF.

## Executed

- Final application: `test result: ok. 1422 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 52.01s`.
- UI: `test result: ok. 1637 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.12s`.
  Includes new-screen generic store/navigation mounting and existing FIFO/record/replay tests.
- Screens: `test result: ok. 2175 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 57.77s`.
- Actual `plx_screens` compiler-negative doctests: `test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.76s`.
- Application FIFO/bookmark/budget/retirement/observer filter: 17 passed, 0 failed before passive
  observation extraction and the final test-fixture adjustments; not relabeled as final full-app proof.
- Boundary controls: `Ran 23 tests ... OK`; production `ci/ui_boundaries.py` exit 0.
- Typed FIFO scanner: `Ran 15 tests ... OK`; production `ci/check-app-command-routing.py` exit 0.
  Tests retain no-execution, double-take, missing arm, wrong partition and side-queue refusals.
- `git diff --check` exit 0. Changed tracked Rust/Python/Markdown scan: no CRLF.

- Renamed-source structural/Sans-IO gate: `check-deps: all gates green`, exit 0.
- Renamed-source shipping features: `make check-nodef` exit 0, 53.62s.
- Application, screens and UI numbers above are all final renamed-source reruns.
- Final application hostsim: `test result: ok. 1640 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 60.97s`.
  Job-334 completed exit 0; WSL log `/tmp/ui-routing-hostsim-final.log`. This is the application
  hostsim suite, not every workspace crate's hostsim suite. The frozen source identity above is
  unchanged; this receipt is excluded from that digest.

## Observed failures and repairs

- Initial typed contract compile RED: missing route contract/types.
- Typed command-storage RED: new external enum could escape the old accumulator scan.
- First app run: 1420 passed / 2 failed because migrated tests entered native font layout.
  Tests now drive production drain/admission/resume directly, avoiding unrelated native layout.
- First screens run: 2165 passed / 10 failed; fixture runners incorrectly acknowledged retired
  sender envelopes as Completed. They now retain the envelope's terminal error; final full screens green.
- Second app run: 1421 passed / 1 failed; a controlled ItemMenu's correct FIFO refusal receipt
  contradicted the former direct-inline test expectation. The test now requires one Refused receipt
  and retains all no-write/no-jobs checks. Final full application rerun is green above.

## Environment and limits

WSL `env -i`, HOME `/home/glinnik`, explicit Python 3.12.16 shim and the repository-pinned toolchain at the tested revision;
no inherited Git overrides. Existing per-checkout `target-fast` / `target-hostsim` / shipping target.
No commit, push, device operation or parent-checkout source edit. Whole `make check` remains deferred.
The initial dependency gate failed on the repository's obsolete page `Route::` substring pin and its
old classifier/unreachable proof. The neutral enum is now `EffectDisposition`; no page-route gate
exemption was added. The scanner now checks typed consuming conversion/executors, retaining the sole
FIFO take, real execution and no-family-accumulator checks. Its 15 negative/live-source controls pass.
The first digest attempt timed out at 10 seconds; the completed 120-second-budget retry produced the
source identity above. Dependency/shipping/hostsim verification on renamed source is in the handoff.

See [routing checkpoint](ui-review-routing-checkpoint-2026-11-16.md) for exact ownership limits:
actual App-child RuntimeOwners decomposition, immutable navigation projection and blanket removal
of external mutable logical screen access are still unfinished. Modal/hold-hint state is untouched.
