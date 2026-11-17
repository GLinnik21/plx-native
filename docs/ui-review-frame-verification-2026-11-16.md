# Frame ownership verification receipt — 2026-11-16

Isolated checkout: `../ui-review-main-20261116`; uncommitted base
`990d0439444c7e8180fe6437e95fdcd9136daae9`. Earlier verified player/outcome/codec repairs retained.

## Source and environment

Final source SHA-256: `4b52d6ce17ac193f2747d349ed77d6573b7a96743b88d152bca09e2a82f8ce64`.
2548 paths: every tracked path plus `docs/ui-review-evidence.md` and
`docs/ui-review-frame-protocol-2026-11-16.md`. Hash construction matches the earlier receipt:
sorted relative UTF-8 path, NUL, binary SHA-256 of file bytes (symlink target bytes for links).
This generated receipt and build/log outputs are excluded. Changed Rust, Python and Markdown
sources are LF; the full tracked-text scan found no CRLF in those extensions.

Rust: repository-pinned toolchain at the tested revision (`0ef1bc386 2026-11-09`); Python: `3.12.16`. WSL commands use
`env -i`, explicit HOME/PATH/toolchain and no inherited `GIT_*` overrides. The existing Python
3.12 shim is `/tmp/plx-pr1133-python`. Earlier failed command 319 printed shell-default Rust
1.98.1 before make, but make's compiler explicitly uses the repository-pinned toolchain at the tested revision; subsequent commands
pin `RUSTUP_TOOLCHAIN` to that channel as well. One early focused guard run used shell-default Python
3.10.4; all final Python checks used the actual 3.12.16 executable.

HEAD stayed at 990d. Final recovered index SHA-256:
`7f9b1d16d122bbe73af553950b57930d033fe55135fb4bbbd08f1a7528bfe3d4`.
The checkpoint's before/after index identities match; this does not claim the earlier receipt's
raw index bytes survived ordinary Git index refreshes. Cached diff is empty (SHA-256 of empty
bytes `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`).

## Executed evidence

| Check | Observed result |
|---|---|
| Old product ownership guard | RED: `product loop still sequences native phases outside the dispatcher owner`; 0 passed, 1 failed. This is a production-loop source regression, not an old-runtime callback trace. |
| Actual AppHost product callback trace | PASS: native pump/results/production step/publication with deterministic SDL and substituted GPU ports; setup+Left, same-frame resource arrival, remaining POST prefix/carry, no second commit, blocked and incomplete-swap paths. |
| Full final app hostsim | `test result: ok. 1640 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 70.48s` |
| Final default app | `test result: ok. 1422 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 53.38s` |
| Final UI | `test result: ok. 1636 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.97s` |
| Boundary guard/negative controls | `Ran 22 tests ... OK`; production token guard exit 0. Negative controls cover escaped legacy/new ports, duplicated/reordered callbacks, bypassed admission and swap completion, native ack, comment/test-only decoys. |
| Structural/Sans-IO freeze | `check-deps: all gates green`, exit 0. Initial foreground run hit the tool's 120s timeout; complete background rerun passed. No ledgers/caps/layer exceptions changed. |
| Shipping feature set | Final `make check-nodef` exit 0, 31.01s; real `play-uri,play-feed` feature set. |
| Repository named lint | Final `make lint` exit 0, 37.96s; only the repository's explicit three shadowed-branch lints. Existing `plx_platform` manifest unused-dependency warnings remain. No blanket lint-policy change. |
| Whitespace | `git diff --check` exit 0. |

The initial full hostsim run was `1639 passed; 1 failed`: the adjacent `shot.rs` source guard
still searched for the removed `prepare_window` function. It now verifies collected changes,
screenshot wake and dispatcher-owned admission, with missing/reversed-publication negative controls.
The complete 1640-test rerun above is green. This was not a screenshot behavior failure.

## Handoff limits

No final whole `make check` was run: explicitly deferred until the remaining review nodes finish.
The earlier 317/full-check source receipt is not relabeled as proof for this changed tree.
No TV, native GPU visual/performance, ARM link or device verification is claimed. No commit,
push, merge, publication, parent-checkout source edit or device operation occurred.

The frame architecture/phase table and separate Bridge/AppFx, modal/TLS and new-screen/type work
are in `docs/ui-review-frame-protocol-2026-11-16.md`. Final focused compilation/tests are green;
the next review node can start from this coherent uncommitted checkpoint.
