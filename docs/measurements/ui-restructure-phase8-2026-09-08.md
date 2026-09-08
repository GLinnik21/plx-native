# UI restructure Phase 8: implementation checkpoint

The controlling specification remains `~/.claude/plans/ui-plxnative-structured-phoenix.md` v4.
This is a progress record, not a replacement or reduced definition of done.

## Library owned-screen wave — implementation active

Parent commit `6421906e` adds `Seat::ProjectedFrom(GroupId)` and uses it at MasterDetail's
master door. The engine resolves its own remembered source key through the current layout and
passes the reconciled placement/index to the master's existing `seat` method. No focus copy in
`Cx::views`, screen state or the component is needed. Projection is read-only: only the explicit
remember effect updates the detail cursor. Source disappearance/unplaceability and cross-entry
memory fall back safely. Host tests cover toolbar entry, both sides, reorder, deletion, fresh and
restored entry, and an immediately updated remembered key. The toolbar-index regression was RED
before this change. `make check` passes (2645 default / 2666 hostsim, 3 ignored each, plus CI),
shipping-feature check and ARM build pass; `/tmp/plx-phase8-library-projection-*.log` holds logs.
This component change awaits the integrated screen wave's independent review.

The real Library migration is active in `fleet/phase8-library-owned`, cut from `6543da63`,
with an explicit disjoint scope from the parent's three core files. The worker has begun owned
identity/layout/parts/transactions and application wiring; it is not a completed adoption yet.
Its branch includes the core projection change. Do not retire the outstanding Library proof or
start a replacement worker merely because a progress observation times out.

A **before-migration native reference**, not a claim about the new screen, was captured from the
debug install, binary MD5 `b1649212219a25ae6c85946c5ec47c9b`, under an exclusive TV lease.
Private artifacts: `/tmp/plx-library-native-before.GOUlgG/`. The parent opened the 1920×1080
captures of the two-library selector, published shelves, six-column grid, Sort and Filter panels,
scrolled card → Detail → Back, rail entry and the held card's context menu. The selected card and
viewport survived the return. No playback or watched-state action was performed.

One parity constraint was confirmed both in the native capture and `ui/library.rs::enter_rail`:
directional rail entry selects the grid item's letter but does NOT jump to that letter's first
item. A letter move/direct click performs the jump. The Library owner must observe MasterDetail's
door event without translating that initial projected entry into a redundant Live-follow jump.
This has been passed to the implementation lane for a regression test.

The set was already awake. Audio was explicitly muted and independently read back muted after
handback; backlight was never turned off. `tv-session down` cleared debug automation/token state
and relaunched normal interactive boot, then the lease was released. Stable was untouched.
The Library worker has no device access. Full screen integration/review/simulator/native proof,
closed replay and all remaining phases remain open.

## Library read-view wave — integrated and independently reviewed

Implementation and one review-fix wave are integrated through `e437513f`, from measured base
`2dc0e2b9`. Following
`.claude/workflows/swarm-gate.js`'s bounded-wave structure, a Sol lane owned section-hub retention,
a Luna lane owned the Library navigation proof, and the parent owned listing/query/directory
views and frame integration. Both worker branches are merged, not copied as uncommitted patches.

`browse/view.rs` retains sparse listing pages, query selections, server-driven menus and letters;
directory prose is retained across frames and rebuilt on table/source/selection changes.
`section_hubs::HubsSnapshot` retains the committed shelves and publication state with explicit
epoch/server/section identity. Bridge captures all three read views once per frame. Copy-on-write
keeps old views valid across page arrivals, optimistic edits, staged commits and resets; an
unmatched edit does not clone unrelated pages/catalogs. Absence has no fabricated section id.
The new views are available in `AppViews`, but **Library is still the legacy screen**: this is
not its owned-screen adoption, completed BrowseStore ownership or serialized initial state.

RED/GREEN regressions fixed partial-page growth and shrink/regrow resurrection, plus the missing
source-directory generation bump when a previously blank server name is learned. An integrated
host run also exposed a test-only Darwin socket race: the cold-direct teardown fixture accepted a
nonblocking socket and raced its request writer. Its reader is now explicitly blocking with bounded
read/write timeouts. Runtime playback code is unchanged.

The old flow 3 falsely passed `Home → Detail → Home` without Library. It now requires
`Home → Library → Detail → Library`, actual card focus without a menu, matching server/item
identity in Detail and after BACK, and equality of every supplied focus field. Regressions reject
missing fields on either/both sides and wrong Detail identity. The legacy probe does not supply
numeric grid position/viewport, so automatic viewport verification is **not** claimed.

Astra's first review found two P2 issues: a page-result reachability change did not invalidate
the directory cache, and the grader accepted a correct first post-BACK frame followed by drift.
The shared source-outcome mutation now notifies exactly once (failure/repeated-failure/recovery
regression), and the grader checks every post-BACK fingerprint and refuses later route departures.
One disjoint fix wave addressed both; Astra re-reviewed `e437513f` and returned **PASS**, scoped to
this read-view/proof slice, not the full plan. No additional prose contradictions were found.

Final gates pass: `make check` (2642 default / 2663 hostsim tests passed, 3 ignored each, plus
lint and CI), shipping-feature check, ARM build and simulator build. Logs are
`/tmp/plx-phase8-library-views-reviewed-{check,shipping,arm,sim,smoke}.log`. Fresh synthetic flow 3
passes at `/tmp/plx-phase8-library-views-reviewed.cnctI6/` (port 32553): selected `sid=0 rk=1046`;
the parent opened `root-3/shot-2.png` and `shot-3.png` at 1920×1080 and observed the same card
and viewport. The original false-positive artifact now refuses. This is live simulator evidence,
not closed replay, native text/rendering or performance proof. No fixture import/rebaseline occurred.

Both implementation workers are closed; their clean worktrees were removed after ancestry checks,
and their orphan Cargo output was reclaimed. Branches remain; the after-wave disk measurement is
75 GiB free. The read-only Astra reviewer and the fix worker are also closed.
No main write/push, release or TV action.
Next are actual Library/MasterDetail adoption (engine-owned focus and projected rail, owned
transactions), Search/system keyboard and nav read migration, plus the still-open closed replay
bootstrap/injection and phases 9–12. None of that scope is waived by these passing checks.

## Wave 2 integrated and independently reviewed

The bounded workflow completed at `85f8b3ec`: two disjoint implementation lanes plus parent-owned
Home boot capture, followed by Astra's read-only review of the integrated batch. The first review
found three P2 issues; one disjoint fix wave addressed them and Astra's follow-up verdict was
**PASS**, scoped to this component/capture slice, not the whole restructure plan.

