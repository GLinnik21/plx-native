# Runtime ownership cut — default production GREEN, fixtures pending

Summary: The actual default production library now compiles with physically separate RuntimeOwners and a borrowed eight-loan Bridge. This is a production compiler checkpoint, not completion of fixture migration or runtime ownership acceptance.

## Verified gate

- Clean WSL environment: `env -i HOME=/home/glinnik PATH=/home/glinnik/.cargo/bin:/usr/local/bin:/usr/bin:/bin`.
- From `rust-modules`: `cargo +<repository-pinned-channel> check -p plxnative-modules --lib --message-format=short` (repository-pinned toolchain at the tested revision) exits **0**, `Finished dev profile` in 14.97s.
- Log: `.empryo/reviews/ui-owner-default-green.log`. Existing `plx_net` incremental-directory permission note remains nonfatal; it is not a source error.
- Compiler progression: 525 → 175 → 30 → 2 fixture-only dead-code helpers → **0**.
- WSL `git diff --check` passes for changed production files, including helper-owned `run.rs`.
- `python3 ci/test_runtime_ownership_borrows.py`: **3 passed**. These compile the actual physical declarations and exact loan constructor with opaque machine internals: scoped/sequential loans compile, local-owner escape rejects with E0515, simultaneous mutable turns reject with E0499. This is a constructor/lifetime proof, not domain behavior evidence.
- Existing `ci/test_runtime_ownership_contract.py`: physical Bridge/App checks pass; fixture gate remains **RED, 346 offending lines**. No fixture was skipped or converted into leaked ownership.
- Added two Rust unit tests in `runtime_owners.rs` for the eight actual field identities, successive turns, independent harness publications and independent child replacement. These are **not yet compiled or executed**, because the existing test corpus still requires migration.
- Hostsim, shipping, Clippy, full make, renderer/device and actual independent-App behavioral acceptance have not been run after this compiler cut.

## Production changes and frozen API

- `bridge.rs`: every production retained-cache access explicitly uses `retained`; Session, Consent, Stores, their runners and the mounter remain direct borrowed physical children. The two now fixture-only read helpers are `cfg(test)`.
- `run.rs`: explicit field bindings keep frame/replay/request/native/recorder loans disjoint; draw retains one coherent short Bridge loan through its callbacks. Reporting returns owned snapshots before re-entering App helpers.
- `content.rs`, `playback.rs`, `mod.rs`: scoped owner loans at the actual application boundaries; owner-backed metadata references read from the actual Stores child rather than a temporary capability.
- `watchlist.rs`: explicit retained publication fields, unchanged store ownership.
- `dev/scenarios.rs`, `dev/scenarios/poster_gate.rs`, `dev/framedump.rs`: transient capabilities for commands; actual-owner projections for long-lived metadata/auth/readout borrows. Framedump hostsim changes are not separately compiled yet.
- `RuntimeOwners::bridge<'a>(&'a mut self, &'a mut BridgeRetained) -> Bridge<'a>` remains the production loan constructor.
- `RuntimeOwners::{metadata_view,metadata_mut,person_view,auth_read}` project directly from their physical children. Hostsim/devtriggers-only `landgate/open_claims` read actual Stores.
- `BridgeRetained` factories continue to return `(RuntimeOwners, BridgeRetained)`; no owned-Bridge compatibility factory exists.
- Test-only `Harness::{new,from_parts,bridge,with_bridge}` owns the factory pair and lends the same capability. `bridge(&mut self) -> Bridge<'_>` does not leak owners, implement Deref or grant a static lifetime.

## Fixture constructor matrix

Local-only fixture:

```rust
let mut owner = Harness::new(now_us);
let mut rig = owner.bridge();
// Existing assertions use the scoped rig; owner outlives it.
```

Other constructors: call the matching `BridgeRetained` factory, pass the returned pair to `Harness::from_parts`, then borrow per actual test boundary. `for_session_test`, `for_consent_test` and `with_publications` currently remain private to `bridge.rs`; widen only to `pub(super)` if an actual sibling consumer needs them.

- `for_test(now_us)` → `Harness::new(now_us)`.
- `for_session_test(init)` → pair from matching retained factory.
- `for_consent_test(consent)` → pair from matching retained factory.
- `for_consent_resource_test(consent)` → pair from matching retained factory; the old regex fixture gate does not include this variant.
- `controlled_home(now_us, initial, mt, replay)` → pair from matching retained factory.
- `new(now_us, init, consent, mt)` → pair from matching retained factory.

A helper returning Bridge or a struct storing Bridge must return/store Harness instead. Callers borrow at use boundaries. Do not use `Bridge<'static>`, Box leaks, Deref forwarding or copied owner machines.

### Inventory accounting

A lexical constructor-name search matches 355 references across 71 files, including documentation. Requiring an actual opening parenthesis matches 348 occurrences across **70 files**. `clock.rs` is documentation only and is excluded below. The legacy guard counts matching lines (346), not exact constructor occurrences, and excludes `for_consent_resource_test`; these numbers are not interchangeable compiler-error totals.

All paths below are relative to `rust-modules/src/app/`; files with production code need only their fixture regions changed.

