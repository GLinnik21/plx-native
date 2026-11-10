# Metadata M4: alt sources and dev inputs

Status: **implemented locally and reviewed**; publication-head local gates and CI are pending. The audit and design instructions below describe the historical starting point and implementation contract, not a proposal or blanket release acceptance.

## Base and dependency audit

Historical base audit: `d06ed4f5ca0bd43e68df4c030d80cb6b76d28ce6`. The observations below describe that base, not the publication head.

Current implementation agrees with `docs/sans-io.md:183-230,393-446,603-729`:

- Metadata M1: `metadata::fetch_full` takes the runner's client through the fetch chain (`metadata.rs:3729-3843`, `adapters/stores.rs:431-447`). M2: `StoreJob::{MetaSeason,MetaEpisodes}` and their ports/bodies exist; `load_season` emits (`metadata.rs:5064-5096`). M3: `StoreJob::MetaRelated` exists (`stores/mod.rs:787-791`), with `metadata_related_job_tests.rs` in **data/src**, and runner start/held arms (`adapters/stores.rs:698-722,1100-1102`).
- W2's SubtitleSearch/ViewReconcile/Catalog and W3's Hubs/Search/SaveRecents variants and admission policy are present (`stores/mod.rs:729-740,768-778,807-862`).
- Browse section hubs/directory/page/discovery are present (`stores/mod.rs:745-763`); discovery and pins are documented as landed, with roster/session leftovers honestly retained (`sans-io.md:603-661`, `globals/data-browse.txt`). ViewState's ViewWrite job and shared `KeyedClients` exist (`viewstate.rs:567-594`, `plex/client_keys.rs:80-111`). Watchlist W-1's explicit auth/profile input is documented at `sans-io.md:393-420`; W-2 remains on its workers/globals ledgers.
- At the audited base, M4 remained to be implemented: the Metadata worker and dev registration still lived in data. This historical observation is not a publication-head gate result.

Scope: only M4. No new ClientKeys columns, competing resolver, UI/screen changes, AppFx routing, unrelated refactor, fixture re-record, or real-account writes.

## Typed request, port and execution

Add comparable/debuggable `MetaAltSourcesReq { sid, rk, guid, roster_gen, others: Vec<ServerId>, keys: ClientKeys }` in `data/src/metadata.rs`, and `StoreJob::MetaAltSources(Job<MetaAltSourcesReq>, MetaAltSourcesPort)` in `data/src/stores/mod.rs`. `Addr.req` is the minted `alt_gen`; use a dedicated alt-address helper with Metadata's addressee/epoch conventions, not a new StoreId or landing rank. The port holds the originating adapter Arc (pointer equality/debug as sibling ports), plus the addressed item if needed to reconstruct the private landing. No result payload cloning.

`request_alt_sources` retains its generation increment, roster capture, slot clear and early settlement for empty GUID or fewer than two captured roster IDs. Capture **all** `server_ids()` in `others`, including the item's own server; do not substitute `keys.iter()` or filter before the existing two-server condition. Clone the whole supplied ClientKeys once into an emitted request. The early returns emit no job and owe no port answer; emission makes `alt_gen != alt_done` until completion/refusal/invalidation.

`finish_alt_sources_job(port, addr, lookup, req)` builds existing `plex::KeyedClients::new(&req.keys, lookup)` as ViewState does. App passes `plex::client_for`; data never names it. `resolve_alt_sources(others, guid, clients)` calls `clients.client(id)` immediately before each server lookup. Missing snapshot key, missing live client, moved instance/token/grant: **skip that server**, not refusal of the whole multi-server read. Continue in captured order, including origin. Preserve `find_by_guid` None versus empty handling (both yield no rows today), `alt_copies_from`, library names and worker-side owner lookup. Newly registered servers are not added mid-job. A swap after the key check while IO is on the wire remains outside this fence (`client_keys.rs:86-88`).

Catch unwind around the complete resolve, then post one `done` outside the guard with the existing `AltResult { gen, roster_gen, sid, rk, list }`; panic produces an empty list, retaining the old catch/default semantics and scrubbed diagnostic. A successful zero-row/fully-skipped lookup is `done`, not `refused`.