- MasterDetail now requires a stable caller-provided `KeyRegion` identity classifier. Real engine
  regressions preserve detail/master ownership after deletion and validate fallback placement,
  including empty/unplaceable children. No component focus copy was introduced. Library and
  track-menu adoption, including the engine-derived projected view, still remain.
- Deploy verification re-hashes after a possible rebuild and repairs a positively confirmed
  missing executable. Failed/malformed presence responses and real hash failures remain refusals;
  a post-deploy absence fails. Twelve mocked host tests pass; no device deployment was performed.
- Recording manifests are encoded through a bounded borrowed serializer before any sink write.
  Initial bytes count against the 64 MiB cap, with space reserved/accounted for the final stop
  note. Regressions cover oversized and escaping-expanded contents, byte accounting and no writes
  after stopping. `serde_json`'s `std` writer feature is enabled; no dependency version changed.

Final gates pass: `make check` (default 2628 / hostsim 2649 passed, 3 ignored each, plus lint and
CI), shipping-feature check, ARM build and simulator build. The offline firmware sweep passes
all nine inventories from 4.4.2 through 11.2.0; the five older failures are below the existing CI
floor (`.github/workflows/ci.yml`, `--min-release 4.4.2`), not an all-firmware support claim.
Logs: `/tmp/plx-phase8-wave2-fixed-{check,shipping,arm,sim,fwcompat-gated}.log`.

Fresh synthetic simulator flow 1 passes at `/tmp/plx-phase8-wave2-fixed-record.ooxpuP/`: 581
frames/state/presentation records, 10 inputs, 2923 effect tags, one result and three lifecycle
records. The privacy gate passes; no fixture was imported or rebaselined. The preceding capture
at `/tmp/plx-phase8-wave2-record.vlwRUv/` also proved the captured initial in-flight request and
its frame-0 arrival agree on source/request/client/token/account-generation tags. This is capture
evidence, not initial-state restoration, closed replay, native rendering or frame-rate proof.

All wave workers/reviewer are closed. Their completed clean worktrees were removed only after
their commits were verified ancestors of the integration branch; branches remain. The copied
worker private config was verified identical to the retained integration copy before teardown.
Orphan build-cache cleanup and the after-wave disk measurement were run. Earlier SSD cleanup
reclaimed roughly 34 GiB of old Cargo output while preserving source and uncommitted work.
No main push, release or TV action occurred. The remaining replay/store/session initialization,
adapter suppression/injection, Library/Search/keyboard and phases 9–12 remain open.

## Earlier wave 2 preparation checkpoint

Following the user's request to reuse the swarm workflow, two independent lanes were cut from
`0a8191d7793f72093adef2a648f2e75879bd02d5`, with parent-owned replay/bootstrap files kept separate.
The deploy-proof lane completed `0d4b3329`, now integrated: local bytes are re-hashed after a
deploy that may rebuild, and failed/empty/invalid hashes cannot be accepted. Its eight mocked
host tests and shell syntax check passed; parent reran the eight tests successfully. The
MasterDetail component lane is still running in `phase8-master-detail-w2`. Neither lane has TV
access. Before-wave disk measurement reported 15 GiB free; external per-lane target directories
and disabled incremental compilation bound build growth. Full integration gates and one Astra
review follow the completed wave, not every small edit.

Parent work adds owned `pms::initial::Initial`: source states/contributions, immutable client ids,
token generations, in-flight tags, retry bits/history, allocator/roster/section/publication
counters, and the published catalog/hub ranges/hero slots. Capture leaves the worker mailbox
untouched. The application adapts a data-layer scalar visitor to `Canon`, with exhaustive field
destructuring: no JSON, pointer, or host-sized integer is hashed. Hidden retry fields alter the
canonical initial hash even when the visible catalog does not change.

Generic recording headers now transport application-defined `init.data`. Only an armed
recording captures/clones Home's contents; ordinary boots do not. The new contents are included
in `init.hash` alongside the coarse application facts. The deliberate fingerprint change is
`0xe61b6d55f4428637` → `0x6c356005c114626c`; previous shapes are refused, not rebaselined.
Focused source-initial/header tests and the parent shipping-feature check pass. This is Home
initial CAPTURE, not whole-app restoration: remaining store/session/client initialization and
adapter suppression still prevent a closed replay claim. Production still uses live adapters.

Logs so far: `/tmp/plx-phase8-wave2-disk-before.log`, `/tmp/plx-phase8-initial-*`.
No new recording or anchor, release, main-branch write or device action in this checkpoint.

## Previous checkpoint: Home request admission is separate from its worker adapter

`Src::begin_request` now owns the single-flight state transition and returns a `HubRequest`
containing the captured account generation, request id, server, client binding and token
generation. It mints only for an admitted request, keeps the last successful catalog and failure
history, and transitions to Loading before any adapter runs. `kick_with` supplies the adapter;
the production `kick` still selects the existing worker. A held request follows the same model
transition without issuing HTTP, and a second kick cannot call the adapter while it is in flight.

`spawn_fetch` only executes the captured request; `HubRequest::complete` constructs its landing
without re-reading current registry/token/generation state. The token generation is now sampled
once for both the source and the request. Existing refusal/backoff handling remains at admission.
Tests cover unique ids across servers/retries/profile reset, no second mint or adapter call for a
single flight, retained catalog/retry history, and original completion tags after token change,
endpoint re-point and account reset. No new global replay switch was introduced.

This is the request-side seam, NOT product replay suppression: startup restoration, adapter
selection/admission recording and the endpoint-refresh side effect still need integration with
owned store state. The broader plan remains open. No TV or anchor changes occurred.

Full host check passes (default 2610 / hostsim 2631 passed, 3 ignored each, plus lint and CI),
as do shipping-feature and ARM checks. Logs: `/tmp/plx-phase8-request-transition-*`.

## Previous checkpoint: supplied results share the live dispatcher execution path

`bridge::frame_with_results` accepts an explicit result supplier at the existing ingest point;
`frame_with_tap` delegates with the live collector. View capture, routing, ordering of live
results and the dispatcher/store delivery path are unchanged. A supplied frame never also drains
the live mailbox. Tests apply a decoded result through the real store while a different live
arrival remains queued, then show an empty supplied frame cannot fall back to that live arrival.

`Recplay::replay_results` reads the current recorded frame, strictly validates its envelopes
(frame, async event type, Hubs store address, request id and payload), and decodes the entire
batch before returning any deliveries. `None` means not replaying; `Some(empty)` means replaying
a frame with zero arrivals. It requires explicit recorded-client bindings and never consults
the current registry. Malformed second records refuse the whole frame, even after a valid first.

The writer/reader/dispatcher/store chain is exercised in a host test using the writer's actual
JSON result record, with zero result-grading differences and the live mailbox left unconsumed.
That test explicitly reuses the current store epoch: it is NOT full initial-state restoration.
The production loop still selects the live collector. Bootstrap restoration and suppression of
live requests must be implemented before selecting the replay reader there; all other remaining
plan requirements remain open. No anchors, simulator runs or device state were changed.

