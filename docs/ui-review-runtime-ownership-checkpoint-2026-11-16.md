# Runtime ownership cut — uncompiled checkpoint

Summary: Physical owner decomposition and a borrowed Bridge type are implemented, including the real App bootstrap boundary. The migration is incomplete and the isolated source does not compile. This checkpoint is not ownership acceptance or a GREEN source freeze.

## Physical source inventory

All paths below are relative to `rust-modules/src/app/`.

- `runtime_owners.rs`: `RuntimeOwners` is an App field with four distinct children, not a renamed rig. `SessionOwner.machine` owns Session and `SessionOwner.adapter` owns its executor. `ConsentOwner.machine` owns Consent and `ConsentOwner.adapter` owns its executor. `StoreOwners.machines` owns Stores and `StoreOwners.runner` starts store jobs. `MounterIo.mounter` owns the product mounter. Dropping SessionOwner cancels its adapter; dropping a transient view does not cancel App work.
- `mod.rs`: App owns `runtime` separately from retained `bridge` cache state. Playback remains the existing fifth logical child, `App.player`; native playback resources and MainThread remain `App.adapters.player`. `snapshot_init` reads the physical Session child directly.
- `bridge.rs`: `Bridge<'a>` holds eight explicit mutable loans: Session, SessionAdapter, Consent, ConsentAdapter, Stores, StoreRunner, AppMounter and BridgeRetained. It owns none of those objects. No Deref, Rc aggregate, unsafe alias, leak or owner-copy was introduced. AppViews and Rig::split compose read publications directly with those borrowed owners.
- `bridge.rs`: `BridgeRetained` holds the previous publication, capture scratch, UI rendering and IO bookkeeping fields. This is a migration boundary, not a claim that every retained field is already a pure publication. In particular HUD/recording/capture bookkeeping still needs domain grading; nothing was removed from canonical state to satisfy a parser.
- `bridge.rs`: live/controlled/test factories moved off the borrowed Bridge onto BridgeRetained and return `(RuntimeOwners, BridgeRetained)`. They initialize physical owners and lend a temporary capability to seed captured publications.
- `boot.rs`: constructs those two values, borrows a bootstrap Bridge, ends that loan before moving both values into App, and borrows a fresh Bridge for controlled bootstrap commands. This is the actual product construction boundary, not a fixture-only owner tree.
- `runtime_owners.rs`: test-only `Harness` owns the same factory outputs and exposes `with_bridge` closure scope. It does not implement owned-Bridge constructor compatibility or leak owners. Existing fixtures have not yet been converted.
- `content.rs`: ScreenCommandApp now distinguishes the command borrow lifetime from the borrowed owner's lifetime. `CommandRig` likewise distinguishes turn, owner and native-resource lifetimes. The Person capture function pointer explicitly quantifies both turn and owner lifetimes rather than fixing the constructor's owner lifetime.
- `plex_visual_inputs.rs`, `watchlist.rs`: borrowed Bridge impl lifetimes; the visual observer uses explicit retained-cache accesses.

## Verification, as of this checkpoint

- Baseline source RED probe against Git HEAD: `AssertionError: RED: baseline Bridge has no owner-borrow lifetime`.
- Focused default compiler: `cargo +<repository-pinned-channel> check -p plxnative-modules --lib --message-format=short` (repository-pinned toolchain at the tested revision) exits 101. First cut: 661 errors; current cut: **525 errors**, recorded in `.empryo/reviews/ui-owner-checkpoint-default.log`. Predominantly old direct cache-field access in Bridge methods and old `app.bridge` method calls that now need explicit scoped owner loans. This is a failed compiler gate, not passing runtime evidence.
- `ci/test_runtime_ownership_contract.py`: two structural checks pass (borrowed child fields and physical App tree), fixture-migration check remains RED with **347** obsolete owned-Bridge constructor call sites. No fixture was skipped, allowed-listed or silently converted to leaked ownership.
- `git diff --check` over edited existing Rust files passes. HEAD remains `990d0439444c7e8180fe6437e95fdcd9136daae9`. No Git-writing operation was run.
- Hostsim, shipping, Clippy, existing source guards, full make, renderer, device and independent-App behavioral proofs were not run after this cut because default compilation is still RED.

## Exact continuation

1. Migrate Bridge's retained-cache accesses explicitly (`self.retained.<field>`), without a field-forwarding/Deref compatibility container. Keep owner accesses direct borrowed fields.
2. Migrate actual App turn/request/draw boundaries to borrow `app.runtime` and `app.bridge` disjointly. Do not return references through a temporary capability or borrow all of App while also needing native resources.
3. Convert every old owned fixture constructor to the actual factory/Harness closure API, including multi-App independence and scoped lifetime failure proofs. Harness currently has no callers; this is unfinished, not green fixture compatibility.
4. Complete retained-cache/resource domain grading, independent Session/Consent/Stores reset behavior and the actual AppHost extension acceptance proof.
5. Only after default compilation succeeds, run focused default/hostsim/shipping, named Clippy and source gates, then the requested final make/renderer stages.

Tap fan-out, direct associated AppEffect routing, sealed player presentation receipts, V13 canonical state and owned modal/hold/lift/page-image implementations were not changed. This does not assert their regressions pass on this uncompiled source.
