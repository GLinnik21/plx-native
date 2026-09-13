# Stores as machines — restructure phase 4

R2B-E endpoint recovery: `stores::apply` returns a `StoreOutcome` containing the existing
`changed` verdict and a bounded, deduplicated set of endpoint requests in first-observation
order. Hubs failure/refetch/retry, Browse discovery and ViewState's hub refetch propagate that
set to their callers. Generic store steps use the layer-neutral `StoreEffectHost`; Bridge and
Onboard translate requests to `AppFx::Session(RequestEndpoint)`. Boot/run accumulate outcomes
locally and share the temporary app-side Session command executor with Bridge. Data modules
no longer execute auth recovery directly. Physical Session ownership remains the next R2B
package: the temporary executor still calls the existing auth controller.

The design note for spec v4 (`ui-plxnative-structured-phoenix.md`) phase 4, written from the
code rather than from the spec's sentence, because the sentence hides four decisions the tree
forces. Read `rust-modules/src/stores/mod.rs` for the vocabulary; this is the reasoning.

Browse retirement Wave 2 completed the destination contract:
`BrowsePublications` is the retained directory/listing/section-hubs aggregate,
`Stores::capture_browse` captures it from one owner borrow in directory-first order, and
`Stores::{browse_run,browse_discover_pump}` plus the matching Bridge methods are explicit,
synchronous owner paths. There is no active selector, bootstrap-adoption token, global Browse
publication, legacy adapter or free mutation/read facade. `ci/check-deps.sh` enforces that zero
surface directly; the migration allowlist was deleted when its count reached zero.

## 1. What a store is, today

Six data modules own the application's server-derived state: `browse` (the Library table and its
per-section paged listing), `pms` (Home's hub catalog), `metadata` (the detail page's item,
seasons and episodes, the playing item), `search`, `person` and `viewstate` (the view-state
WRITE queue). Browse is the exception to the older global shape: each `Bridge` owns one
`BrowseStore`, whose `BrowseState`, `BrowseAdapter` and notice generation are per-instance. The
adapter holds Browse's page, genre, letter, source-discovery and section-hub mailboxes and
single-flight flags. The other five still use compatibility globals and mailboxes, with a worker
spawned through `task::spawn_small`, generation atomics that supersede a late landing, and a
once-a-frame pump in their existing callers.

The census that sized this phase (2026-09-07): browse exports 87 `pub(crate) fn`, metadata 51,
pms 24, person 21, search 17, viewstate 4. Of those, the MUTATORS a screen calls directly are
exactly these (file: callers):

| store | mutator | callers outside the store |
|---|---|---|
| browse | `BrowseCmd` (including `Reset`) | owned screens emit `AppFx::Store`; `app/bridge.rs` delivers the command to that Bridge's `BrowseStore`; synchronous app boundaries call `Stores::browse_run` on an explicit aggregate |
| browse | `StoreWork::{BrowseDiscovery,Browse}` | Onboard schedules the roster-only owned pass; Library schedules the full owned landing pass; `app/bridge.rs` delivers both to the addressed Bridge's `BrowseStore` |
| browse::section_hubs | `kick`, `commit_staged`, `invalidate_all`, `set_watched_local`, `left_the_deck` | Library mutations are carried by `StoreCmd::Browse`; the BrowseStore owns the section-hub adapter and its per-section state |
| viewstate | `request` | `app/input.rs`, `screens/detail/mod.rs` |
| person | `open`, `close`, `pump` | `screens/person.rs` |
| metadata | `request_detail`, `load_detail_now`, `clear`, `load_season`, `set_now_playing`, `set_watched_local`, `pump_season` | `screens/detail/mod.rs`, `app/{input,playback,run}.rs` |
| metadata | `install_playing`, `mark_skipped`, `pump_detail`, `pump_alt_sources` | `route/decision.rs`, `app/{playback,run}.rs` |
| search | `set_query`, `reset`, `pump` | none direct — reached only through `StoreCmd::Search` from the owned `screens/search/mod.rs` (`ui/search/mod.rs` and `ui/search/recents.rs` are both deleted) |
| pms | `request_refetch_hubs`, `request_retry`, `reset`, `pump` | `app/{boot,run}.rs` (`ui/home.rs` is deleted; the owned Home emits `StoreCmd::Hubs(..)` and never a mutator — see the Phase 8 note below) |

The phase-4 census is historical. Browse's sole production owners are the `BrowseStore` values
inside Bridges. `screens/library/*` and `screens/onboard.rs` emit Browse effects, `app/bridge.rs`
delivers them to the addressed machine, and every fixture that needs mutable Browse data owns a
`BrowseStore` or `Stores`. Retained `DirectoryView`, `ListingView` and `HubsView` values are the
only cross-layer reads; the old free `crate::browse` publication and mutator functions are gone.

