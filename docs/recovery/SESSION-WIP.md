# In-progress Session owner recovery

This is an unverified working-source snapshot, NOT a completed refactor or release.
It overlays the named source paths from an immutable WIP commit onto
the audited integration recovery tree. No claim of an atomic application checkpoint,
a compiling tree, or completed physical Session ownership is made.

Worker base: `77fe6e8bbfaa6902b0f7e8b03a49ed3e6db35bcb` on `codex/p12-session-owner`.
In-progress source checkpoint: `c4f10510580352e0dbe2a5a9e2edc21bee7c0467`.
Audited integration parent: `9733e44dff4db331301d0b028a553ac6a47fa2d1`.

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
- `rust-modules/src/app/recorder.rs`
- `rust-modules/src/app/run.rs`
- `rust-modules/src/app/search_owned_tests.rs`
- `rust-modules/src/app/session_protocol_tests.rs`
- `rust-modules/src/auth.rs`
- `rust-modules/src/auth/observation.rs`
- `rust-modules/src/auth/owner.rs`
- `rust-modules/src/browse/section_hubs.rs`
- `rust-modules/src/browse/view.rs`
- `rust-modules/src/dev/scenarios.rs`
- `rust-modules/src/plex/session.rs`
- `rust-modules/src/pms.rs`
- `rust-modules/src/screens/registry.rs`
