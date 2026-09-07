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
| browse | `set_cur`, `note_library_choice`, `kick_letters`, `kick_genres`, `want`, `save_view`, `set_sort_by_key`, `toggle_unwatched`, `set_genre_by_id`, `retry_cur_source`, `recheck_shares`, `pump` | `ui/library.rs` |
| browse | `apply_pins`, `retry_discovery` | `ui/onboard.rs` (`apply_pins` also `ui/library.rs`) |
| browse | `reset`, `discover_pump` | `app/boot.rs`, `ui/home.rs`, `ui/onboard.rs`, `ui/search/mod.rs`, `ui/settings.rs`, `ui/search/field.rs` |
| browse::section_hubs | `kick`, `commit_staged`, `invalidate_all`, `set_watched_local`, `left_the_deck` | `ui/library.rs`, `viewstate.rs` |
| viewstate | `request` | `app/input.rs`, `ui/detail.rs` |
| person | `open`, `close`, `pump` | `ui/person.rs`, `ui/detail.rs` |
| metadata | `request_detail`, `load_detail_now`, `clear`, `load_season`, `set_now_playing`, `set_watched_local`, `pump_season` | `ui/detail.rs`, `app/{input,playback,run}.rs` |
| metadata | `install_playing`, `mark_skipped`, `pump_detail`, `pump_alt_sources` | `route.rs`, `app/{playback,run}.rs` |
| search | `set_query`, `reset`, `pump` | `ui/search/mod.rs`, `ui/search/recents.rs` |
| pms | `request_refetch_hubs`, `request_retry`, `reset`, `pump` | `ui/home.rs`, `app/{boot,run}.rs` |

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
   player side (`route.rs`, `player/`) already spells its two writes through the vocabulary and
   joins the gate's scope in phase 9.
2. **One notice.** Every applied command and every landing that changed the store bumps that
   store's generation and marks it dirty; `stores::take_notices()` drains `(StoreId, gen)` once
   per frame at the loop's drain point (`legacy::mirror`, right after NAV COMMIT) into the shadow
   dispatcher as `Dispatcher::store_changed(ord, gen)`, which delivers `ScreenEvent::StoreChanged`
   to every live instance. A `LegacyPage` ignores it; a migrated screen (phase 5b) reconciles on
   it (spec §7.3 step 6).
3. **The dispatcher path is real.** `AppFx::Store(StoreId, StoreCmd)` is the application's first
   effect: the shadow rig turns it into `Fx::Deliver(MachineId::Store(ord), Machine(AppMsg::Store(cmd)))`,
   and `Rig::deliver` steps the store. A migrated screen emits the effect; a legacy screen calls the
   shim. Both end in the same `step`.
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
- **Store state is not in the recorder's hash.** The phase-2 anchor fixture is refused on a
  `state_fp` change and phase 4 is not a fixture-producing phase (spec §5.5), so the stores'
  generations stay out of `recorder::state_hash` until 5b records a new anchor.
- **`dev_flags_reach_machines_only_as_recorded_sys_results` stays pending.** No machine reads a
  dev flag yet; the first that does is the Settings family (5b), which is where the `Sys` result
  path gets its first consumer and its test.

## 5. How to add a mutation after this phase

Add a variant to the store's `Cmd` enum, apply it in that store's `step`, and call
`stores::<store>::apply(Cmd::…)` from the screen. Do not add a `pub(crate) fn` to the data module
that a screen calls: `check-deps` will refuse it, and the point of the vocabulary is that the
mutation set is one `match` a reviewer can read.
