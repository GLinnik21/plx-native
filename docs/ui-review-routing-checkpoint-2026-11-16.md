# Typed effect routing checkpoint — 2026-11-16

Isolated checkout: `ui-review-main-20261116`, uncommitted on `990d0439444c7e8180fe6437e95fdcd9136daae9`.
Earlier player/outcome and actual product-frame changes are preserved. This checkpoint does not
claim completion of passive Tap separation, App ownership decomposition or product extension acceptance.

## Executed contracts

- The layer-neutral `Host::Fx` implements `AppEffect`, associating `Inline` and `ExternalFx`.
  `InlineFx<H>` / `ExternalFx<H>` project those types without application imports in UI/machine.
  One exhaustive consuming `AppFx::route` conversion supplies the production partition.
- `Rig::app_fx` accepts only the inline type. The retained/taken `AppCommand` accepts only the
  external type. The dispatcher directly invokes the consuming associated `AppEffect::route` and
  matches one owned `EffectDisposition`, never a classifier boolean or overridable Rig classifier.
  `Rig::route_app_fx` no longer exists. Production Bridge inline and external matches have no
  unreachable family catchalls. Actual product compile-fail doctests reject mixing admitted types.
- The resource-bundle helper accepts `ResourceAppFx`, not the entire emission vocabulary.
  Domain-specific resource admission (Home, Library, Content, Player, menus) remains typed;
  no erased payload, registry, event bus or second family queue was added.
- `command_observer::Observation` is passive: begin captures observations, finish records successful
  outcomes. The production execute arms contain no test conditionals. Neither observation callback
  admits or executes commands. `app_command_done` retains its production failure logging policy.
- `Observer::{Caller, Owned}` makes the single callback source explicit. An active native `Recplay`
  is the owned observer; Off uses the caller. The same choice is used across yield/resume, with the
  observer borrow ending before command resources are borrowed. No callback is sent to both.
- A separately implemented/mounted `ExtensionScreen` in the library fixture emits existing generic
  navigation/store effects. Its test verifies the real store update, same-frame mount, no yielded
  resource obligation and no lost tail. No enum case, central classifier, runner or CI list is added
  for that screen. This does not establish every product registry/route extension case.
- Command-accumulator scanning recognizes the new typed enums. Negative controls reject retaining
  them outside the FIFO, restoring boolean classification, widening envelope/executor types,
  unreachable handlers, removing the observer boundary and inline test observations. Comment and
  test-only decoys cannot satisfy production pins.

## RED and verification

The new registry contract first failed compilation: `AppFx::route`, `InlineAppFx`, `ExternalAppFx`
and the neutral effect contract did not exist. The command-accumulator negative control separately
failed with `[] is not true` for `Vec<ExternalAppFx>` before the scanner recognized the typed enums.

The first full application run compiled but failed two newly migrated controlled-admission tests
(`1420 passed; 2 failed`): a full frame consulted the native font measurer. Those tests now exercise
the real FIFO drain, yielded admission and resume directly, without taking an unrelated layout path.
No production font rule was weakened. The first full screens run then exposed ten identity-fixture
failures (`2165 passed; 10 failed`): the newly external fixture runner acknowledged a retired sender
as Completed. Fixture runners now preserve the taken envelope's terminal error, rather than bypassing
the dispatcher's sender/outcome fence. No production retirement check was weakened.

Verification commands use WSL `env -i`, explicit HOME/PATH, Python 3.12.16 and the repository-pinned toolchain at the tested revision, no
inherited Git overrides. Final results are recorded in the handoff; the earlier frame receipt's hash
is historical and does not certify these edits. Whole `make check`, ARM and device acceptance remain
deferred. No commit, push, publication, device or parent-checkout source operation occurred.

## Direct-route follow-up: executed prefix

The sourceguard first failed on the actual overridable `Rig::route_app_fx` and its dispatcher call.
The dispatcher now invokes `plx_machine::machine::AppEffect::route(app_fx)` directly exactly once.
Negative controls reject restoring the override, replacing the direct route, dropping the effect
or consuming it twice; existing routing controls reject catchalls, lost arms and multiple head takes.

Existing fixture `Net` remains synchronous. Dedicated `ExternalNet` and Page974's explicit external
Mount emission exercise PRE and POST yielded commands without a Rig routing override. The original
two-receipt, same-frame mount and budget assertions remain unchanged. Actual product tests now
invoke the associated conversion, not the removed Rig method. No family codec semantics changed.

Executed clean-environment evidence: UI1642 tests, product partition compile-fail doctests2,
AppHost hostsim1649 tests (including actual Player draw/receipt recording/replay), shipping
`play-uri,play-feed` library check and workspace/all-targets constrained Clippy passed. Python
boundary controls28 and routing controls18 passed. The last code freeze tested by job380 is
`9fef58720aeaf09a5ea58208bda81983a8bc733dfed6f374101bcc29091d29c3`;
source/HEAD/raw-index/staged entries stayed stable. These documentation updates follow that
executed code freeze and are not a new fullmake/renderer/device acceptance claim.

The prerequisite hostsim compile failure at `src/app/run.rs:5371` was independently corrected by
making the draw-census closure return unit; actual Player assertions were not changed or weakened.

## Explicit remaining work

Bridge still physically contains Session, Consent, Stores, their adapters and the mounter. Moving
those owners into actual App/RuntimeOwners children with a frame-borrowed rig is NOT implemented.
No grouping of fields or relocation of impl blocks is offered as that ownership repair. Navigation
still exposes its existing mutable container surface; blanket removal of external mutable logical
screen access and immutable navigation projections is NOT implemented here. The modal/hold-hint
ownership follow-up has separate executed evidence in the owned-policy receipt; this routing stage
does not alter it.
