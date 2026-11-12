# Sans-IO: logic never performs IO

Status: the rule is written down and its four ledgers are gated (`ci/check-deps.sh`, inside
`make check`). Nothing here changes product behaviour: each ledger freezes the sites that exist
today, so that later changes can only shrink them. The player's foreground lifecycle is the first
player machine: `player/lifecycle.rs` is the machine alone (private state, one writer, `step`),
`player/foreground.rs` its drivers and `player/transport.rs` the transport-pause contract; its shape
is gated by `ci/machines.ini` (see "Machine shape" below).

## Combined verdict source checkpoint (historical; not acceptance)

The integration tree retains full ancestry from base `f8f6dbb7`, Session `33f18e52`,
Watchlist W2 `abd26d21`, enforcement `dd1664f7`, and Screens `10bd53db`. Actual
ledger entries are **4 workers / 19 globals / 61 forks / 11 fxlanes** after Person and
remaining data captured-input retirement; the pins
match those entries. Watchlist workers/globals and Screens globals are zero.
No remaining allowance was removed merely to reach a target count.

Follow-up `0a98b17e` retains Watchlist captures as an immutable `Arc` handle
shared by both host capture paths. The cache compares the full `ClientKeys`
value and primitive UTC day before cloning keys, collecting reach or formatting
a date. It also removes the per-pump key clone and defers auth copying until
write/stream work is owed, preserving write-before-stream order. Two added
Rust regression sources cover a populated stable capture
(`watchlist_snapshot_cache_retains_populated_capture_and_buffers_on_stable_passes`)
and same-generation key/fact/eligibility changes, day changes and old in-flight
snapshot immutability. At this historical checkpoint they had not been compiled
or run; the later exact-head milestone below supersedes that execution status. The scoped follow-up
touches `stores/watchlist{,_ports,_stream_job}.rs`, app store adapter and Bridge.

A terminal-edge follow-up records list read-start/progress in the addressed
stream kind: held rows, awaiting a first page, or publishing a first load. A
panic before page zero is not a later-page failure merely because old rows are
held. Held complete/partial rows retain their own completeness, while a first
load that published page zero remains truncated. Two additional Rust regression
sources cover those cases before/after page zero; execution was pending at that
historical checkpoint (see the later milestone below).
The write-ledger stamps are not used as list-read identities.

Session now binds the actual `SessionMachine.state` (`attempt::Attempt`) to
`transition` and `from_init` with `state_reads = immutable`. The structural
read-only proof, not a getter-name exception, validates shared reads. The
registry contains ten machines, four with strict writer contracts. Exact wall
ports include the 17-line Catalog fetch/calendar worker adapter and Watchlist
calendar capture in the app store adapter; neither exemption contains a
machine. `ports.ini` is a new gate relative to the base, not a grown old list.

The only textual merge conflicts were in `app/bridge.rs`: Watchlist pump keeps
W2 key/profile/snapshot capture before the machine-only `StepScope`, then drains
Watchlist jobs after it; the end-of-file resolve-drain test and Screens input
publication/cache test modules are both retained. Stores merged without a
textual conflict, preserving all twenty scope sites and captured arguments.
The reader-negative regression and both ledger pin reductions coexist in the
Python harness. Source-gate follow-up fixes two lexical false positives: visible
`#[cfg(test)]` modules are still test-only, and captured field labels are not
registry function values. Added regressions retain production-thread rejection,
RHS getter calls and bare callback-reference rejection; no allowance was added.
All replay fixtures and V5/V6 JSON goldens are unchanged from the base.

Source verification uses the committed tree: `ci/check-machine-shape.py`,
`ci/check-ports.py`, `ci/check-ledger-ratchet.py`, the seven CI Python suites
for shape/writers/read-only proof/scopes/ports/ratchet/test-module discovery,
and the harness ledger and reader regressions. These are source checks only.
Rust compilation, runtime/replay tests, Clippy and independent review remain
acceptance prerequisites; no Rust stage/build was performed for this checkpoint.

After the shared Rust lease, exercise owning-crate filters `step` (base),
`assert_fx` (machine), `step_scope_tests` (ui),
`lifecycle_obligation_tests` (media), `auth::owner` (session, including Attempt
and codecs), `watchlist` (data and app, including `watchlist_snapshot_cache`), and `screen_input`
(app), plus the full
plex and screens suites. Run Clippy with `clippy::wildcard_enum_match_arm`
enforced for ForegroundLifecycle, PictureGate, PlayerReadout and SessionMachine;
filtered tests do not replace the parent full gates or independent review.

## Verified integration milestone (before steady-state cost fixes)

On exact commit `4a4f0b258e05df9f0d94fbadab0e51100fafac7f`, tree
`e156bab180042b3e372a28b34089c86c674fe0b4`, the coordinator's run 350
compiled and ran the full Data suite (**1621 passed**), including all **145
Watchlist** tests and the capture/terminal-edge regressions above. The focused
Session (**333**), Screens (**2142**), app Session (**149**) and app screen-input
(**10**) tests also passed, as did the no-default-features and lint gates.
These are results for that exact pre-fix commit, not acceptance of a later head.

The same full `make check` reported three Python failures: the unclassified
test-only `captured_store_fixture` module, the build-bench `app_rebuilt`
fixture assertion, and the TV-lock self-test timing. The fixture classification
is corrected in this follow-up without adding layer edges or allowances. The
other two are separate tooling/environment investigations. No full-check pass
or new-head acceptance is claimed here; publication still requires its actual
gates, independent review and benchmark evidence.

## Notify integration checkpoint (historical; not acceptance)

Notify's typed Direct stream lane and app per-connection cursor are written; the
frozen API and integration evidence are in `.empryo/reviews/verdict-notify-core.md`
and `.empryo/reviews/verdict-notify-integration.md`. StoreId Notify is appended at
12; old ordinals, Home/tape records and NOTIFY_FILLS `[Browse, Hubs]` are unchanged.
Bridge captures fresh stream eligibility before the one-frame cursor, settles each
initial admission before that connection's ordinary drain, then maps once and
drains fan-out jobs. Accepted-held release failures use ordinary terminal timing.
Production opens off-frame; test boot refuses unless explicitly scripted.

The three Notify allow-area files have been retired. The coordinator reports actual
ledger totals of 5 workers / 81 globals / 62 forks / 11 fxlanes, down from the
S4 pre-Notify checkpoint of 6/89/65/11; the pins matched that checkpoint. The source-only stale-key
check passed in orun-27; runtime tests, capture comparisons and full gates remain
pending. Per-connection source ordering is preserved; asynchronous worker wall-time
is not pinned, and disabled-state repair is explicitly intentional.

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
| `globals` | a production read of a process-global registry in a machine: `client_for`, `current_server`, `current_snapshot`, `keypin::revision`, `keypin::blocked_for`, `session::peek`, `grant::offers`, the session cache's `session::peek_settled` and `session::current_gen`, the server registry's roster readers (`server_ids`, `server_roster_gen`, `server_facts_gen`, `server_probe_result`), the controlled-run switch `tape::active`, the subtitle registries' colour-pick readers (`sidecar::takes_colour`, `sidecar::selected`, `ass_source::takes_colour`, `ass_source::selected`: no site; the Subtitles menu reads the fact from the session's readout), the player's pause readers (`transport::viewer_paused`, `preview::paused`: each takes the session but also reads process-global state, the claim hold's slot and the preview machine's phase; `transport::paused` reads only the session's readout and is no longer a spelling), the device's capability answer (`devcaps::caps`, and the zero-argument `is_dp_audio` / `is_dp_audio_track` that used to read it), and `player::TX` in the player's lifecycle (at zero) | `<path>::<enclosing fn>::<spelling>` |
| `forks` | a production test/production fork whose predicate reads `test` or `feature = "test-support"`: a `#[cfg(..)]` attribute that a production build can satisfy (`not(test)`, `not(any(test, feature = "test-support"))`, `all(target_os = .., not(test))`, ...), or an inline `cfg!(..)` of any value | `<path>::<item name>`, or `<path>::<enclosing fn>` for a statement or an inline `cfg!`, or `<path>::<struct>` for a field |
| `fxlanes` | an `AppFx::<Variant>` arm of `Bridge::app_effect` that does not push `Fx::Deliver(`: one site for each variant on its head, and one for an arm that names no variant (`other => ..`, `_ => ..`) | `<path>::AppFx::<Variant>`, or `<path>::AppFx::_` |