Phase 7 (2026-09-08) mounted Detail and Person from `screens/` and retired their old `ui/` files;
the table now names the live callers. Phase 8 (2026-09-09) did the same to Home and took two of the
table's cells with it: `ui/home.rs` is deleted, so it is no caller of anything, and `pms::pump` —
the "legacy callers' combined pass" it was the last caller of — is deleted with it, along with
`stores::hubs::pump`. The pms row is `request_refetch_hubs`, `request_retry`, `reset`, called from
`app/{boot,run}.rs`; the owned Home emits `StoreCmd::Hubs(..)` and never a mutator, and the store's
own `tick` is what a frame drives now. Filmography reads
the Person store and reacts to its notices but does not mutate it.

Every one of those calls is followed, in the SAME frame and often in the same statement, by a
read that assumes it took effect: `set_cur` then `kick_letters` (reads `cur()`), `set_query` then
the caret placed against `query()`, `set_sort_by_key`'s returned bool deciding `grid_reset()`,
`clear()` ordered before `request_detail` because it supersedes the generation the request is
about to establish. That fact is what decides §3 below.

## 2. What phase 4 makes true

1. **One vocabulary per store.** `stores::StoreCmd` is the complete, enumerated set of mutations
   — `Browse(BrowseCmd)`, `ViewState(..)`, `Person(..)`, `Metadata(..)`, `Search(..)`,
   `Hubs(..)` — and a store's `Machine::step` is the ONE place its mutation vocabulary is decoded.
   An owned screen emits `AppFx::Store(StoreId::Browse, StoreCmd::Browse(cmd))`; Bridge delivers
   it to its own BrowseStore, while synchronous application boundaries name their `Stores` owner.
   A screen names the Browse command vocabulary, never `crate::browse::set_cur(i)`;
   `ci/check-deps.sh`'s new `mutators` gate refuses the old spelling on every production line
   of `ui/` and `app/` (test modules are skipped by brace depth wherever they sit in a file);
   `ci/allow/mutators.txt` is EMPTY — an entry there would be a debt with a phase number. The
   player side (`route/plan.rs`, `route/decision.rs`, `player/`) already spells its two writes
   through the vocabulary and joins the gate's scope in phase 9.
2. **One notice.** Every command that changes observable state and every landing that changes the
   store bumps its generation and marks it dirty. `Stores::take_notices()` drains the owned Browse notice
   together with the remaining compatibility notices once per frame at `app/bridge.rs`'s drain
   point (right after NAV COMMIT), and `bridge::frame` delivers the aggregate as
   `Dispatcher::store_changed(ord, gen)` to every live instance. The owned Browse path also
   coalesces a captured publication change with that notice, so one landing produces one
   `ScreenEvent::StoreChanged`.
3. **The dispatcher path is real.** `AppFx::Store(StoreId, StoreCmd)` is the application's first
   effect: `app::bridge::Bridge` turns it into `Fx::Deliver(MachineId::Store(ord),
   Delivery::Machine(AppMsg::Store(cmd)))`. Its `Rig::deliver` branch steps the per-Bridge
   `BrowseStore` directly for Browse and dispatches the other stores through their compatibility
   machines. Screen effects and explicit synchronous owner calls preserve one command vocabulary
   without a process-wide Browse selection path.
4. **`Landing` reserves one terminal per exact admitted address** (spec §5.2, R2Q1 clarification).
   Both a per-addressee cap and a total cap bound running requests plus undrained terminals.
   `admit` returns typed `Duplicate` or `Capacity`; the requester handles rejection synchronously,
   without spawning or queueing a refusal. Unlimited rejected attempts cannot have a bounded
   queued answer each. OS spawn refusal AFTER admission remains a reserved `Refused` terminal.
   A full one-shot data lane atomically queues one `Dropped` terminal in arrival order;
   duplicate/unknown publications cannot complete a second request. `clear` discards queued terminals but only
   cancels running reservations: their workers queue sequenced, payload-free acknowledgements;
   reservations retire and cancellation drops are counted when the main thread drains or clears
   those terminals, which are never delivered to the addressee.
   Metadata propagates the admission outcome directly and settles a rejected new generation's
   spinner immediately. Drop counters include discarded terminals per canonical MachineId.
   **R2Q2 adds explicit streams on this same transport:** `admit` remains one-shot;
   `admit_stream` reserves one operation for ordered `progress` followed by exactly one terminal.
   `Landed::terminal` distinguishes the two; draining progress never retires admission. Stream
   `put` uses its reserved terminal slot even when the capped data queue is full. An overflowing
   `progress` instead closes the stream with one ordered `Dropped`; the producer must stop and
   cannot replace that partial-flow failure with later success. Queue storage is bounded by the
   data cap plus accepted terminal reservations. `cancel(addr)` discards only that operation's
   queued progress/terminal on the main thread, retaining running reservations until their
   ordered acknowledgement is consumed; `clear` applies this to all operations. A worker creates
   `completion_guard(addr)` inside its running closure: early return or unwind queues `Dropped`
   once, while an explicit terminal already queued/consumed makes guard drop a no-op. The caller
   still answers OS spawn refusal, because no worker guard exists when the closure never starts.
   This clarifies §5.2's one-answer rule for §2.3's multievent login protocol as one terminal per
   admitted stream, with progress explicitly nonterminal. Metadata remains strictly one-shot.
   The bounds count records/reservations, not bytes of unconstrained payloads: Session integration
   must enforce QR/HTTP/result payload limits. Session adapter wiring and full queued-payload
   canonical replay hashing remain mandatory subsequent work.
