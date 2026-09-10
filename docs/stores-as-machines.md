# Stores as machines — restructure phase 4

The design note for spec v4 (`ui-plxnative-structured-phoenix.md`) phase 4, written from the
code rather than from the spec's sentence, because the sentence hides four decisions the tree
forces. Read `rust-modules/src/stores/mod.rs` for the vocabulary; this is the reasoning.

## 1. What a store is, today

Six data modules own the application's server-derived state: `browse` (the Library table and its
per-section paged listing), `pms` (Home's hub catalog), `metadata` (the detail page's item,
seasons and episodes, the playing item), `search`, `person` and `viewstate` (the view-state
WRITE queue). Every one is the same shape: main-thread `static mut` state, a worker spawned
through `task::spawn_small` with the client captured at the spawn site, a `Mutex` mailbox the
worker fills, generation atomics that supersede a late landing, and a `pump()` the loop calls once
a frame — route-gated inside a screen's `update` for browse / pms / search / person, unconditional
in `app/run.rs` for the detail, the view-state queue and the alt-sources resolve.

The census that sized this phase (2026-09-07): browse exports 87 `pub(crate) fn`, metadata 51,
pms 24, person 21, search 17, viewstate 4. Of those, the MUTATORS a screen calls directly are
exactly these (file: callers):