`blocking` (the frame thread's `allow_blocking(` exceptions) is the older ledger these follow. It now
runs on the same scanner, so it lists the same sites as before but reads them more accurately: it skips
any item under a `#[cfg(..)]` that production can never satisfy (not only a `mod`; also a `mod` with
other attributes in front, and `feature = "test-support"` and `all(test, ..)` items), ignores string
literals and block comments, and sees a call after a `"http://"` string that the old `//` stripping
dropped.

**Scope.** `workers` and `globals` read the machines: everything under `rust-modules/data/src`,
`session/src`, `screens/src`, `ui/src`, `appkit/src` and `machine/src`, plus
`plex/src/plex/session.rs` (the session owner's state), the player's `lifecycle.rs`, `recovery.rs`
(the failed-open ladder machine), `release.rs` (the native object's release machine) and the route planners `route/plan.rs` and `route/plan_memory.rs`. The IO layers (the rest of `plex/`, `media/`,
`net/`, `platform/`) are where those calls belong, so they are not scanned. `forks` reads the whole
of every crate that holds a machine (`FORK_SCOPE` in `ci/check-deps.sh` names them: `data/`, `plex/`, `media/`,
`play-feed/`, `session/`, `screens/`, `ui/`, `appkit/` and `machine/` among them), because a fork anywhere in them is a test that does not run the
product's code. It read only the first four until the scope was widened to the rest; the
widening recorded the 17 forks that were already there (three in `screens/`: `legal.rs`
`build_sha`, `library/lookahead.rs` `rows` and `login.rs` `harness_driven`; thirteen in `ui/`; one in
`machine/`) as a coverage expansion, not new code. `fxlanes` reads the one fn, `Bridge::app_effect` in `rust-modules/src/app/bridge.rs`.

**Decisions the table hides.**

- A bare `spawn(` / `submit(` / `peek(` / `revision(` / `blocked_for(` / `offers(` / `current_gen(` /
  `active(` / `selected(` / `takes_colour(` is a site only in a file that imports it from `task`,
  `storage_worker`, `session`, `keypin`, `grant`, `tape`, `sidecar` or `ass_source` (singly, as the last or any other item of a braced group, or through a glob;
  `Builder::spawn`, a local fn, `it.peek()` and another module's `active()` are not workers or registry
  reads); `peek` and `current_gen(` are also sites inside `plex/src/plex/session.rs`, the module that
  defines them. `current_gen` and `active` are generic names, so their bare form counts only as a call; a
  `use ... as` rename of `tape`, `tape::active`, `sidecar`, `ass_source` or any of the new names fails outright. A local launcher
  (`launch_browse_discovery`) is no site of its own: the real call inside it is.
- The roster, session-cache and tape readers (`server_ids`, `server_roster_gen`, `server_facts_gen`,
  `server_probe_result`, `session::peek_settled`, `session::current_gen`, `tape::active`) read process-wide
  state behind a free function, as `client_for` does: the first four are the `plex/src/plex/mod.rs`
  re-exports of `servers::ids`, `roster_gen`, `facts_gen` and `probe_result` over atomics and the slot
  table, `peek_settled` locks the session cache, and `current_gen` is `current_snapshot().generation`.
  `tape::active` is the one that is thread-local rather than process-wide (the tape the controlled
  bootstrap arms), and it is a site because it is ambient state a step reads instead of being handed, the
  same hazard as a fork. The four `server_*` names and `peek_settled` are as distinctive as `client_for`
  and are sites bare anywhere (`x.server_ids()` is not); the spelling the rule keys is the one the call
  uses (`session::peek_settled` when qualified, `peek_settled` when bare). Widening the rule to them
  was a coverage expansion: it recorded 57 reads that were already there (`globals` 57 to 114) and
  deleted none. Not in the list on purpose: `i18n::current` (a display read) and the
  `plx_media::route::*()` settings getters, which the architecture board kept out of this gate; and the
  other registry accessors (`server_facts`, `server_count`, `describe_server`, ...), which the rule does
  not name yet.
- `task::beside` is not a `workers` site: its scoped sibling is joined before the IO body that asked for
  it returns, so no worker outlives the call and there is no ledger line to shrink; scoped concurrency
  inside an IO body a runner already runs off the frame is allowed, a machine step never calls it (it
  would block the frame on the network, and `beside` asserts it is not on the frame thread).
- A getter named without a call (`.and_then(client_for)`) reads the registry as surely as a call
  does, so it is a site. Only a rename (`use ...::client_for as x`) fails outright, because the
  calls through `x` are invisible to the scan.
- The command worker's `.submit(` is not covered: it is `#[cfg(any(test, feature = "test-support"))]`
  in `base/src/storage_worker.rs`, so no production line can call it. `.submit_ticket(` is covered
  although no machine calls it today: a machine that did would be starting a worker.
- The transport Pause is the session's readout (`PlayerReadout::paused`), stepped at every writer, and the Feed keeps its own
  gate copy (`FEED_TX.paused`, in `plx_play_feed::shared`) written by commands only. A machine reaches it through three readers,
  `transport::paused`, `transport::viewer_paused` and `preview::paused`, each taking the session. `transport::paused` reads only
  the readout and is not a spelling any more. The other two stay sites, qualified or bare after an import, because each
  still reads process-global state behind the session argument: `viewer_paused` the claim hold's slot and serial
  (`claim_hold::owns_pause`), `preview::paused` the preview machine's phase (`preview::occupies`). A `use ... as`
  rename of any of the three still fails outright. The spellings of the owners themselves (`crate::player::TX`,
  `player::TX`, `super::TX`, a bare `TX`, and the Feed owners `shared::FEED_TX`, `shared::FEED_SHARED`
  and a bare `FEED_TX` / `FEED_SHARED`) stay pinned to `lifecycle.rs`, the machine, which now reads
  none: that pin is a zero-guard (the machine takes the pause as a value on its event). The HUD's
  direct `FEED_TX.paused` reads are not a machine's, and a writer (`set_paused`, `set_transport_paused`) is
  an IO call, not a registry read. Moving the contract out of `lifecycle.rs` deleted its three `TX`
  lines from `ci/allow/globals/media.txt`; the same change widened the rule to the readers above and
  seeded the three `preview::paused` reads the screens already had (`screens/src/detail/mod.rs`) as a
  coverage expansion, so this change's net effect on the pinned total is zero (a relocation lowers no
  pin on its own).
- The device's capability answer (`devcaps::caps()`) is the same kind of read: a process-wide value,
  now write-once (the boot snapshot; a refusal the install learns never moves it). The route's planner
  takes the attempt's `&Caps` as an argument (`PlaybackSession::attempt_caps`, derived once by the
  app's play start, `devcaps::start_caps` over the stored refusals), so `devcaps::caps`
  and the zero-argument `is_dp_audio(` / `is_dp_audio_track(` (the `_with` forms take the value and
  are not sites) are spellings of this rule, qualified or bare after an import; a `use ... as`
  rename of any of them fails outright. Widening the rule to them is a coverage expansion, not a
  shrink: no machine reads them, so it seeded no ledger line and moved no pin. The boot-snapshot
  readers (the engine's sink guards, the diagnostics and lab read-outs) are in crates and files this
  rule does not scan; the Forced preview reads the start value (`devcaps::start_caps`) and the track
  menu's "converted by your server" mark reads the attempt (`route::audio_converted_by_server(ps, ..)`).
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

`ci/machines.ini` is the sole machine registry. Every entry binds a real event enum, a real
mutable effect-vector channel (or exact `effect = none` for a pure view), and one private inherent
`transition` reducer. The checker binds the complete argument list: exactly `&mut self`, one
registered event enum (possibly borrowed), one concrete mutable effect vector (absent for
`effect = none`), and only the explicitly declared closed `aux_facts` slot where required.
No additional state/model, context bag, reference, generic or callback argument is admitted,
regardless of its parameter name. Duplicate, missing and unknown bindings fail closed.
Concrete effect-vector aliases are resolved; an owned-state alias is not an event or an
auxiliary-fact certificate, and an opaque setter cannot become a writer through a renamed
frame/root wrapper.

The actual canonical headers require three auxiliary inputs, not just Session time:
`tick_ms: u32` for Session, `now_ms: u64` for URI, and backend
`capabilities: Option<Capabilities>`. Clock facts require unshadowed primitive identities
(or canonical primitive paths), by value, with the exact declared name and width. The backend
fact is pinned to the actual playback-contract declaration with exactly nine named boolean
fields; neither an arbitrary immutable model nor a familiar type name is sufficient. The
closed semantic slots cannot be extended by adding a type name to the registry. Reducer logic
may legitimately change phase based on these facts; the checker does not prove monotonic
clock history or semantic event admission. An event wrapping raw state and ignoring admission
is not made sound by its signature. Rust type checking and behavior review remain required.

Every owned struct field is private and write-guarded by default: pending work, counters, outboxes,
caches and nested owners are not exemptions. `state_field` selects a variant view, never the write
coverage boundary. Fresh constructors initialize all fields and cannot accept an existing machine.
All subsequent physical writes belong inside `transition` itself, not a helper merely called by it.
Public adapters may dispatch typed owner events; consuming drains must do the same. Whole-machine
aliases/replacement, live mutable projections, nested mutating functions/closures and unknown
machine-capturing calls/macros fail closed. Detached owned values may have their own pure helpers.

Shared readers require closed structural proof for the data they expose. A user `Copy` bound is
not an inertness certificate: generic identities need a private closed seal covering only proved
inert actual types. Numeric/discriminant reads may use bounded structural proof without exposing
opaque payloads. Foreign types remain opaque without reproducible source-backed dependency proof;
a familiar dependency name is not a certificate.

`ci/readonly_generated.py` and `ci/proofs/platform-generated-enums.json` separately certify
the actual platform language include: pinned package/module/include, complete production
build scripts, generator/templates, manifests/configuration and locked graph. The reviewed
pipeline emits only unit enum variants in every configuration; strict catalogue/schema and
ASCII variant validation permit new languages without a count/list pin. The same evidence
pins the machine ID macro source and proves its concrete `u32` fields, not a `Copy` promise.
Reviewed machine snapshots differ only in `NavOp::SelectTabWithChild`, i18n snapshots only in clock formatting/helpers; either
source changing requires review. No generator or Cargo/cache code executes in native CI.
The certificate assumes ordinary Cargo/Rust/std/serde execution and no out-of-band output,
compiler or external configuration overrides. It certifies values only, never locale contexts,
locks, callbacks or generated method authority. Unknown layouts/includes remain opaque.

Every source also registers a matching `[module file]` enum policy and unconditionally denies (or
forbids) `clippy::wildcard_enum_match_arm`. Local `allow`, `expect` and `warn` cannot weaken it.
Production child modules and literal includes are traversed exhaustively. Conditional
`cfg_attr(path)`, opaque/generated includes and unresolved production modules are rejected rather
than guessed; proven test-only code is excluded. Numeric and open wire-string wildcard matches
remain legal; the compiler/Clippy classify enum exhaustiveness, not a textual underscore ban.

The native tests derive the cohort and every owned field from this registry, retain the original
writer/alias/macro negatives, and add typed-event/effect, full-field and production-closure matrices.
Fixture and mutation passes are not full-cohort Rust, Clippy, behavior or performance acceptance.
Bool/Option field counts are report-only.

## Layout

One file per rule and area, `ci/allow/<rule>/<area>.txt`. The area is the crate (`app` for
`rust-modules/src`), except in `data/`, where it is `data-<module>` for the top-level module under
`data/src`, and `stores/<stem>.rs` joins the module of its own name (`stores/watchlist.rs` and
`watchlist.rs` are one area, `data-watchlist`). One file per area means two migrations that touch
different areas never edit the same `# count:` header. The directories do not collide with the flat
`ci/allow/*.txt` rule names, which `allowed()` indexes.

Each file starts with `# count: N` (it must equal the entry count), then a comment header saying what
the rule is, then one key per line. A line filed in the wrong area fails the gate and names the right one.
`tests/test_harness.py` pins each rule's total (`PINNED_LEDGER_TOTALS`), and `ci/check-ledger-ratchet.py`
fails a PR whose entry counts or pins rise above its base's, so a ledger, its count and its pin cannot
grow together.

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
or fork in a machine is moved behind an effect instead, and CI fails a PR whose ledger counts rise
above its base's (`ci/check-ledger-ratchet.py`). A ledger file that reaches zero entries is
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
re-point. It never mixes clients within one page: the opening season's episodes
(`fetch_episodes`) are read through the same client (Metadata M2, below).
The Collection page's request is the second: its worker start and its three `client_for` reads are
gone from `data/src/collection.rs`. The Person page's requests are the third: its two worker starts
and its one `client_for` read are gone from `data/src/person.rs`, but unlike the two before it the
Person step now takes complete captured inputs (the bullet headed "Person's complete captured inputs"). Person has up to six requests out at once, so a pump emits several jobs, one per fetch slot,
in ascending slot order. The catalog page's read is the fourth: its one worker start is gone from
`data/src/stores/catalog.rs`. Unlike the other three it names no server and no client (it is a
plex.tv read as the captured identity), so it takes no `ClientKeys` and no tape admission, and the
Catalog step reads no registry once it takes the seated profile from the Bridge (the bullet headed
"The Watchlist and catalog session input"). Subtitle search and the view-state read-back are the fifth and sixth (the bullet
headed "Subtitle search and the view-state read-back"). Home's Hubs read is the seventh (the bullet
headed "The Hubs lane"): the worker start and the `client_for` read are gone from `data/src/pms.rs`
and its three `data-pms` ledger files are deleted. The Search screen's per-source fetches and its
recents writes are the eighth (the bullet headed "The Search lane"): both worker starts and both
`client_for` reads are gone from `data/src/search.rs` and `data/src/search/recents.rs`, and its two
`data-search` ledger files are deleted. A library's section hubs, the Browse store's list
and row reads, are the ninth (the bullet headed "The section hubs lane"): their two worker starts
and three registry reads are gone from `data/src/browse/section_hubs.rs`. The same store's Library
listing reads, the filter menus' value lists and the grid's pages, are on the lane too (the bullet
headed "The listing lanes"): their two worker starts and two registry reads are gone from
`data/src/browse/mod.rs`. The Library's source discovery (the section list, the server's name and the
item counts) is the twelfth (the bullet headed "The discovery lane"): its worker start and its one
registry read are gone from `data/src/browse/mod.rs`, and its `data-browse` workers ledger file is
deleted. The Home pins take the session from the Bridge (the bullet headed "The pins' session input"), and so do the Watchlist and catalog stores, for the seated profile and its plex.tv identity (the bullet headed "The Watchlist and catalog session input"), which deletes three `data-watchlist` globals lines. S4 replaces Browse's roster/gate/current reads with explicit `BrowseRosterInput` and the existing host-filled `ClientKeys`; the obsolete 14-entry Browse globals file was retired. The S4 checkpoint was **6 workers / 89 globals / 65 forks / 11 fxlanes** (recount before deletion: 6/103/65/11); post-Notify retirement the coordinator reports **5/81/62/11**. Runtime and integrated verification remain pending; legacy fixture/caller migration remains tracked in `.empryo/reviews/verdict-s4-integration.md`. Other stores' and Session's global sites remain unchanged. The Metadata store's
season switch and episode-page window are the tenth (the bullet headed "The Metadata reads"): their
two worker starts and the `client_for` reads of `fetch_episodes` and `fetch_episode_page` are gone
from `data/src/metadata.rs`; the Related row's tail window read joins them in the same bullet, and its
worker start and the `client_for` read inside that worker are gone too. The household's watch-state writes are the eleventh (the bullet headed
"The view-state write lane"): the worker start and all five `client_for` reads are gone from
`data/src/viewstate.rs`, and its two `data-viewstate` ledger files are deleted.

