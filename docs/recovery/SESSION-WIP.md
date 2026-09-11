# In-progress Session owner recovery

This is an unverified working-source snapshot, NOT a completed refactor or release.
It overlays the named source paths from an immutable WIP commit onto
the audited integration recovery tree. No claim of an atomic application checkpoint,
a compiling tree, or completed physical Session ownership is made.

Worker base: `77fe6e8bbfaa6902b0f7e8b03a49ed3e6db35bcb` on `codex/p12-session-owner`.
In-progress source checkpoint: `fcfe1ae88226a48fda8c7a7a8c29814617e25c30`.
Audited integration parent: `5f8a41b5e9abd8a8b557ee00bd6c5e50e444463e`.

The worker still has production controller/mailbox migration, command/result and
screen/bootstrap wiring, single publication and full regression proof outstanding.
Owner init, QR transitions, worker observation sinks and adapter transport are partial.
No private configuration, recordings, binary artifacts or old working history included.
The live worker files and index were not changed by this capture.

## Captured source paths

- `rust-modules/src/app/adapters/mod.rs`
- `rust-modules/src/app/adapters/session.rs`
- `rust-modules/src/app/boot.rs`
- `rust-modules/src/app/bridge.rs`
- `rust-modules/src/app/library_shelf_action_tests.rs`
- `rust-modules/src/app/search_owned_tests.rs`
- `rust-modules/src/auth.rs`
- `rust-modules/src/auth/observation.rs`
- `rust-modules/src/auth/owner.rs`
- `rust-modules/src/screens/registry.rs`

## Approved next implementation step

[Session deferral contract](session-deferral-contract.md) records the parent decision for lossless bounded result ordering. It is a requirement, not a claim that this checkpoint implements it.
