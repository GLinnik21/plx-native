# Sans-IO: logic never performs IO

Status: the rule is written down and its four ledgers are gated (`ci/check-deps.sh`, inside
`make check`). Nothing here changes product behaviour: each ledger freezes the sites that exist
today, so that later changes can only shrink them. The player's foreground lifecycle is the first
player machine: `player/lifecycle.rs` is the machine alone (private state, one writer, `step`),
`player/foreground.rs` its drivers and `player/transport.rs` the transport-pause contract; its shape
is gated by `ci/machines.ini` (see "Machine shape" below).

## The rule

Logic never performs IO. A machine (a screen, a `data/` store, the session owner, the player's
foreground lifecycle, a route planner) starts no worker, makes no network, storage or LS2 call, and
reads no wall clock or process-global registry; its work leaves only as a typed effect value
carrying its address, the app's runners execute it, and every result returns as one addressed event
in the same drain, so tests and replay deliver events and logic has no `cfg(test)` fork.
Allowlists only shrink.

The reason is testing. A machine that is a pure function of (state, event) has no seam to fake:
a test and a replay feed it the same events the product does, so what the tests prove is what
ships. Every site below is a place where that is not true yet. `docs/stores-as-machines.md` is how
the `data/` stores became machines; this is the rule that finishes the job.

## What the four ledgers cover