Full `make check` passes (default 2609 / hostsim 2630 passed, 3 ignored each, plus lint and CI),
as do shipping-feature and ARM checks. The final recorder-chain tests also pass on default
and hostsim features. Logs: `/tmp/plx-phase8-replay-result-*` and `/tmp/plx-phase8-supplied-results-tests.log`.

## Previous checkpoint: state-only rebaseline cannot discard result differences

A RED/GREEN regression found that the rebaseline tool accepted a log containing both result and
state differences, producing a state-only adoption record that omitted the result mismatch.
The tool now refuses if either a result-divergence diagnostic or a nonzero `result_diffs` summary
is present. Tests include contradictory/missing detail lines and verify the original manifest
and segment remain byte-identical with no `divergence.json` written. The tool still needs an
adapter-aware adoption record before result divergences can be intentionally rebaselined.

The full Python harness passes (256 tests), as does the final targeted replay-fixture suite.
Logs: `/tmp/plx-phase8-rebaseline-result-{red,check,final}.log`. No Rust or device changes; the
full implementation goal and the remaining offline replay/migration requirements stay open.

## Previous checkpoint: replay grades result arrivals and preserves rebound identities

The replay observer now compares every observed Home result against the recorded frame's next
result: complete payload, store address and request id, in order. Changed/extra arrivals report
immediately; missing arrivals report at the frame tail. Each diagnostic contains only frame,
ordinal and a finite reason, never payload text. The result cursor resets every frame, grading
continues after differences, and `result_diffs` participates in the final SAME/DIVERGED verdict.
The existing verdict readers tolerate the added summary field; result-only differences still
cannot authorize the state-divergence-based rebaseline tool.

A host regression holds state hashes equal while exercising payload/order/address changes,
missing/extra arrivals and recovery at the next frame. A negative control restoring the old
state/presentation-only verdict failed: the test observed `same() == true` despite differing
results. This closes a real false-success path, not the missing offline adapter architecture.

Review also found that decoding with another process's client mapping and then encoding changed
the result's client identity to that resource's allocation id. A RED/GREEN regression now proves
that `LandingClient` keeps recorded identity separate from its bound client resource. The wire
format and fingerprint remain unchanged, while the original pointer/token checks still use the
resource. Token-generation initialization still belongs to the missing replay bootstrap.

This remains LIVE-ASSISTED replay. Full initial conditions, adapter suppression/injection, effect
payload codecs and effect-stream grading, pointwise resolution and the remaining screen/engine
migrations are still required. No replay anchor was imported, replaced or rebaselined; no device
or main-branch work occurred. Logs: `/tmp/plx-phase8-result-grade-*` and
`/tmp/plx-phase8-result-binding-{red,green}.log`.

Final `make check` passes: default 2608 / hostsim 2629 passed, 3 ignored each, plus lint and CI.
Shipping-feature check and ARM build also pass (`result-grade-final-*` logs). These are host/build
checks of the grader and binding correction, not a new simulator or device replay claim.

## Previous checkpoint: complete Home result payloads are recorded

`Recplay::result` now encodes the addressed Home result at the recorder's own frame origin.
`pms/record.rs` preserves the whole source projection, including Continue Watching sort keys,
provider hub keys, all movie/episode fields, failure versus empty success, and the request's
generation/sequence/server/token-generation tags. Blur colours encode IEEE bits so JSON cannot
erase negative zero, NaN payloads or infinities. The payload version is explicit and strict;
missing optional fields are refused rather than being silently interpreted as null.

Client resources are represented by immutable logical instance generations, NOT pointers,
origins, tokens or Plex device identifiers. The live landing still carries its original client
reference and uses the unchanged pointer/token guard. The decoder requires an explicit mapping
from recorded client id to the corresponding client resource, including superseded instances;
it refuses unresolved ids and wrong-server mappings and preserves stale token tags verbatim.
That mapping and replay bootstrap are NOT yet wired in the product. Replay still uses live
adapters, and full store initialization, suppression and result injection remain required.

The result vocabulary deliberately changes `state_fp` from `0x5ba434add5db5bdf` to
`0xe61b6d55f4428637`; a regression verifies refusal of the previous shape. No anchor was imported,
replaced or rebaselined. A fresh synthetic simulator flow 1 passes at
`/tmp/plx-phase8-result-record.bdPJ9d/`: 579 frames, 10 inputs, 2913 effect tags, **1 complete
result**, 3 lifecycle records. The mock and simulator exited. This is capture-path evidence,
not a same-build replay or native rendering claim.

The synthetic-alphabet gate initially refused the newly exposed payload strings. A RED/GREEN
Python test accompanies the narrowly added mock path grammars, finite codec/rating/hub constants,
dates and exactly 24 synthetic summary words. Household prose, named media paths and credential
query suffixes remain refused. The fresh recording now passes the privacy gate.

Full host check passes (default 2607 / hostsim 2628 passed, 3 ignored each, plus lint and CI),
as do shipping-feature, ARM and simulator builds. The final targeted codec tests and Python
harness also pass. Logs: `/tmp/plx-phase8-result-codec-*`, `/tmp/plx-phase8-result-alphabet-*` and
`/tmp/plx-phase8-result-record-privacy.log`. No TV or main-branch changes.

## Previous checkpoint: addressed Home results reach the product dispatcher

The production bridge drains Home's worker results at dispatcher ingest, orders them by the
store-wide request id, and delivers each as `AppMsg::HubsResult` to the Hubs store. The store
refuses messages addressed to another ordinal. Request ids now come from one Hubs sequence rather
than per-server counters, which previously aliased two servers' first requests. Profile reset
does not rewind it while old workers can still be running.

Arrival application and retry ticking share the existing generation/client/token guards and
merge path, but are separate operations. The owned Hubs machine's tick no longer drains live
results; arrivals can land when Home is covered or absent without advancing its retry clock or
starting a new hubs fetch. Legacy callers retain their combined pump during migration. The
frame retains its captured Hubs view; a newly committed catalog is published on the next capture.

Host regressions exercise the actual bridge result tap, exactly-once ingest, wrong-store refusal,
failure retention, mailbox isolation from ticks, and request uniqueness across sources/retries/
profile resets. This is NOT yet recorder payload encoding: `Recplay::result` still has its default
no-op, and replay still needs the codec, injection, adapter suppression and full initialization.
The request sequence and source table still live in the legacy PMS backing state; their move into
an owned, canonically encoded store remains required. No existing anchor was replaced.