`MetaAltSourcesPort::refused` only settles its own outstanding generation in unrecorded adapter bookkeeping, never writes `alt_slot` or a Detail landing. Use generation-aware monotone settlement (`fetch_max`, matching MetaSeason's pattern), so a stale refusal cannot make a newer completed generation appear owed. No retry/new job is invented: the old refused spawn settled without retry. One start path owns one terminal answer: `done` OR `refused`, never both. Cloning the adapter/port handle for spawn refusal is allowed; cloning results is not.

Add exhaustive arms to StoreJob `store`/`admission` and runner `start`, `retire_parked_details`, `run_held_where` (the implementation behind `run_held`). Existing Metadata `take_jobs_of`/`has_jobs_of` aggregation must continue to reach the variant. Admission is **Direct**, including controlled runs: no tape/Home admission, validator changes or AppFx. A newer Detail request keeps parked alt jobs, just as it keeps Related; they answer through their own old port and the generation/roster/address fences discard obsolete content. Reset rotates the adapter (`stores/metadata.rs:348-351`), isolating old jobs.

### Held policy

Choose **live under `held()` and `park_page_reads()`, parked under `held_all()`**. Add `LiveKind::MetaAltSources` to those two live sets, not to the empty held_all set. Unlike Related (whose tests fabricate landings), alt currently starts a real worker whenever a Detail lands, even with a Held Detail runner; `Bridge::run_held_store_jobs_for_test` runs arbitrary parked work on the test thread. The same blocking-fixture hazard that justified discovery staying live applies to this multi-server read (`adapters/stores.rs:208-214`, `sans-io.md:260-268`). Do not silently park it in every existing rig or claim that current suites proved this policy. New held_all tests explicitly release mock replies before running blocking work. Preserve existing four live kinds unchanged. A parked job keeps its generation owed; running it rechecks each snapshot key, and terminal refusal must settle immediately even in dump mode.

## Fences, landing, and same-call drains

Keep the `AltResult` take **gated and by value** (`metadata.rs:4793-4810`), its `adapter_rank`, the stale-generation filter inside the take and the `(sid,rk)` addressed store/read contract (`:4460-4466`). Keep captured roster comparison; roster invalidation bumps generation, settles `alt_done`, clears slot and prunes without reasking (`:4769-4777`). Facts movement only restamps ownership (`:4783-4785`); a real landing still regrades against current facts, overwrites a stand-in and clears stand-in ownership (`:4492-4513`). Do not turn facts refresh into cancellation. `alt_prune_inactive(state, keys)` retains copies only if `keys.key(copy.sid).is_some()`; it reads no live registry.

Port completion should not overwrite a newer generation's slot: check generation under the slot lock before installing, as well as retaining the gated consumer filter. Refusal writes no recorded mailbox, and advances settlement in the refusing call, so dump mode cannot starve waiting for a job that never started. Roster invalidation and reset must also end the old wait. Do not let a stale completion settle a newer request or a stale refusal regress done.

`install_landed_detail` calls `request_alt_sources` from inside `pump_detail_with_gate` (`metadata.rs:4264-4273`): this emission is why a pre-pump backstop alone is insufficient.

- `Bridge::metadata_pump_detail` (`bridge.rs:2015-2021`): retain pre-pump `start_late_store_jobs`; **refill ClientKeys immediately before** the detail pump, pass keys through `MetadataStore::pump_detail_with_gate` to installation/emission, save changed, **drain Metadata immediately AFTER** the pump, return changed. Never delay to frame end or move Detail's take.
- `Bridge::metadata_pump` aggregate test entry (`bridge.rs:1712-1716`): refill, pass inputs, pump and drain in its own call. `Stores::metadata_pump` and `MetadataStore::pump` must receive/forward those inputs. Do not rely on the next backstop.
- `MetadataStore::pump_detail`, raw `metadata::pump_detail` test/support helpers, `MetadataStore::pump`, and `Machine::step(StoreEv::Pump)` are compatibility entries that can reach installation. Provide explicit keys/dev input forms; no hidden registry fallback. A no-input public compatibility path must refuse the input-requiring pump (assert plus log) rather than consume a landing and silently omit its alt job. Migrate tests to explicit empty/synthetic values and drain/answer emitted jobs themselves.
- Keep existing `Bridge::metadata_run` and bare-store callers' post-command drains (including the season press and describe_movie handoff). Run/season/Related pumps do not currently emit alt work. If any new path reaches installation, it takes the same keys and owns the same-call drain. Do not add Metadata to fan-out lists for ApplyView-only senders.
- `Bridge::metadata_pump_alt_sources` (`bridge.rs:2035-2038`) owns one synchronous **begin → host → finish** call: refill ClientKeys **before registration**, then begin data bookkeeping/prune with those keys and supplied controlled/chance state; handle only the emitted stand-in request using the retained directory/library; finish with the matching pass and optional owned list. No Metadata step/reset, invalidation, key refill/prune or publication may intervene between begin and finish. Fold the accumulated changed flag into one notice only at completion; preserve frame order and no additional publication for an unchanged list. Do not prepare/register before begin or refill after registration for this pass.

## Dev host extraction and API compatibility

Actual host module is **`rust-modules/src/dev/scenarios.rs`**, not the stale `src/app/dev/scenarios.rs` path. Move `dev_source`, `alt_dev_stand_in`, `alt_stand_in_slot`, trigger file reads and the registry registration behavior there (a subordinate module is fine). Keep pure `metadata::alt_stand_in` in data.

Important current call-path correction: `dev_source` directly feeds **`Detail::source`** (`metadata.rs:1795-1800`), not just the alt pump. On this base, `fetch_detail` constructs no source field (`:2267-2363`); `fetch_full` calls it on the Detail worker (`:3729-3843`, runner `:431-447`). Moving only the pump would leave ambient IO in the Detail's source projection. Carry the dev-source value through the worker/Detail pipeline as well; do not restore a trigger read in the worker or draw to preserve the old API. Keep `Detail::source()`'s no-argument UI-facing signature using the supplied override retained on the Detail plus its existing real-facts fallback; owner facts are not S4 in this lane. An absent override means unarmed, **Some(empty) means force no owner**. Do not flatten those states.

Concrete input plumbing to implement:

1. Host `dev_source` preserves the once-per-process shared-trigger cache, consulted only outside controlled tape. Capture its owned optional string at the runner boundary before launching/running the Detail body, and pass it into an explicit `fetch_full` input form and the detail construction. Both `launch_detail` and the Detail arm of `run_held_where` supply it. `finish_detail_fetch`'s injected fetch closure remains the test seam. Direct fetch_full callers must supply the value (or an explicitly neutral compatibility wrapper); they must not read ambient dev state. No new field in recorded admission JSON; keep any new Detail-only input out of the frozen record schema/encode neutral on replay. Do not store it in speculative ClientKeys columns.
2. Split the alt pump into explicit same-call phases, not an IO callback inside data logic. **Begin:** keep current roster/facts reads and bookkeeping/prune in `pump_alt_sources_with_library` (the existing raw function may become the begin bookkeeping helper), preserving both exact registry ledger keys. Capture the **pre-registration** roster comparison value, accumulated changed flag and adapter/item identity in an ephemeral single-use pass. Evaluate supplied controlled/chance state; consume only a due noncontrolled chance and emit an owned minimal request (rk, runtime and resolution fields needed by the pure stand-in builder), never a cloned result. **Host:** handle only that request: read triggers, optionally register/select a second slot and build an optional list using the retained Browse directory's library, not a new global library lookup. **Finish:** consume the matching pass plus list by value, install any stand-in with its existing ownership, then take the real result through the existing gate using the pass's CAPTURED PRE-REGISTRATION roster. Keep generation filtering unchanged and fold changed once at completion. A new slot is observed by roster invalidation on the NEXT pass, not in this call. Thread these phases through `MetadataStore::pump_alt_sources_with_directory`, raw directory/gate wrappers and library helper; neutral/scripted compatibility callers follow the same ordered phases. Reject reused or mismatched passes (including adapter/item changes or reset) before installation/take. No new long-lived runtime or async machine, registry site or ClientKeys column; no ledger rename to disguise a read.
3. Preserve old chance identity and cadence in begin: supplied controlled state returns from chance evaluation **before** consumption or any host IO; no current item means no chance; `stand_in_rk` compares rk alone, and marks even an unarmed/empty result. Do not opportunistically change it to `(sid,rk)` in M4. Exactly one per-item shared-trigger read for a due noncontrolled request, including unarmed items; the separate source latch remains once per process. Represent not-due versus due-with-no-list explicitly: absent library/input, failed/absent registration or unarmed triggers still consume a due chance, so they cannot reread every frame. Even without a request/list, finish performs the ordinary gated real-result take.
4. Armed nonempty handle plus a directory library permits the stand-in; unarmed/empty handle does not. Source override's armed-empty semantics differ intentionally. Stand-in list skips ownership regrade only while it owns the list; an armed trigger does not suppress a later real answer. Preserve the real landing's precedence at the end of the pump.
5. Prefer a different active registered server; otherwise obtain current client and a nonempty token trigger and register `standin-{machine_id}`; UNSET/no client/no token means no stand-in. Preserve a real second client/source-switch target, never synthesize an unnavigable sid. Read token only on this fallback. Keep current server (not d.sid) as the stand-in's here value and the pure builder's runtime/resolution/library behavior.
6. The host move must address the existing origin warning in `plex/CLAUDE.md:63-71`: use existing `register_origin` with the current client's origin/resolve-pin/connection facts instead of losing TLS in host/port reconstruction. Preserve synthetic machine identity, token selection and second-slot reuse. This is a narrow dev-host correction, not a production endpoint/registration redesign; test origin preservation and source switching.

Host feature gating may use existing devtriggers stubs; no test/production fork in logic or new ambient fallback. Test boot supplies neutral/scripted values explicitly, not a cfg(test) branch returning None. No screen call sites need editing. Controlled record/replay suppresses both dev effects before IO/cache initialization/chance consumption, as today's tape checks do; do not read the source trigger for a controlled Detail worker. The source value is an input, not a reason to tape Direct alt jobs.

## Ledger deltas

Current file sums, read from every area header (also equal to the visible entry counts):

- workers: metadata 1 + notify 1 + watchlist 1 + plex 2 + session 2 = **7**.
- globals: browse 14 + metadata 13 + notify 8 + person 11 + pms 3 + search 14 + viewstate 2 + watchlist 7 + media 3 + plex 12 + screens 20 + session 4 = **111**. The earlier 121 total was an arithmetic error, not ten additional sites.
- forks: metadata 3 + notify 3 + person 2 + machine 1 + media 4 + play-feed 3 + plex 33 + screens 3 + session 3 + ui 13 = **68**.
- fxlanes: app 11 = **11**, unchanged.

Delete `ci/allow/workers/data-metadata.txt` (its sole key `rust-modules/data/src/metadata.rs::request_alt_sources::spawn_small`). Delete `ci/allow/forks/data-metadata.txt` (three keys, same path prefix: `::alt_dev_stand_in`, `::alt_stand_in_slot`, `::dev_source`).

From `ci/allow/globals/data-metadata.txt`, delete exactly these eight keys, each prefixed `rust-modules/data/src/metadata.rs::`:

```
alt_dev_stand_in::current_server
alt_prune_inactive::client_for
alt_pump_stand_in::tape::active
alt_stand_in_slot::client_for
alt_stand_in_slot::current_server
alt_stand_in_slot::server_ids
dev_source::tape::active
resolve_alt_sources::client_for
```

Keep that globals file with **5** entries unchanged:

```
pump_alt_sources_with_library::server_facts_gen
pump_alt_sources_with_library::server_roster_gen
pump_detail_with_gate::tape::active
request_alt_sources::server_ids
request_alt_sources::server_roster_gen
```

Implementation recount of every area header AND entry corrects the earlier arithmetic: actual totals are workers **7→6**, globals **111→103**, forks **68→65**, fxlanes **11→11**. Existing pins were 7/111/71/11; the forks pin was a loose ceiling over 68 actual entries. New pins are **6/103/65/11**, all downward. The earlier 121→113 claim was a summation error; no sites were added/renamed to reconcile it. The newer three globals entries explain the difference from the attached M4 globals −5: actual globals **−8**. The globals file must NOT be deleted.

Remaining unledgered reads: `server_facts(..).handle` in `alt_regrade` and worker projection, `Detail::source`'s real-facts fallback, worker `Instant::now` in fetch_full. Request roster enumeration/gen and pump roster/facts gens remain explicitly impure; no claim that M4 makes all Metadata pure. No S4 fields are added and `client_keys.rs` needs no edit.

## Acceptance tests and verification

Add data `metadata_alt_sources_job_tests.rs`, runner cases beside Related/ViewWrite tests, and Bridge alt drain/admission cases. Each test must fail if its corresponding emission, port terminal or drain is removed:

- Land a Detail with GUID and two captured roster IDs: exactly one MetaAltSources request, exact Addr generation/addressee, sid/rk/guid/roster/others and whole snapshot; emission starts no worker. Empty GUID and one roster ID emit none and settle immediately.
- Script three servers: include own copy; absent snapshot key, removed client, retoken, revoked grant and repointed instance skip only their respective slots. Matching later servers still execute once, unseen new slots never execute. None/empty lookup and library/owner/runtime projection preserve current behavior.
- Every accepted job gets exactly one done; panic answers empty once outside catch. Force refused launch at a narrow injected launch seam/port (do not depend on OS refusing a host thread): exactly one refusal, no slot/record pollution, no retry spin, dump owed false immediately. A late refusal cannot regress a newer done.
- Held default starts alt off-thread; held_all parks it without early answer, then runs once; second run runs zero. New Detail keeps the parked job; same rk on another sid, clear, roster movement, reset and supersession cannot expose its payload to the new addressed reader. Stale done must not overwrite a newer pending/landed slot. Test reset uses the old adapter only.
- Bridge detail landing emits and drains within the SAME call (ordinary and controlled), aggregate helper likewise; `late_store_jobs == 0` and `assert_no_store_jobs_left_for_test`. Check explicit-input raw/Store/Machine compatibility paths and no-input refusal; don't make missing inputs look like a sign-out or a successful no-alt landing.
- Dev host scripted reads: latch precedence including armed-empty/unarmed, one per-item stand-in read (rk-only identity), no controlled IO or consumed chance, absent library, genuine second server, token fallback registration, absent registration/UNSET, preserved TLS origin, switch onto the registered second slot, real answer replacing stand-in, facts restamp after replacement. Exercise supplied source override through BOTH live and held Detail bodies and its unchanged no-arg source projection; no file-dependent test fork.
- Same-call ordering regression: seed a ready current-generation `AltResult` under the starting roster (with adapter roster bookkeeping aligned); begin captures that roster, fallback registration advances the live roster between phases, and finish must still let the real result replace the stand-in in the SAME call with one folded notice. Assert no intervening refill/prune/invalidation/publication; the NEXT pass observes the new roster and performs existing invalidation. Cover no registration/unarmed, due-with-no-list consumed without retry, controlled/no-item/not-due, stale/reused/mismatched pass and reset rejection, plus neutral/scripted compatibility callers following identical phases. Moving registration before bookkeeping must fail this regression.
- Byte-compare controlled admission output before/after with forced GUID/two-server Detail landing: Detail Tape bytes unchanged, no MetaAltSources admission bytes in record OR replay. Keep gated AltResult take, validators, rank and fixtures **1/6/12 unchanged**; run Targets and Resolve replay modes. No result cloning to satisfy borrowed events.

Expected publication-head gates include `make check` and `make check-nodef`; local and CI results remain pending. The repository-pinned Rust is specified in `rust-modules/rust-toolchain.toml`. Supplied pre-change Metadata baseline: 232 passed, 0 failed. The unchanged platform-independent replay driver also permits Linux verification; the convenience `make check-replay` target uses macOS.

Observed final local verification (shell outputs are primary evidence):

- Targeted data Metadata **245 passed/0 failed** (`.empryo/m4-data-tests.log`), app meta **17/0**, hostsim meta **17/0**; baseline data **232/0**.
- `make check-nodef` **PASSED**, dev profile finished in **44.76s** (`.empryo/m4-nodef.log`); data compile-fail lifetime doc tests **2/0** (`.empryo/m4-doc-tests.log`). No source edits after these latest passing nodef/data/doc tests.
- FULL app **FAILED: 1300 passed; 1 failed; 0 ignored; 0 filtered out** (`.empryo/m4-app-full.log`); FULL hostsim **FAILED: 1495 passed; 2 failed; 0 ignored; 0 filtered out** (`.empryo/m4-hostsim-full.log`). All three new runner and all five new dev-host tests show `... ok` in the full app log; they were executed, not merely filtered.
- Existing LF-exact pins fail on CRLF: `rust-modules/src/app/run.rs:9838-9843` cannot split its LF module marker and sees its own forbidden literal; hostsim additionally fails the lifecycle no-control section test at `rust-modules/appkit/src/player_hud/loaded.rs:127-133` matching multiline LF-only HUD text. Coordinator direct read-only in-memory probe found actual marker raw CRLF **0** / normalized LF **1**, forbidden self-literal present in raw unsplit prefix but **absent** in normalized prefix, and actual HUD disc pattern raw **0** / normalized **1**. Neither source nor fixture was normalized; no unrelated test patched. Tests remain **FAILED**, not skipped/passed.
- Full `make check` **FAILED** (`.empryo/m4-check.log`): `bash\r`/`python3\r` execution failures, Python 3.10 without `tomllib`, checkout symlinks/patch digests, source-bundle/GPL inputs, deleted uncommitted allowlists still in `git ls-files`, and build-bench rebuild-count failure. Not all causes are proven preexisting or resolved. Cargo/harness branches **STOPPED** on sibling failure, not passed.
- The actual M4-introduced docs duplicated-release policy failure was fixed by referencing `rust-modules/rust-toolchain.toml`; independent exact policy-test rerun: **Ran 1 test in 7.498s — OK**. This is not a full-gate pass.
- Ledger ratchet previously printed **ok** against `d06ed4f5c`; postcounts/pins **6/103/65/11**, no new sites. Scanner itself **did NOT pass** (CRLF shebang).
- Native Git audit `orun-19-7a18c289` **PASSED**: `git diff --check`; protected fixtures/manifest/replay driver/scanner/awk/toolchain file unchanged; original user `.gitignore` `+.worktrees/` diff and screenshots/runtime preserved. New files untracked; all evidence is **dirty-tree**, not committed-tree release evidence. No staging/commit to obtain source-bundle success.
- On prior tree `cfa3cba398266e37b2de759bd6896faa05932fda`, the unchanged Linux replay driver passed fixtures 1/6/12 in both modes: 6/6 replays, 3,412 graded frames, zero differences. This is prior-tree proof, not committed-publication-head verification. Full Linux host gates were not rerun; publication-head local gates and CI remain pending. Replay proves neither ARM/link/TV nor video behavior.

## Approval boundary

Owner approval covers the request/port contract, per-server skip behavior, live-held/parked-held_all policy, explicit source/stand-in inputs, synchronous begin → host → finish ordering with pre-registration roster capture, and narrow origin-preserving dev registration move. Moving registration ahead of bookkeeping advances the roster, bumps alt_gen and clears a ready real slot: that is a behavior regression, not an equivalent extraction. Source capture at live launch/held execution is an intentional **timing change**, not a pure move from draw-time observation. Implementation must preserve fixtures and exact site deletions; gate results are reported only when actually observed. A private lifetime-bound pass may statically prohibit reset/clear/away-and-back/reuse instead of validating an owned revision token. Prior-tree replay is not a substitute for publication-head gates or device verification.
