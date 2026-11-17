# UI review: evidence and remaining architecture

Review date: 2026-11-16. Base: `990d0439444c7e8180fe6437e95fdcd9136daae9`.
The repairs were reviewed as an isolated working-tree patch. The [publication receipt](ui-review-publication-2026-11-16.md)
records the later publication and current acceptance boundary.
[Current owned-policy receipt](ui-review-modal-verification-2026-11-16.md) identifies the
joined tested source and distinguishes executed gates from unverified device acceptance.
The [earlier verification receipt](ui-review-verification-2026-11-16.md) remains historical,
with its own different frozen source identity.

## Review-point disposition

PASS means the bounded repair is demonstrated, not that the overall UI specification is complete.
PARTIAL identifies a remaining contract or missing acceptance evidence; FAIL means a known gap remains.

| Review point | Status | Repair / remaining limit |
|---|---|---|
| Terminal delivery | PASS | Production `Bridge::app_command_done` logs non-success terminal verdicts with original command id and sender; actual Bridge tests inspect the production logging policy. Dispatcher tests retain exactly-once, stale/duplicate completion, retirement and FIFO/budget checks. This is an explicit diagnostic failure policy, not asynchronous job success or a new screen-visible terminal protocol. |
| Player single writer | PASS (bounded source) | Addressed owner events and next-frame presentation receipts retain mounted owner/instance/seal fences; input, hashing and rendering share the captured presentation. Current actual AppHost regressions pass. Full native playback bootstrap and current-TV replay remain separate, unverified acceptance. |
| Dispatcher / product frame | PASS | The dispatcher-owned product protocol invokes borrowed native IO ports, makes one admission decision and acknowledges an actual swap only. Actual AppHost regressions check callback order, FIFO prefix/carry, frame index and refusal/incomplete-swap preservation. This bounded host/source repair is documented in the frame checkpoint and receipt; no current-TV scheduling/performance claim follows. |
| Bridge ownership/downcasts | PASS (bounded source) | RuntimeOwners lends the actual nine physical fields, including the sole HudStore driver; Bridge retains read-only publications rather than a second logical HUD owner. Actual owner-identity and independent-runtime tests pass. Read-only projections remain; this is not removal of every downcast or all resource-executor composition. |
| Screen extension / AppFx | PASS (bounded source) | Registry-owned route/probe metadata removes both external AppArg exhaustiveness obligations, without a new effect/resource command or legacy byte change. A fresh private ReviewStatic variant compiles/runs through actual AppHost, RuntimeOwners and AppMounter with only ownscreen/lib/registry source edits; real direction input seats focus, real Store/Nav effects and open/back preserve identity. Baseline compiler RED identifies exactly words/run; repaired overlay GREEN. The original factory test remains separately scoped. See the [extension receipt](ui-review-extension-verification-2026-11-16.md) and routing checkpoint; native acceptance remains separate. |
| Tap suppression / replay authority | PASS (bounded source) | Actual callback-suppression regression executed RED (caller saw zero rather than two effects across yield/resume), then GREEN. Pure passive `Tap` fans out owned+caller once; separate `ReplayAuthority` answers only through the explicit owned `FrameObservers::Replay` capability. Actual AppHost Settings input/result callbacks and Player PRE/continued POST command receipts survive owned recording and both replay modes; malicious caller authority and duplicate/drop/second-owner controls retain fail-closed behavior. See the [observer checkpoint](ui-review-observer-checkpoint-2026-11-16.md). This does not complete App ownership, extension or native acceptance. |
| Ambient modal / hold / lift / page image | PASS (bounded source) | ModalStack captures immutable owner-addressed presentation; Input owns gesture-bound lift/lesson facts; PageImageRead is derived per step from the actual owned image and live navigation entry/instance/transition. All former modal/veil/lift/page-image publishers and five page-image logical readers are removed. Compiled same-thread and commit-floor RED→GREEN regressions, unchanged Recommended BACK assertions, strict current codec and planted renamed-global controls demonstrate the bounded repair. Final shipping/full-check/current-renderer evidence is separate in the owned-policy receipt; this is not whole-UI/native acceptance. |
| Public acceptance/docs | PASS | Public, portable evidence links replace ignored-report references. Historical CI/native observations below are explicitly separate from the current patched source and unverified TV acceptance. |