Full `make check` passes (default 2604 / hostsim 2625 passed, 3 ignored each, plus lint and CI).
Shipping-feature and ARM cross-build checks pass. Gate logs are
`/tmp/plx-phase8-addressed-{check,shipping,arm}.log`. No television access or main-branch write.

## Previous checkpoint: Home mailbox capture separated from application

`pms::pump` now delegates to a single batch-application path with a supplier invoked after roster
reconciliation. `take_landings` only transfers the worker mailbox; it does not mutate the catalog
and releases its lock before application. Existing generation, sequence, client and token checks,
retry scheduling, merge ordering and failure-only repaint behavior remain on the common path.

Two host regressions prove capture does not apply data, draining transfers a batch only once,
applying it leaves later worker arrivals queued, an empty supplied batch does not drain live work,
and a captured pre-reset batch cannot replace the new identity's catalog. This is an internal
seam, NOT result serialization/injection in the recorder or suppression of live network work.
Those and complete initialization remain the next work; no new replay closure claim is made.

Full `make check` passes (default 2601 / hostsim 2622 passed, 3 ignored each, plus lint and CI);
shipping-feature check and ARM cross-build pass. Logs: `/tmp/plx-phase8-home-batch-{check,shipping,arm}.log`.
No TV access. The user's morning instruction supersedes the earlier night rule: leave the
backlight ON, with audio muted, until newer direction.

## Previous checkpoint: product dispatcher observer connected

The production loop now calls `bridge::frame_with_tap` with its owned `Recplay`; only the host
test convenience wrapper supplies `NoTap`. A RED/GREEN test over the real AppHost bridge and
MemSink proved the supplied observer was previously discarded, then verified drained effect
records, Mount/Enter lifecycle records, a state record, and the recorder's zero-based frame origin.
Unarmed and replaying observers return before formatting or timing work.

The observer records library effect TAGS and actual async addresses where present, plus lifecycle
names from the existing ScreenEvent vocabulary. It does not invent request ids for ordinary
deliveries. Frames with drained effects are now state-graded even without user input. This is
NOT complete application effect payload encoding, effect-stream comparison, result capture,
adapter suppression, or replay initialization; those remain required. The existing raw input and
present paths remain singular, rather than being duplicated through the dispatcher tap.

Fresh live synthetic flow 1 passes at `/tmp/plx-phase8-drain-record.aXV30T/`. Its recording contains
579 frames/state records/presentation records, 10 input records, 2912 effect tags, 3 lifecycle
records, and ZERO results. Synthetic-alphabet check passes; only six finite lifecycle protocol
names were added to the alphabet. No existing recording was replaced or imported as an anchor.
Simulator and mock processes exited. This is recording-path evidence, not closed replay proof.

Full host check passes (default 2599 passed / 3 ignored; hostsim 2620 passed / 3 ignored, plus
later CI), as do shipping-feature check and simulator build. Logs:
`/tmp/plx-phase8-recorder-tap-{check,shipping,sim}.log`. No TV access this turn.
Next: complete adapter payload codecs and route real worker arrivals through captured/injected
results, alongside replay initialization. Keep the full remaining migration/proof scope intact.

## Previous checkpoint: fresh replay exposes live-adapter dependence

Built the current simulator and recorded flow 1 into a fresh synthetic runtime at
`/tmp/plx-phase8-replay.7b9tUT/record/root-1/plxnative-recordings/latest`. The live smoke passes,
and the recording passes the synthetic-alphabet check. It contains 581 frames, 10 input records,
5 state records and 581 presentation records, but ZERO effects, results or lifecycle records.

Same-build replay with the seed-1 mock PMS running at the original endpoint reports
`graded=5 diverged=0 present_diffs=0 verdict=SAME`. Replaying the same artifact with no server
listening reports `graded=5 diverged=5 present_diffs=0 verdict=DIVERGED`. Neither replay reported
an initial-probe or trigger mismatch; this is not proof that full store initialization was equal,
because that probe does not capture it. First divergence: frame 125, expected
`0xd418c52053b9c67a`, offline `0xa3a0ac5d0cf36eba`, two input records.
Logs are under `play/plxnative-events.log` and `play-offline/plxnative-events.log` in the same root.
Both simulator processes exited and the mock server was stopped. No TV access occurred.

Concrete missing wiring: `ui::rec::Writer` exposes `effect` and `result`, but the application
`Recplay` only records ticks/inputs/presentation/event-frame state; `bridge::frame` runs with
`NoTap`. `stores::hubs::pump` still invokes the live PMS pump, whose worker mailbox supplies data
outside this recording. The online SAME result is therefore live-assisted evidence, not the
closed adapter replay/adoption proof required by the plan. Do not import it as an accepted
anchor or retire legacy Home on its strength. No existing fixture was changed.

`plxnative-rec info` now exposes input/effect/result/lifecycle counts, with a RED/GREEN Python
regression proving counts and non-disclosure of payload strings. The full Python harness suite
passes (`/tmp/plx-phase8-replay-harness-check.log`). No Rust behavior changed in this checkpoint.
Next: implement complete replay initialization and captured/injected adapter results, then repeat
the same online/offline comparison and the pointwise resolution gate. The full remaining plan
scope is unchanged; this is a discovered implementation gap, not an external blocker.

## Previous checkpoint: capped navigation and removed-item recovery

A mounted Home/dispatcher test now walks every shelf of both a full 16-shelf catalog and an
oversized offered catalog, checks repeated DOWN at the final shelf, and proves invalid addressed
row/column requests leave focus unchanged. This exercises the published catalog and real engine,
not just the cap arithmetic or a manually appended projection.

The remaining removed-item policy difference is fixed. The real return regression failed at
old row 1 / column 2, recovering to column 0. Each registered item now remembers its last published
row/column. A surviving identity still wins; a removed item falls to its clamped old column in the
same hub, or the clamped old row when that hub disappeared. This is metadata for every item, not
a duplicate active cursor. Return memory carries it across eviction, and restoration reapplies
the captured positions before projecting current data.

The regression passes for middle-item deletion, last-item deletion, whole-shelf removal and
reorder-before-deletion, each with a retained and CAP-evicted Home. The last case proves positions
are refreshed when an existing identity moves rather than remaining at allocation order.
Both live-state and return-memory encoders include the fields; new state pin is
`0x5ba434add5db5bdf`, and the preceding `0x72524cf7ed8d97a3` is explicitly refused.

Full `make check` passes: default 2598 passed / 3 ignored, hostsim 2619 passed / 3 ignored,
plus subsequent CI. Shipping-feature check and ARM cross-build pass. Logs:
`/tmp/plx-phase8-home-recovery-final-check.log`, `/tmp/plx-phase8-home-recovery-shipping.log`,
`/tmp/plx-phase8-home-recovery-arm.log`. No deployment or new native proof for this delta.