```text
bridge.rs
bridge_test_support.rs
browse_discovery_admission_tests.rs
browse_discovery_drain_tests.rs
browse_hubs_drain_tests.rs
browse_listing_admission_tests.rs
browse_roster_bridge_tests.rs
consent_owner_tests.rs
content.rs
deeplink_route_tests.rs
detail_panel_tests.rs
detail_related_landing_tests.rs
dispatch_return_tests.rs
home_admission_tests.rs
home_retention_tests.rs
hubs_dump_tests.rs
input.rs
item_menu_player_return_tests.rs
library_bookmark_tests.rs
library_collections_arrival_tests.rs
library_deferred_tests.rs
library_diagnostic_tests.rs
library_host_freeze_tests.rs
library_navigation_tests.rs
library_push_capture_tests.rs
library_query_tests.rs
library_shelf_action_tests.rs
library_tab_arrival_tests.rs
lifecycle.rs
menu_play_season_tests.rs
metadata_alt_sources_bridge_tests.rs
notify_bridge_tests.rs
opener_route_tests.rs
owned_publication_tests.rs
page_regions_tests.rs
person_admission_tests.rs
person_boundary_tests.rs
person_lifecycle_tests.rs
playback.rs
plex_session_app_tests.rs
recorder.rs
recording_erasure_tests.rs
reread.rs
restore_characterisation_tests.rs
run.rs
search_fetch_pin_tests.rs
search_owned_tests.rs
search_publication_tests.rs
session_controller_regression_tests.rs
session_dev_bootstrap_tests.rs
session_dispatch_tests.rs
session_endpoint_policy_tests.rs
session_picker_regression_tests.rs
session_plaintext_repair_tests.rs
session_profile_regression_tests.rs
session_protocol_tests.rs
session_resource_tests.rs
session_stored_home_tests.rs
session_worker_adapter_tests.rs
store_admission_tests.rs
strip_home_pill_tests.rs
surface_navigation_tests.rs
viewstate_controlled_tests.rs
viewstate_directory_policy_tests.rs
watchlist_press_tests.rs
watchlist_wiring_tests.rs
words.rs
adapters/session.rs
adapters/stores.rs
bootstrap/tests.rs
```

### Owned-return / owned-field groups

- `bridge_test_support.rs:159`: World.bridge must own Harness; its many constructors and methods must borrow per turn, keeping command resources/native loans disjoint. Its two `impl Bridge` blocks need explicit borrowed lifetimes and retained accesses.
- `browse_discovery_admission_tests.rs:41`, `home_admission_tests.rs:34`, `hubs_dump_tests.rs:15,149`, `opener_route_tests.rs:12`: helper results currently contain owned Bridge.
- `browse_hubs_drain_tests.rs:38`, `deeplink_route_tests.rs:30`, `menu_play_season_tests.rs:27`, `page_regions_tests.rs:110`, `playback.rs:1762`: fixture structs currently store owned Bridge.
- `reread.rs:292`, `restore_characterisation_tests.rs:254`: tuple-return helpers currently contain owned Bridge.
- `session_controller_regression_tests.rs:27,248`, `session_dev_bootstrap_tests.rs:9`, `session_picker_regression_tests.rs:45`, `session_profile_regression_tests.rs:30`, `session_resource_tests.rs:585`: session helper returns need Harness.
- `session_protocol_tests.rs:57,397`: helper return and Jobs.rig need Harness, including the later helper return in the same file.
- `viewstate_controlled_tests.rs:25`, `watchlist_press_tests.rs:180`, `watchlist_wiring_tests.rs:115`: tuple helpers need Harness, preserving returned owned diagnostic values and all assertions.
- `person_boundary_tests.rs:370`: fixture-only Bridge impl needs explicit lifetime and retained fields.

### Shared World dependency group (27 files)

This group extends beyond constructor-hit files: changing World.bridge changes every caller. Assign the whole dependency group to one lane; `bridge.rs` contains the support-module wiring and must not race another lane writing fixture regions in that file.

```text
bridge.rs
bridge_test_support.rs
account_menu_route_tests.rs
arrival_seat_tests.rs
card_rest_route_tests.rs
collection_crumb_route_tests.rs
deep_shelves_route_tests.rs
header_cache_route_tests.rs
home_chrome_route_tests.rs
home_hero_route_tests.rs
library_back_lift_tests.rs
library_memory_route_tests.rs
library_push_capture_tests.rs
library_reread_route_tests.rs
library_restore_arrival_tests.rs
library_route_tests.rs
library_scroll_route_tests.rs
library_switch_route_tests.rs
recommended_enter_tests.rs
sort_filter_vocabulary_route_tests.rs
surface_navigation_tests.rs
under_posters_band_route_tests.rs
under_posters_pages_route_tests.rs
under_posters_route_tests.rs
under_posters_shelf_route_tests.rs
watchlist_disc_route_tests.rs
watchlist_press_tests.rs
```

No fixture source has been migrated by this production checkpoint, except the helper's existing in-file run.rs factory-pair/actual-owner corrections. The parent assigns disjoint fixture lanes next; production GREEN must remain a checked join invariant, not be inferred after further edits.