5. **The detail mailbox carries identity.** `metadata`'s one-slot `DETAIL_SLOT` becomes a
   `Landing` keyed on `(ServerId, ratingKey)`: a landing for a different server's item of the same
   number is skipped rather than installed, which is the gap the spec's evidence line names
   (`DetailResult` at metadata.rs:2021 carried no `(sid, rk)`).
   A wrong-key result is a discarded terminal: it releases its reservation but does not settle
   the awaited item's spinner. A subsequent valid success must belong to a newly admitted
   request; a second terminal for the discarded address is ignored.
6. **A store's step is O(result size).** `browse` sized a section's item vector to the listing's
   `totalSize` on the main thread when the first page landed — `Vec<Option<PmsMovie>>` of every
   item in the library, allocated in the drain. The store is chunked by PAGE now (`SecItems`): the
   outer vector is one slot per page, a page is allocated when its items land, and the
   missing-page scan is over pages rather than items.

## 3. The decision the spec's §14 sentence hides: explicit owner calls apply NOW

§14 says a legacy mutator becomes "a pure synchronous validation plus `queue(StoreCmd)`, so legacy
and migrated callers land in the same drain". Browse still has answers consumed in the same turn:
owned Library and Onboard screens emit `AppFx::Store`, `app/bridge.rs` delivers the command to the
owning `BrowseStore`, and synchronous boot/input boundaries call `Stores::browse_run` on the owner
they already hold. Both paths step on the main thread before the frame presents, and the aggregate
drain delivers that owner's notice to its live screens. Preserving that timing requires no global
selector or adapter; the other five stores retain their older compatibility arrangement until
their ownership slices land.

## 4. What is NOT in phase 4, and why

- **The other mailboxes stay.** Search's `SLOT[NSRC]`, `person`'s `FETCH[]`, `viewstate`'s
  `MAIL` and `pms`'s `RESULTS` keep their one-slot / per-source shapes. Browse's page, genre,
  letter, source-discovery and section-hub mailboxes are now fields of its per-`BrowseStore`
  `BrowseAdapter`, not process-wide `PAGE_RESULT`/`GENRE_RESULT`/`LETTER_RESULT`/`SRC_RESULT`/
  `HUB_FETCHING` state. Each remaining compatibility store is single-flight by construction
  (`FETCHING`/`IN_FLIGHT` bounds the worker count), so
  the backpressure `Landing` adds is a no-op for them today, and their supersede rules are keyed on
  generations the screens read. Browse is already stepped by `app/bridge.rs` for `StoreWork::Browse`
  and `StoreWork::BrowseDiscovery`; the other stores retain their compatibility paths until their
  ownership slices land.
- **The pumps stay where they are.** A route-gated pump moved to the machine's `Tick` would fetch
  behind the player, which `pms::pump`'s doc forbids for a reason. Browse's owned full and
  roster-only work events are both delivered by `app/bridge.rs`; the gate moves with the explicit
  owner that drives the work.
- **Store state is not in the recorder's hash — still true after 5b's new anchor.** The phase-2
  anchor fixture is refused on a `state_fp` change and phase 4 is not a fixture-producing phase
  (spec §5.5). 5b DID re-pin `state_fp` and record a new anchor (`app/recorder.rs`'s `tree:u64`
  term folding in `Dispatcher::state_hash`), but that term is the CONTAINER TREE's own
  `LogicalState` — the Settings family's live instances, its surface phases, the engine's focus
  and the queue depth — not the six PMS-derived stores. `browse`/`pms`/`metadata`/`search`/
  `person`/`viewstate`'s generations stay out of `recorder::state_hash`; Browse ownership does
  not by itself make store state part of the recorder hash, and the remaining stores still await
  their ownership slices.
- **`dev_flags_reach_machines_only_as_recorded_sys_results` stays pending — 5b did NOT close it.**
  This section predicted the Settings family would be the `Sys` result path's first consumer; it
  is not. `AppFx` (`screens/registry.rs`) has `Store`/`Consent`/`Loop` and no `Sys` variant, the
  Settings family's own boot-target trigger (`/tmp/plxnative-settings=privacy|home`) is read by
  `dev::read` directly in `app/run.rs` before any screen mounts, and the pending test
  (`ui/fixture.rs`'s `phase_2` module) is still `#[ignore]`d. No machine reads a dev flag yet.

## 5. How to add a mutation after this phase

Add a variant to the store's `Cmd` enum, apply it in that store's `step`, and emit
`AppFx::Store(StoreId, StoreCmd::…)` from an owned screen. A legacy caller may use the temporary
`stores::<store>::apply(Cmd::…)` shim. Do not add a `pub(crate) fn` to the data module that a
screen calls: `check-deps` will refuse it, and the point of the vocabulary is that the mutation
set is one `match` a reviewer can read.