Next Home adoption gate is fresh recording/replay evidence, without overwriting older anchors.
The recorder's live-store dependence and missing pointwise resolve grading remain important
proof gaps; a green live navigation smoke is not a substitute. Keep legacy Home until the
adoption evidence supports retirement. MasterDetail/Library/Search/keyboard and phases 9–12
remain unchanged in scope.

## Previous checkpoint: substantive Home assertion parity

Restored the full legacy assertion bodies for wash bounds/interpolation/overshoot, prefetch
singleton/count/range/uniqueness, outgoing-art coverage, orphan-episode logo fallback, resume
caption/bar agreement, and source-text flow offsets/advance/ink/budget. The port changes only
test helper naming and the `c_int` alias to `i32`; these assertions now exercise the owned Home
helpers. Also restored unscaled/scaled card-center and non-shrink checks, chip/top-band equality,
populated Ready/Failed readout suppression, no-snap bounds, and every strip action bypassing Retry.

New real-container coverage walks chip → Home → every present type → Search and back, including
both endpoints, for all four Movies/Shows compositions. It found a latent boundary bug: geometric
RIGHT escape from the final tab could seat a page control. LEFT/RIGHT now explicitly Stop;
UP/DOWN retain geometric page transitions. Observed RED before the policy change and GREEN after.
A real application dispatcher test delivers LEFT-at-Play and repeated RIGHT-at-Info, checks the
carousel changes while control focus remains, and confirms chevron/dots never register hit stops.
The four-library/two-server Browse fixture now passes through ChromeSnapshot and proves exactly
two type destinations, with chip/Home/Search in the proper order. Both hero keys project Away.

Full `make check` passes: default 2596 passed / 3 ignored, hostsim 2617 passed / 3 ignored, plus
later CI. Shipping-feature check passes. Logs:
`/tmp/plx-phase8-home-parity-complete-check.log` and
`/tmp/plx-phase8-home-parity-complete-shipping.log`. No schema field change, fixture rebaseline,
or new device run occurred. Earlier native captures do not independently prove this endpoint fix.

Remaining retirement blockers: removed-item recovery policy (first surviving item versus legacy
slot clamping), real capped/oversized-catalog navigation and invalid-row command recovery, plus
fresh replay/adoption evidence. Legacy Home remains compiled until those contracts are settled.
MasterDetail/Library/Search/keyboard and phases 9–12 remain part of the same goal.

## Previous checkpoint: focused-tab recovery and first native Home check

The parity audit found a real removal-policy regression: the container recovered a withdrawn
focused type tab to the first visual member (the profile chip), whereas legacy Home recovered to
Home. `TabContainer::strip_fallback` now declares that policy independently of visual order;
Home publishes its Home key. The regression primes the first Home Tick before simulating the
previously published Movies member, so initial CTA seating cannot mask the case. Observed the
primed test fail on Chip with the fallback disabled, then pass with the policy applied.
Fallback participates in the navigation hash; new pin `0x72524cf7ed8d97a3` refuses the old pin.
Full host gate passed: default 2593 / hostsim 2614 passed, 3 ignored each; shipping and ARM build
pass. Logs: `/tmp/plx-phase8-tab-policy-{check,shipping,arm}.log`.

The owner approved waking the TV, then explicitly overrode the night panel rule: it is morning,
do not turn off the backlight. The TV was woken through lock acquisition. No screen-off command
was issued. Audio was muted via the device-inventoried `com.webos.service.audio/setMuted` API,
and independent `getVolume` readbacks before the run and after handback confirmed mute with the
volume unchanged. The debug install and dev feature set were verified from the boot log; local
and deployed binary bytes matched (MD5 `ca5bf89f6edd7f28456869246e354c96`).

Opened and inspected six native DISPLAY captures at 1920×1080 under the private local directory
`/tmp/plx-phase8-native-home.9Y7F7B/`: `hero.png`, `grid-row0.png`, `grid-row1.png`, `detail.png`,
`grid-return.png`, `itemmenu.png`. Observed: hero/logo/synopsis/controls; DOWN to both shelves;
regular grid item → owned Detail → BACK preserving the selected card and viewport; held press
opens ItemMenu, with its originating card lifted above the dimmed host. No playback or watch-state
action was selected. These are real-library/private artifacts and must not enter public fixtures.
This is a targeted native UI check, NOT the full Phase-8 eleven-scene/performance/cross-target
replay proof. No FPS claim is taken from these captures.

The first `up` reported a false deploy mismatch: it cached the local checksum before `make deploy`
rebuilt the artifact. Direct comparison proved the new local and remote bytes matched; rerunning
`up` then verified and launched them. This driver defect remains to fix with a host regression.
Handback completed: injected token/triggers cleared, normal interactive debug boot restored,
lease released, backlight left alone/on, audio still muted. Stable install was not modified.

### Remaining Home parity and the next component boundary

Read-only worker audit at `b455337f` compared all 34 legacy tests against actual replacement
assertions. Same names are NOT assertion parity. High-priority missing assertions remain:
wash dark/bright per-channel bounds and halfway/overshoot cases; prefetch singleton/range/unique
bounds; incomplete outgoing art; orphan-episode logo fallback; positive resume-bar condition;
source-flow exact offsets/advance and worst-case quarter-line budget. Real strip traversal across
all type compositions, delivered pager edge keys, capped-catalog navigation and status escapes
also need stronger fixtures. The old Home module is deliberately not deleted yet.
Removed-item fallback still chooses first surviving item rather than legacy slot clamping; decide
and test the intended parity policy before retirement. The removed-tab finding is fixed above.

MasterDetail remains unimplemented. Its concrete Library seam is `enter_rail`/`rail_jump`:
toolbar or grid → rail seats from the displayed grid item; UP/DOWN follows the letter without
moving focus out of the rail; LEFT returns to the source GROUP, not always the grid. The rail is
excluded from vertical geometric entry and disappears with grid readouts. Projection needs the
engine-owned remembered detail cursor; do not recreate the legacy GR/GC/RAIL_F focus copies.
The component must provide Part drawing/prepare and Focusable delegation, not only a door helper.

Remaining plan scope: this parity/replay work, MasterDetail/Library/Search/keyboard, broader queued
effect audit and phases 9–12. Native evidence above does not complete those gates or trunk squashes.

## Previous checkpoint: budget-carried press identity

The outstanding carried-commit concern is now reproduced and fixed. A fixture test fills the
real pre/post drain budgets, lets release become queued work after its arm retires, changes
focus, and drains the following frame. Before the fix it delivered the old commit to the new
cursor. The test now covers both hold and commit, with unchanged-target positive controls.

`PressEvent` and the queued `Delivery::Press` retain the original key alongside press id and
owner. At execution the dispatcher validates the live owner, instance, current key and placement
before handing the unchanged `ScreenEvent::PressHold/PressCommit` protocol to the screen. No
screen-local duplicate cursor or separate pending-gesture registry was added.