| store | mutator | callers outside the store |
|---|---|---|
| browse | `set_cur`, `note_library_choice`, `kick_letters`, `kick_genres`, `want`, `save_view`, `set_sort_by_key`, `toggle_unwatched`, `set_genre_by_id`, `retry_cur_source`, `recheck_shares`, `pump` | none direct — reached only through `StoreCmd::Browse` from `screens/library/*` (`stores/browse.rs`'s `Machine::step` is the one caller; `ui/library.rs` was deleted in phase 8) |
| browse | `apply_pins`, `retry_discovery` | none — closed by phase 5b, same day; see note below (no caller outside `stores/browse.rs` survives: the `ui/library.rs` test fixtures that seeded through `apply_pins` went with that file in phase 8) |
| browse | `reset`, `discover_pump` | `app/boot.rs`, `screens/onboard.rs` (both direct); `discover_pump` also reached from `screens/home/mod.rs` and `screens/search/mod.rs` via `Fx::App(AppFx::StoreWork(BrowseDiscovery))`, resolved by `app/bridge.rs`'s own dispatch (`ui/home.rs` and `ui/search/` are both deleted) |
| browse::section_hubs | `kick`, `commit_staged`, `invalidate_all`, `set_watched_local`, `left_the_deck` | `viewstate.rs`; the Library reaches them only through `StoreCmd::Browse` (`stores/browse.rs`'s `Machine::step`) since `ui/library.rs` was deleted in phase 8 |
| viewstate | `request` | `app/input.rs`, `screens/detail/mod.rs` |
| person | `open`, `close`, `pump` | `screens/person.rs` |
| metadata | `request_detail`, `load_detail_now`, `clear`, `load_season`, `set_now_playing`, `set_watched_local`, `pump_season` | `screens/detail/mod.rs`, `app/{input,playback,run}.rs` |
| metadata | `install_playing`, `mark_skipped`, `pump_detail`, `pump_alt_sources` | `route/decision.rs`, `app/{playback,run}.rs` |
| search | `set_query`, `reset`, `pump` | none direct — reached only through `StoreCmd::Search` from the owned `screens/search/mod.rs` (`ui/search/mod.rs` and `ui/search/recents.rs` are both deleted) |
| pms | `request_refetch_hubs`, `request_retry`, `reset`, `pump` | `app/{boot,run}.rs` (`ui/home.rs` is deleted; the owned Home emits `StoreCmd::Hubs(..)` and never a mutator — see the Phase 8 note below) |

**This table is the census phase 4 sized itself against, and two of its rows were already stale by
the end of the same day.** Phase 5b (2026-09-07, the Settings-family restructure) retired
`ui/onboard.rs` — the one caller the `apply_pins`/`retry_discovery` row named — and replaced it with
`screens/onboard.rs`'s owned `OnboardScreen`, which does not call either mutator directly at all:
both leave the screen as `Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::ApplyPins(..)
/ RetryDiscovery)))`, exactly the vocabulary spelling §2 below describes, so `stores/browse.rs`'s
`Machine::step` is now the ONLY direct caller of either — the "one place a legacy mutator is
called" rule already stated a paragraph down, finally true for this pair rather than aspirational.
(Until phase 8, `ui/library.rs`'s own test fixtures still called `crate::browse::apply_pins`
directly to seed a known state; that file is deleted, and the only surviving direct caller is
`browse/mod.rs`'s own `apply_pins_writes_the_whole_batch_in_one_record`. The gate this phase's
`mutators` rule enforces skips test code by design.)
The `reset`/`discover_pump` row is NOT closed the same way — `screens/onboard.rs` still calls both
directly, unchanged from `ui/onboard.rs`'s own habit, because neither is a `BrowseCmd` variant
(`reset` has no per-store notice worth raising for a whole-app profile switch, and `discover_pump`
is a per-frame poll, not a mutation `Machine::step` could usefully gate) — only the caller's
FILENAME moved, which the row above already reflects. Do not re-derive this by re-grepping the
census columns without reading this note; the table's own count is dead the moment it is taken, and
this paragraph is the amendment for the one phase that happened to land on the same day.

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
   `Hubs(..)` — and a store's `Machine::step` is the ONE place a legacy mutator is called. A screen
   names `stores::browse::apply(BrowseCmd::SetCur(i))`, never `crate::browse::set_cur(i)`;
   `ci/check-deps.sh`'s new `mutators` gate refuses the old spelling on every production line
   of `ui/` and `app/` (test modules are skipped by brace depth wherever they sit in a file);
   `ci/allow/mutators.txt` is EMPTY — an entry there would be a debt with a phase number. The
   player side (`route/plan.rs`, `route/decision.rs`, `player/`) already spells its two writes
   through the vocabulary and joins the gate's scope in phase 9.
2. **One notice.** Every applied command and every landing that changed the store bumps that
   store's generation and marks it dirty; `stores::take_notices()` drains `(StoreId, gen)` once
   per frame at the loop's drain point (`bridge::frame`, right after NAV COMMIT) into the real
   dispatcher as `Dispatcher::store_changed(ord, gen)`, which delivers `ScreenEvent::StoreChanged`
   to every live instance. A `LegacyPage` ignores it; a migrated screen (phase 5b) reconciles on
   it (spec §7.3 step 6).
3. **The dispatcher path is real.** `AppFx::Store(StoreId, StoreCmd)` is the application's first
   effect: `bridge::Bridge` (the `Rig<AppHost>` impl) turns it into
   `Fx::Deliver(MachineId::Store(ord), Delivery::Machine(AppMsg::Store(cmd)))`, and `Rig::deliver`
   steps the store. A migrated screen emits the effect; a legacy screen calls the shim. Both end in
   the same `step`.
4. **`Landing` is complete** (spec §5.2): per-addressee admission (`inflight_cap`), `Refused` and
   `Dropped` on the control lane, per-landing drop counters, and the four §15.1 tests
   (`the_landing_cap_drops_the_newest_and_hashes_the_count`,
   `a_full_landing_replies_dropped_and_retires_inflight`, `a_refused_spawn_lands_a_refusal_event`,
   `a_same_rating_key_on_a_different_server_is_skipped`).
5. **The detail mailbox carries identity.** `metadata`'s one-slot `DETAIL_SLOT` becomes a
   `Landing` keyed on `(ServerId, ratingKey)`: a landing for a different server's item of the same
   number is skipped rather than installed, which is the gap the spec's evidence line names
   (`DetailResult` at metadata.rs:2021 carried no `(sid, rk)`).
6. **A store's step is O(result size).** `browse` sized a section's item vector to the listing's
   `totalSize` on the main thread when the first page landed — `Vec<Option<PmsMovie>>` of every
   item in the library, allocated in the drain. The store is chunked by PAGE now (`SecItems`): the
   outer vector is one slot per page, a page is allocated when its items land, and the
   missing-page scan is over pages rather than items.

## 3. The decision the spec's §14 sentence hides: the shim applies NOW

§14 says a legacy mutator becomes "a pure synchronous validation plus `queue(StoreCmd)`, so legacy
and migrated callers land in the same drain". Read against §1's last paragraph that is not
implementable without rewriting every screen that calls a mutator: the screens were written
against synchronous application, and the deferred form changes what a screen reads on the press
frame (the Library's `Xfade` commit re-queries at alpha 0 and resets the grid on the mutator's
answer in the same statement; deferring by one frame draws the old listing for the first frame of
the fade-in). So in phase 4 the shim is `stores::<store>::apply(cmd) -> bool`: it steps the store
machine IMMEDIATELY on the main thread and returns the store's own answer, exactly as the mutator
did. What moved is the OWNER — the mutation happens in `step`, is named by a `StoreCmd`, and
raises the notice — not the frame it happens on. The "same drain" ordering becomes true when the
loop itself is the dispatcher and the screens emit `AppFx::Store` (each screen's own phase), at
which point the shim has no caller and is deleted with the ladders (§14, phase 12). The record of
this deviation is this section; the spec's sentence is not made false by it, it is made true
later than its phase number says.

## 4. What is NOT in phase 4, and why

- **The other mailboxes stay.** `browse`'s `PAGE_RESULT`, `search`'s `SLOT[NSRC]`, `person`'s
  `FETCH[]`, `viewstate`'s `MAIL` and `pms`'s `RESULTS` keep their one-slot / per-source shapes.
  Each is single-flight by construction (a `FETCHING`/`IN_FLIGHT` flag bounds the worker count), so
  the backpressure `Landing` adds is a no-op for them today, and their supersede rules are keyed on
  generations the screens read. They convert in the phase that migrates their screen (5b for none,
  7 metadata/person, 8 browse/search/pms), where `Event::Store` replaces the generation reads.
- **The pumps stay where they are.** A route-gated pump moved to the machine's `Tick` would fetch
  behind the player, which `pms::pump`'s doc forbids for a reason. The store's `Pump` event exists
  and the screen's `update` calls it through the store; the gate moves with the screen.
- **Store state is not in the recorder's hash — still true after 5b's new anchor.** The phase-2
  anchor fixture is refused on a `state_fp` change and phase 4 is not a fixture-producing phase
  (spec §5.5). 5b DID re-pin `state_fp` and record a new anchor (`app/recorder.rs`'s `tree:u64`
  term folding in `Dispatcher::state_hash`), but that term is the CONTAINER TREE's own
  `LogicalState` — the Settings family's live instances, its surface phases, the engine's focus
  and the queue depth — not the six PMS-derived stores. `browse`/`pms`/`metadata`/`search`/
  `person`/`viewstate`'s generations stay out of `recorder::state_hash` until the phase that
  migrates their own screen (7 metadata/person, 8 browse/search/pms, per this section's first
  bullet).
- **`dev_flags_reach_machines_only_as_recorded_sys_results` stays pending — 5b did NOT close it.**
  This section predicted the Settings family would be the `Sys` result path's first consumer; it
  is not. `AppFx` (`screens/registry.rs`) has `Store`/`Consent`/`Loop` and no `Sys` variant, the
  Settings family's own boot-target trigger (`/tmp/plxnative-settings=privacy|home`) is read by
  `dev::read` directly in `app/run.rs` before any screen mounts, and the pending test
  (`ui/fixture.rs`'s `phase_2` module) is still `#[ignore]`d. No machine reads a dev flag yet.

## 5. How to add a mutation after this phase

Add a variant to the store's `Cmd` enum, apply it in that store's `step`, and call
`stores::<store>::apply(Cmd::…)` from the screen. Do not add a `pub(crate) fn` to the data module
that a screen calls: `check-deps` will refuse it, and the point of the vocabulary is that the
mutation set is one `match` a reviewer can read.