Each ledger is a rule in `ci/check-deps.sh`, found by `ci/ledger.awk` (comments, string literals and
test code are never sites; the awk's header says exactly how). Every production SITE is one line,
and the lines are compared to the tree as an exact multiset.

| Rule | A site is | Key |
|---|---|---|
| `workers` | a production call that starts a worker in a machine: `task::spawn(`, `spawn_small(`, `spawn_small_keeping(`, `spawn_small_or_inline(`, `spawn_off_frame(`, `spawn_off_frame_keeping(`, `storage_worker::submit(`, `submit_retained(`, the command worker's `.submit_ticket(` | `<path>::<enclosing fn>::<spelling>` |
| `globals` | a production read of a process-global registry in a machine: `client_for`, `current_server`, `current_snapshot`, `keypin::revision`, `keypin::blocked_for`, `session::peek`, `grant::offers`, the player's transport-pause readers (`transport::paused`, `transport::viewer_paused`, `preview::paused`), and `player::TX` in the player's lifecycle (at zero) | `<path>::<enclosing fn>::<spelling>` |
| `forks` | a production test/production fork whose predicate reads `test` or `feature = "test-support"`: a `#[cfg(..)]` attribute that a production build can satisfy (`not(test)`, `not(any(test, feature = "test-support"))`, `all(target_os = .., not(test))`, ...), or an inline `cfg!(..)` of any value | `<path>::<item name>`, or `<path>::<enclosing fn>` for a statement, a field or an inline `cfg!` |
| `fxlanes` | an `AppFx::<Variant>` arm of `Bridge::app_effect` that does not push `Fx::Deliver(`: one site for each variant on its head, and one for an arm that names no variant (`other => ..`, `_ => ..`) | `<path>::AppFx::<Variant>`, or `<path>::AppFx::_` |

`blocking` (the frame thread's `allow_blocking(` exceptions) is the older ledger these follow. It now
runs on the same scanner, so it lists the same sites as before but reads them more accurately: it skips
any item under a `#[cfg(..)]` that production can never satisfy (not only a `mod`; also a `mod` with
other attributes in front, and `feature = "test-support"` and `all(test, ..)` items), ignores string
literals and block comments, and sees a call after a `"http://"` string that the old `//` stripping
dropped.

**Scope.** `workers` and `globals` read the machines: everything under `rust-modules/data/src`,
`session/src`, `screens/src`, `ui/src`, `appkit/src` and `machine/src`, plus
`plex/src/plex/session.rs` (the session owner's state), the player's `lifecycle.rs` and the route
planners `route/plan.rs` and `route/plan_memory.rs`. The IO layers (the rest of `plex/`, `media/`,
`net/`, `platform/`) are where those calls belong, so they are not scanned. `forks` reads the whole
of `data/`, `plex/`, `media/` and `session/`, because a fork anywhere in them is a test that does not
run the product's code. `fxlanes` reads the one fn, `Bridge::app_effect` in `rust-modules/src/app/bridge.rs`.

**Decisions the table hides.**

- A bare `spawn(` / `submit(` / `peek(` / `revision(` / `blocked_for(` / `offers(` is a site only in a
  file that imports it from `task`, `storage_worker`, `session`, `keypin` or `grant` (singly, as the
  last or any other item of a braced group, or through a glob; `Builder::spawn`, a local fn and
  `it.peek()` are not workers or registry reads); `peek` is also a site in
  `plex/src/plex/session.rs`, the module that defines it. A local wrapper (`spawn_discovery`) is no
  site of its own: the real call inside it is.
- A getter named without a call (`.and_then(client_for)`) reads the registry as surely as a call
  does, so it is a site. Only a rename (`use ...::client_for as x`) fails outright, because the
  calls through `x` are invisible to the scan.
- The command worker's `.submit(` is not covered: it is `#[cfg(any(test, feature = "test-support"))]`
  in `base/src/storage_worker.rs`, so no production line can call it. `.submit_ticket(` is covered
  although no machine calls it today: a machine that did would be starting a worker.
- The transport-pause atomic (`player::TX`) is reached by a machine through three readers,
  `transport::paused`, `transport::viewer_paused` and `preview::paused` (each is `TX.paused` behind
  a function), so those spellings are sites, qualified or bare after an import; a `use ... as`
  rename of any of them fails outright. The spellings of `TX` itself (`crate::player::TX`,
  `player::TX`, `super::TX`, a bare `TX`) stay pinned to `lifecycle.rs`, the machine, which now reads
  none: that pin is a zero-guard (the machine takes the pause as a value on its event). The HUD's
  direct `TX.paused` reads are not a machine's, and a writer (`set_paused`, `set_transport_paused`) is
  an IO call, not a registry read. Moving the contract out of `lifecycle.rs` deleted its three `TX`
  lines from `ci/allow/globals/media.txt`; the same change widened the rule to the readers above and
  seeded the three `preview::paused` reads the screens already had (`screens/src/detail/mod.rs`) as a
  coverage expansion, so this change's net effect on the pinned total is zero (a relocation lowers no
  pin on its own).
- `forks` counts an attribute the product can satisfy. `#[cfg(test)]` and `any(test, feature =
  "test-support")` items are skipped whole, to the end of their own body (only a test can see them);
  `all(test, ..)` too. `any(feature = "devtriggers", test)` is counted, because a dev build can
  satisfy it. An inline `cfg!(..)` is counted whatever it evaluates to, `cfg!(test)` included:
  `if cfg!(test) {..} else {..}` compiles an arm that production runs and a test does not.
- `fxlanes`: an arm that pushes `Fx::Deliver(` rides the ordered FIFO, where a result returns as one
  addressed event in the same drain. An arm that parks its request on a side queue of the bridge
  (`reqs`, `content_reqs`, ...) is a second lane with its own timing, so it is a ledger line, and a
  NEW variant with a side queue fails the gate. An or-pattern arm (`AppFx::A(r) | AppFx::B(r) =>
  ..`) is one site for each variant, and an arm that names no variant (`other => ..`, `_ => ..`) is
  the site `AppFx::_`.
- A key names the file and the fn's name, and a ledger is a multiset of keys, so a site can move
  between two same-named fns of one file (a `new` in each of two `impl` blocks) without the gate seeing it.

## Machine shape

`ci/machines.ini` lists the state machines whose shape is gated, one `[file::Struct]` section each
(paths relative to `rust-modules/`), with the machine's `event` and `effect` types and a `step`
line saying how it is exposed, driven and owned. `ci/check-machine-shape.py` (in `make check`)
fails on a `pub`, `pub(crate)` or `pub(super)` field of a listed struct, so its state stays
private and its one writer stays `step`, and on an entry whose file or struct is gone. It also
prints one line per machine with the number of its `bool` and `Option<..>` fields: a report-only
flag ratchet, read in review and never red. The first entry is `ForegroundLifecycle`.

## Layout

One file per rule and area, `ci/allow/<rule>/<area>.txt`. The area is the crate (`app` for
`rust-modules/src`), except in `data/`, where it is `data-<module>` for the top-level module under
`data/src`, and `stores/<stem>.rs` joins the module of its own name (`stores/watchlist.rs` and
`watchlist.rs` are one area, `data-watchlist`). One file per area means two migrations that touch
different areas never edit the same `# count:` header. The directories do not collide with the flat
`ci/allow/*.txt` rule names, which `allowed()` indexes.

Each file starts with `# count: N` (it must equal the entry count), then a comment header saying what
the rule is, then one key per line. A line filed in the wrong area fails the gate and names the right one.
`tests/test_harness.py` pins each rule's total (`PINNED_LEDGER_TOTALS`), so a ledger and its count
cannot grow together unseen.

## What fails

- a site that no ledger line names (the message names the file it belongs in);
- a ledger line whose site is gone;
- a `# count:` that is not the number of entries;
- a line in the wrong area file;
- a rename of a worker start or a registry getter (`workers`, `globals`).

## How a migration shrinks a ledger

A migration makes one machine pure: its worker start becomes a typed effect the app's runners
execute, its registry read becomes a field of the event or an argument, its `cfg(not(test))` arm
becomes one code path that tests and the product both drive. The PR then:

1. deletes the machine's lines from its `ci/allow/<rule>/<area>.txt` files;
2. lowers each file's `# count:` and the rule's number in `PINNED_LEDGER_TOTALS`;
3. leaves the gate green (`ci/check-deps.sh`).

It never edits a line to make a new site pass, and never adds one: a new worker start, registry read
or fork in a machine is moved behind an effect instead. A ledger file that reaches zero entries is
deleted; a rule whose last file is gone has a total of zero, and its pin says so.

A store the Bridge runs leaves as a store job, not an `AppFx`, while a stepped machine such as the
session owner leaves as an `Effects` value; the next section is the rule for the two.

## The store job lane

The Metadata store's detail request (`request_detail`) was the first to stop starting its own
worker. Its three ledger lines are gone and the shape is the one the other `data/` stores copy.
The fetch chain behind that job now takes its client as an argument too (Metadata M1), as the
Collection, Person, Hubs and subtitle-search bodies already do: `StoreRunner::start_detail` resolves
the server's client once, at the admission, and `finish_detail_fetch` hands it to `fetch_full` and
through it to the GETs of the chain, so the eight `client_for` reads of `fetch_detail`,
`fetch_item_streams`, `fetch_seasons`, `fetch_primary_trailer`, `fetch_extras_rows`,
`resolve_trailer`, `fetch_related` and `fetch_playing_item` (whose caller, the stream resolve,
already holds its client) are gone from `data/src/metadata.rs`. The old chain re-resolved before
each GET: after a sign-out its later GETs found no client, and after a re-point they went to the
slot's new client. This chain keeps the client it was handed. The registry blanks that client's
token on both events (`revoke_all`, and the re-point arm of `register_lazy`), so the rest of the
chain asks tokenless and lands `None`: the same as before for a sign-out (the requests are now sent
and refused where they used to be skipped), and a partial page where the old chain followed a
re-point. Bar the opening season's episodes it never mixes clients within one page. The one read
left in it is those episodes (`fetch_episodes`), shared with the season worker until that moves.
The Collection page's request is the second: its worker start and its three `client_for` reads are
gone from `data/src/collection.rs`. The Person page's requests are the third: its two worker starts
and its one `client_for` read are gone from `data/src/person.rs`, but unlike the two before it the
Person step is not pure (the bullet headed "What the Person step still reads" lists what it still
reads). Person has up to six requests out at once, so a pump emits several jobs, one per fetch slot,
in ascending slot order. The catalog page's read is the fourth: its one worker start is gone from
`data/src/stores/catalog.rs`. Unlike the other three it names no server and no client (it is a
plex.tv read as the captured identity), so it takes no `ClientKeys` and no tape admission, and the
Catalog step is not pure either (the bullet headed "What the Catalog step still reads" lists what it
still reads). Subtitle search and the view-state read-back are the fifth and sixth (the bullet
headed "Subtitle search and the view-state read-back"). Home's Hubs read is the seventh (the bullet
headed "The Hubs lane"): the worker start and the `client_for` read are gone from `data/src/pms.rs`
and its three `data-pms` ledger files are deleted. The Search screen's per-source fetches and its
recents writes are the eighth (the bullet headed "The Search lane"): both worker starts and both
`client_for` reads are gone from `data/src/search.rs` and `data/src/search/recents.rs`, and its two
`data-search` ledger files are deleted.

- **The shape.** `plx_machine::job` holds `Job<R>` (an `Addr` and a request that carries everything
  the worker needs), `Port<T>` (where the one answer goes: exactly one `done` or `refused` per job)
  and `assert_fx!` (a step test reads the jobs it emitted instead of waiting on a thread). A store
  keeps its own job enum, request types and port: `plx_data::stores::StoreJob`,
  `MetadataStore::take_jobs`, `plx_data::metadata::{DetailReq, DetailPort}`,
  `plx_data::collection::{CollectionReq, CollectionPort}`,
  `plx_data::person::{PersonReq, PersonPort}` and
  `plx_data::stores::catalog::{CatalogReq, CatalogPort}`,
  `plx_data::pms::{HubRequest, HubsPort}`,
  `plx_data::search::{SearchReq, SearchPort}` and
  `plx_data::search::recents::{SaveRecentsReq, SaveRecentsPort}`. The app's `StoreRunner`
  (`app/adapters/stores.rs`) executes a job and owns what `data/` must not: the tape's admission
  record, the thread and the `client_for` read. It is `Live` or `Held` (a test parks the job and
  runs it on the test thread), chosen when the `Bridge` is constructed; a newer Detail request
  retires only the parked Detail jobs, never another store's. `Held` parks per kind:
  `StoreRunner::held()` leaves the Hubs read live, as production runs it (a rig that waits for a
  Home landing needs a real one), and `StoreRunner::held_all()` parks it too, for a test that
  chooses when the read settles (`Bridge::run_held_store_jobs_of_for_test`).
- **A registry read is an input.** A step that decides from "which client does this server slot
  hold" is handed `plx_plex::plex::ClientKeys` (one `ClientKey { instance, token }` per server) as
  an argument. The Bridge owns one buffer and refills it in place
  (`adapters::stores::refill_client_keys`, the only loop over `client_for` that feeds a step)
  immediately before the step, so a pump that runs every frame allocates nothing for it. The job
  carries the key its step decided on, and the runner starts it only if `client_for` still answers
  that exact client inside the launch the tape's admission record holds: a replay and a recording
  then agree on the one boolean they hold, and a refused job hands its claim back.
- **The Detail job carries no key.** Its step reads no registry, so `DetailReq` names a server and
  no `ClientKey`: the runner reads `client_for(sid)` ONCE in `start_detail`, records that client's
  instance in the admission and runs the chain through that same client (bar the opening season's
  episodes). That is a presence check, not the key compare of the jobs above. A server the registry
  holds no client for at the admission still launches, and its worker answers the failure without
  asking anything, as the first read of the old chain did.
- **Two drains, one rule.** A stepped machine (the session owner) emits its work through `Effects`,
  and the owner's drain starts it. A store the Bridge runs emits through its own `take_jobs`.
  `Stores::take_jobs_of(StoreId)` takes ONE store's outbox and `Stores::take_jobs` aggregates every
  store's, and each Bridge entry point drains the outboxes its step can fill, in the same call as
  the step. A step that touches only its own store drains it with `start_store_jobs_of`; a `Stores`
  call that fans out to other stores (`viewstate_run`, `viewstate_pump`, `notify_pump`) drains every
  outbox named in its `*_FILLS` list (`Stores::VIEWSTATE_FILLS`, `Stores::NOTIFY_FILLS`) with
  `Bridge::start_store_jobs_in`. `Bridge::metadata_run` steps then drains Metadata; `Bridge::person_pump` and
  `Bridge::collection_pump` refill the keys, step, then run
  `Bridge::start_store_jobs_of(StoreId::Person)` and `(StoreId::Collection)`;
  `Bridge::catalog_open` and `Bridge::catalog_pump` step the Catalog store and run
  `(StoreId::Catalog)` (no keys to refill; the pump drains because its retry and its profile-switch
  re-open both emit). `Bridge::search_run` drains `(StoreId::Search)` after every command (a recents
  edit emits its write there) and `Bridge::search_pump` refills the keys, steps and drains it.
  Draining inside each pump, not at the frame's end, keeps every store's
  admissions in the tape where they were: the frame pumps Person, then Collection, then reconciles
  the Detail page. A caller that holds only the store (`describe_movie` gets a `&mut MetadataStore`)
  drains with `Bridge::start_store_jobs` the moment its call returns. A new store joins by implementing its
  own outbox and adding it to `Stores::take_jobs_of` and `Stores::has_jobs_of` AND to the `*_FILLS`
  list of every fanning call that reaches it. Joining only the first pair strands its jobs until the
  backstop, which the rigs count as a bypass.
- **Whoever steps a store drains it.** `MetadataStore`'s `Machine::step` runs the command and does
  not drain: a `RequestDetail` stepped through it leaves its job on the outbox and nothing starts
  it. The same holds for the `pub` `Stores::metadata_run`. The owner drains in the same call, as
  `Bridge::metadata_run` does; no type enforces it, the counter below catches the lapse.
  `CollectionStore`'s and `PersonStore`'s `Machine::step` are unreached and pass empty keys: a
  future sender must pass the host's keys and drain. The same holds for the public
  `CatalogStore::open` and `CatalogStore::pump`: `catalog_feed` and `catalog_reread` call
  `Bridge::catalog_open` and `Bridge::catalog_pump`, which drain in the same call. And for
  `HubsStore::run_with_directory`, `land_with_directory` and `tick_with_directory`: the
  Bridge's `Store(Hubs(..))`, `HubsResult` and `StoreWork::Hubs` arms and `Bridge::hubs_run` drain
  with `start_store_jobs_of(StoreId::Hubs)` in the same call, and a controlled Home arm admits the
  jobs (`Bridge::admit_hubs_jobs`, the bullet headed "The Hubs lane"). And for the public
  `SearchStore::run_with_directory` and `pump_with_directory_and_gate`: the Bridge's
  `Store(Search(..))` and `StoreWork::Search` arms, `boot.rs`'s reset and `dev/scenarios.rs`'s query
  all reach them through `Bridge::search_run` and `Bridge::search_pump`, which drain in the same call.
- **Results do not return as FIFO events.** A gated store's result (Detail, Collection, Person,
  Search, subtitle search, the view-state read-back) goes back through the landing and its gate
  (`landgate`, the `adapter_rank` of the landing key), where the pump takes it by value. This is a
  deliberate exception to "every result returns as one addressed event in the same drain": it is
  kept so that the replay anchors hold, because the landing is where they already record a result.
  Catalog's landing is an ungated mailbox take (`CatalogStore::pump` takes no `Gate`): its answer,
  its `NotStarted` refusal and its panic all return as a plain landing that the next
  `Bridge::catalog_pump` takes, and the replay anchors do not record them.
