# Writing Sans-IO code: how to do it right while you write it

`docs/sans-io.md` is the design record: what the ledgers freeze and how each migration shrank them.
This is the other half, for the moment you are writing a store, a screen or a machine step. The
rule (AGENTS.md, "Logic never performs IO"): a machine's step starts no worker, reads no
process-global registry and has no `cfg(test)` fork. Its work leaves as a typed effect or job that
the app's runners execute, and the result returns as one addressed event. The reason is that a
step then has no seam to fake: a test and a replay feed it the events the product feeds it.

## Which code is a machine

`ci/check-deps.sh` (the `MACHINES` array) scans these for `workers` and `globals`:

- `rust-modules/data/src`, `session/src`, `screens/src`, `ui/src`, `appkit/src`, `machine/src`;
- `rust-modules/plex/src/plex/session.rs` (the session owner's state);
- `rust-modules/media/src/player/{lifecycle,recovery,release}.rs` and
  `media/src/route/{plan,plan_memory}.rs`.

`forks` reads every machine crate whole (`FORK_SCOPE`): all of `data/`, `plex/`, `media/`,
`session/`, `screens/`, `ui/`, `appkit/` and `machine/`.

The scan is by file; the rule is about what a `run`, `pump`, `tick` or `step` body does. These are
NOT steps, and are where IO belongs: a job's body (`finish_collection_job` in `data/src/collection.rs`,
which lives in `data/` and runs on the worker the runner started), a function that takes `&OffFrame`
(`build_stream` in `media/src/route/plan.rs`), the runner (`StoreRunner`,
`rust-modules/src/app/adapters/stores.rs`), the `Bridge` (`rust-modules/src/app/bridge.rs`), an
actuator (`ForegroundActuator` in `media/src/player/foreground.rs`) and a seam the job body calls
(`LiveCatalogIo`, called inside `finish_catalog_job`'s `catch_unwind` in `data/src/stores/catalog.rs`).
CI still reads a body that sits in a scanned file, so give it what it needs as arguments too.

Two leftovers look like that list and are not models. `LiveIo` (`data/src/watchlist.rs`) is a seam the
Watchlist STEP calls (`reachable`, `roster`, `copy_rank`, `today`, `facts_gen`, `roster_gen`), and its
methods hold the `ci/allow/globals/data-watchlist.txt` lines (its `client_for` and `current_server`
reads, and the roster readers `server_ids`, `server_probe_result`, `server_roster_gen` and
`server_facts_gen`) and the `today()` clock read `docs/sans-io.md` lists (W-2). `build_stream`'s registry reads and its fork are ledger lines too
(`ci/allow/globals/media.txt`, `ci/allow/forks/media.txt`). Do not copy either.

## What counts as IO in a step

The principle first. Any call that blocks, touches the network or disk, reads the clock, starts a
thread, or reads process-wide state through a free function is IO, whether or not a CI list names
it. The ledgers freeze the sites that were found; they are not the definition.

- Starts work: `task::spawn`, `spawn_small*`, `spawn_off_frame*`, `storage_worker::submit*`,
  `std::thread`, `task::beside` (below).
- Blocks or touches the world: a curl or LS2 call, a file read, a lock held across a call, `sleep`.
- Reads the clock: `Instant::now`, `SystemTime::now`, a `today()`.
- Reads a registry the globals rule matches: `client_for`, `current_server`, `current_snapshot`,
  `session::peek`, `keypin::revision`, `keypin::blocked_for`, `grant::offers`,
  `transport::paused`, `devcaps::caps`, and the ones `docs/sans-io.md` lists as remaining:
  `plex::server_ids()`, `server_roster_gen()`, `server_facts_gen()`, `server_probe_result()`
  (re-exports in `plex/src/plex/mod.rs`), `session::peek_settled()` and `session::current_gen()`
  (both in `plex/src/plex/session.rs`) and `tape::active()` (`data/src/stores/tape.rs`). A new use of
  any of these fails `make check` unless a ledger line names it, and is as wrong as a new `client_for`.
- Reads one the rule does not name. The rule matches a closed list of names, so a registry read it
  does not spell passes CI with no ledger line and is IO all the same. Two are left out on purpose,
  and the principle above still covers them: `i18n::current` (a display read) and the
  `plx_media::route::*()` settings getters (the architecture board kept settings getters out of
  this gate). When you add a registry reader, add its name to the rule in the same change.

## The moves

**(a) The step needs to fetch: emit a job.** The step claims the request and pushes a `Job`
(`machine/src/job.rs`: an `Addr` and a request that captured everything) with its `Port`. The
runner starts it; the answer returns through the port. Collection (#987), `maybe_spawn` in
`data/src/collection.rs`:

```rust
let generation = self.generation;
adapter.fetch.claim(generation);
self.stamp = self.ledger.stamp();
self.jobs.push(crate::stores::StoreJob::Collection(
    Job { addr: collection_addr(generation), req: CollectionReq { sid, key, work } },
    CollectionPort::of(adapter),
));
```

The Bridge refills the keys, steps and drains in one call (`Bridge::collection_pump`):
`refill_client_keys`, then `stores.collection_pump(&self.client_keys)`, then
`start_store_jobs_of(StoreId::Collection)`. The runner's `launch_collection`
(`rust-modules/src/app/adapters/stores.rs`) is where the thread starts.

Hubs (#1011) is the same shape for several sources at once, except that it raises its claim at the
launch and not in the step (`docs/sans-io.md`, "The Hubs lane"). A new store also joins
`Stores::take_jobs_of`, `has_jobs_of` and the `*_FILLS` list of each fanning call that reaches it
(`rust-modules/data/src/AGENTS.md`).

**(b) The step needs the session, the profile or a client: the Bridge reads, the step takes the
value.** #1108 removed the session reads from the Watchlist and catalog stores (the `client_for` and
roster reads that remain are listed in `docs/sans-io.md`). The Bridge builds the input
(`Bridge::watchlist_input`) and the step receives it:

```rust
pub(crate) fn watchlist_input() -> plx_data::watchlist::WatchlistInput {
    let profile = Self::seated_profile();
    let credential = profile.user.as_ref().and_then(plx_plex::plex::session::plex_tv_credential);
    let client_id = plx_plex::plex::session::peek().client_id.clone();
    plx_data::watchlist::WatchlistInput::new(profile, credential, &client_id)
}
```

The Bridge calls the store's `run_with_input(cmd, &WatchlistInput)` (`WatchlistStore` in
`data/src/stores/watchlist.rs`), and the step compares with `WatchlistAuth::is_current(&CurrentProfile)`
(`data/src/watchlist.rs`), a pure function. The Browse pins did the same (#1106:
`Bridge::browse_session`, then `BrowseStore::run_with_session`). Which client a server holds is an
input too: the Collection step asks `keys.key(sid)` of the `ClientKeys` it is handed (`maybe_spawn`
in `data/src/collection.rs`), never `client_for`.

A screen gets a session read the same way: the Bridge reads it and folds it into what a store
publishes (`Bridge::household_ids` into `Stores::search_snapshot`). The screens' own `peek` and
`client_for` reads are ledger lines (`ci/allow/globals/screens.txt`), not a pattern.

**(c) A stepped machine: `step` returns effects, a runner executes them.** The player lifecycle
(#1003) is `step(&mut self, ev, fx: &mut Vec<ForegroundEffect>) -> Handled` (`lifecycle.rs`).
It reads no global; the pause arrives on the event (`Park { viewer_paused, .. }`). The driver runs
each effect through an actuator and feeds the answer back as the next event
(`run_foreground` in `media/src/player/foreground.rs`):

```rust
while let Some(ev) = next.take() {
    let handled = lifecycle.step(&ev, &mut fx);
    for effect in fx.drain(..) {
        match effect {
            ForegroundEffect::Prepare { id, resume_ns } => {
                next = Some(ForegroundEvent::PrepareAnswered {
                    id,
                    answer: PrepareAnswer::of_start(actuator.prepare_resume(ps, resume_ns)),
                });
            }
```

A screen is the same kind of machine: it pushes an `AppFx` (`screens/src/registry.rs`) and the
Bridge's `app_effect` runs it; an arm that does IO without pushing `Fx::Deliver(` is a new `fxlanes` site.

**(d) The step needs time: count the `dt` it is handed.** `Tick` is "the ONLY clock a machine sees"
(`machine/src/machine.rs`). Search's debounce accumulates whole microseconds, an exact integer
compare (`data/src/search.rs`):

```rust
let dt_us = (dt * 1_000_000.0).round() as u32;
state.settle_us = state.settle_us.saturating_add(dt_us);
if state.settle_us >= SETTLE_US_TARGET {
```

Never `Instant::now` or `SystemTime::now` in a step. The `wall` rule (`ci/check-deps.sh`; its
instruments are listed in `ci/allow/wall.txt`) fails a clock read in most machine scope: `ui/`,
`screens/`, `data/src/stores/`, the session owner and the migrated store files. Outside it, CI does
not count the ones that remain (a diagnostic timer in `step_landings_with_scope` in `data/src/pms.rs`,
one in the fetch body `fetch_full` in `data/src/metadata.rs`), and `WatchlistIo::today` is a
documented remaining read; do not add to them.

**(e) A test needs different behaviour: a seam or an argument, never a `cfg` branch.** A body takes
the one call that reaches the world as a parameter. `finish_browse_page_job` passes the real read
and a test passes one that panics (#1028, `data/src/browse/listing_jobs.rs`):

```rust
pub fn finish_browse_page_job(port: &BrowsePagePort, addr: Addr, client: &'static Client, req: BrowsePageReq) {
    finish_page_with(port, addr, client, req, fetch_listing_page);
}
fn finish_page_with(port: &BrowsePagePort, addr: Addr, client: &'static Client, req: BrowsePageReq,
    read: impl FnOnce(&Client, ServerId, &SectionQuery<'_>, bool, Option<&Restore>) -> ListingPage) {
```

An IO trait works the same way when only the job body calls it (`CatalogIo`, `data/src/catalog.rs`);
a trait the step calls (`WatchlistIo`) is a registry read with one more hop. The shape to avoid is
`#[cfg(not(test))]` or `if cfg!(test) { fake } else { real }` in production code:
`ci/allow/forks/*.txt` list the ones that remain, to be deleted, not copied. An item only
a test can see (`#[cfg(test)] fn`) is no fork.

**(f) A command arrives without its input: refuse it loudly.** A `run` that holds no input refuses
the commands that need one, with a debug assertion and a log line, and returns without acting.
`BrowseStore::run` in `data/src/stores/browse.rs` (the watchlist's twin is `WatchlistStore::run` in
`data/src/stores/watchlist.rs`):

```rust
if matches!(cmd, BrowseCmd::ApplyPins(_) | BrowseCmd::Discovery(_)) {
    debug_assert!(false, "a pin or discovery command reached a step handed no session");
    plx_base::eventlog::log(
        "browse: a pin or discovery command reached a step handed no session — dropped");
    return false;
}
```

## `task::beside` and blocking calls

`task::beside` (`base/src/task.rs`) asks for a request on a scoped sibling thread and joins it
before the scope returns. The `workers` gate does not count it, which is not permission to use it
in a step: it exists for a worker-side body that has several independent requests (`build_stream`
in `route/plan.rs`, which takes `&OffFrame`, and the detail chain in `metadata.rs`). Anything that
blocks belongs in such a body, or in the runner. A step never calls it, and never `allow_blocking`.

## The ledgers

`ci/allow/{workers,globals,forks,fxlanes}/` only shrink. A new line there, a raised `# count:`
header or a raised `PINNED_LEDGER_TOTALS` entry (`tests/test_harness.py`) is never the fix: CI
(`ci/check-ledger-ratchet.py`) fails a PR whose ledger counts rise above its base's, so all three
raised together still fail. If a step truly seems to need IO, stop and say why in the PR or the
handoff; do not add the exception.
A one-for-one rename of the fn that already holds a site, the site itself unchanged (#1116 renamed
`mod.rs::sink` to `mod.rs::raw_sink` in `ci/allow/forks/media.txt`), changes only its key; say so in
the PR. It never covers a new call. A migration deletes lines (`docs/sans-io.md`, "How a migration
shrinks a ledger"). The ledgers hold today's numbers; read the files, do not copy totals into prose.

## Before you commit machine code

1. The step calls no spawn, submit, curl, file, LS2 or sleep, and no `task::beside`.
2. It reads no registry, matched or not (the list above); the Bridge passes the value in.
3. It reads no clock; time is the `dt` or `Tick` it is handed.
4. Work leaves as a job on the store's outbox or as an effect, and the answer comes back as an
   event or through the job's `Port` (exactly one `done` or `refused`).
5. Whoever steps a store drains it in the same call (`start_store_jobs_of`).
6. No `cfg(test)` or `cfg!(test)` in production code; the test uses a seam or an argument.
7. A command that needs an input and lacks it is refused (assert plus log), not defaulted.
8. `git diff ci/allow tests/test_harness.py` shows only deleted lines, lowered counts, or such a
   one-for-one rename.
