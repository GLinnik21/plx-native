# UI review verification receipt — 2026-11-16

Historical receipt for the explicitly frozen `ae82` source below, before later frame/routing/owned-policy repairs.
Base HEAD: `990d0439444c7e8180fe6437e95fdcd9136daae9`; isolated, uncommitted source repairs.
See [review dispositions and historical evidence](ui-review-evidence.md) and the
[current joined owned-policy receipt](ui-review-modal-verification-2026-11-16.md). These old counts are not substituted for current gates.

## Exact source and environment

The final gate uses the repository-pinned toolchain at the tested revision, existing Python `3.12.16`, WSL Ubuntu 22.04,
and an `env -i` environment without inherited `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE`
or `GIT_INDEX_FILE`. No guard is disabled. The isolated worktree's `.git` pointer is a
portable relative path so child tests can create genuinely independent repositories.
Tracked UTF-8 text was canonicalized to LF; committed symlink placeholders were materialized
as their exact relative symlinks. Assertions and historical fixture hashes were not rewritten.

Frozen source SHA-256: `ae82d3f9e5f006531e1950489c4e5e889809035d9ad6b288b6f313397a093533`.
The digest covers 2547 paths: every tracked file plus `docs/ui-review-evidence.md`.
For each sorted relative path, SHA-256 receives UTF-8 path, NUL, and the binary SHA-256
of its file bytes (or symlink target bytes). Generated build outputs and this receipt are excluded.
The latter is generated evidence, allowing observed results to be recorded without modifying tested inputs.

Frozen recovered index SHA-256: `930d017b999eeaa9ee1ad7d6c915b5a5e5ac08ca28234962e2dc7b6316edc159`;
cached diff was empty. The single frozen `make check` exited **0** after **1091.32 seconds**.
Before/after source digest, HEAD, raw recovered-index digest and empty cached-diff digest were
identical. These results apply only to the isolated patch: it is not applied to the parent checkout
or current published main. Only this generated evidence receipt changed after the tested-source freeze.

## Executed focused checks

| Command / boundary | Observed outcome |
|---|---|
| `make test-crate C='app ui appkit screens'` | PASS: app 1421, UI 1636, appkit 955, screens 2174; zero failures. This preceded the final test-fixture corrections; the frozen gate must recheck the final source. |
| `make check-nodef` | PASS on the exact frozen source, including shipping playback backends; final run completed in 4m00s including the build-lock wait. |
| `make lint` | PASS with the repository's three explicitly named shadowed-branch lints, not blanket clippy. The frozen gate repeats it. |
| Actual AppHost owner-event record/replay | RED reproduced `unsupported Home message`; after narrow pure-message codec repair, recording and both targets/resolve modes pass. Native player commands and compiled-resource replay still refuse, covered by negative tests. |
| Actual AppHost continuation | PASS in the full 1637-pass hostsim run: exact remaining POST prefix, FIFO order, carried tail, stable frame index and no second navigation commit. |
| Actual AppHost setup-before-input | PASS after fixing the fixture to publish real captured duration, install chapter metadata, settle the HUD and enter the actual product-turn protocol. The unchanged expectation requires queued tab-1 setup followed by Left to end on tab 0. |
| Outbound replay-fixture boundaries | PASS: two focused tests after removing leaked Git environment overrides; no publication command executes. |
| `make check-python-rest` | Initially 135/136 steps PASS; only checkout symlink materialization failed. The exact committed links were restored; all 18 `ci/test_agent_files.py` tests then PASS. Generated/read-only certificates and strict source gates passed without pin changes. |
| Frozen `make check` | PASS, exit 0, 1091.32s: all 18 default-feature and 11 hostsim test binaries passed; all 136 Python-rest steps passed; harness ran 745 tests successfully with 4 existing skips. Named lint and lab/placeholder feature checks passed. |
| Final actual app/UI/player/screens counts | App default 1422 and hostsim 1638; UI 1636; appkit (including HUD/player) 955; screens 2174; zero failures. Existing ignored tests in other crates remain reported, not converted into passes (default media 104, telemetry 1, net 1, graphics 14; hostsim media 104, telemetry 1, graphics 14). |

## Local Git isolation incident and recovery

The intended no-commit boundary was violated unintentionally by repository-fixture tests:
explicit exported Git overrides inherited from earlier verification commands leaked into temporary
`git init`/`git commit` operations. They created local scratch/seed commits in the isolated worktree,
changing its detached HEAD to `fc6da1f7c85ddd0875c5cbb4c154497135dd74e9` and staging current files.
This was not a source repair commit or published history, but it is a real metadata incident.
The original raw index identity was lost; it must not be reported unchanged.

Recovery was explicitly authorized for this isolated task metadata only. Before recovery, HEAD,
reflog, index and 2548 source-byte hashes were backed up, and the scratch commit was retained under
`refs/empryo/recovery/bg341-metadata-backup-1794797529`. Scope, detached HEAD and the base ancestor
were asserted. A compare-and-swap restored HEAD to 990d, and `git read-tree 990d` restored the index
without reset, checkout or source writes. All 2548 source hashes were identical before/after.
The final frozen run starts from this **new recovered metadata baseline**, not a fictional claim
that initial raw metadata survived. Parent working-tree source, origin and published history were
not edited; no push or device operation occurred.

## Acceptance limits

Host tests are not current TV, ARM, simulator visual, native idle or panel-on performance evidence.
The modal/hold-hint ownership and top-level App/platform limits here describe this receipt's older
source, not the later bounded repairs and current gate identities listed in the review matrix. The synthetic player owner-event tape is not full native playback replay.