- **Why the job is not an `AppFx`.** A new `app_effect` arm that starts IO without pushing
  `Fx::Deliver(` is a new `fxlanes` site; `bootstrap::effects::app` fails closed on a variant it
  cannot encode in controlled Home; and every `Fx::App` is an `"e":"App"` record, so the replay
  fixtures would change. A later store that routes its jobs through `AppFx` brings all three back.
- **A bypassed drain is a red test, and in production a logged one.** `metadata_pump_detail` keeps
  a backstop under the senders: a job it finds on any store's outbox is started there, so the
  request still settles, and is counted in the Bridge's `late_store_jobs` and logged as one `stores: WARN` line.
  `Bridge::assert_no_store_jobs_left_for_test` (`#[cfg(test)]`) fails a frame that ends with a job
  still on an outbox or with that count above zero, so a sender that bypassed its store's Bridge
  drain (`metadata_run`, `person_pump`, `collection_pump`, `catalog_open`, `catalog_pump`,
  `subsearch_pump`, `search_run`, `search_pump`, `reconcile_pump`, `hubs_run`, `viewstate_run`, `viewstate_pump`, `notify_pump`,
  `start_store_jobs_of`, `start_store_jobs_in`, `start_store_jobs`)
  is red in every rig that makes the check, the one that runs `update()`'s pump included. The rigs
  make it at the frame's end.