## Ordinary screen extension boundary

A new non-IO screen uses its own module, the conventional `screens/src/lib.rs` module declaration,
and `screens/src/registry.rs` registration; scenario/manifest changes apply when adding public
scenarios. Registry-owned route/probe metadata must declare a route word and probe policy explicitly.
Ordinary screens use existing generic navigation and Store effects; a new domain resource command
is a different extension and may legitimately require an app executor/IO adapter. Screen steps do
not acquire filesystem, platform, player or network IO authority. Legacy recorded route/focus bytes
must remain unchanged; generic screens expose their own logical probe through `Screen::state` and
engine-owned focus, not a new app downcast or an external exhaustive AppArg match.

## Remaining architecture, exactly scoped

`Dispatcher::product_frame` now owns the product lifecycle and invokes explicit borrowed
`FrameRuntime` IO ports for pumping, SDL ingestion, results/playback/clock publication, the logical
step, post-commit publications, admission, native preparation and draw/swap. Its own gate takes one
admission snapshot and acknowledges only a returned actual swap. Dispatcher phase state refuses a
second logical step/nav commit; publication closes the original remaining POST budget. Native App
resources remain in `src/app/run.rs`, outside machine steps; the product path no longer invokes the
empty Bridge pump/prepare hooks as platform implementations. The typed-routing checkpoint addresses
consuming effect partition, observer borrowing and the generic fixture extension. Broader App/Bridge
resource decomposition remains open. Bounded modal/hold/lift/page-image source ownership is repaired:
[modal checkpoint](ui-review-modal-checkpoint-2026-11-16.md). See the
[frame protocol checkpoint](ui-review-frame-protocol-2026-11-16.md) and the
[routing checkpoint](ui-review-routing-checkpoint-2026-11-16.md) for their bounded evidence.

The narrow player observation codec does not authorize native `AppFx::Player` commands or compiled
resource replay. Negative tests retain those refusals and distinguish changes to time, duration,
tab and reset payloads. No gate, historical fixture hash or privileged playback policy is disabled.

## Historical public CI and native evidence

These observations do **not** certify the new working-tree patch:

* Historical PR head `43e6b841b2f1b2e81242846455b2440685c4dd70` had 15 successful checks
  reported (nine CI, six CodeQL). On this review date the public API independently confirmed
  [CI run 43756021407, attempt 2](https://github.com/plxnative/plx-native/actions/runs/43756021407)
  and [CodeQL run 43755455634, attempt 1](https://github.com/plxnative/plx-native/actions/runs/43755455634)
  completed successfully on that exact SHA. Cancelled earlier attempts are not passes.
* Retained historical native reports recorded 69 PASS / 1 not applicable on an **older** ELF.
  A later bounded retest of the 43e6 ELF covered Library idle and critical interactions
  (37 heartbeats, 18 presents); it was not a fresh broad 69-route run or panel-on FPS acceptance.
  Private device logs/recordings are not published here, and these historical numbers were not
  independently remeasured during this source-verification task.
* A private local current-format Filmography tape was reported SAME in targets and resolve
  (2287 frames each, eight counters zero). It was not established against exact main 990d
  or this patched tree and is not a published fixture. A historical incompatible tape was
  deliberately refused, not passed.

The original source review produced no ARM artifact, simulator visual registry, TV deployment,
native route matrix, settled-idle or panel-on performance result. Later verification is recorded
separately in the [publication receipt](ui-review-publication-2026-11-16.md); historical observations
above do not become exact-head native acceptance.