Canonical input now includes every arm field and the press-id allocator; queued press envelopes
include source, target, id, key, hold/commit kind and FIFO position. RED/GREEN coverage distinguishes
arm identities and equal-depth queues carrying different press targets. Other queued effect
payloads still need the broader dispatcher census; this is not a claim that the whole effect queue
is fully encoded. State pin is `0x702bf9f7e7c8fe57`; prior `0x76d41ddbe1726b88` is refused.

Full `make check` passes (default 2592 passed / 3 ignored; hostsim 2613 passed / 3 ignored,
plus later CI gates). Shipping-feature check and ARM cross-build pass. Logs:
`/tmp/plx-phase8-press-delivery-final-check.log`,
`/tmp/plx-phase8-press-delivery-final-shipping.log`, `/tmp/plx-phase8-press-delivery-arm.log`.
No new live-simulator, replay, or device proof is claimed for this delta. No TV commands.

Remaining: legacy Home contract parity/fresh replay and native gates, broader queued-effect audit,
Library/MasterDetail/Search/keyboard, and phases 9–12. Main and published releases remain untouched.

## Previous checkpoint: Home canonical input state and removed press targets

A RED/GREEN test proves the old hash collision: with identical engine focus, snap 0.49 activates
hero item 2 and snap 0.51 activates grid item 1, but their hashes were equal. Home now encodes its
snap position/velocity, hero slide/outgoing/direction, control-pop motion, vertical target and
velocity, per-row placement/reveal motion, current row/element projection and projected generation.
Textures/backdrop paint caches and spinner phase remain excluded. Shared motion writers and Home
use exhaustive field destructuring; tests pin shared array extents and distinguish motion/projection
changes. The shape no longer falsely lists query fields that its encoder does not write.

New state fingerprint: `0x76d41ddbe1726b88`, pinned only after the prior pin failed and the Home
census was checked. Previous pinned `0x8af1d09ebbb11d47` is explicitly refused. No replay fixture
was overwritten or rebaselined; old-schema recordings are not fresh product proof.

A separate real-dispatcher RED/GREEN regression presses item 1, removes it, reconciles to the
fallback and releases. Reconciliation now cancels the active gesture instead of playing item 2.
This test does NOT prove a commit already queued across the dispatcher work budget; that needs
its own regression/identity-delivery audit. The queue's depth-only canonical encoding and the
input arm's abbreviated encoding also remain broader dispatcher audit work, not fixed by Home's
census.

Full `make check` passes: default host 2590 passed / 3 ignored; hostsim 2611 passed / 3 ignored,
plus the subsequent CI gates. Shipping-feature check and ARM cross-build pass; the ARM compiler
emits the existing unstable `neon` target-feature warning. Logs:
`/tmp/plx-phase8-home-canon-final-check.log`, `/tmp/plx-phase8-home-canon-shipping.log`, and
`/tmp/plx-phase8-home-canon-arm-build.log`. No deploy, device run or new native proof occurred.

Remaining scope is unchanged: carried-press identity, legacy Home contract parity, fresh replay
proof and remaining device gates, then Library/MasterDetail/Search/keyboard and phases 9–12.

## Previous checkpoint: Home drawn geometry and live navigation

Hero placement now uses the incoming slide offset for Drawn queries; SpringTarget stays at the
destination. Home's control pop uses captured `PressRead` through `CtlPop::scale_with`, not the
legacy global press. Cards share one drawn-geometry function between paint and placement, so
press dip/bounce affects the hit rectangle while the resting/menu anchor remains unpressed.
Status actions deliberately use their fixed `StatusOverlay` bounds, without stale hero pop/slide.
Regressions for hero slide, card press and status bounds were observed failing before their fixes
and passing afterward; a real hit-map test checks the moved hero center hits and its old center
misses. No legacy caller of `CtlPop::scale` was migrated implicitly.

`make check`: lint passed; default host 2586 passed, 1 existing schema-pin failure, 3 ignored.
Shipping-feature check, simulator build and separate dependency gate pass. The failed pin stops
the later hostsim/CI stages, so this is not a full-check green claim. Pin is not rebaselined.
Logs: `/tmp/plx-phase8-home-geometry-{check,shipping,sim-build,deps}.log`.

The strengthened live simulator flows 1 and 2 pass (zero skips): Home → chip → hero → row 0 →
row 1, and grid → Detail → BACK. Artifacts:
`/tmp/plx-phase8-home-geometry-flow.E2LMeg/{1-boot-home-chip-grid,2-grid-detail-back}.fp`.
This supersedes the old false-positive flow-1 artifact. It is not pointwise product replay proof.
Opened and inspected `/tmp/plx-phase8-home-geometry-shot.hIN0GT/home.png`, authored 1920×1080:
settled grid, enlarged selected card, caption/resume bar, shared header and next shelf are coherent.
Synthetic colored art only; no native rasterization/performance or full animated visual parity
claim. All simulator/mock processes for these checks exited. No television command was made.

Next: canonical activation-state audit, pending-press removal, full legacy-contract parity,
remaining simulator/replay and native gates, then the rest of Phase 8 and phases 9–12.

## Previous checkpoint: Home restore review corrections

Parent/Astra review now covers the real Home return lifecycle, not just a copied memory struct.
The integration regression selects row 4 / column 18, moves left to column 14 to retain a
non-minimal horizontal viewport, pushes one page or past CAP, then pops back. It covers both
unchanged data and a hub reorder while covered, and checks first-frame visible hit geometry.
Observed failures before fixes: hero mode with grid focus; horizontal position shifted by 1120 px;
and a reordered hub's selected card registered at y = -870, outside its clip.

Fixes: `FocusEngine::enter` delivers reveal even for an unchanged retained key; Home remembers
group-keyed row offsets and vertical offset, clamps/reveals the reconciled item before first draw,
and reserves instant snap for actual memory restoration. Fresh hero reseating still animates.
Row render state now follows hub identity through live reorder. Pending offsets survive empty
loading publications and are included in another return-memory capture while loading.
Viewport/pending-restoration fields are included in canonical data and shape descriptions.

The unchanged/reordered retained/evicted return regression passed. Reorder and loading-offset
regressions were observed RED/GREEN, including the repeated-eviction loading extension.
Astra's read-only follow-up found no immediate defect in the narrow reveal correction. This is
host evidence, not native visual proof. The final shipping-feature and dependency gates pass.
The schema pin is intentionally still unchanged pending the broader activation-state audit.
Final `make check`: lint passed; default host 2583 passed, 1 schema-pin failure, 3 ignored.
The failure stops later hostsim/CI stages; `ci/check-deps.sh` was run separately and passed.
Logs: `/tmp/plx-phase8-home-restore-final-{check,shipping}.log` and
`/tmp/plx-phase8-home-restore-deps.log`. No full-check or replay green claim is made.