- **The port holds the adapter `Arc`.** `Reset` replaces the adapter and the store keeps no handle to
  the retired one, so an epoch id could only reach it through a registry of retired adapters. A job
  emitted before a `Reset` answers into the adapter it was made for, as the request did before.
  `PersonPort` also carries the slot and the generation its claim was made under, because several
  Person jobs are out at once and `Addr.req` is only a per-job sequence there (`RequestId` stays
  unique per addressee): `done` and `refused` use the port's slot and generation, never the
  address, so a refusal gives back only its own generation's claim.
- **What the Person step still reads.** Person starts no worker and reads no `client_for`, but its
  step is NOT pure. It still reads `tape::active()` (which decides whether a job exists and carries
  the controlled identity), `session::peek_settled()` (the settled plex.tv account the two global
  reads are made as; the gate's `session::peek(` spelling does not match it), the session watch, the
  refused-discover check (`discover::person_profile_refused` / `person_credits_refused`, which
  lands the refusal in the step with no job, because routing it through the runner would release
  and respawn it every ~2 s), `plx_base::devtrig::read("personcredits")` (dev-trigger builds only:
  `seed_dev_credits` reads it on every pump while a person is open and no tape is armed),
  `server_ids()` (the source roster) and `server_roster_gen()`. Its
  two fetch bodies, `fetch_profile` and `fetch_credits`, stay in `data/person.rs`, so their `forks`
  ledger lines stay. A source whose server holds no key backs off in the step and emits no job.
- **What the Catalog lane does, and does not, change.** Catalog is a plex.tv read: the job carries
  the captured `WatchlistAuth` and the title, and the read goes through the store's `CatalogIo`
  seam, which rides on the job's `CatalogPort` (so the request is a comparable value). The body is
  the one the store's worker ran: `LiveCatalogIo::fetch`, so the dev-build read refusal inside
  `AccountClient::watchlist_transport` is unchanged. Two behaviours are new. The body runs under
  `catch_unwind` and posts its answer outside the guard, so a read that panics lands as
  `Load::Unreachable` instead of leaving the page on `Loading` for ever. And a job the runner
  could not start (the OS refused the thread) answers through the port's `refused`, which posts a
  store-local `NotStarted` landing into the store's mailbox: the next pump marks the request owed
  and asks again after `RETRY_FRAMES` (120) pumps, with no job in between. That refusal is applied
  one pump later than the failed spawn it replaces was. A landing, a refusal or a panic: every job
  is answered once.
- **What the Catalog step still reads.** `open` and `pump` call the store's `CatalogIo`, whose
  production seam (`LiveCatalogIo`) calls `WatchlistAuth::capture()` and `WatchlistAuth::is_current()`
  in `data/src/watchlist.rs`, whose `session::peek` and `current_snapshot` reads stay on the
  `data-watchlist` globals ledger (`capture::current_snapshot`, `capture::session::peek`,
  `is_current::current_snapshot`) and leave only with the Watchlist store's own migration. The
  Catalog step is therefore not pure, and claims no purity. Its test seam, `with_spawn_for_test`, is
  gone: a test runs the emitted job through `finish_catalog_job`.
- **A refusal the step can observe.** The runner may decline to start a job: the thread ceiling, a
  registry that moved on between the step and the launch (the client the job names is no longer the
  one the slot holds), or a `Held` runner whose client is gone. It then calls the port's `refused`,
  so every job gets exactly one answer, a refusal included. A refusal that only released the claim
  would have the next pump send the same request at once, every frame, so the step has to see it. A store whose claim is a `Fetch` (subtitle search)
  reads `Fetch::take_refused(gen)` at the head of its next pump: the port's
  `Fetch::refuse_for(gen)` frees that generation's claim and remembers the refusal for
  `take_refused` to answer once, and a refusal for a superseded generation (one whose claim a newer
  request or a supersede has replaced) frees nothing and records nothing. A store whose claim is a mailbox of landings
  (the view-state read-back) has its port post a store-local `NotStarted` landing, and the next pump
  puts the flight's items back and waits. A refusal never goes into a recorded mailbox (a recorded
  take would write it as an async result and change the bytes). It reaches the step one pump after
  the launch; no fixture holds a refused start, so no recorded byte moves.
