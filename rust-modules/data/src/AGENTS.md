# data/ stores: the Sans-IO recipe (read before adding or changing a store)

Every store here is a machine: its step (`run`, `pump`, `Machine::step`) must start no worker, read no
process-global registry and read no clock. Work leaves as a job on the store's outbox, and the
answer returns through the job's port. `docs/sans-io-guide.md` has the general moves and the
checklist; this is the store case, which is the most common one. `HudStore` (`hudpacks.rs`) is an
exception: a stepped machine registered in `ci/machines.ini`, with effects out and addressed events in, and it
has no outbox and no `Stores` wiring, so rule 6 does not apply to it.

## Who does what

- **The store (this crate)** keeps state and the step; owns its `StoreJob` variant (`stores/mod.rs`,
  `enum StoreJob`), a request type that captures everything the worker needs (the server, the
  `ClientKey` the step decided on, the work), a port, and the IO body `finish_<kind>_job(port, addr,
  client, req)`. Model: `collection.rs` (`maybe_spawn`, `CollectionReq`, `CollectionPort`, #987).
- **The runner (`rust-modules/src/app/adapters/stores.rs`)** resolves the key, records the tape's
  admission and starts the thread (`launch_collection`). This is the only place in the path that
  calls `client_for` or `spawn_small`.
- **The Bridge (`rust-modules/src/app/bridge.rs`)** refills `ClientKeys`, steps and drains the outbox
  in ONE call (`collection_pump`), and reads the session where a step needs one.

## Rules

1. **The step emits, it never starts.** Claim the request in the step (`adapter.fetch.claim(gen)`),
   push `StoreJob::<Kind>(Job { addr, req }, Port)`, return. Hubs raises its claim at the launch
   instead, because a replayed Home-admission job is never launched (`docs/sans-io.md`, "The Hubs
   lane").
2. **A registry read is an argument.** Which client a server holds is `&ClientKeys`
   (`keys.key(sid)`); the seated profile and the plex.tv identity are a `&WatchlistInput`
   (`Bridge::watchlist_input`, into `WatchlistStore::run_with_input`; #1108); a session is a
   `&Session` (`Bridge::browse_session`, #1106). Not in a step: `client_for`, `current_server`,
   `session::peek`, `peek_settled`, `current_gen`, `server_ids`, `server_roster_gen`, `tape::active`,
   a clock.
3. **A command that needs an input and was handed none is refused**, with a `debug_assert!` and a
   log line, not defaulted to the registry (`BrowseStore::run` in `stores/browse.rs`,
   `WatchlistStore::run` in `stores/watchlist.rs`).
4. **Every job gets exactly one answer on its port**, `done` or `refused`. A refusal the step can
   observe is what stops the next pump resending the same request every frame: `Fetch::refuse_for`
   with `take_refused` at the head of the next pump, or a store-local `NotStarted` landing
   (`docs/sans-io.md`, "A refusal the step can observe").
5. **The body takes its client as an argument**, keeps the `catch_unwind` guard and posts the
   answer outside it. A test swaps the one call that reaches the server through a parameter
   (`finish_page_with` in `browse/listing_jobs.rs`), never a `cfg(test)` branch.
6. **Whoever steps a store drains it, in the same call** (`Bridge::start_store_jobs_of`). A new
   store joins `Stores::take_jobs_of`, `has_jobs_of` and the `*_FILLS` list of every fanning call
   that reaches it; one that does not strands its jobs until the Bridge's backstop, which the test
   rigs count as a bypass.
7. **Results return through the landing and its gate** (`plx_machine::landgate`), where the pump
   takes them by value, so the replay anchors hold; Catalog's mailbox and Metadata's episode-page
   and Related-window takes are ungated. For a writer (`viewstate.rs`): one write is out at a time,
   a refused one never reached the server and is judged again, and a worker that panics answers
   with a failed write, never `NotStarted`.
8. **Time is the `dt` the pump is handed** (Search's debounce, in `search.rs`).
9. **No new line in `ci/allow/**` and no raised count.** A one-for-one rename of the fn that already
   holds a site, the site itself unchanged (#1116 renamed `mod.rs::sink` to `mod.rs::raw_sink` in
   `ci/allow/forks/media.txt`), changes only its key; say so in the PR. It never covers a new call.
   If the step seems to need IO, stop and say why in the PR.

Read `docs/sans-io-guide.md` and the store's own section of `docs/sans-io.md` before you start.