- **The shape.** `plx_machine::job` holds `Job<R>` (an `Addr` and a request that carries everything
  the worker needs), `Port<T>` (where the one answer goes: exactly one `done` or `refused` per job)
  and `assert_fx!` (a step test reads the jobs it emitted instead of waiting on a thread). A store
  keeps its own job enum, request types and port: `plx_data::stores::StoreJob`,
  `MetadataStore::take_jobs`, `plx_data::metadata::{DetailReq, DetailPort}`,
  `plx_data::collection::{CollectionReq, CollectionPort}`,
  `plx_data::person::{PersonReq, PersonPort}` and
  `plx_data::stores::catalog::{CatalogReq, CatalogPort}`,
  `plx_data::pms::{HubRequest, HubsPort}`,
  `plx_data::search::{SearchReq, SearchPort}`,
  `plx_data::search::recents::{SaveRecentsReq, SaveRecentsPort}`,
  `plx_data::browse::section_hubs::{BrowseHubsReq, BrowseHubsPort}`,
  `plx_data::browse::listing_jobs::{BrowseDirectoryReq, BrowseDirectoryPort}`,
  `plx_data::browse::listing_jobs::{BrowsePageReq, BrowsePagePort}`,
  `plx_data::browse::discovery_job::{BrowseDiscoveryReq, BrowseDiscoveryPort}` (`BrowseStore::take_jobs`) and
  `plx_data::metadata::{MetaSeasonReq, MetaSeasonPort, MetaEpisodesReq, MetaEpisodesPort}` and
  `plx_data::metadata::{MetaRelatedReq, MetaRelatedPort}`. The app's `StoreRunner`
  (`app/adapters/stores.rs`) executes a job and owns what `data/` must not: the tape's admission
  record, the thread and the `client_for` read. It is `Live` or `Held` (a test parks the job and
  runs it on the test thread), chosen when the `Bridge` is constructed; a newer Detail request
  retires only the parked Detail jobs, never another store's. `Held` parks per kind
  (`LiveKind`, test only; a kind and not a store, because Browse emits several kinds: `BrowseHubs`,
  `BrowseDirectory`, `BrowsePage`, `BrowseDiscovery`): `StoreRunner::held()`
  leaves the Hubs read, the Metadata season switch and the Library grid's page read live, as
  production runs them (a rig that waits for a Home, a season or a grid landing needs a real worker;
  `menu_play_season_tests` polls for the season landing and never runs held jobs), leaves the
  Library's source discovery live for the reason given below, and
  `StoreRunner::held_all()` parks them too, for a test that chooses when the read settles
  (`Bridge::run_held_store_jobs_of_for_test`).
  `StoreRunner::park_page_reads` (`Bridge::park_page_reads_for_test`) parks only the page read of
  a held runner, for a rig that grades which reads an entry started. Source discovery is parked by
  `held_all()` only: it is live under `held()` and under `park_page_reads()`, so a rig that runs
  every parked job on the test thread (the `hostsim` helpers of `app/run.rs`) never runs a
  discovery's blocking reads there, where the worker blocks harmlessly off-thread. No rig waits for
  a discovery landing from a real worker (every one fabricates it), and parking it under `held()`
  was tried: the whole `hostsim` and `app` suites passed, 27 discovery jobs running on the test
  thread through `run_held`, so no suite forced the choice. Every other kind parks under
  all three, the section hubs' read, the filter menus' directory read, the episode-page jobs and
  the Related window job included: no rig of the suite waits for any of those landings from a real worker, and a parked
  read is one the test starts when it chooses (the one `hostsim` rig that needs the section hubs'
  read to reach the server, `the_recommended_page_fetches_the_sections_hubs`, runs it from the
  parked set with `Bridge::run_held_store_jobs_of_for_test(StoreId::Browse)`). The page read is live
  because four `library_route_tests` rows wait for a failed-page landing and
  `session_dispatch_tests::production_bridges_do_not_share_browse_state_or_landings` waits for a
  loopback request; each failed with the page parked, and every suite is green with the directory
  read parked.
