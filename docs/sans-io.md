# Sans-IO: logic never performs IO

Status: the rule is written down and its four ledgers are gated (`ci/check-deps.sh`, inside
`make check`). Nothing here changes product behaviour: each ledger freezes the sites that exist
today, so that later changes can only shrink them.

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
| `globals` | a production read of a process-global registry in a machine: `client_for`, `current_server`, `current_snapshot`, `keypin::revision`, `keypin::blocked_for`, `session::peek`, `grant::offers`, and `player::TX` in the player's lifecycle | `<path>::<enclosing fn>::<spelling>` |
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
- `player::TX` is read as logic only in `lifecycle.rs`, as `crate::player::TX`, `player::TX`,
  `super::TX` or a bare `TX`; the HUD's reads are not a machine's.
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

- **The shape.** `plx_machine::job` holds `Job<R>` (an `Addr` and a request that carries everything
  the worker needs), `Port<T>` (where the one answer goes: exactly one `done` or `refused` per job)
  and `assert_fx!` (a step test reads the jobs it emitted instead of waiting on a thread). A store
  keeps its own job enum, request types and port: `plx_data::stores::StoreJob`,
  `MetadataStore::take_jobs`, and `plx_data::metadata::{DetailReq, DetailPort}`. The app's `StoreRunner`
  (`app/adapters/stores.rs`) executes a job and owns what `data/` must not: the tape's admission
  record, the thread and the `client_for` read. It is `Live` or `Held` (a test parks the job and
  runs it on the test thread), chosen when the `Bridge` is constructed.
- **Two drains, one rule.** A stepped machine (the session owner) emits its work through `Effects`,
  and the owner's drain starts it. A store the Bridge runs emits through its own `take_jobs`, and
  `Stores::take_jobs` aggregates every store's outbox: `Bridge::metadata_run` drains it right after
  the store step, so a job is started in the same Bridge call that emitted it. A caller that holds
  only the store (`describe_movie` gets a `&mut MetadataStore`) drains with
  `Bridge::start_store_jobs` the moment its call returns. A new store joins by implementing its
  own outbox and adding it to `Stores::take_jobs` and `Stores::has_jobs`, in that one place.
- **Whoever steps a store drains it.** `MetadataStore`'s `Machine::step` runs the command and does
  not drain: a `RequestDetail` stepped through it leaves its job on the outbox and nothing starts
  it. The same holds for the `pub` `Stores::metadata_run`. The owner drains in the same call, as
  `Bridge::metadata_run` does; no type enforces it, the counter below catches the lapse.
- **Results do not return as FIFO events.** A store's result goes back through the landing and its
  gate (`landgate`, the `adapter_rank` of the landing key), where the pump takes it by value. This
  is a deliberate exception to "every result returns as one addressed event in the same drain":
  it is kept so that the replay anchors hold, because the landing is where they already record a
  result.
- **Why the job is not an `AppFx`.** A new `app_effect` arm that starts IO without pushing
  `Fx::Deliver(` is a new `fxlanes` site; `bootstrap::effects::app` fails closed on a variant it
  cannot encode in controlled Home; and every `Fx::App` is an `"e":"App"` record, so the replay
  fixtures would change. A later store that routes its jobs through `AppFx` brings all three back.
- **A bypassed drain is a red test, and in production a logged one.** `metadata_pump_detail` keeps
  a backstop under the senders: a job it finds on an outbox is started there, so the request still
  settles, and is counted in the Bridge's `late_store_jobs` and logged as one `stores: WARN` line.
  `Bridge::assert_no_store_jobs_left_for_test` (`#[cfg(test)]`) fails a frame that ends with a job
  still on an outbox or with that count above zero, so a sender that bypassed `Bridge::metadata_run`
  is red in every rig that makes the check, the one that runs `update()`'s pump included. The rigs
  make it at the frame's end.
- **The port holds the adapter `Arc`.** `Reset` replaces the adapter and the store keeps no handle to
  the retired one, so an epoch id could only reach it through a registry of retired adapters. A job
  emitted before a `Reset` answers into the adapter it was made for, as the request did before.