- **Subtitle search and the view-state read-back** are the fifth and sixth stores on the lane. The
  worker start and the `client_for` read are gone from `data/src/subsearch.rs` and
  `data/src/stores/viewreconcile.rs`, and their four ledger lines are deleted. Each step reads
  `plx_plex::plex::ClientKeys` as an argument and emits a `StoreJob`
  (`plx_data::subsearch::{SubSearchReq, SubSearchPort}` and
  `plx_data::stores::viewreconcile::{ReconcileReq, ReconcilePort}`); the runner starts the job
  only if `client_for` still answers the key the step decided on. `Bridge::subsearch_pump` and
  `Bridge::reconcile_pump` refill the keys, step, then run `Bridge::start_store_jobs_of` for their
  own store in the same call; `Bridge::subsearch_run` only drains, because the command step takes no
  keys and emits nothing. Neither kind is on the tape, so the runner makes no admission record for
  them and a replay launches them live.
- **The Hubs lane.** Home's source reads (`data/src/pms.rs`) are the seventh store on the lane. The
  kick decides from `ClientKeys` which sources to read, mints the request id and emits
  `StoreJob::Hubs(Job<HubRequest>, HubsPort)` on `HubsStore`'s outbox instead of spawning.
  `HubRequest` carries what the worker needs (the server, the `ClientKey` its step decided on, the
  page, the held windows, the deck lane); `HubRequest::fetch` is the IO body and `complete` builds
  the landing from it (`launch_hubs_job` and `finish_hubs_job` are the runner's two calls). The port
  holds the adapter `Arc`, so a job emitted before a `Reset` answers into the adapter it was made
  for. The Hubs read differs from the six before it in four ways.
  - **A third admission family.** `StoreJob::admission()` names `Tape` (Detail, Collection, Person),
    `Direct` (Catalog, subtitle search, view reconcile, Search's fetches and recents writes) or `Home`,
    as one exhaustive match. A `Home`
    job is recorded in a controlled Home run's own ordered ledger (`HomeIo::admit`), not on the tape,
    with the bytes `HubRequest::admission` returns (the bytes `hubs_descriptor` returned before it,
    unchanged). A controlled arm of the Bridge steps Hubs, then admits what it emitted inside the
    arm with `StoreRunner::start_admitted(job, Some(io))`, so the hubs line precedes the discovery
    line in the ledger (`home_admission_tests` pins that order). Every other sender uses `start`,
    which launches a `Home` job as a direct one. Replay pops the recorded answer and never
    launches: a recorded launch answers nothing (the recorded landing settles it), a recorded
    refusal is answered once by the runner.
  - **The claim is raised at the launch.** `PmsAdapter::owed` (what the landing gate's dump mode
    waits on) goes up on the calling thread before the worker exists (`HubsPort::claim` inside
    `launch_hubs_job`; in `Held` at the park) and comes down when a take moves the landing out, or
    when the launch fails. It is never raised at emission, because `Port` has no drop guard: a job
    the runner never launches (a replay's recorded admission, a refusal, a client the registry no
    longer holds) holds no claim.
  - **A refusal is not a landing.** `HubsPort::refused` pushes the request id on
    `PmsAdapter.refused`, a list outside the landing mailbox: only the mailbox becomes a recorded
    `AppMsg::HubsResult`, so a refusal reaches no recorded byte. The next Hubs step settles it first
    (`settle_refused`), as the failure it is, one step after the launch that failed. A `Reset`
    rotates the adapter and retires the list with it.
  - **Replay identity.** The landing compare builds its key from the landing's bound client
    (`resource.instance_gen()`), never from the logical `instance` the landing records: a replayed
    landing carries the recording's id there, and only the resource it was bound to is the registry's.

  The two test seams the old inline spawn had (a refused spawn and a late worker) are the
  runner's, `with_refused_hubs_launches_for_test` and `with_late_hubs_launches_for_test`, `#[cfg(test)]`
  in `app/adapters/stores.rs` beside the boundary they stand for. A step test reads the jobs
  (`HubsStore::take_jobs`) or answers them (`refuse_jobs_for_test`).
- **The Search lane.** The Search screen's fetches (`data/src/search.rs`) are the eighth store on the
  lane, and its recents writes (`data/src/search/recents.rs`) ride the same outbox. The pump claims
  a due source's mailbox and emits `StoreJob::Search(Job<SearchReq>, SearchPort)` instead of
  spawning: the server, the `ClientKey` the step decided on (from the `ClientKeys`
  `Bridge::search_pump` refilled; a server that holds no key lands as a failed fetch in the step, as
  the missing client always did), the query, the favourite snapshot and the work (the source's first
  answer, or one window of a lane read on a copy of the source's lane), addressed by the query's
  generation. `finish_search_job` is the runner's call. The body keeps the `catch_unwind` guard the
  old worker had and posts its answer outside it, so a fetch that panics lands as a failed fetch
  and the source backs off instead of staying out. It reuses two earlier shapes, the
  `refuse_for`/`take_refused` backoff (as subtitle search) and the runner-resolved `&'static Client`
  (as every lane), and differs in two ways: several jobs share one address, and a second kind has a
  unit answer.
  - **Several jobs, one per source, and the slot is in the port.** `Addr.req` is the query's
    generation, so `SearchPort` carries the slot it answers into and the adapter it was made from:
    `done` stamps the landing with the generation, `refused` names the slot. A `Reset` rotates the
    adapter, and a job emitted before it answers into the retired one.
  - **A refusal backs the source off (reused).** `SearchPort::refused` is `Fetch::refuse_for(generation)`: it
    gives the claim back and remembers the refusal only while the claim is still that generation's.
    The next pump reads it (`take_refused`) before it decides to ask, so a refused source waits
    `RETRY_FRAMES` instead of being emitted again every frame, which is what the refused inline
    spawn did. A refusal for a request a newer query has superseded leaves the newer claim alone.
  - **The client is resolved once, by the runner (reused).** `StoreRunner::launch_search` resolves the slot's
    client against the step's key and hands the worker that `&'static Client`; the worker body reads
    no registry, so both `client_for` reads are gone from the ledger. A slot a roster change
    re-points while a fetch is out is caught by the roster-generation supersede, which drops its
    answer at the next pump. The kind is `Direct` and never taped.
  - **A second kind, with a unit answer.** A recents edit queues the new history in memory
    (`PENDING`, as before) and the step that made it emits `StoreJob::SaveRecents(Job<SaveRecentsReq>,
    SaveRecentsPort)`, addressed by a sequence of its own. The runner starts `finish_save_recents_job`,
    which drains the queue into the session file, and the write answers with nothing. A refused
    write is remembered on the store's adapter and the next recents edit, even one that changes
    nothing, asks for a drain once more (as the queue-full retry that edit already had). The runner
    cannot be made to refuse a launch on a host (`spawn_small` has no failure knob), so its refused
    branch is pinned at the port (`a_refused_save_is_asked_again_at_the_next_edit_once`).
  What the Search step still reads: `server_ids()` (the roster), `server_roster_gen()`,
  `session::current_gen()` for a scoped query, and the recents cache's session reads, which stay in
  `recents.rs` with the statics `STORE` and `PENDING`. A test drives it with `Owner::run_jobs`
  (`search_test_support.rs`), a fixture that plays the runner against a served fake, in place of the
  inline fake-IO fork the old spawn carried.