- **A registry read is an input.** A step that decides from "which client does this server slot
  hold" is handed `plx_plex::plex::ClientKeys` (one `ClientKey { instance, token, grant }` per server) as
  an argument. The Bridge owns one buffer and refills it in place
  (`adapters::stores::refill_client_keys`, the only loop over `client_for` that feeds a step)
  immediately before the step, so a pump that runs every frame allocates nothing for it. The job
  carries the key its step decided on, and the runner starts it only if `client_for` still answers
  that exact client inside the launch the tape's admission record holds: a replay and a recording
  then agree on the one boolean they hold, and a refused job hands its claim back.
- **The Detail job carries no key.** Its step reads no registry, so `DetailReq` names a server and
  no `ClientKey`: the runner reads `client_for(sid)` ONCE in `start_detail`, records that client's
  instance in the admission and runs the whole chain through that same client, the opening
  season's episodes included. That is a presence check, not the key compare of the jobs above. A server the registry
  holds no client for at the admission still launches, and its worker answers the failure without
  asking anything, as the first read of the old chain did.
- **Two drains, one rule.** A stepped machine (the session owner) emits its work through `Effects`,
  and the owner's drain starts it. A store the Bridge runs emits through its own `take_jobs`.
  `Stores::take_jobs_of(StoreId)` takes ONE store's outbox and `Stores::take_jobs` aggregates every
  store's, and each Bridge entry point drains the outboxes its step can fill, in the same call as
  the step. A step that touches only its own store drains it with `start_store_jobs_of`; a `Stores`
  call that fans out to other stores (`viewstate_run`, `viewstate_pump`, `reconcile_pump`,
  `notify_pump`) drains every outbox named in its `*_FILLS` list (`Stores::VIEWSTATE_FILLS`,
  `Stores::RECONCILE_FILLS`, `Stores::NOTIFY_FILLS`) with `Bridge::start_store_jobs_in`.
  `Bridge::metadata_run` steps then drains Metadata, and so does `Bridge::metadata_pump_season`
  (a guard: no landing pump emits a job today, so it finds nothing); `Bridge::person_pump` and
  `Bridge::collection_pump` refill the keys, step, then run
  `Bridge::start_store_jobs_of(StoreId::Person)` and `(StoreId::Collection)`;
  `Bridge::catalog_open` and `Bridge::catalog_pump` step the Catalog store and run
  `(StoreId::Catalog)` (no keys to refill; the pump drains because its retry emits and the profile-switch
  re-open it takes (`CatalogStore::take_reopen`) goes through `Bridge::catalog_open`). `Bridge::search_run` drains `(StoreId::Search)` after every command (a recents
  edit emits its write there) and `Bridge::search_pump` refills the keys, steps and drains it.
  Browse's reads (the section hubs, the filter menus' directory reads and the grid's pages) are
  drained at five entries: `Bridge::browse_run` (which
  the `Store(Browse(..))` arm calls), the `StoreWork::Browse` arm and `Bridge::reconcile_pump`'s
  membership re-query each refill the keys, step and run `(StoreId::Browse)`, and `viewstate_pump`
  and `notify_pump` reach Browse through their `*_FILLS` lists (ViewState's terminal
  `HubsInvalidateAll`, the notification map's `HubsInvalidateServer`); `viewstate_run` and
  `viewstate_pump` drain `VIEWSTATE_FILLS` too, whose ViewState entry is where a household write
  leaves (`StoreJob::ViewWrite`), and `reconcile_pump` drains `RECONCILE_FILLS`
  (`[ViewState, ViewReconcile]`) because an end-of-stream fan-out it queues starts the next write.
  Draining inside each pump, not at the frame's end, keeps every store's
  admissions in the tape where they were: the frame pumps Person, then Collection, then reconciles
  the Detail page. A caller that holds only the store (`describe_movie` gets a `&mut MetadataStore`, and the
  menu-play wait's season press in `input.rs` runs `LoadSeason` on the bare store)
  drains with `Bridge::start_store_jobs` the moment its call returns. A new store joins by implementing its
  own outbox and adding it to `Stores::take_jobs_of` and `Stores::has_jobs_of` AND to the `*_FILLS`
  list of every fanning call that reaches it. Joining only the first pair strands its jobs until the
  backstop, which the rigs count as a bypass.
- **Whoever steps a store drains it.** `MetadataStore`'s `Machine::step` runs the command and does
  not drain: a `RequestDetail` stepped through it leaves its job on the outbox and nothing starts
  it. The same holds for the `pub` `Stores::metadata_run`. The owner drains in the same call, as
  `Bridge::metadata_run` does; no type enforces it, the counter below catches the lapse.
  Person's inputless `Machine::step` refuses Open/pump explicitly, while Close, Reset and view edits
  remain usable. Other store senders must likewise supply captured inputs and drain their jobs. The same holds for the public
  `CatalogStore::open` and `CatalogStore::pump`: `catalog_feed` and `catalog_reread` call
  `Bridge::catalog_open` and `Bridge::catalog_pump`, which drain in the same call. And for
  `HubsStore::run_with_directory`, `land_with_directory` and `tick_with_directory`: the
  Bridge's `Store(Hubs(..))`, `HubsResult` and `StoreWork::Hubs` arms and `Bridge::hubs_run` drain
  with `start_store_jobs_of(StoreId::Hubs)` in the same call, and a controlled Home arm admits the
  jobs (`Bridge::admit_home_jobs`, the bullet headed "The Hubs lane"). And for the public
  `SearchStore::run_with_directory` and `pump_with_directory_and_gate`: the Bridge's
  `Store(Search(..))` and `StoreWork::Search` arms, `boot.rs`'s reset and `dev/scenarios.rs`'s query
  all reach them through `Bridge::search_run` and `Bridge::search_pump`, which drain in the same call.
- **Results do not return as FIFO events.** A gated store's result (Detail, Collection, Person,
  Search, subtitle search, the view-state read-back, the view-state write) goes back through the landing and its gate
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
  `subsearch_pump`, `search_run`, `search_pump`, `reconcile_pump`, `browse_run`, `hubs_run`, `viewstate_run`, `viewstate_pump`, `notify_pump`,
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
- **Person's complete captured inputs.** The host captures before Open and every pump, borrowing
  its ONE retained `ClientKeys` including every registered slot, not only slots holding a key.
  `PersonInput` supplies settled session, visible generation, controlled mode, provider refusal
  decisions and dev biography/credits seeds. The origin is appended once even when unregistered.
  No step reads the roster/session/tape/dev provider; all 11 Person global ledger sites are gone.
  Stable pumps compare borrowed instance/grant signatures and session strings without cloning
  request data; token-only refresh preserves shelves. Local missing-client backoff, full-key job
  admission, rotated adapters, generation-addressed claims and ascending gated landing order stay.
  A captured fixed provider refusal settles in the emission turn without a job or retry; controlled
  jobs still carry Controlled and answer the failure value through their unchanged typed ports.
  Inputless Open/pump refuse explicitly; Close, Reset and view edits remain usable independently.
  Worker-body fork allowances are unchanged. Python source acceptance is not Rust/replay acceptance.
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
- **The Watchlist and catalog session input.** Neither store reads the registry to learn whose list it
  reads. `WatchlistAuth::capture` is gone, `WatchlistAuth::is_current` is a pure comparison against a
  `&CurrentProfile`, and `capture_auth` / `is_current` left the `WatchlistIo` and `CatalogIo` seams. This is
  the third consumer of the Search scope's pattern (`Bridge::household_ids`) and the Browse pins'
  (`Bridge::browse_session`): the Bridge reads, the store takes the value. No job kind and no runner arm is
  added; the five worker starts, `find`, `guid_of`, `copy_rank` and the clock read are W-2's.
  - **Two helpers, two shapes.** `Bridge::seated_profile` is `current_snapshot()`, an `Arc` clone handed to
    both stores' frame passes (`WatchlistStore::pump_with_gate`, `CatalogStore::pump`): a pass notices a
    switch or a sign-out in the same frame as before and captures nothing. `Bridge::watchlist_input` builds a
    `WatchlistInput` (the profile and `Option<WatchlistAuth>`) from ONE snapshot, `plex_tv_credential` and the
    client id, and is called only where a capture can happen: `WatchlistCmd::Open` (`watchlist_feed`,
    `watchlist_open_if_idle`, `catalog_feed`) and `CatalogStore::open` (`Bridge::catalog_open`). It is built
    per call and never kept, so a capture reads the credential at the moment it is made.
    `plex_tv_credential` reads the registry (`peek()`, the legacy-owner fallback), so it is called there and
    never from `data/`.
  - **`run(cmd)` holds no input.** Every command but `Open` runs through it; an `Open` is refused (a
    `debug_assert`, and a dropped command with a log line in a release build), and `run_with_input` is the
    stepping entry. The watchlist's `Machine` arms hand the step neither input nor profile: a `Cmd` goes
    through `run`, and a `Pump` is refused the same way, because a pump handed no profile would read as a
    sign-out. Nothing sends either.
  - **The catalog re-open.** A landing for a profile that is gone is dropped in `CatalogStore::pump`, which
    can capture nothing: it clears the identity and records the title as owed a read (`take_reopen`).
    `Bridge::catalog_pump` takes it and calls `catalog_open`, which reads the input then and starts the job,
    all in the pump's own call. An `open` or a `reset` clears the owed title.
  - **W1 leftovers (retired by W2).** At W1, the Watchlist still read `copy_rank::current_server`,
    `find::client_for`, `guid_of::client_for`, `server_ids` / `server_probe_result` / `server_roster_gen` /
    `server_facts_gen`, and `today()`. These are not current step exceptions: W2 moved client lookup to
    worker transport and roster, ranking and UTC-date facts to host-captured snapshots. At W1, the test
    entries (`WatchlistStore::pump`, `run_for_test`, `WatchlistInput::for_test`) handed a generation-1 seat.
- **A refusal the step can observe.** The runner may decline to start a job: the thread ceiling, a
  registry that moved on between the step and the launch (the client the job names is no longer the
  one the slot holds), or a `Held` runner whose client is gone. It then calls the port's `refused`,
  so every job gets exactly one answer, a refusal included. A refusal that only released the claim
  would have the next pump send the same request at once, every frame, so the step has to see it. A store whose claim is a `Fetch` (subtitle search)
  reads `Fetch::take_refused(gen)` at the head of its next pump: the port's
  `Fetch::refuse_for(gen)` frees that generation's claim and remembers the refusal for
  `take_refused` to answer once, and a refusal for a superseded generation (one whose claim a newer
  request or a supersede has replaced) frees nothing and records nothing. A store whose claim is a mailbox of landings
  (the view-state read-back and the view-state write) has its port post a store-local `NotStarted`
  landing, and the next pump puts the flight's items back and waits (a write is judged again first,
  and dropped when its profile, its server or its client's key moved). A refusal never goes into a recorded mailbox (a recorded
  take would write it as an async result and change the bytes). It reaches the step one pump after
  the launch; no fixture holds a refused start, so no recorded byte moves.