Remaining Home work: translated/scaled hit geometry, canonical activation-state correctness,
catalog removal during a pending press, substantive legacy test parity and simulator/replay gates.
The final worker already removed the fake `(0,0)` draw fallback; review the visible-focus policy
against its actual implementation before reporting that older finding again. Native verification
and the rest of Phase 8 / phases 9–12 remain. No device access occurred.

## Previous checkpoint: owned Home mounted; review corrections underway

Home is mounted on `codex/ui-phase8` at `e94dbc66`, including worker final
`de3519a8` by cherry-pick. Application chrome, addressed Home commands, focus probe and opener
redraw now use the owned screen. The old `ui/home.rs` remains deliberately until substantive
legacy-contract parity is demonstrated; it is not the production Home implementation.

Parent review corrections in the working tree:
- Frame views are captured once before input; later splits retain the same publication. The
  mid-frame reorder/click regression protects the identity of the item the user actually saw.
- Pointer controls use the same control/card hold policy as keyboard input. The new regression
  passed, as did the complete focused input suite.
- A partly visible focused row keeps neighboring cards hoverable; other partly visible rows do
  not steal focus. Observed the new regression fail before the change, then pass afterward.
- Removed the obsolete legacy Home boot initialization. Shipping-feature check passed again
  after the hover correction.

Astra acceptance issues still open: cold/evicted viewport restore, translated/scaled hit geometry,
BACK-fold visual selection, and canonical state excluding activation-relevant spring state.
Catalog removal during a pending press needs a separate regression; reorder alone does not prove it.
The strengthened flow-1 checker rejects the earlier false-positive simulator trace; rerun on the
latest binary. Fingerprint pin/anchors need a final schema census, not blind rerecording.

Simulator captures established basic Home/grid layout and reproduced then corrected the dim
item-menu opener. They are not native visual or frame-rate proof. No device commands were issued;
all eventual device runs must keep backlight and sound OFF, including FPS/capture runs.

The worker has stopped implementation; parent owns remaining Home corrections. Its worktree and
external build tree remain, so fleet teardown/GC is not complete. Two accidental derived build
directories were moved to Trash (recoverable), not source files. Latest recorded free disk: 16 GiB.
Library/Search/MasterDetail/keyboard and phases 9–12 remain; this is not Phase 8 completion.

Latest `make check`: lint passed; default host tests 2577 passed, 1 failed, 3 ignored.
The failure is `app::recorder::tests::the_init_probe_is_synthetic_and_the_shape_is_pinned`
(actual 11018145524105529622, previous pin 10012012826793680199). The gate stopped there;
later hostsim/CI stages were not run. Do not claim full check green or update the pin before
the canonical-state review. Log: `/tmp/plx-phase8-home-review-check.log`.
The local simulator and mock PMS sessions were stopped, and the completed worker was closed.

## Historical checkpoint: Home integration in progress

The newer work below supersedes the prerequisite-only status of the earlier sections:

- `24df08b4` publishes catalog, hub ranges and hero slots in one immutable `Arc`. A retained
  `HubsSnapshot` survives commits/reset; `HubsView` borrows it. `Bridge::split` refreshes the
  snapshot, including status, without cloning movies or strings per frame. A RED/GREEN regression
  fixed generation advancement on optimistic and roster commits (previously only some callers
  bumped it). PMS's provider listing key is preserved as a tagged, server-scoped identity fallback.
- `StoreWork::{Hubs,BrowseDiscovery}` is addressed through the same dispatcher drain as commands,
  without inventing a generation change on idle polls. Astra found no introduced correctness or
  lifetime issue in this commit. Existing caveat: failure-only hub landings do not raise a store
  notice, so Home must read snapshot status on Tick too, not cache it solely by catalog generation.
- `ae4f038c` adds captured-data rendering APIs for shared tabs and the profile chip. Compatibility
  wrappers remain for the legacy routes. The application still needs to own and supply the shared
  chrome snapshot when Home's actual draw path is wired.
- `61ca43f6` replaces positional strip rectangles with keyed `StripMember`s (drawn/target/clip),
  preserves focus through removal/reordering, includes strip keys in the canonical hash, and
  registers the container's pointer stops. Astra caught covered strip hits reaching a modal;
  a RED/GREEN regression now checks that only the active entry's hits resolve and outside-click
  dismissal still works. Existing Detail/Legal hit tests supply their owner explicitly.
- Home's registry contract is integrated (`e1b6ccaf`, cherry-pick of lane `1a0b5311`). `7a2f8d7b`
  wires semantic actions, original instance/return memory, source-scoped item lookup, legacy
  playback/detail dispatch and menu anchoring. The Home body is NOT yet mounted, and its opener
  geometry and lifted redraw still require checking against the completed screen implementation.

Current host unit suite: **2518 passed, 3 ignored**, repeated in an isolated runtime root.
The focused input suite passes 25 tests. Earlier `make check` at the captured-bar checkpoint
passed default2512/hostsim2533 (+3 ignored each). Do not extend that result to the latest work:
the shipping/lint gate currently reports unused Home contracts until their screen consumer lands.
No warning gate was weakened to call that integration complete.

The current intermediate fingerprint is `0x51aca85cc16b4b59` (Home memory + strip keys); its test
pin was updated after observing the old pin fail, and immediately preceding shapes are refused.
Committed product anchors still carry the previous schema. They must be rerecorded and replayed
after Home's final logical shape lands; current product replay is NOT claimed green.

One implementation worker remains active in `.claude/worktrees/phase8-home-owned`, branch
`fleet/phase8-home-owned`, cut from `24df08b4`. Its registry checkpoint was integrated; the owned
`screens/home/` body is still in progress. Parent owns application integration and shared chrome.
No device access is granted to the worker. Native checks, ARM/simulator rebuilds for this latest
integration, and the rest of Phase 8 remain outstanding. The full phases 8–12 objective is intact.

## Integration base

`main` at `6745ca98` was merged into the bridge integration branch at `1a063831`, preserving the
capture benchmark and its host tests, the ABI query fix, and the owned profile-session integration.
The competing Profiles implementation was reconciled rather than discarded wholesale:

- Two new tests reproduced stale exposed PIN state after edits and closure, without an intervening
  Tick. PIN handlers now publish those changes immediately.
- Main's queued opening re-seat, topology/hole, roster/footer navigation and empty-roster cases
  were carried over using the production focus query view and engine.
- The Profiles module remains registered in shipping builds, not merely under `cfg(test)`.
- The focused Profiles suite passed (28 tests); the complete `make check` passed after merging,
  including main's capture-benchmark tests. Main itself was neither changed nor pushed.

