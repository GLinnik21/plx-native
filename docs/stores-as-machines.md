# Stores as machines — restructure phase 4

R2B-E endpoint recovery: each store's own owner `run` method returns a `StoreOutcome` containing the
existing `changed` verdict and a bounded, deduplicated set of endpoint requests in
first-observation order. Hubs failure/refetch/retry, ViewState's hub refetch and Browse all
propagate that set through their own owner path (`HubsStore::run`, `ViewStateStore::run`,
`BrowseStore::run`) — there is no dispatcher that differs by store; only the translation step,
`EndpointRefreshSet::emit`, is generic, over the layer-neutral
`StoreEffectHost`. Bridge and
Onboard translate requests to `AppFx::Session(RequestEndpoint)`. Boot/run accumulate outcomes
locally and share the temporary app-side Session command executor with Bridge. Data modules
no longer execute auth recovery directly. Physical Session ownership remains the next R2B
package: the temporary executor still calls the existing auth controller.

The design note for spec v4 (`ui-plxnative-structured-phoenix.md`) phase 4, written from the
code rather than from the spec's sentence, because the sentence hides four decisions the tree
forces. Read `rust-modules/data/src/stores/mod.rs` for the vocabulary; this is the reasoning.

Browse retirement Wave 2 plus the ViewState and Person ownership stages completed their destination contracts:
`BrowsePublications` is the retained directory/listing/section-hubs aggregate,
`Stores::capture_browse` captures it from one owner borrow in directory-first order, and
`Stores::{browse_run,browse_discover_pump}` plus the matching Bridge methods are explicit,
synchronous owner paths. There is no active selector, bootstrap-adoption token, global Browse
publication, legacy adapter or free mutation/read facade. `ci/check-deps.sh` enforces that zero
surface directly; its deleted Browse migration allowlist is distinct from the retained, general
`ci/allow/mutators.txt` gate allowlist (currently empty).
`Stores::viewstate` likewise owns the write queue, in-flight request, retry/refresh latches, rotated
`Arc` mailbox and notice. Its zero-tolerance `viewstate-owner` gate has no allowlist.
`Stores::view_ledger` is the one cross-store piece of state: the publish order of view-state edits
(`viewledger`), recorded into by `viewstate` and held by every store that reads. A read stamps the
ledger when it is requested and, when it lands, is given every edit whose number is above that
stamp. An edit is numbered at the press and RENUMBERED when the server takes its write (a refused
write is retired), so a press survives every read requested before its write landed, including one
the server answered before the write arrived; a read requested after the landing is the server's own
word and is left alone. `ViewState`'s reset (sign-in, profile or server switch) clears the table.
It is a main-thread `Rc` handle, and the stamp travels into a worker as a plain number. A store
built after `Stores::default` (`HubsStore::from_parts` at boot) takes the ledger as an argument.
`Stores::reconcile` (`stores::viewreconcile`, `StoreId::ViewReconcile`, appended after the earlier
ordinals) is the read-back of a view-state write. ViewState hands it the `(server, ratingKey)` of every
item whose write landed, taken or refused, once its burst is idle (a stopped playback the server took enqueues its item directly); it reads them with one
`metadata_many` per server per round (an episode's answer names its season and show, which ride in a second round; one in flight per server, an item asked for again meanwhile is marked dirty
and re-queued once), stamps the batch from the ledger, drops any row a newer press covers, and publishes
the rest as `ViewFact::Facts` edits at leaf scope, which, like the stop's `Offset`, patch Home's hubs, Browse's listing, the rows Search, Person and
Collection hold, and a Detail page's Related and collection-member tiles. Each store counts a Facts
or Offset row as a hit (a merge, a commit, a notice) only when it differs from the row (`ViewEdit::changes`); a Detail page's own
item, episodes and seasons take no Facts (they are re-read), only a stop's Offset, which also lands on the loaded item and its episode (exact key); Browse's section shelves take neither (they are re-read). A stop the server refused asks for no read-back, but it lists its item for the Detail pages' own re-read. The read-back never sends a
read while a view-state write is queued or out (the stamp is taken when the batch is sent, so a press whose write had not
landed would otherwise sit before it and be undone by the read's older body); queued items wait for the
burst to be idle, and for the player to leave the top (`Stores::hold_network`). A refused scrobble returns the tile to what the server holds, for the item (and the
copies the fan-out took) a read-back asks about: a refused Show- or Season-scope press re-reads only the show
or season row, and the episode rows the optimistic edit flipped in Browse listings stand until their next
fetch. Facts are not recorded in the ledger (they are the server's word). `Bridge::reconcile_pump` runs it
after `viewstate_pump` every frame, and also pumps Browse while a Browse section is owed a membership
re-query (below): a Library page with another page pushed over it is not ticked.
Browse marks a section whose membership depends on viewing (Unwatched filter, `lastViewedAt` sort) stale
when a Facts edit covers one of its rows, whether or not the row already agrees (the server's answer marks
staleness; the optimistic press alone does not). The Library screen asks for the re-query (`LibraryWork::Requery`) only once a
PAGE has been pushed over it (`WillLeave(Deeper)`, then `Cover`), never while it is visible and never under a
modal such as a compact menu (Sort, Filter, the item menu), which leaves the page visible and ticking; so tiles
do not leave under the cursor. The re-query is a refresh of the SAME query: the fetch generation moves (a page
still out for the old fetch is dropped) but `ListingId::query` does not, so the Library screen sees no query
edit and keeps its scroll and its focus on the reader's tile (the bookmark names the item). A stale section
that is not the current one is not refreshed in place: it forgets its pages and its cursor (like a first
visit), so when the reader switches back to it the listing is fetched afresh and opens at the top.
`Stores::person` owns the open model, generation, retry ladders and dev-seed latch alongside a
rotated `Arc<PersonAdapter>` containing the unchanged indexed fetch claims/mailboxes. `AppViews`
lends its `PersonView` to Person, Filmography and PersonBio; `person-owner` rejects every old free
read/mutation facade and storage selector with no allowlist.

`StoreId::ViewStop` (appended last) is no store: it is the landing gate's slot for the stop mailbox
(`route::take_landed_stops`), because the gate keeps one mailbox per ordinal and `ViewState`'s is taken. The
mailbox belongs to one identity: `route::retire_pending_stops` (from `activate_server_owned`, before the
stores reset) drops the records posted and the ones a stalled `stopped` POST posts later.

## 1. What a store is, today

Seven data modules own the application's server-derived state: `browse` (the Library table and its
per-section paged listing), `pms` (Home's hub catalog, behind the `stores::hubs` machine),
`metadata` (the detail page's item, seasons and episodes, the playing item), `search`, `person`,
`collection`, and `viewstate` (the view-state WRITE queue; `viewreconcile`, its read-back, is an eighth store). The first seven now own their state per `Bridge` (Metadata
was the last to complete its port): each `Bridge` owns one of each store. `BrowseStore`'s
state/adapter/notice, `HubsStore`'s `PmsState`/`Arc<PmsAdapter>`/notice, `SearchStore`'s
state/adapter/notice, `MetadataStore`'s state/`Arc<MetadataAdapter>`/notice and `ViewStateStore`'s
queue, flight, retry and refresh state, worker adapter and notice are per-instance. The Browse
adapter holds Browse's source-discovery mailbox and single-flight flag, and a `Fetch` (claim,
mailbox and refusal list) for each of the page, the genre list, the letter list and the section
hubs (one for both the list and the row reads); those four reads are started by the app's runner
(`StoreJob::{BrowsePage,BrowseDirectory,BrowseHubs}`), not by the store. Person's adapter keeps the recorded fetch numbering (`slot * 3 + kind` for
servers 0..15, the two global fetches at 48 and 49, then `50 + (slot - 16) * 3 + kind`; see
`person::fx`) and the controlled-record schema; Person, Search, Metadata and ViewState all rotate adapters on reset
(D3: Metadata's `Clear` is the one exception — see `stores/metadata.rs`'s struct doc). `metadata`'s
worker is still spawned through `task::spawn_small`, with generation atomics that supersede a late
landing and a once-a-frame pump in its existing callers — only WHERE its state/adapter/notice live
changed, not that shape.

Collection follows the same ownership contract: one open model, generation and retry ladder live
beside a rotated `Arc<CollectionAdapter>` mailbox, and `AppViews` lends `CollectionView` only to
the mounted Collection screen.

The census that sized this phase (2026-09-07): browse exports 87 `pub(crate) fn`, metadata 51,
pms 24, person 21, search 17, viewstate 4. Of those, the MUTATORS a screen calls directly are
exactly these (file: callers):

| store | mutator | callers outside the store |
|---|---|---|
| browse | `BrowseCmd` (including `Reset`) | owned screens emit `AppFx::Store`; `app/bridge.rs` delivers the command to that Bridge's `BrowseStore`; synchronous app boundaries call `Stores::browse_run` on an explicit aggregate |
| browse::listing_jobs | `kick_directory` (reached through `BrowseCmd::Addressed` with `Genres` or `Letters`) and `maybe_spawn_owned` (the `StoreWork::Browse` pump) | The step takes the section's `ClientKey` from the `ClientKeys` the Bridge filled, raises the read's claim and leaves a `StoreJob::BrowseDirectory` / `StoreJob::BrowsePage` on the outbox, drained by the same entries as the section hubs. The landings are unchanged; a refused read is settled by the store's next step. A controlled Home run never emits either |
| browse | `StoreWork::{BrowseDiscovery,Browse}` | Onboard schedules the roster-only owned pass; Library schedules the full owned landing pass; `app/bridge.rs` delivers both to the addressed Bridge's `BrowseStore` |
| browse::section_hubs | `kick`, `commit_staged`, `invalidate_all`, `set_watched_local`, `left_the_deck` (reached through `BrowseCmd::ApplyView`) | Library mutations are carried by `StoreCmd::Browse`; the BrowseStore owns the section-hub adapter and its per-section state. A step that needs a read decides it from the `ClientKeys` the Bridge filled and leaves it as a `StoreJob::BrowseHubs` on the store's outbox; every Bridge entry that can run such a step starts it in the same call (`Bridge::browse_run`, the `StoreWork::Browse` arm, `Bridge::reconcile_pump`, and `viewstate_pump` / `notify_pump` through their `*_FILLS` lists) |
| viewstate | `ViewStateCmd::{Request,Reset}` | owned Detail emits `AppFx::Store`; `app/bridge.rs` delivers to its `ViewStateStore`; synchronous item-menu/boot boundaries call that Bridge; run pumps and drains addressed Detail refreshes through the same owner |
| person | `PersonCmd::{Open,Close,Reset,ApplyView}` + owner pump | Person emits addressed Open/Close effects; Bridge boot/run and ViewState call the same aggregate's `PersonStore` |
| collection | `CollectionCmd::{Open,Close,Reset,ApplyView}` + owner pump | Collection emits addressed Open/Close effects; Bridge boot/run and ViewState call the same aggregate's `CollectionStore` |
| metadata | `MetadataCmd::{RequestDetail,Clear,ClearItem,Withdraw,Reset,LoadSeason,SetNowPlaying,ApplyView,InstallPlaying,MarkSkipped}` — the private `request_detail`/`clear`/`clear_item`/`withdraw`/`load_season`/`set_now_playing`/`set_watched_local`/`install_playing`/`mark_skipped` functions behind them are not reachable directly | Detail emits `AppFx::Store` (the mount-time `RequestDetail` `screens/registry.rs` used to queue for a fresh Detail page raced `DetailScreen`'s own `Enter(Fresh)` decision and issued it twice; `registry.rs` no longer queues one, so `Enter` is the one place a page decides it needs a fetch — nothing re-asks on `Tick` or `StoreChanged`, because the slot is a mailbox other flows (the item menu's Play, the play paths) fill by design; a Detail SURFACE that is dismissed sends `Withdraw` on `ScreenEvent::Closing`, ahead of its host's `Enter(Restored)`, so its unlanded request cannot displace the item that page found loaded, and its `ClearItem` follows at teardown unless the page withdrew and then asked for its own address again — `data/src/stores/metadata.rs` says which cases); `app/bridge.rs` delivers to that Bridge's `MetadataStore`; `app/boot.rs` and `app/content.rs` call `Bridge::metadata_run` directly (`load_detail_now` is deleted, D7) |
| metadata | `pump_detail`, `pump_season`, `pump_alt_sources` | PUMP doors, `pub(crate)` by design — stepped by the store's own `run`/pump path, not called by a screen |
| search | `set_query`, `reset`, `pump` | none direct — reached only through `StoreCmd::Search` from the owned `screens/search/mod.rs` (`ui/search/mod.rs` and `ui/search/recents.rs` are both deleted). Every pump emits its per-source fetches as `StoreJob::Search` and every recents edit its write as `StoreJob::SaveRecents`; `Bridge::search_run` and `Bridge::search_pump` drain them to the app's runner in the same call (`docs/sans-io.md`, "The Search lane") |
| pms | `run_with_directory`, `tick_with_directory`, and result landing | HubsStore owns the state and adapter. Home emits `StoreCmd::Hubs(..)`. Every step, Retry and page work included, captures its immutable request and the current Browse scope and emits it as a `StoreJob::Hubs` the app's runner starts. |

The phase-4 CENSUS is historical: the 87/51/24/21/17/4 `pub(crate) fn` counts above are a
2026-09-07 snapshot, never re-run. The table's CALLER column is not — it is the part kept current
at each phase that changes the truth (Phase 7, Phase 8 and the store-ownership migration all did),
which is what makes "the table now names the live callers" below defensible rather than a stale
claim riding along with a frozen census. Browse, Person and ViewState's sole production owners are their
store values inside Bridges. Library, Onboard, Detail and Person emit store effects; `app/bridge.rs`
delivers them to the addressed machine, and every fixture that needs mutable Browse data owns a
`BrowseStore` or `Stores`. Retained `DirectoryView`, `ListingView` and `HubsView` values are the
only cross-layer reads; the old free `crate::browse` publication and mutator functions are gone.

Phase 7 (2026-09-08) mounted Detail and Person from `screens/` and retired their old `ui/` files;
the table now names the live callers. Phase 8 (2026-09-09) did the same to Home and took two of the
table's cells with it: `ui/home.rs` is deleted, so it is no caller of anything, and `pms::pump` —
the "legacy callers' combined pass" it was the last caller of — is deleted with it, along with
`stores::hubs::pump`. HubsStore forwards commands through `run_with_directory`/`tick_with_directory`. Every step
captures its immutable `HubRequest` and emits it as a `StoreJob::Hubs`, which the app's runner
launches or refuses; a controlled Home arm steps through the same calls and differs only in
admission (`admit_hubs_jobs`). `pms::controlled_work` is a data-layer test driver, not a capture path. The owned Home emits `StoreCmd::Hubs(..)` and never calls a catalog
mutator. The store owns the work and result handling. Person, Filmography and
PersonBio read only the owner-borrowed
`PersonView`; Filmography reacts to Person notices but does not mutate the store.

Every one of those calls is followed, in the SAME frame and often in the same statement, by a
read that assumes it took effect: `set_cur` then `kick_letters` (reads `cur()`), `set_query` then
the caret placed against `query()`, `set_sort_by_key`'s returned bool deciding `grid_reset()`,
`clear()` ordered before `request_detail` because it supersedes the generation the request is
about to establish. That fact is what decides §3 below.

## 2. What phase 4 makes true

1. **One vocabulary per store.** `stores::StoreCmd` is the complete, enumerated set of mutations
   — `Browse(BrowseCmd)`, `ViewState(..)`, `Person(..)`, `Collection(..)`, `Metadata(..)`,
   `Search(..)`, `Hubs(..)` — and a store's owned `run`/`step` is the ONE place its mutation vocabulary is decoded.
   An owned screen emits `AppFx::Store(StoreId::Browse, StoreCmd::Browse(cmd))`; Bridge delivers
   it to its own BrowseStore, PersonStore or ViewStateStore, while synchronous application boundaries name their `Stores` owner.
   A screen names the Browse command vocabulary, never `crate::browse::set_cur(i)`;
   `ci/check-deps.sh`'s new `mutators` gate refuses the old spelling on every production line
   of `ui/` and `app/` (test modules are skipped by brace depth wherever they sit in a file);
   `ci/allow/mutators.txt` is EMPTY — an entry there would be a debt with a phase number. The
   player side (`route/plan.rs`, `route/decision.rs`, `player/`) already spells its two writes
   through the vocabulary and joins the gate's scope in phase 9.
2. **One notice.** Every command that changes observable state and every landing that changes the
   store bumps its generation and marks it dirty. `Stores::take_notices()` drains each of the seven
   owners' own notices once per frame at `app/bridge.rs`'s drain
   point (right after NAV COMMIT), and `bridge::frame` delivers the aggregate as
   `Dispatcher::store_changed(ord, gen)` to every live instance. The owned Browse path also
   coalesces a captured publication change with that notice, so one landing produces one
   `ScreenEvent::StoreChanged`.
3. **The dispatcher path is real.** `AppFx::Store(StoreId, StoreCmd)` is the application's first
   effect: `app::bridge::Bridge` turns it into `Fx::Deliver(MachineId::Store(ord),
   Delivery::Machine(AppMsg::Store(cmd)))`. Its `Rig::deliver` branch steps every per-Bridge
   store — `BrowseStore`, `HubsStore`, `MetadataStore`, `PersonStore`, `CollectionStore`,
   `SearchStore` and `ViewStateStore` — directly through its own owner. Screen effects and explicit synchronous
   owner calls preserve one command vocabulary without a process-wide Browse selection path.
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
and migrated callers land in the same drain". That temporary legacy-apply guidance is now moot: all
seven stores — Browse, Hubs, Person, Collection, Search, Metadata and ViewState — have answers consumed in the same
turn: owned screens emit addressed effects, `app/bridge.rs` delivers each command to the matching owned store, and
synchronous boot/input boundaries call the explicit Bridge/Stores owner they already hold. Both
paths step on the main thread before the frame presents, and the aggregate drain delivers that
owner's notice to its live screens. Preserving that timing requires no global selector or adapter;
Metadata completed its port last (Stage C, `docs/agent-reference.md`), so nothing here still awaits
an ownership slice.

## 4. What is NOT in phase 4, and why

- **The mailbox shapes stay — only where they live moved.** `metadata`'s mailboxes are now fields
  of its per-`MetadataStore` `Arc<MetadataAdapter>` (`DETAIL_LANDING`/`SEASON_LANDING`/
  `ALT_LANDING`/`NOW`/`CURRENT`/`TRACKER`/`NOTICES`, not process-wide statics), with the same
  worker/generation/supersede shape this section describes. Search's `SLOT[NSRC]` shape is now
  `SearchAdapter`'s per-slot fetch claims/mailboxes, owned per Bridge without changing slot
  ordinals or record fields. Person's `FETCH[]` shape is now `PersonAdapter::fetch`, owned per
  Bridge without changing slot ordinals, claims, mailbox multiplicity or record fields. Browse's
  page, genre, letter, source-discovery and section-hub mailboxes are now fields of its
  per-`BrowseStore` `BrowseAdapter`, not process-wide `PAGE_RESULT`/`GENRE_RESULT`/
  `LETTER_RESULT`/`SRC_RESULT`/`HUB_FETCHING` state. Hubs' fetch results and request counter are
  now fields of its per-`HubsStore` `Arc<PmsAdapter>`, not a process-wide `RESULTS`, and `pms`'s
  source table and roster fingerprint live as `PmsState` fields, not statics. ViewState's `MAIL`
  moved into its per-owner rotated adapter. Collection's generation-stamped single fetch mailbox
  likewise lives in its per-owner rotated adapter. Metadata is single-flight by construction
  (`FETCHING`/`IN_FLIGHT` bounds the worker count), so the backpressure `Landing` adds is a no-op
  for it today, and its supersede rules are keyed on generations the screens read. Browse and Hubs
  are already stepped by `app/bridge.rs` for `StoreWork::{Browse,BrowseDiscovery,Hubs}`, while
  ViewState's, Search's and Metadata's route-unconditional pumps are all owner-bound now
  (`app/run.rs`'s per-frame `metadata_pump_detail`/`metadata_pump_season` calls, the same idiom as
  the rest).
- **The pumps stay where they are.** A route-gated pump moved to the machine's `Tick` would fetch
  behind the player, which `pms::pump`'s doc forbids for a reason. Browse's owned full and
  roster-only work events are both delivered by `app/bridge.rs`; the gate moves with the explicit
  owner that drives the work.
- **Store state is not in the recorder's hash — still true after 5b's new anchor.** The phase-2
  anchor fixture is refused on a `state_fp` change and phase 4 is not a fixture-producing phase
  (spec §5.5). 5b DID re-pin `state_fp` and record a new anchor (`app/recorder.rs`'s `tree:u64`
  term folding in `Dispatcher::state_hash`), but that term is the CONTAINER TREE's own
  `LogicalState` — the Settings family's live instances, its surface phases, the engine's focus
  and the queue depth — not the seven PMS-derived stores. `browse`/`pms`/`metadata`/`search`/
  `person`/`collection`/`viewstate`'s generations stay out of `recorder::state_hash`; physical ownership does
  not by itself make store state part of the recorder hash, and that stays true now that all seven
  have completed their ownership slices.
- **`dev_flags_reach_machines_only_as_recorded_sys_results` stays pending — 5b did NOT close it.**
  This section predicted the Settings family would be the `Sys` result path's first consumer; it
  is not. `AppFx` (`screens/registry.rs`) has `Store`/`Consent`/`Loop` and no `Sys` variant, the
  Settings family's own boot-target trigger (`/tmp/plxnative-settings=privacy|home`) is read by
  `devtrig::read` directly in `app/run.rs` before any screen mounts, and the pending test
  (`ui/fixture.rs`'s `phase_2` module) is still `#[ignore]`d. No machine reads a dev flag yet.

## 5. How to add a mutation after this phase

Add a variant to the store's `Cmd` enum, apply it in that store's `step`, and emit
`AppFx::Store(StoreId, StoreCmd::…)` from an owned screen. All seven stores are physically owned;
there is no shim for an unowned store any more — every caller goes through its concrete `Stores`
owner (`Bridge::<store>_run`, or the owner method a same-turn boundary already holds). Do not add
a `pub(crate) fn` to the data module that a screen calls: `check-deps` will refuse it, and the
point of the vocabulary is that the mutation set is one `match` a reviewer can read.

The stores are machines in state but not yet in IO: their modules still start their own workers,
read `client_for(current_server())` and the other process-global registries (the Metadata detail
fetch chain, the season switch and the episode-page window no longer do: each runs through the one
client the runner resolved for its job, `docs/sans-io.md`, "The store job lane"), and a few arms are
`cfg(not(test))` forks. `docs/sans-io.md` states the rule that retires those (a machine performs no
IO; its work leaves as a typed effect and every result returns as one addressed event) and the four
ledgers, `ci/allow/{workers,globals,forks,fxlanes}/`, that freeze today's sites. A new worker start,
registry read or test/production fork in a store fails `check-deps`; a migration shrinks the ledger
by deleting the lines of the store it made pure.