- **Subtitle search and the view-state read-back** are the fifth and sixth stores on the lane. The
  worker start and the `client_for` read are gone from `data/src/subsearch.rs` and
  `data/src/stores/viewreconcile.rs`, and their four ledger lines are deleted. Each step reads
  `plx_plex::plex::ClientKeys` as an argument and emits a `StoreJob`
  (`plx_data::subsearch::{SubSearchReq, SubSearchPort}` and
  `plx_data::stores::viewreconcile::{ReconcileReq, ReconcilePort}`); the runner starts the job
  only if `client_for` still answers the key the step decided on. `Bridge::subsearch_pump` refills
  the keys, steps, then runs `Bridge::start_store_jobs_of` for its own store in the same call;
  `Bridge::reconcile_pump` drains `Stores::RECONCILE_FILLS` with `Bridge::start_store_jobs_in`,
  because an end-of-stream fan-out it queues sends a ViewState write; `Bridge::subsearch_run` only drains, because the command step takes no
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
    `Direct` (Catalog, subtitle search, view reconcile, Search's fetches and recents writes, the section
    hubs' read, the Library's directory and page reads, the view-state writes, the Metadata season switch,
    episode-page window and Related window) or `Home` (the Hubs read and the Library's source discovery),
    as one exhaustive match. A `Home`
    job is recorded in a controlled Home run's own ordered ledger (`HomeIo::admit`), not on the tape,
    with the bytes `HubRequest::admission` returns (the bytes `hubs_descriptor` returned before it,
    unchanged). A controlled arm of the Bridge steps Hubs, then admits what it emitted inside the
    arm with `StoreRunner::start_admitted(job, Some(io))`, so the hubs line precedes the discovery
    line in the ledger (`home_admission_tests` pins that order). The discovery job is admitted the
    same way, from the controlled `StoreWork::BrowseDiscovery` arm only (`Bridge::admit_home_jobs`
    with `StoreId::Browse`). Every other sender uses `start`,
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
  Search now receives `SearchInput` before commands, pumps and publications: the same complete
  `ClientKeys` as the other stores plus captured session/profile/tenure and recents facts. The
  scope compares borrowed exact facts on stable frames; recents keeps `STORE` and `PENDING`
  without reading the session registry. A test drives it with `Owner::run_jobs`
  (`search_test_support.rs`), a fixture that plays the runner against a served fake, in place of the
  inline fake-IO fork the old spawn carried.
- **The section hubs lane.** A library's shelves (`data/src/browse/section_hubs.rs`) are the ninth
  store on the lane: the whole-list read and each row's page read leave as one job kind,
  `StoreJob::BrowseHubs(Job<BrowseHubsReq>, BrowseHubsPort)`, on `BrowseStore`'s outbox. The step
  (`hubs_kick`, `hubs_page`, `hubs_invalidate*`, the pump's `hubs_tick_all`) is handed the
  registry's `ClientKeys` and the outbox, and decides the server and the `ClientKey` to ask with;
  `finish_browse_hubs_job` is the IO body (the old workers' bodies, unchanged) and the log line
  `libhubs: section N landed ...` the landing writes is unchanged. Admission is `Direct`: a
  controlled Home run never emits this work (`bootstrap/effects.rs` `work_value()` refuses every
  `StoreWork` but `Hubs` and `BrowseDiscovery`), so no replay fixture moved. It differs from the
  Hubs read in three ways.
  - **One claim, one mailbox, one read in flight.** Both reads of every section share one
    `Fetch<HubResult>` on the adapter: its claim is what `hubs_pump_spawns` is gated on, its mailbox
    holds the one answer, its refusal list holds the runner's refusal. The claim is raised at the
    STEP (where the job is emitted), not at the launch as Hubs' is: the pump is claim-gated and
    several steps run before one drain (the ViewState closure sends a `HubsInvalidateAll` per
    burst), so a launch-time claim would let a second step emit a second job for the same slot. The
    landing's take, or the runner's refusal, lowers it.
  - **A refusal is settled by the store's next step.** The runner refuses a job whose key moved by
    the launch (`resolve` found a different client) or whose thread the system refused. The refusal
    lowers the claim in the call that refused and records the request id; nothing else moves. The
    store records the one read it has out (`hubs_flight`: request id, epoch, section, list or row)
    and settles a refused one at the head of its next `hubs_pump_spawns` to the state of a read that
    never started: the list is owed again, the row is no longer in flight. A flight whose adapter
    has been replaced (a `Reset`, a roster retire) has no claim and no refusal and is dropped.
    `pump_needs_work` is true while a flight is out with no claim, so a refusal is never left
    waiting for another reason to wake the pump.
  - **The identity compare is a compare of keys.** The landing carries the `ClientKey` its job asked
    with; `hubs_land` takes it only if it equals `keys.key(sid)` of the section's server now, else
    the answer is `landing_was_not_ours` and the section is owed the read again. The three
    `client_for` reads (the two spawns and the landing's) are gone from the file.

  A step test reads the jobs (`BrowseStore::take_jobs`, in the data crate `take_hubs_job`) or answers
  them through the port; the runner's branches and every Bridge entry's drain have a test that fails
  when the branch or the drain is removed (`app/adapters/stores.rs`, `app/browse_hubs_drain_tests.rs`).
- **The listing lanes.** The Library's filter menus and grid read through two more kinds on
  `BrowseStore`'s outbox (`data/src/browse/listing_jobs.rs`):
  `StoreJob::BrowseDirectory(Job<BrowseDirectoryReq>, BrowseDirectoryPort)` is one value list of
  a section (`which`: the genres or the first letters) and
  `StoreJob::BrowsePage(Job<BrowsePageReq>, BrowsePagePort)` is one page of the section's listing.
  `kick_directory` (through `kick_genres` and `kick_letters`, from `LibraryWork::Genres` and
  `Letters`) and `maybe_spawn_owned` (from the pump) take `keys.key(sid)` from the borrowed
  `ClientKeys`, raise the read's claim and push the job; the runner resolves the key at the launch
  and hands the body the `&'static Client` (the registry reads of the file are gone). Both kinds are
  `Direct`, and a controlled Home run never emits one: `bootstrap/effects.rs` `store()` accepts only
  `Browse(Reset)` and `Browse(Discovery(..))` and `work_value()` refuses `StoreWork::Browse`, so no
  replay fixture moved. `app/browse_listing_admission_tests.rs` pins it in record and replay mode.
  The landings are as they were (`land_directory_owned_with_gate` and the page landing inside
  `pump_owned_with_gate`): the answer carries the client the key resolved and `key.token`, and the
  landing's `commit_if_current` / `commit_reachability_if_current` is the one identity check.
  - **Three claims, three mailboxes.** The page, the genres and the letters each hold their own
    `Fetch` on the adapter, with their own request counter on `BrowseState` (`page_req`,
    `genres_req`, `letters_req`), and each claim is raised at the step.
  - **A refusal is settled by the store's next step.** A key that moved by the launch, or a thread
    the system refused, refuses the job: the claim is lowered in the refusing call and the
    request id remembered. `settle_refused_reads` takes the three markers at the head of the next
    step and wakes the pump (`idle::invalidate`); nothing is recorded and nothing backs off. The
    directory is owed again (`genres_done` is still false, or the letters are still not current
    (`SecState::letters_current`), and the menu's tick sends the work again),
    and the page is still the first one missing, so the next pump asks again with the then-current
    key, exactly what the old `!spawned` arms left.
  - **The guard is in the body, the landing outside it.** A page read that panics lands as a failed
    page (`total < 0`: the listing keeps what it had and `retry_cd` backs the pump off), a genre read
    that panics lands as an empty list, and a letters read that fails or panics lands as `ok: false`:
    the landed list stays as it was (hidden, it no longer describes the grid) and the same key waits
    120 steps (`SecState::letters_backoff`), an edit asking at once. The seams `finish_page_with` and
    `finish_directory_with` take the read as a closure so a test can swap in one that panics.
  - **The letters carry the grid's terms.** The Letters job is asked with `SecState::query_filters`,
    the terms of the page read, and a `LettersKey` (applied Unwatched, applied genre, a generation
    that a membership change moves and, for an Unwatched key only, a watched state read back from
    the server; `data/src/browse/letters.rs`) that the answer returns. A list is current, and
    shown, only while its key is the section's present one, checked where it is read. The landed
    list and the one it replaced are both held (`SecState::letters_kept`: an unfiltered list
    survives a genre or Unwatched switched on and off; an Unwatched list is never the kept one),
    and a third key evicts the oldest.
- **The discovery lane.** The Library's source discovery (`data/src/browse/mod.rs`,
  `maybe_discover_owned`) is the twelfth store job: the section list, the server's name and the
  item counts of one source, as `StoreJob::BrowseDiscovery(Job<BrowseDiscoveryReq>,
  BrowseDiscoveryPort)` (`data/src/browse/discovery_job.rs`; `finish_browse_discovery_job` is the
  IO body, the old worker's, unchanged, and the runner's `launch_browse_discovery` starts the
  `"sources"` worker). The step is handed the registry's `ClientKeys` and decides from
  `keys.key(sid)`; the runner resolves the key at the launch, so the step's `client_for` read is
  gone. Admission is `Home`, unlike every other Browse kind: a controlled Home run starts it (the
  `StoreWork::BrowseDiscovery` arm), records its request with the bytes the old
  `DiscoveryRequest::descriptor` wrote (a literal pinned in `home_admission_tests` and
  `browse_discovery_admission_tests`), and a replay never launches it.
  - **A claim with no slot, one mailbox.** `src_fetching` is a `Fetch<()>` whose slot is unused:
    the answer still lands in `src_result` (the mailbox `record.rs` encodes and the landing gate
    takes), and the claim is raised at the STEP and lowered by `apply_discovery` after its epoch
    check, so the landing gate's dump mode still waits on it.
  - **A refusal is settled by the store's next step, and backs off.** A key that moved by the
    launch, or a thread the system refused, refuses the job: the port lowers the claim in the
    refusing call and remembers the request id (`Fetch::refuse_for`). The store records the one
    read it has out (`src_flight`) and `discovery_settle`, at the head of the next
    `maybe_discover_owned`, reads `take_refused`: if it was refused and the table epoch is the
    flight's, the source's `retry_cd` is set to the full `SRC_RETRY_CD`, as a refused launch
    always did, so a source is not asked again at 60 Hz. Nothing is invalidated and nothing is
    recorded. The refusal settles one step later than the old launcher's (which set the countdown
    in the launching call); no replay fixture holds a refused discovery. `BrowseStore::sync_roster`
    settles before it syncs the roster, so a source the sync re-arms in the same step (a
    re-pointed or retokened client, `retry_cd = 0`) is asked again at once, as it always was.
  - **Every entry drains it.** `Bridge::browse_discover_pump` refills the keys, steps, and starts
    the jobs of Browse; `frame_ingest` and the live `StoreWork::BrowseDiscovery` arm go through it,
    and the controlled arm drains inside the arm to admit. `app/browse_discovery_drain_tests.rs`
    fails when an entry's drain is removed, and `app/adapters/stores.rs` has a test for each branch
    of the runner's start.
- **The pins' session input.** `resolve_pins_from` (through `append_sections_with` and `record::apply_to`),
  `pin_record_for`, `record_pins_from` (through `apply_pins_from`) and `onboard::asks_for_sources` read no
  registry: each is handed the `&Session` the app read. This is the second consumer of the pattern Search's
  household scope began (`Bridge::household_ids`, the Bridge reads, the store takes the value), and of the
  controlled run's `preferences`, which is the same value handed under the `""` key. No job kind and no
  runner arm was added at this checkpoint; every replay fixture was byte-identical. The roster reconcile
  (`sync_roster_owned` -> `reconcile_pins_from` -> `pin_record_for`) still used an in-machine `peek()`
  then. S4 retired that ledger entry: `sync_roster_owned` now consumes `BrowseRosterInput`, and pin
  reconciliation receives the captured `input.session` and `input.profile_key`.
  - **Observe, then read.** `Bridge::browse_session` is the one production reader: it calls
    `BrowseStore::observe_pin_write` FIRST, then `session::peek()`. The write's receipt is sent after the
    storage worker installed the written session, so a session read after seeing the receipt contains the
    write. `PinWrite::observe` polls and latches; `PinWrite::saved` only reads the latch and never polls, so
    a step can never pair a receipt it saw with a session read before it. A write not yet observed answers
    with the local pending record. Each write has its own latch (`record_pins_from` replaces
    `pending_pins`). The two lines of `browse_session` must not swap, and its `Arc` is not kept across
    Bridge calls. Its callers are `browse_run`, `browse_discover_pump`, the `StoreWork::Browse` arm and
    `reconcile_pump`'s requery.
  - **`run(cmd, keys)` holds no session.** It is the Stores fan-outs' entry (`ApplyView`,
    `HubsInvalidateAll`, `HubsInvalidateServer`, `ListingsChanged`), and it refuses `ApplyPins` and
    `Discovery` (a `debug_assert`, and a dropped command with a log line in a release build) rather than
    read the registry; both reach `run_with_session`. The `Machine` arms hand the step no keys and no
    session.
  - **Historical leftovers (machine reads retired by S4).** This checkpoint still listed
    `current_profile_key`, `peek_settled`, and B-E's reads (`discovery_needs_pump`, `sync_roster_owned`).
    S4 retired the Browse globals ledger: roster reconciliation and discovery now consume captured
    roster, session, settled and profile facts through `BrowseRosterInput`. The `queue_update` /
    `queue_update_ticket` session merge remains IO-boundary work. The first-run gate's session is read
    by its callers, boot and `follow_auth_landing`. At this checkpoint, the test entry points
    (`*_for_test`, the `#[cfg(test)]` `resolve_pins` / `apply_pins` / `record_pins` shims) read the registry
    after observing the pin write, as the Bridge did.
- **The Metadata reads.** The season tabs, the long-season page window and the Related row's tail
  window (`data/src/metadata.rs`) are the tenth store reads on the lane. `load_season` flips the tab and emits
  `StoreJob::MetaSeason(Job<MetaSeasonReq>, MetaSeasonPort)`, addressed by the season generation it
  minted; `want_episodes` emits `StoreJob::MetaEpisodes(Job<MetaEpisodesReq>, MetaEpisodesPort)` for
  at most `EP_JOB_PAGES` pages, addressed by a sequence of its own (pages are keyed by
  `(server, season, page)`, not by generation). Both are `Direct` and never taped. The runner
  resolves `client_for(sid)` once, at the launch, and hands `finish_season_job` /
  `finish_episodes_job` that `&'static Client` (a presence check: the requests carry no
  `ClientKey`); `fetch_episodes` and `fetch_episode_page` take it, so the detail chain is one client
  end to end. A server it holds none for still launches, and the body answers the failure without a
  GET (a season lands `eps: None`, a page `rows: None` and gives its key back). Both bodies keep the
  `catch_unwind` guard and post their answer outside it. The season landing is gated, as it was
  before: `pump_season_with_gate` takes it through the landing gate, dump mode waits on it while
  `season_loading()` holds, and a replay takes it on its recorded frame (which is why a refusal
  catches `season_done` up in the call that refuses it). Only the page landing
  (`pump_episode_pages`) is an ungated take.
  - **A refusal leaves what the inline spawn's `!spawned` arm left.** `MetaSeasonPort::refused`
    catches `season_done` up to the job's generation with `fetch_max`, in the call that refused it,
    so `season_loading()` is false at once and a refusal that arrives after a newer request landed
    cannot turn it back on; the tab stays on the flipped season and the old episodes stay listed.
    `MetaEpisodesPort::refused` drops the inflight keys the job held (the port carries them,
    because a refusal is told only the address), so the next `WantEpisodes` asks again.
  - **A page job posts each page as it is read.** The body reads its pages in order and posts each
    to the port as it arrives, as the inline worker landed it: the window asks for a restore target
    first, and the strip waits on it, not on the pages behind it. Each post lands the page and gives
    its inflight key back; the job's one `done` carries nothing and is only the end, so it releases
    no key (a later window may have asked for the same page again since it landed).
  - **Drains.** `load_season` and `want_episodes` are reached through `Bridge::metadata_run` (the
    dispatcher) or on the bare store (`input.rs`, which drains with `start_store_jobs`). No fan-out
    reaches either (the view-state closures send `ApplyView` only), so no `*_FILLS` list names
    Metadata for them. The runner's branches are pinned in `adapters/stores.rs`'s tests; the OS
    refusing the thread cannot be forced on a host, so the refused branches are pinned at the port
    (`a_refused_season_job_ends_season_loading_and_leaves_the_tab_and_the_list`,
    `a_refused_page_job_releases_its_inflight_keys_and_the_next_want_asks_again`).
  - **The Related window.** `ask_related` (an edge ask, `WantRelated`, or a seek, `SeekRelated`)
    takes the single-flight claim in the adapter's `rel_pages` exactly as before (the adopt path
    that un-withdraws a read asked for again, and the `served` check that skips an edge that already
    landed, are unchanged) and then emits `StoreJob::MetaRelated(Job<MetaRelatedReq>,
    MetaRelatedPort)`, `Direct`, addressed by a sequence of its own (`RelPages.sent`; it shares
    `season_addr`'s id space, and nothing routes by an `Addr` alone). The request carries what the
    worker closure captured: the item, the ask, the window it starts from with its cards, the keys
    the tail must not show, the hubs with the lengths learned so far, the ledger the window carries
    and the view-state stamp. A controlled run never emits it (`bootstrap/effects.rs` has no related
    arm), so no replay fixture moved. The runner resolves `client_for(sid)` once, at the launch,
    which moves the resolve that used to run inside the worker onto the main thread: a read keeps
    its client for its few serial GETs, and a server the registry holds none for still launches and
    its body answers the failure (`window: None`) without a GET. The body is the old worker's:
    `read_row` over `tail_page`, behind `catch_unwind`, the answer posted outside the guard.
    - **The claim is the port's to give back.** `done` lowers `inflight` and `out`, drops the
      answer when the reader withdrew the read while it was out (taking the flag), and otherwise
      lands the window for `pump_related_pages`, which is unchanged and ungated. `refused` lowers
      the claim and clears `withdrawn` in the call that refuses, which is what the inline
      `!spawned` arm left, so the next edge asks again and no later read is discarded on a stale
      flag. A job the held runner parks keeps the claim up, as a worker that has not answered does;
      a cancel meanwhile leaves the answer to be discarded when the parked job runs.
    - **The return value.** `ask_related` returned `spawned`; it returns true when the job was
      emitted. Its readers are `MetadataStore::run`'s `changed` (the notice is bumped for every
      command but `ApplyView`, so a true never changes whether it rises) and the tests; the one
      production sender, `deliver_inner`, ignores it. A true for a job later refused changes no
      screen state beyond what a spawned read's true did.
    - **Drains.** The Detail screen's effects reach `ask_related` through `Bridge::metadata_run`,
      which drains Metadata after its step; that is the only production path. No bare-store sender
      and no fan-out sends `WantRelated` or `SeekRelated` (the view-state closures send `ApplyView`
      only), so no drain was added; `the_bridge_metadata_run_starts_the_related_window_it_emitted`
      pins the dispatcher path. The held runner parks the kind under `held()` and `held_all()`: no
      rig in the app, screens or hostsim suites waits on a Related landing through a real worker
      (`detail_related_landing_tests` lands the window with `land_related_for_test`), so there is
      no `LiveKind` for it.
- **Metadata M4: alt sources and dev inputs.** Detail installation emits a Direct, untaped
  `StoreJob::MetaAltSources(Job<MetaAltSourcesReq>, MetaAltSourcesPort)`. The request captures the
  item's `(sid,rk)`, GUID, roster generation, ordered whole roster **including origin**, and whole
  `ClientKeys` snapshot. `finish_alt_sources_job` constructs `KeyedClients` from that snapshot and
  the runner's lookup; a missing/moved key skips only that server immediately before its GET.
  Servers registered later never join the read; credential changes after a request is on the wire
  remain outside this fence. Resolve panic answers one empty list, posted outside `catch_unwind`.
  Done does not settle before the gated by-value take; refusal settles immediately and monotonically
  without a mailbox, record or retry, so dump-mode owed cannot starve on a refused launch.
  - **Publication locking.** Request generation increment/slot clear, roster invalidation, and port
    publication all acquire `alt_slot` first, then inspect/change `alt_gen`. A stale writer cannot
    overwrite a newer slot. Consumer generation filtering, landing rank, `(sid,rk)` addressing and
    captured-roster fencing are retained. Facts changes restamp credit; they do not cancel work.
  - **One call, three phases.** Bridge refills keys BEFORE begin; `pump_alt_sources_with_library`
    retains the roster/facts reads and bookkeeping/prune, then consumes an eligible noncontrolled
    rk-only stand-in chance and returns a private, non-Clone lifetime-bound pass plus owned request.
    The pass exclusively borrows Metadata and shares the supplied keys through finish: clear,
    reset, away-and-back, competing steps/publication, reuse, and refill cannot interleave in safe
    callers. A private per-pass identity binds the owned host answer to its pass. The host handles
    only a due request using the retained Browse library; finish installs a stand-in first, then
    gated real data compared with the **PRE-registration roster**, folding one notice. Registration
    is observed by invalidation on the NEXT pass, never between begin and finish.
  - **Dev host, not logic IO.** Shared-trigger source latch, per-item stand-in reads and registry
    registration live in `src/dev/scenarios/metadata.rs`. Source is captured at live Detail launch
    and held execution, rather than first draw: this is an approved **timing change**, not a pure
    move. `Detail::source()` uses retained `Option<String>` then real owner facts: `None` is unarmed,
    `Some(empty)` forces no credit. The field is serde-default AND serialization-skipped, preserving
    frozen terminal bytes and neutral decoded replay. Controlled capture suppresses before cache/IO;
    `Bridge::controlled_home` constructs fresh Metadata (only Hubs are restored), so live Details
    cannot leak retained overrides across that host boundary. Controlled installation is neutral too.
    Stand-in reads remain once per due noncontrolled rk, even without library/list; source latch is
    separately once per process. Token is read only on fallback registration. The host preserves
    current-server identity and uses `register_origin` with origin/pin/connection facts.
  - **Drains and held policy.** Bridge detail landing and aggregate Metadata pumps refill immediately
    before pumping and drain Metadata immediately AFTER it, within the same call. Store/raw support
    pumps take explicit keys/control inputs; Machine Pump without them refuses before taking data.
    ApplyView-only fan-out lists do not change. Default held/park_page_reads keep alt live off-thread;
    held_all parks it, and a new Detail does not retire parked alt. Reset rotates adapter identity.
  - **Ledger and verification scope.** Removed one Metadata worker, eight globals and three forks;
    five exact globals remain (request roster enumeration/gen, pump roster/facts gen, Detail tape
    active). Every area header and entry was recounted: actual totals 7/111/68/11 become
    **6/103/65/11**; the brief's earlier 121/113 globals sums were arithmetic errors, and the old
    forks pin 71 was a loose ceiling. M4 does not claim all Metadata pure (owner facts and worker
    timing remain). On prior tree `cfa3cba398266e37b2de759bd6896faa05932fda`, the unchanged Linux
    replay driver passed 6/6 fixture-mode replays (3,412 graded frames, zero differences); this is
    historical evidence, not the publication head. M4 landed as PR #1315 (merge
    `5b4e8db7745167e3211b9842147dfc34423755d3`) after 19/19 checks on exact head `29a`.
    Exact publication replays were 6 SAME, 2,442 graded frames, with all differences zero. Local
    full `make check` remained FAILED: two isolated runner-environment issues and a benchmark
    timestamp defect were independently reproduced on BASE `0aaf`. This does not establish S4
    testing or ARM, TV or video behavior.