Phase 8 work is on `codex/ui-phase8`, in the same worktree. Phase 6/7 trunk squashes and native
verification remain pending; this branch retains their integrated code as its dependency base.

## Implemented prerequisites

- `NavPresentation` is captured once per dispatcher draw pass and passed through `DrawFrame` as
  page/chrome alpha, pending tab selection, and blur amount. Surfaces still use their own appear
  alpha. A failing propagation test was observed, then passed after wiring; it also checks that
  drawing does not mutate logical state.
- Home hub identities are exposed by the PMS view as provider identifier plus server namespace.
  The merged Continue Watching group is source-independent. Titles, owner labels and row positions
  do not define identity. Tests cover source collisions, renaming/reordering, and merged-deck order.
  A missing provider identity is explicitly absent, not a supposedly stable positional key.
- `Effects::remember(group, elem)` lets a master/detail controller update the engine's remembered
  selection without moving current focus. It is scoped to the active emitting instance, validates
  group membership and placement, and rejects covered sources and invalid/non-instance requests.
  A regression exposed the reserved strip range accepting a nonexistent member; placement
  validation fixes that. The engine's canonical state includes the remembered selection.

These are prerequisites only. Home, Library and Search have NOT yet been moved to owned screens.

## Review follow-up

Astra found two nested Settings boundaries that the direct dispatcher fixtures could not see.
Both were reproduced before fixing them:

- Covered inner pages could emit `Remember`, which the forwarding surface re-stamped as its own
  active instance. The forwarder now checks the original inner instance against the current top
  before forwarding a focus projection. Covered emissions are dropped; active ones still pass.
- Inner draw frames copied page alpha but reset chrome alpha, pending tab and blur to defaults.
  The actual nested-page draw path now carries one complete snapshot through all three frame
  construction sites. An injected GL-free screen records values at rest after push/pop, mid-push,
  and mid-pop; it also checks page order and that drawing leaves the logical hash unchanged.

Parent review checked the extraction and cascade preservation. Existing Settings scrim/entrance
reads of legacy page alpha remain; migrating the remaining navigation reads is still required.
The stale bridge comment claiming `Host::Memory` was `()` was corrected against its actual
`PageMemory` implementation. No device work or main-branch writes occurred.

## Host-side checks

At `617b7069`, `make check` passed (default 2503 +3 ignored; hostsim 2524 +3 ignored), the shipping
no-default-features check and simulator build passed, and anchors 1/6/12 replayed SAME:
575/1218/926 frames, 5/9/4 graded, zero state or presentation differences.
The review follow-up also passes the ARM cross-build, simulator build and shipping-feature check.
The follow-up `make check` passed (default 2505 +3 ignored; hostsim 2526 +3 ignored), including
the lint and Python gates. Repeated anchors 1/6/12 retained the same frame/graded counts and SAME
verdicts, with zero state or presentation differences. These checks do not prove TV pixels or FPS.

## Required remaining work

1. Freeze and implement the real store-view contract in `AppViews` and the effect-driven write/pump
   path. Owned screens must not keep synchronous store mutations or pumps in their Tick handlers.
2. Implement reusable `MasterDetail` over real focus groups, with `Follow::Live` / `OnCommit`,
   projected seats and a group-only return door. The grid and rail must not acquire screen-local
   focus copies. In particular, entering the rail from the toolbar still needs a projection from
   the engine's remembered grid item; settle that contract before porting the ladder.
3. Add whole-run UTF-8 text commits and system-keyboard ownership/adoption/withdrawal semantics.
   Preserve prediction replacement, caret boundaries, account-cover commit versus page withdrawal,
   and profile-isolated recents. `Insert(char)` alone is insufficient.
4. Migrate Home: owned hero carousel/snap/render state; engine-owned hero/hub/strip focus; stable
   hub/item identities; real container strip; preserve the existing 34 regression tests through
   actual engine/hit-map assertions. Unknown provider hub identity needs an explicit fallback.
5. Migrate Library: remove its zone/row/column/rail focus globals and manual hit arrays. Keep
   deferred semantic transactions and epoch refusal, sparse-slot identities, same-item restore,
   per-section Browse bookmarks and scroll, and engine-owned page menus. There are 67 existing
   tests to preserve, including readouts, rail doors, dynamic shelf identity and pointer geometry.
6. Migrate Search and its application-specific submodules; remove `Zone`/`Below` and positional
   focus/hit ladders. Its 83 tests cover field editing (31), screen state/navigation (30), results
   (10), recents (8), and empty/readout behavior (4).
7. Rewire callers in `app/{input,run,nav}.rs`, `focusprobe.rs`, and generic overscan checks in
   `ui/consts.rs`. Library/Search draws consume `DrawFrame` presentation rather than live `ui::nav`
   reads. No wrappers around the retired ladders, and no sibling-screen imports.
8. Update canonical arguments/memory/shape census and adoption recordings; run host/shipping/ARM,
   simulator visual/navigation checks, and the required native scenes under the night rule.

The earlier product replay limitation remains: explicit pointwise `--resolve` grading and
adapter-independent product playback are not wired merely by adding these prerequisites.
Phases 9–12 remain in the original plan; none is declared complete by this checkpoint.

## Next implementation seam: Home

Read-only worker mapping recommends a copyable, frame-borrowed `HubsView<'a>` in `AppViews<'a>`:
generation/state plus private references to the committed catalog, hub ranges and hero slots.
Expose `hub_count`, `hub(i)`, `hero_count`, `hero(i)`, `state` and `generation`. Hub references
carry identity/title/source and an item slice; hero references carry an item and source. Reuse
`PmsMovie` references, with no per-frame media/string cloning. `AppViews` is currently zero-sized.

The shared bar also needs injected Browse tabs and a published profile-chip DTO; otherwise Home
draws still indirectly read Browse/session through `widgets.rs`. Route-ground seeding also reads
Home's hero and session today. These shared reads must not be hidden behind a new Home wrapper.

Home's direct work entrypoints are the Hubs and Browse discovery pumps in `home_update`, and
the status Retry command. Keep `HubsCmd::Retry`; route ticking through store-work effects to the
existing pumps, without artificially bumping generation every frame. Boot/refetch and optimistic
view-state edits retain their existing authority. Freeze this effect contract before migration.

For identities, preserve the provider `Hub.key` currently discarded by `pms::Shelf`. Use distinct
merged-Continue-Watching, server/identifier and server/key variants. Neither localized title nor
row position is a stable fallback; both provider fields absent is an explicit unresolved case.

Callers to retire live in `app/{input,run,nav}.rs`, `focusprobe.rs`, and route-ground seeding in
`ui/route_screen.rs`. The next step is this real Home slice, not another generic-only completion
claim. This mapping is a recommendation; its contracts have not yet been implemented or verified.