- **The view-state write lane.** The household's watch-state writes (`data/src/viewstate.rs`: mark
  watched, mark unwatched, remove from Continue Watching, and the fan-out of a watched write, or of
  an end of stream, to the title's other copies) are the eleventh store on the lane. `kick` starts no
  worker: it emits `StoreJob::ViewWrite(Job<ViewWriteReq>, ViewWritePort)` on `ViewStateStore`'s
  outbox, one write out at a time. Admission is `Direct`, and a controlled Home run refuses every
  write before it reaches the store (`app/viewstate_controlled_tests.rs`), so no replay fixture moved.
  It differs from the reads above in four ways.
  - **Two inputs, not one.** The step takes the `ClientKeys` and the profile the press was made
    under (`Bridge::profile_scope()`, which is `plex::session::current_gen()`: the Session owner's
    profile-scope counter, allocated only when a profile is seated, detached or signed out, and
    never moved by a same-profile token refresh). `Req` captures both at the press. At the kick, and
    when a refusal lands, a write whose profile moved, or whose client carries another key than the
    press named, is dropped and its ledger entry retired: a profile switch re-tokens the registry's
    clients in place, so the slot still answers, under the next profile's credential, and a write
    is one household member's statement. The key compare is not optional: a switch swaps the tokens
    in the frame the session adopts them and the new scope is published by a later commit, so for
    those frames the profile still reads as the press's. A genuine same-profile token refresh drops
    a press too (the read-back restores what the server holds); keeping it would take an identity
    that moves with the token swap.
  - **The fan-out reaches other servers through the snapshot.** The job carries the whole
    `ClientKeys` the step decided under, and the worker (`finish_view_write_job`) builds
    `plx_plex::plex::KeyedClients` from it, with the registry read the runner passes in as a `fn`
    pointer, so no data-layer line names `client_for`. `fan_out` and `ask_sources` ask it for each
    other source, and a source whose client no longer carries the key (re-tokened by a switch after
    the press) is sent neither the lookup nor the write. The origin's client is resolved once, at the
    launch, and the worker is handed that `&'static Client`. The check covers the moment a request
    is built: one already on the wire is not recalled.
  - **A refusal is not a failure.** The runner refuses a write whose origin key moved by the launch
    (or by the time a parked job runs), or whose thread the system refused, and the port posts
    `NotStarted` into the owner's mailbox. Nothing left the device, so the pump changes no ledger
    entry and owes no read-back or hub refresh: it judges the write again like any queued write (a
    moved profile, a vanished server or a re-tokened client drops it) and puts it back at the head
    of the queue, and waits out the retry ladder. A worker that panics answers with a failed write, never `NotStarted`: the origin write
    may already be on the server, and a second scrobble would add a second viewCount.
  - **One outbox, two fill lists.** ViewState sits in `Stores::VIEWSTATE_FILLS` at its `StoreId`
    position, and `Stores::RECONCILE_FILLS` is what `Bridge::reconcile_pump` drains. A reset keeps
    the outbox: a job already sent answers into the adapter it was emitted under, which the owning
    store has retired.

  A step test drives the lane through the job's port (`data/src/viewstate.rs`), the key-checked
  fan-out has a test that fails when `fan_out` reads the registry again
  (`stores/viewreconcile_tests.rs`), and the runner's branches have tests that fail without the key
  compare in `resolve` (`app/adapters/stores.rs`).
