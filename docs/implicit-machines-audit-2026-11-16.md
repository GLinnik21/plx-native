# Implicit state machines: audit of 2026-11-16

A census of the lifecycles this codebase tracks **without** one enum and one transition function:
flag clusters, phase enums with scattered writers, statics that encode a phase, loose timers, and
multi-step flows held in `pending_*` Options. It is a dated snapshot of `main` at `8fc1b62e1`;
line numbers are that commit's. `ci/machines.ini` (ten machines) is what "explicit" means here,
and those ten are excluded.

## How it was made, and what grade of evidence it is

1. A mechanical pass over every production struct flagged the ones with two or more `bool`
   fields or four or more `bool`/`Option` fields, and every enum named `*State`, `*Phase`,
   `*Stage`, `*Status`, `*Mode`, `*Step`, `*Progress` or `*Lifecycle`: 401 structs and 145 enums.
2. Seven readers, one per area below, judged each lead by reading the type and its writers, and
   looked for what a field count cannot see (statics, atomics, thread-locals, timers).
3. The eleven defects marked **fixed** were then reproduced by tests that failed before the fix
   (three of them, encoder cleanup, the telemetry tail and the trace Start, against the seam the
   fix introduced with the old logic in it, not against the original commit, which has no such
   function to call). One reader's row overstated its reach: the PLAY key's D-pad entry is real in the
   code, but no state a viewer can be in was found that reaches it (its row says why, from a television
   pass on 2026-11-17). One fix was itself wrong on first
   landing (`cancel_page` dropped a deferred quiet read) and was caught in review, which is why
   the remaining rows stay leads and not facts.

Everything else is **read, not run**. "Traced" in the tables means the reader followed the code
path line by line; it does not mean the path was executed, and none of it was checked on a
television. State and writer counts are approximate. A row is a lead to verify before acting on
it, exactly like a ledger line in `ci/allow/`. Each area's blind spots are listed at the end.

Kinds: **a** a flag cluster; **b** a phase enum assigned from many sites; **c** the same in
statics, atomics or thread-locals; **d** a timer, debounce or backoff in loose fields; **e** a
multi-step asynchronous flow in `pending_*` fields. Size is the reader's estimate of making the
lifecycle explicit: S, M or L.

## What to do with this list

**Convert a lifecycle when it has a traced defect or when it is being changed anyway. Never in
bulk.** There are 164 rows. Most have no known misbehaviour, and their behaviour is pinned by
replay fixtures, hostsim rigs and a television no CI has: a rewrite of all of them is a
regression budget nobody can verify. What the rows are for is the moment before an edit.

Every defect below has the same shape, and it is the reason to care about the rest: **a transition
wrote one field of a cluster and not its sibling, or a reader tested a stand-in for the state
instead of the state.** So the first move is always the same and is small:

- Find the fact a compound condition stands in for and give it **one name**: a predicate, or a
  variant of the enum that is already there. (`WatchlistState::unread` replaced two
  `rows.is_empty()` tests that meant "no list has landed".)
- Give the transition **one writer** that reads what is already in force before it writes.
  (`HudPage::ask` is where every install question opens, so it is where "a question is already
  open" is decided.) Then follow the transition's effect to whoever receives it: that fix was
  half a fix until the row was also made inert while `HudStore` is busy, because the store
  refuses the install the page had just confirmed.
- A flag that exempts a failure, a retry or a reset names the state it exempts, not a neighbour
  of it. (Browse's `!state.relist` meant "a re-list holding rows" and also matched a first read
  holding none.)

Order of work: the traced defects that a host test can prove, then the ones that need the
television, then the `S` rows where one enum replaces the flags.

## Traced defects

| where | what goes wrong | status |
|---|---|---|
| `screens/src/hud_settings.rs` (`asking`, `confirming`, `offered`, and the drop rows) | A drop file's row opened its install question while a fetch was in flight; the fetch's offer then replaced the open question under the same title, the viewer's answer went to the offer, and the file was never installed (`HudStore` takes `InstallDrop` only when idle, and refuses in silence). | **fixed**: a drop row is inert while the store is busy, and `ask` never replaces the viewer's own question (an offer that lands behind one waits for the answer). Still open: a listing one tick behind the store can open the question over a busy store, and the store then drops the confirmed install with no notice |
| `data/src/browse/mod.rs` (`SecState.fetch`, `relist`, `total`) | A library-change notice during the first page read set `relist`; if that read then failed, the failure arm's `!state.relist` skipped `Failed`, and the Library grid spun through every retry. | **fixed**: only a re-list that holds rows is exempt |
| `data/src/stores/watchlist.rs` (`Status`, `rows`) | "Never read" was spelled `rows.is_empty()`, so an empty Watchlist returned to `Loading` at every mount and a failed background reread replaced it with `Failed` (which removes the Watchlist row from item menus). | **fixed**: `WatchlistState::unread` |
| `data/src/browse/mod.rs` (page landing) | `retry_cd = 120` is armed before the generation check, so a failed page of a section the viewer has left delays the new section's first page by about 2 s. | open; a design call (`relist_regression_a_failed_later_page_still_retains_notifications` treats the backoff as the server's) |
| `data/src/pms.rs` (`Src.deferred`) | `cancel_page` and `request_page` clear `page`/`quiet` but not `deferred`; the next tick then launches a prompted whole-source read. `deferred` is set only when the fan-out gate refuses, so it needs five or more servers. | **fixed**: `begin_request` clears `deferred` (a read going out answers the gate's refusal); `cancel_page` and `pump_reloads` keep it only for a quiet read the gate deferred (`deferred = quiet`), where clearing it outright stranded `quiet` with nothing to launch it. Four tests in `pms_staged_tests.rs`. Not shown: the real width-4 gate timing on a set with five or more servers |
| `src/app/playback.rs` (`key_play`), `src/app/input.rs` (`Pointer`) | The PLAY key's entry into D-pad mode set `dpad_mode` and `cur_hidden` but not `mot_accum = 0`, `back_cursor = None` or `edge.stop()`, which the arrow-key entry does. Whether a viewer can reach it is not determined: `key_play`'s only production callers are the `Key::Play` and `Key::PlayPause` arms of `run.rs`'s key ladder, which run only when `tree_owns_key` is false (no surface up and no page whose focus comes from the engine), and every production screen's focus does (`FocusSource::Legacy` has no production declarer and `set_focus_source_override` no caller). On the television (2026-11-17) PLAY on the Library grid went to the page's tree, as the code says, and the line never ran. | **fixed** as code: one `Pointer::enter_dpad`, called by both entries (each keeps its own `hide_cursor()` condition); hostsim test `the_play_key_enters_dpad_mode_exactly_as_an_arrow_key_does` drives `key_play` directly. No state a viewer can be in that reaches it was found |
| `src/app/run.rs` (background arm) | `button_down` is reset only inside the playback-park guard and `ok_from_click` never, although the comment says the button-up will not arrive. | open; needs the device fact (is the release lost) |
| `src/app/run.rs` (`retry_refused_foreground`) | The refused-restore retry is not gated on the window being active, unlike its sibling claim paths, and the background arm never clears `foreground_retry`: a Load can be started on a backgrounded window. | open; device outcome not determined |
| `media/src/player/preview.rs` (`settle`) | A preview start deferred behind a running Unload is settled as a refused landing; the trailer engine that launches afterwards has no owner. Needs an Unload slower than one frame after the plan lands. | open; hostsim |
| `media/src/route/decision.rs` (`ENCODER_CLEANUP.checking`) | No drop guard: a panic in the cleanup check leaves `checking` set for the process and defers every ABR trial on that server. | **fixed**: `EncoderCleanupClaim` releases the claim without a verdict when the observation unwinds (`a_panicking_encoder_cleanup_check_releases_its_claim`). The effect on `encoder_cleanup_ready` is read from the ledger, not asserted through that function |
| `play-feed/src/native_executor.rs` (in-place seek) | With `sendSegment == 0` the presentation gate is never re-armed: every seek stalls at least 4 s and reloads, and the "reload per seek" downgrade does not survive the reload. | open; hostsim to reproduce, TV to verify |
| `play-feed/src/feed_resources.rs` (`pending_seek_ready`) | A seek after the producer reached end of file is armed against a thread that has exited: about 3.6 s of spinner, then a reload. | open; hostsim to reproduce, TV to verify |
| `play-feed/src/feed_pump.rs` (ACB bind) | If `plane_send_video_data` returns -1 nothing changes and the envelope is re-sent every tick with no terminal state: audio, no picture, no error. | open; conditional on firmware |
| `net/src/curlio.rs` (`CurlSource::reopen_until`) | Clears `off`, `size` and `poisoned` but not `failure` or `heal_pending`, so a later failure is reported with an earlier body cut's code. | **fixed**: a successful `reopen_until` clears both (a successful `seek` clears only `heal_pending` and leaves `failure` masked behind it, so `body_failure()` answers `None` after either); a refused one still reports the cause it was answering (`a_successful_reopen_until_clears_the_stale_body_failure`). No curl option or FFI declaration changed. The HLS segment reader was not run end to end |
| `net/src/stream.rs` (`HttpStream`) | A peer close where the next chunk-size line belongs is a clean end of stream in the blocking reader and an error in the non-blocking drain. | open; which answer is intended is not written down |
| `telemetry/src/telemetry/mod.rs` (`FLUSHING`, `AGAIN`) | A three-instruction window loses the wakeup for a new consent decision, so a withdrawn category's records wait for an unrelated trigger. | **fixed** at the worker's tail: it flushes again when a flush was asked for or the installed decision is newer than the one it ran with (`worker_tail_reruns`, tested as data). The admission order in `flush_soon` is unchanged and no real-thread interleaving was run |
| `platform/src/playback_trace.rs` (`execute` Start) | The failure exits return `Failed`/`Unavailable` without storing it, after `stop()` moved the status to `Saved` and the file was truncated: Settings shows a saved trace that is empty. | **fixed**: a Start that does not reach `Recording` stores its status; availability and the path are arguments of `start`, so the test reaches the Unavailable exit and the open-failure exit without GStreamer; the header-write exit is stored by the same `start_stored` line and is not driven |
| `screens/src/profiles.rs` (`retire_pad`) | Publishes `pad_open = false, pin_denied = true` for one tick. Only the state digest sees it. | **fixed**: the verdict is cleared before the state is published (`back_on_a_denied_pad_publishes_a_closed_pad_with_no_verdict` asserts the published state) |
| `ui/src/dispatch/press.rs` (`release_at`) | 0 is the "still held" sentinel and `release()` can store it; a key-up at `tick.ms == 0` is dropped. Reachable only at the `u32` wrap. | **fixed**: `release()` stores `now.max(1)`, as `commit_at` and `HeldKey::release` do |

Dead or redundant state found on the way: `Phase::Hidden` in `ui/src/containers/modal.rs` (never
assigned, matched at 14 sites), `Engine.startup_controlled` in `play-feed` (constant `true` in
production), `Engine.video_info_sent` (redundant with `stage == Streaming`), `HitMap.suppressed`
(no production writer).

## Already explicit, not registered

One private state and one step function already; what they lack is a `ci/machines.ini` entry and
the strict writer contract it brings. As reported by the readers, not re-checked:
`src/app/deeplink.rs::DeepLink`, `src/app/reread.rs::Reread`,
`media/src/player/release.rs::NativeRelease`, `playback-contract/src/adaptive.rs::TrialProtocol`,
`play-uri/src/acb_bind.rs::AcbBind`, `play-uri/src/uri_lifecycle.rs::Lifecycle` (a phase with eight
bools beside it, all written inside `step`), `platform/src/tv/plane.rs::Plane`,
`play-feed/src/native_stall.rs::Watch`, `play-feed/src/era_callbacks.rs::Watch`,
`platform/src/storage/state.rs::Status`, `telemetry/src/telemetry/session.rs::Machine`,
`net/src/ws/connection.rs::State`, `ui/src/card_motion.rs::ArtPhase`,
`machine/src/landing.rs::Reservation`, `screens/src/plaintext_question.rs::PlaintextQuestion`.

## Duplicated lifecycles

The same machine written more than once is the cheapest place to make one explicit, because the
second copy is where the rules drift:

- "A setting write with a receipt": seven copies with four rule sets across `screens/src/settings.rs`,
  `settings_under_posters.rs` and `hud_settings.rs`.
- The transport linger/dismiss and scrub gesture: `screens/src/player/` and again in
  `screens/src/detail/trailer.rs::Transport`.
- First-seat provenance: `LibraryScreen`, `RecommendedScreen.initial`, `WatchlistScreen.seat`.
- Dormancy and focus observation, and the page-request flow: `ui/src/cards/shelf.rs` and `grid.rs`.
- "Hold at the start until ready, with a cap": `PopoverMotion`, `RoutePush.warm`, `PageDip`, `PageImage`.
- The ACB bind ladder: explicit in `play-uri/src/acb_bind.rs`, loose flags in `play-feed/src/feed_state.rs`.
- The three-state fetch (`Loading`/`Ready`/`Failed` plus a retry countdown): `browse::SecFetch`,
  `pms::HubState`, `CollectionStatus`, `watchlist::Status`, the search `Source.status`, each with
  its own companion flags.
- `gen`/`done` atomic pairs for "a request is out": five times in `data/src/metadata.rs`.

## The inventory

### Screens (`rust-modules/screens/src/`)

Paths are relative to `rust-modules/screens/src/`. 29 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size |
|---|---|---|---|---|---|---|---|---|
| 1 | Player transport HUD + scrub gesture + FF/REW tap | `player/input.rs:72` (Scrub), `:261` (HudState); `player/mod.rs:141` | a, d | ~8 HUD x ~7 scrub | ~125 (`player/mod.rs` 308-2194, `player/seek_jump.rs` 25-112) | yes | no (two hazards behind a lost key-up) | L |
| 2 | "Setting write with receipt", 7 hand copies | `settings.rs:1128-1145` (auto, trailer, plaintext), `settings.rs:1981` (Language), `settings_under_posters.rs:310-366` and `:820-836`, `hud_settings.rs:194` | e, a | 3-4 each | ~35 | yes | no (divergent rules; one hazard) | M |
| 3 | HUD-package install prompt | `hud_settings.rs:196-198` (`asking`, `offered`), `:147` (`confirming`), `prompt` | a, e | 3 + latch | 7 | yes | YES | S |
| 4 | Detail hero preview / full-trailer + trailer Transport | `detail/mod.rs:206-246`; `detail/trailer.rs:152` | a, c, d | ~6 + transport ~4 x scrub ~4 | ~16 + ~12 Transport methods | yes | no (one cross-instance hazard) | M-L |
| 5 | Profiles PIN pad + selection ack | `profiles.rs:274` (Pad), `:653` (PendingSelection), `:723-726` | a, e | ~7 | ~20 | yes | YES (state digest only) | M |
| 6 | Library first-seat provenance (also Recommended `initial`, Watchlist `seat`) | `library/mod.rs:280-326`; `library/recommended.rs:226`; `watchlist/page.rs` (`seat` field) | a | ~5 | ~35 + 5 + 3 | yes | no | M-L |
| 7 | Detail server-refresh + focus-restore | `detail/mod.rs:193`, `:249-257`; enum at `registry.rs:1443` | b, e | 3 x 3 | ~27 | yes | no | M |
| 8 | Preferences `Txn` + `CONFIRMED` thread_local | `preferences.rs:190-209`, `Io` at `:115` | a, c, e | ~7 | ~20 | yes | no | M |
| 9 | Login report alert (`Sheet` + alert + `alert_for`/`last_resolve`/`support_for`) | `login.rs:686-717` | b, a | 4 | ~12 | yes | no | S-M |
| 10 | Login stalled-wait clock + restart correlation | `login.rs:895-903`, `:931-932` | d, e | 4 | 6 | yes | no | S |
| 11 | Home hero carousel | `home/mod.rs:773-792`, anchor `:749` | a, d | 5 | ~17 | yes | no | M |
| 12 | Home hero/grid page dive (`snap_target`, `free`, `visible_activation`, `reveal_press`, `restore_reveal`, `strip_chosen`, `cta_available`) | `home/mod.rs:759`, `:794-808` | a | ~5 | ~34 | yes | no | M |
| 13 | Player segment offer / auto-skip / Up Next pair | `player/mod.rs:142-144`, `:172`; `player/input.rs:307-311` | a, d | ~5 | ~14 | yes | no | M |
| 14 | Return-hydration family | `collection.rs:322-336`; `person.rs` Page `return_pending`; `search/mod.rs:98`; `home/mod.rs:758-759`; `library/recommended.rs:223` | a, e | 2-3 each | 10 + 6 + 3 + 3 + 3 | yes | no | S each |
| 15 | HudPage address field (`editing`, `address_error`, `list.fetching`) and cursor seat (`provisional`, `pending_select_focus`, `visible`, `suspended`) | `hud_settings.rs:151-160`, `:202` | a | 4 / 3 | 7 / 9 | yes | no | S |
| 16 | Detail ask-once-and-retry trio (`StripSeat`, `EpWant`, `RelatedSeek`) | `detail/mod.rs:128`, `:147`, `:164` | d, e | 3 each | not counted | yes | no | S each |
| 17 | Settle debounces: Detail season (`pending_season` + `season_settle`), Filmography preview (`pv_want`/`preview`/`pv_still`) | `detail/mod.rs:196-203`; `filmography.rs:205-213` | d | 2-3 | 6 / 4 | yes | no | S |
| 18 | Detail teardown latch (`teardown_cleared` + `withdrawn`) | `detail/mod.rs:269-278`; writers `:2402`, `:2429` | a | 3 | 2 | indirect | no | S |
| 19 | Home Backdrop art hand-over and key-hold dwell (`keyed`, `bound_art`, `held`, `pending_art`, `repeat_ms`, `dwell`) + `HeroTint` | `home/mod.rs:315-343`, `:251` | a, d | ~4 | not counted | yes | no | S-M |
| 20 | Onboard armed press + self-answer latch (`armed_kind`, `self_answered`, `band`) | `onboard.rs:91-115` | e, a | 2 + 2 | 5 | yes | no | S |
| 21 | Search keyboard and recents clear (`editing`, `recent_clear_pending`, `mounted`, `restored`) | `search/mod.rs:77-98` | a, e | 2 x 2 | 8 | yes | no | S |
| 22 | Settings family push/pop (`open`, `leaving`, `leaving_page`) | `settings.rs:92-101`; writers `:248-249`, `:438-444`, `:792-793` | a | 3 | 8 | yes | no | S |
| 23 | Library grid `Relist` enum | `library/parts.rs:162`; writers `:252`, `:265`, `:347`, `:367` | b | 3 | 4 | yes | no | S |
| 24 | Library page visibility and Collections round trip (`live`, `buried`, `collections_asked`, `type_before_collections`) | `library/mod.rs:267-270`, `:312-317` | a | 3 | 9 | indirect | no | S |
| 25 | Watchlist wash hold (`ground_catching_up` + `removal_owed`) | `watchlist/page.rs:776-785` | a | 3 | 4 | cosmetic | no | S |
| 26 | Player `BackMotion` (nested `Option<Option<..>>`) | `player/mod.rs:119-123` | a | 3 | 5 | yes | no | S |
| 27 | Player failure edge (`failure_up`, `failure_sel`, `repair_alert`) | `player/mod.rs:190-198`; writers `:842-854` | a | 2 | 4 | yes | no | S |
| 28 | Login QR texture hand-off (`qr_png_pending`, `qr_tex`, `qr_tex_gen`, `qr_px`) | `login.rs:907-917`; writers `:1021`, `:1928-1959` | e | 3 | 6 | yes | no | S |
| 29 | `AppMounter` mount seeds, written by the app at `src/app/bridge.rs:1191-1497` | `registry.rs:2223-2258`; consumed `:2295`, `:2331`, `:2418-2419` | e | 2 each | 6 writers, 4 takes | indirect | no | S |

### UI framework (`ui/`, `appkit/`, `gfx/`, `machine/`)

Paths are relative to `rust-modules/`. 33 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size |
|---|---|---|---|---|---|---|---|---|
| 1 | Popover host-snapshot lifecycle (statics) | ui/src/popover.rs:1064-1700, counters :99/:178/:183 | c | ~12 | ~33 internal + 13 counter sites; ~33 external calls in 11 files | yes: stale or late menu ground, fps on open/close | none proven | L |
| 2 | Press + Arm + dip_owner | ui/src/dispatch/press.rs:88, ui/src/input.rs:43 | a+b+d | ~8 | 6 methods in press.rs, 3 in input.rs, 7 sites in dispatch.rs | yes: tap / hold / activation | yes, latent: a release at tick.ms==0 was dropped (fixed, see Traced defects) | M |
| 3 | Modal surface lifecycle (Surface.phase + PopoverMotion hold flags + Popover bools + global counters) | ui/src/containers/modal.rs:34,95,260; ui/src/popover.rs:528 | a+b+c | 4 (1 dead) x 3 hold sub-states | modal.rs 4, dispatch.rs 1, popover.rs 13, app bridge 5 | yes: menu open/close, input owner, host freeze | dead state `Phase::Hidden` (0 writers, 14 readers); no misbehaviour | M-L |
| 4 | Shelf/Grid dormancy and focus observation (duplicated) | ui/src/cards/shelf.rs:52; ui/src/cards/grid.rs:153 | a | ~5 | shelf 13 sites in 3 fns; grid ~10 in 3 fns | yes: focus pop on entry / BACK | none proven | M |
| 5 | Shelf/Grid page-request flow | ui/src/cards/shelf.rs:502-643; ui/src/cards/grid.rs:600-634; Retry at ui/src/cards/mod.rs:474 | e | ~5 | 7 fns (shelf), 1 fn / 8 sites (grid) | yes: pagination, skeleton cells | none proven | M |
| 6 | Present gate | machine/src/present.rs:85 | a | 4 | 9 methods, all fields private | indirect: a missed or extra frame | none proven | S-M |
| 7 | TexCache per-key residency (membership in 3 collections) | ui/src/tex.rs:461 | a/e | 5 | accept, prepare, evict_for, free, drain_all, resolve | yes: poster placeholder / re-fetch | unproven: stale `unresident` notice (see section 2b) | M |
| 8 | Backdrop source Entry | gfx/src/gfx/backdrop.rs:749 | a | ~5 | ~20 sites in 8 fns (lines 830-1615) | yes: glass correctness and cost | none proven | L |
| 9 | PageDip phase + floor flags | ui/src/containers/transition.rs:161 | b+d | 4 x 4 | 10 sites in 5 methods | yes: route dip | latent: `floor_spent` not cleared on In->Idle; unreachable via its single caller | S |
| 10 | PageImage + PAGE_IMAGE_HELD + dispatcher page_debt / page_image_alpha / page_quiescent | ui/src/containers/transition.rs:411,424; ui/src/dispatch.rs:1385,1660,1664 | a/c | 5 | 4 fns + 3 dispatcher sites | yes: held page image on a push | none proven | S-M |
| 11 | Fenced GPU readback pipelines (snapshot fence, GroundProbe x2, field chain, capture chain) | gfx/src/gfx.rs:2740-2811, 6193-6330, 7760-7791, 7998-8300 | c/e | ~4 each | 4 statics (snapshot), 6 statics (field), 5 fields (probe) | yes: modal open hold, underlay field | none proven | L |
| 12 | GL feature lazy-init / latch-off family (BLURST+BLUR_OFF+BLUR_VALID+BLUR_DIRECT_OFF+BLUR_IN_PASS, CAPST+CAP_LATCHED_OFF, FIELDST+FIELD_OFF, FrameCache valid/checked/off/fbo_off, DISSOLVE_TRIED) | gfx/src/gfx.rs:4760-4765, 5871, 5887, 7784-7785, 7998-8002, 3583, 885 | c | 3 each | BLUR_OFF 4, FIELD_OFF 3, CAP_LATCHED_OFF 2, FrameCache.valid 8 | degraded visuals only | unproven: FrameCache `valid` survives a texture replacement (section 2b) | M-L |
| 13 | HeldKey (hold-to-scrub key) | appkit/src/held_key.rs:30 | d | 5 | 7 methods; `dir`/`engaged`/`since` are pub | yes: scrubbing | none proven | S |
| 14 | Scrub-preview Motion | appkit/src/scrub_preview.rs:394 | a/d | ~6 | 2 fns (`step_live` :452, `observe` :525) | yes | none proven | S |
| 15 | ScrubPreview still loader | appkit/src/scrub_preview.rs:104 | e | 5 | `step` :142, `release` :195 | yes | none proven | S |
| 16 | cards::Landing play flags | ui/src/cards/landing.rs:178 | a | ~6 | ~20 sites in 5 fns | yes: landing animation | none proven | S-M |
| 17 | HoldHint dwell + LEARNED/SHOWN/CHANGED thread-locals | ui/src/hold_hint.rs:372, 224-284 | d+c | 5 | one `step` (:422) + 4 global mutators | yes | none proven | S |
| 18 | HudMotion sections (wants / appear / page / held / dismissed, play_at / pause_seen) | appkit/src/player_hud/motion.rs:137 | a | 4 per section | follow :241, cut_sections :283, hold_focus :317, dismissed_on :327 (rest of file not read) | yes | none proven | M |
| 19 | OfferTimer (auto-skip countdown) | appkit/src/skip_policy.rs:53 | d/e | 4 | 5 methods | yes | none proven (Deferred re-fire is tested) | S |
| 20 | Up-next Countdown | appkit/src/up_next.rs:72 | d | 3-4 | tick :112, cancel :157, reset | yes | none proven | S |
| 21 | Keyboard ownership (`keyboard` + `keyboard_owner`) | ui/src/input.rs:57-60 | a | 3 | dispatch.rs:1140, 2262, 2298 | yes: presses refused while up | unproven: no cleanup when the owner unmounts (dispatch.rs:2940-2942 clears only the press) | S |
| 22 | Dispatcher pending_app continuation | ui/src/dispatch.rs:412-422, 430 | e | 4 | :2086-2088, :2109, :2188 | no | none proven | S |
| 23 | Dispatcher frame flags + ambient mirrors (`prepared`, `pending_back`, `lifted_last`+LIFT_OWNS, VEIL) | ui/src/dispatch.rs:1010/1433, 2415/2765, 1632-1633, 1198; ui/src/popover.rs:66; modal.rs:327 | a/c | 2 each | LIFT_OWNS set at screen.rs:1022, grid.rs:1325, cleared at dispatch.rs:1633 | indirect | none proven | S |
| 24 | TrackMenuState selection mirror + one-shots | appkit/src/track_menu.rs:475 | a/e | ~4 | 949-951, 1027-1028, 1257, 1331-1332, 1404-1405, 1669-1671, 2174-2179, 2307 | yes | none proven | L |
| 25 | "Hold at the start until ready, with a cap", written four times (PopoverMotion hold/holding/held_ms/held_ticks; RoutePush.warm; PageDip floor_*; PageImage.settle_since) | modal.rs:95-124; ui/src/route_screen.rs:403-437, 610-631; transition.rs:161, 411 | d | 3-4 each | 2-4 each | yes: first frame of a surface or push | none proven | S each |
| 26 | Xfade | ui/src/xfade.rs:76 | b | 4 | 7 sites in 5 methods | yes | none proven | S |
| 27 | ModalUnderlay latch (`held` + `pending` ticket) | ui/src/containers/modal.rs:538 | e | 3 | sync :586-612, reset :614 | yes: dim field | none proven | S |
| 28 | HitMap pointer / D-pad mode | ui/src/hit.rs:64 | a | 2 | :118-119, :125-129 | yes: hover | `suppressed` has 0 production writers (documented at hit.rs:13-15) | S |
| 29 | cards::Stack restore / wheeled | ui/src/cards/stack.rs:278 | a/e | 3 | :516, :702, :748-785 | yes | none proven (body not read) | S |
| 30 | PanelMotion placed / slide | ui/src/panel_motion.rs:109 | a | 3 | :250, :284, :351, :382 | yes | none proven | S |
| 31 | scroll_chrome::Handoff (tucked + two fades) | ui/src/scroll_chrome.rs:103 | a | 4 | not counted (struct only read) | yes | not examined | S |
| 32 | watchlist_disc::Disc (membership + pressed) | appkit/src/watchlist_disc.rs:92 | a | 3 | press :115, set :128, reset :142 | yes | none proven | S |
| 33 | landgate Gate (`armed` atomic shadows `mode != Off`) | machine/src/landgate.rs:190, 276 | b/c | 4 | arm_* at :297-375 | no (record / replay / dump instrument) | none proven | S |

### Data stores (`rust-modules/data/src/`)

Paths are relative to `rust-modules/`. 21 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size |
|---|---|---|---|---|---|---|---|---|
| 1 | Watchlist list read, stream and optimistic write (`WatchlistState` + `WatchlistStore`) | data/src/stores/watchlist.rs:147, :246 | a, e, d | ~11 list x 3 write | ~60 in 16 fns | yes | YES | L |
| 2 | Library listing fetch and re-list (`SecState.fetch/total/relist*` + `BrowseState.gen/requery_gen/retry_cd`) | data/src/browse/mod.rs:629, :979 | a, b, d | ~8 | ~22 in 7 fns | yes | YES (2) | M |
| 3 | Home per-source fetch (`pms::Src` + `PmsState.deck_ask`, fan-out gate) | data/src/pms.rs:2469, :372 | a, b, d, e | ~12 | ~95 in ~20 fns | yes | YES (narrow reach) | L |
| 4 | Metadata request lifecycles held in atomics (`MetadataAdapter`: detail, season, alt-sources, `EpPages`, `RelPages`) | data/src/metadata.rs:181, :5152, :5488, :4498 | c, e | 3-4 each x 5 | ~35 | yes | documented hole only | L |
| 5 | Section shelves (`SecHubs` + `BrowseState.hubs_flight`) | data/src/browse/section_hubs.rs:244 | a, d, e | ~9 list x 4 row | ~30 in 14 fns | yes | none proven | M |
| 6 | Shared single-flight claim `stores::Fetch<M>` | data/src/stores/mod.rs:1114 | c | 4 | 8 methods, 10 owners | indirect | self-documented soft hole | S-M |
| 7 | Person page (`person::Src` resolve/media/roles chain + `Person.landed/profiled/profile_tried/credited` + `Backoffs`) | data/src/person.rs:131, :191, :353 | a, e, d | 4 per source x 3 x 2 | ~20 in 6 fns | yes | hazard: no terminal failure state | M |
| 8 | Read-back queue by set membership (`ViewReconcileStore`: pending/pending_parents/flights/dirty/candidates/quiet/landed) | data/src/stores/viewreconcile.rs:228 | e, d | ~6 per key | 9 mutating fns | indirect | hazard, none proven visible | M |
| 9 | Notify connection (`Conn.state` + link/gap_owed/resynced_unstable/attempts) | data/src/stores/notify.rs:99 | b, d | 5 + 2 flags | 22 in 9 fns | indirect | none proven | S |
| 10 | Source discovery (`BrowseSource.sections_done/counts_done/retry_cd/state` + `src_flight` + `src_fetching`) | data/src/browse/mod.rs:127, :998, :1078 | a, d, e | ~6 | ~22 in 9 fns | yes | none proven | S-M |
| 11 | Search debounce, per-source attempt, tail lanes, slide (`armed/settle_us`, `Source.status/retry_cd/skipped`, `tail::Lane`, `Win.pending`) | data/src/search.rs:833, :743, :501; data/src/search/tail.rs:66 | b, d, e | 3 per source + 2 + lane | ~30 in 10 fns | yes | none proven | M |
| 12 | Collection page (`CollectionStatus` + header_ready/more/read/budget/`id.rk.is_empty()` + retry_cd) | data/src/collection.rs:63, :219 | a, b, d | ~7 | ~18 in 6 fns | yes | none proven | S-M |
| 13 | Letters and bands rail, twin keyed caches with backoff (`letters_for/kept/backoff/gen`, `bands/kept/backoff`, `viewing_gen`) | data/src/browse/mod.rs:655-677; data/src/browse/letters.rs:182-395 | a, d | ~4, twice | ~22 | yes (rail) | none proven | S-M |
| 14 | View-state write queue (`queue/sent/retry_cd/want_hubs/want_reread/hold_refresh`) | data/src/viewstate.rs:604 | e, d | 3 + 2 owed flags | 7 in 5 fns | indirect | none proven | S |
| 15 | Recent searches in process statics (`STORE`, `PENDING`, `SearchAdapter.recents_refused`) | data/src/search/recents.rs:51, :222 | c, e | ~3 | 6 | minor | none proven | S |
| 16 | Catalog page (`hex/load/auth/reopen/req/owed/retry`) | data/src/stores/catalog.rs:158 | a, d | ~5 | ~15 in 4 fns | yes | none proven | S |
| 17 | Pin write optimistic confirm (`pending_pins`, `recorded`, `Receipt{result,observed}`) | data/src/browse/mod.rs:1050, :2334; data/src/browse/preferences_job.rs:65 | e | 4 | 5 | minor | none proven | S |
| 18 | Subtitle search (`SearchStatus` + `DownloadPhase` + one `retry_cd` shared by retry and poll) | data/src/subsearch.rs:74, :108, :267 | b, d | 4 x 6 | ~12 | yes | none proven | S |
| 19 | Worker-side port phases (`StreamPort` `Claim{phase,cancelled}`; `ResolvePort`/`WritePort` `AtomicU8` 0/1/2; notify `Inbox` latches) | data/src/stores/watchlist_stream_port.rs:46; data/src/stores/watchlist_job.rs:48, :155; data/src/stores/notify_job.rs:15 | b, c | 3 each | 4-5 each | no | none proven | S |
| 20 | Notify pass cursor (`NotifyStore.active` + `NotifyPass.pending` + `NotifyTurn.launch/settled`, enforced by asserts) | data/src/stores/notify.rs:60-67, :306-394 | a | 4 | 6 | no | none proven | S |
| 21 | Replay tape thread-local (`Tape.active/replay/failure`) | data/src/stores/tape.rs:14, :27 | c | 3 + failure latch | ~8 | no (controlled runs) | none proven | S |

### Playback core (`media/`, `playback-contract/`)

Paths are relative to `rust-modules/`. 19 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size |
|---|---|---|---|---|---|---|---|---|
| 1 | Route reducer `PLAYER_CONTROL` (`ControlPhase` + flight markers + pending intents + Original trial) | media/src/route/decision.rs:2522, 2573, 2695 | b+a+c | 16 phases x 5 flight kinds | 33 `.phase =` in ~22 fns; 6 flight writes + 9 `clear_flight()` | yes (spinner wedge, wrong route) | none proven | L |
| 2 | Hero-preview lifecycle (`preview::Machine` + `ps.preview` / `resolved_as_preview` + `PlayerAdapter.preview_admission`) | media/src/player/preview.rs:86, 146, 505 | b+c | 7 | 14 mutator methods; `clear_preview` from 4 sites | yes | YES (deferred start settled as Refused) + in-code KNOWN GAP | M |
| 3 | Session terminal-cause cluster + `player::state()` ladder | media/src/route/decision.rs:148-229; media/src/player/mod.rs:844 | a+c | ~9 causes | ~30 in ~14 fns, 4 files | yes (error screen vs spinner) | none proven | M |
| 4 | Resolve/landing mailbox (`PLAY_GEN`, `PLAY_BUSY`, `PLAY_SLOT`, `PLAY_RESUME`, `ACTIVE_TRACE_GENERATION`) | media/src/route/decision.rs:10082-10175 | c+e | 6 | ~20 in 5 fns + worker closure | yes (Resolving spinner) | hazard (guard reads the wrong session), no effect proven | M |
| 5 | Telemetry funnel attempt (`ATTEMPT`, `SAW_*`, `LAST`, `REBUFFER_*`, `TRACE`, `LEDGER`) | media/src/player/report.rs:75-184 | c+d | 7 + rebuffer window | 28 in 10 fns | no (dashboard numbers) | none proven | M |
| 6 | Pause ownership (`claim_hold::HOLD`/`SERIAL` + `ps.reconfigure_pause_goal` + `Startup.initial.paused`, over readout `paused`/`seek_preroll`) | media/src/player/claim_hold.rs:28-39; media/src/player/transport.rs:57 | a+c | 5 owners | ~10 | yes (stuck paused, replayed seconds) | none proven | M |
| 7 | `backend_startup::Startup` (`Stage` + `issuing_initial_play` + `initial_play_command`) | media/src/player/backend_startup.rs:7-33 | b | 10 | 22 `stage =` in 4 methods | yes (start never Plays) | none proven | S |
| 8 | PMS encoder cleanup ledger (`ENCODER_CLEANUP`: `checking`, `stop_needed`, `physical_absent`) | media/src/route/decision.rs:3971-4085 | a+c | 5 per entry | 6 | indirect (Auto stops upshifting) | conditional: `checking` latched if the worker panicked (fixed, see Traced defects) | S |
| 9 | URI attempt lifecycle in `PlayerAdapter` (`uri_route_attempt`, `uri_route_settled`, `uri_stop_requested`, `uri_finalized`, `uri_recovery.released`, pending/refused picks) | media/src/player/adapter.rs:487-497 | a+e | ~7 | ~30 in 9 fns | only with the experimental URI opt-in | none proven | M |
| 10 | Stop/scrobble pipeline (`ScrobbleJoin`, `TIMELINE_STOP_FENCE`, `STOP_MAIL`/`STOPS_OWED`/`STOP_EPOCH`, `PlayerControl.timeline`, `ps.finished`) | media/src/route/decision.rs:8106-8600, 13825 | c+e | ~5 per barrier | ~20 | yes (watched state, Up Next chain) | none proven | M |
| 11 | Sidecar subtitle fetch (`sidecar::STATE`) | media/src/player/sidecar.rs:128-145 | c+e+d | 4 + restyle flag, 6 s failure timer | ~14 in 6 fns | yes | none proven | S |
| 12 | ABR transaction inside `Controller` (`pending`, `pending_reserve_policy`, `pending_exploration_budget_ms`, `rollback_rung`, `recovery_target`, `common_budget_frontier_ms`) | media/src/abr/controller.rs:318 | a | 5 | ~35 (`observe_inner`, `commit`, `reject`, `set_executed_exploration_budget`) | yes (rung switches) | none proven | M |
| 13 | Backend offer admission (`ps.backend_policy`, `planned_feed_offer`, `planned_feed_purpose`, `admitted_feed_offer` + `BackendPolicy.crossed`/`rollback_used`/`last_offer`) | media/src/route/decision.rs:151-157, 1392-1557; media/src/route/backend_policy.rs:70 | a | ~5 | ~16 | indirect | none proven | S-M |
| 14 | Track-selection knownness (`selection_known`, `carried_selection`, `sub_mode`, `cur_memory_note`, `write_back`, `side_subs_refused`) | media/src/route/decision.rs:404-455 | a | ~4 | ~15 | yes (lost pick on retry) | none proven (shallow read) | M |
| 15 | `backend_metrics::Physical` | media/src/player/backend_metrics.rs:75 | a | ~6 | ~25 in 5 fns | no (local metrics) | none proven; fragile copy-then-overwrite | S |
| 16 | ASS render `Mailbox` (`pending`, `in_flight`, `withdrawn`, `requested`, `answered`, `plan`, `run`) | media/src/player/ass/mailbox.rs:180 | a+e | not reconstructed | not counted | yes (subtitle frames) | not traced | M |
| 17 | Audio-switch caption (`SHARED.audio_switch` + `audio_pick_ms`, 30 s window) | media/src/player/shared_resources.rs:48; media/src/player/mod.rs:324, 2765 | c+d | 3 | 4 | caption only | none proven | S |
| 18 | Auto/Original watch facts in session (`cur_auto_original_watched`, `auto_bootstrap_rung`, `auto_switches`, `auto_last_switch`) | media/src/route/decision.rs:322-352 | a | 2-3 | 7 for the watch flag (2378, 5248, 5293, 6394, 9355, 11694, install) | indirect | not traced | S |
| 19 | `ass_source::Store` reopen (`generation`, `media_key`, `awaiting_reopen`) | media/src/player/ass_source.rs:180 | a | 3 | 4 | minor | none proven | S |

### Playback backends and the C seam (`play-feed/`, `play-uri/`, `media-codecs/`, `platform/src/tv/`, `src/*.c`)

Paths are relative to `rust-modules/`; `src/*.c` is the repository's own `src/`. 19 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size | needs the TV |
|---|---|---|---|---|---|---|---|---|---|
| 1 | Feed in-place seek / rebase / prime / landing | play-feed/src/feed_state.rs:120-166 (Engine), shared.rs:293-312, seek.rs:72-77 | a,c,d,e | ~9 | ~45 (4 files, 2 threads) | yes: seek spinner, stalls, reloads | YES (2) | L | yes |
| 2 | Feed clock-hold ownership (HlsClockState + mirrors + `prime_play`) | play-feed/src/shared.rs:119-156, 426-463 | b,c | 5 phases x 5 holds x 2 flags | 24 phase writes in 17 methods; mirrors from 3 threads | yes: pause/resume, rebuffer, A/V drift | none proven | L | yes |
| 3 | Feed startup-Play handshake | play-feed/src/feed_state.rs:166-178, feed_driver.rs:19-31 | a,e | ~7 | 17 | yes: first Play, stuck Buffering | dead state proven; one hazard | M | host mostly; TV for refusal timing |
| 4 | Feed Stage / ACB bind ladder | play-feed/src/feed_state.rs:111-119 | b | 4 (+2 side) | 7 | yes: black plane | redundant flag proven; unbounded retry proven conditional on rv=-1 | S-M | yes |
| 5 | UriBackend attempt flag bag | play-uri/src/backend.rs:181-231 | a,e | ~8 | ~45 | yes: URI start/stop | hazard only | M | host for logic; TV for service ordering |
| 6 | Native object lifecycle: C seam statics + Rust release slots | src/starfish.c:141-166, 287-302; play-feed/src/feed_resources.rs:43-56; native_executor.rs:735-746; control.rs:223-262 | c,e | ~7 (C) + 5 slots | ~20 C, ~15 Rust, 3 threads | yes: playback refused until relaunch | hazard only | L | yes (no host test runs the C) |
| 7 | Transport paused-seek preroll | play-feed/src/shared.rs:1856-1882 | a,c | 5 | 12 | yes: paused scrub, mute | none proven (illegal combination representable) | M | host mostly |
| 8 | Feed native Load handshake (issue #74) | play-feed/src/feed_pump.rs:63-94, 704-832; native_gate.rs:15-30; shared.rs:273, 350-384; src/starfish.c:300 | c,d,e | ~7 | ~14, 3 threads | yes: Connecting forever / LoadTimeout | none proven | M | yes |
| 9 | uri_lifecycle::Lifecycle ACB release tail (8 bools beside `phase`) | play-uri/src/uri_lifecycle.rs:93-109 | a (inside an explicit reducer) | 5 phases + 5-step tail | 30, all inside `step` | yes: URI release / quarantine | none proven (latent stale flags) | S | no (pure, 12 tests) |
| 10 | Side subtitle reader statics | play-feed/src/side_reader.rs:106-128 | c | ~5 | ~20, 3 threads | yes: subtitles, burn fallback | none proven | S-M | host for logic |
| 11 | Feed failure latches | play-feed/src/shared.rs:330-356, 533, 551 | c | cause set | ~12, 3 threads | yes: error wording | none proven | S | no |
| 12 | dv_start::Guard | play-feed/src/dv_start.rs:191-200 | a | 6 | 6 methods | yes: DV landing reload | none proven | S | no (11 tests) |
| 13 | HlsRoundPort trial scratch (10 Options shadowing the adaptive reducer) | play-feed/src/ff.rs:4552-4574 | a,e | ~7 | ~15 | indirect (ABR) | none proven (fresh per round, ff.rs:5516) | M | no |
| 14 | ReserveDeadlineState / HealBudget / TransportWatchdog | play-feed/src/ff.rs:706-727, 937, 1162 | d | 4 clocks + expired | 5 | indirect | none proven | S | no |
| 15 | PreAudioHeadroom (`granted`/`released`) | play-feed/src/ff.rs:5670-5677 | a | 3 | 5 methods | indirect | none proven | S | no (8 tests) |
| 16 | AcquisitionRuntime `phase` via public `enter()` | play-feed/src/ff_acquisition.rs:205-233 | b | 4 | 4 (ff.rs:3886, 4143, 4211; ff_acquisition.rs:231) | no | none proven | S | no |
| 17 | NativeGate phase (contained, private) | play-feed/src/native_gate.rs:41-98 | b (mild) | 5 + 3 gate | 9 | indirect | none proven | S | no |
| 18 | CloseSignal (AtomicU8 0..3) | platform/src/tv/uri.rs:22-38 | c | 4 | 2 | no | none proven | S | no |
| 19 | crashtrace end-of-life latches (`crashing`, `freeze_busy`, `exiting`) | src/crashtrace.c:145, 432-433, 531 | c | 4 | 5 | no | none proven | do not convert (signal context) | n/a |

### Application crate (`rust-modules/src/`)

Paths are relative to `rust-modules/src/` unless they start with `media/` or `ui/`. 21 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size |
|---|---|---|---|---|---|---|---|---|
| 1 | Parked-playback restore claim (`App.decoder_parked` + `App.foreground_retry` + `App.repause_at` + `WindowActivity.active`, layered on the registered `ForegroundLifecycle`) | app/mod.rs:302-309, app/run.rs:1243-1283, 1876-2008 | a, e | 6 | 9 field writes + 5 claim call sites | yes (film restore after app switch / screensaver) | YES: retry claim not gated on foreground | M |
| 2 | Pointer mode / cursor visibility / button (`Pointer`, `SHOWN_CURSOR`, `App.down_sym`) | app/input.rs:31, app/boot.rs:1057 (`SHOWN_CURSOR` has since moved to `system.rs`, behind `Shell`) | a, c | 2x4x3 | about 19 | yes (cursor, OK-as-click, edge glide) | YES: two sibling-flag omissions (the PLAY entry's is not shown reachable; see Traced defects) | M |
| 3 | Poster slot lifecycle (`Pslot.state` as a `c_int` const + 9 sibling fields, in `static STORE`) | app/adapters/poster.rs:115-130, 156 | b, c, d | 8 | 11 state writes in 7 fns, 2 threads | yes (every poster) | none proven | L |
| 4 | Player-exit to hubs refetch to Home reveal hold (`App.refresh_hubs` + `ReturnHold` + `PageDip` floor) | app/mod.rs:333-335, app/playback.rs:1028, app/run.rs:2258-2288 | a, d, e | 5 | 8 | yes (Continue Watching after playback) | none proven | S-M |
| 5 | Preview to feature-play hand-off (`thread_local HELD_FEATURE`, `PREVIEW_HOST`, plus media `preview::occupies/abandoning`) | app/content.rs:120-173, 203-333 | c, e | 4 | 5 | yes (Play over an auto-playing trailer) | none proven (no cancel path, see detail) | S |
| 6 | `MenuPlayAwait` (show/season Play continuation and the deeplink landing) | app/input.rs:350-674 | b, d, e | 4 (one unnamed) | 11 | yes | none proven | S-M |
| 7 | `HudDriver` compile pipeline (`reading`/`pending{stage}`/`compiled`/`unfit`/`read_failed`/`dirty`/`started`/`language`/`shown`) | app/adapters/huds.rs:604, 859-1092 | a, b | about 7 | about 22 in 7 methods | yes (which HUD draws) | none proven | M |
| 8 | `SessionAdapter` persistence FIFO (`PendingCommit`/`PendingErase` with `ticket`+`retry_at`+`incomplete`+`kept`; `cache_refresh` slot + `Bridge.cache_refresh_begun`; `live_completion`) | app/adapters/session.rs:269, 290, 613-668, 846-925; app/bridge.rs:588, 3641 | d, e | 4 per job | about 14 | yes (sign-out, delete-all, credential persist) | none proven | M |
| 9 | System keyboard latch (`static mut STARTED`, `PENDING`), partnered with SearchScreen's editing flag | textinput.rs:166-175, 240-333 | c | 2 (x screen flag x real panel) | 3 fns | yes (search keyboard) | none proven here; past desyncs documented at textinput.rs:290-321 and run.rs:1155-1171 | S |
| 10 | Collections count probe (`Entry{answer,at,settled,probing}` in `static CACHE`; `Walk{queue,running,now}`; `WARMED`) | app/collections_probe.rs:51, 63, 244, 271-314 | c, d | 5 | 3 fns + 3 Walk methods | yes (Library header count) | none proven | S |
| 11 | Marker access read (`AccessState{profile,answer,settled,awaiting,at,scope}` + `MarkerAccessRunner.pending`; `awaiting` and `pending` both mean in-flight) | app/preferences/marker_access.rs:38, 115 | a, d | 4 | 4 methods + 3 `pending` writes | yes (skip intro/credits rows) | none proven | S |
| 12 | Person-open slot (`Bridge.pending_person_open` + `person_owner`) | app/bridge.rs:621-622, 1973-2036 | a, e | 3 | 5 | yes (cast/crew page) | none proven | S |
| 13 | Lab upload (`INFLIGHT` + `TOAST.view.phase` u8 consts + `until_ms`) | lab/upload.rs:29-48, 76-113 | c, d | 4 | 5 | lab builds only | none proven; no drop guard, so a panic in `send` leaves INFLIGHT set and the toast on SENDING (upload.rs:105-109) | S |
| 14 | `EdgeScroll` dwell/ramp glide (5 Options) | app/input.rs:169-300 | d | 3 per axis | 4 methods, private | yes | none proven | S |
| 15 | Watchlist `Feed.reread_at` debounce + `removed` one-shot (the rest is a last-sent memo) | app/watchlist.rs:58, 156-160, 303 | d | 2 | 3 | yes | none proven | S |
| 16 | Bridge pending slots `trace_save`, `session_ready` | app/bridge.rs:594, 604, 3464-3491, 3618-3626 | e | 2 each | 2-3 each | Settings trace export; sign-in ready | none proven | S |
| 17 | `App.running` + `Recplay.failure` + shutdown sequence | app/mod.rs:261, 552-567; run.rs:224, 270, 872, 884, 1095, 1138, 1577, 3491 | a | 2 | 8 | exit | none proven | S |
| 18 | `Recplay` (explicit Off/Recording/Replaying enum; `Replay` has 4 cursors + `started` + sticky `failure`) | app/recorder.rs:362-394, 650 | b | 3 + cursors | many, in one impl | dev/CI instrument | none proven | M (low value) |
| 19 | `shot.rs` settle capture (`SETTLED`/`SETTLE_FIRED`/`ON_DEMAND`/`FIRST_TICK`) | shot.rs:100-141 | c, d | 3 | 4 | dev/sim | none proven | S |
| 20 | `capture.rs` client slots (`ENABLED`/`QUIT`/`LISTEN_FD`/`FD_JPEG`/`FD_MPEG`/`MPEG_OFF`, 3 threads) | capture.rs:114-125, 163-432, 729 | c | about 4 per slot | 10 | dev stream | not analysed in depth | M |
| 21 | Dev scenario arms: `Scenarios` (about 25 `*_tried`/`*_last`/`*_step`/`*_down` micro-scripts), `BenchClock` (`phase` + separate `done`), `poster_gate::Scene` (`checked/mode/stage/started/finished`), `ContentBoot` (`waiting_person/ready_seen`), `Subtiming`, `ScreenshotArms`, framedump `Dump.done` | dev/scenarios.rs:90, 695; dev/scenarios/bench.rs:144, 270-312; dev/scenarios/poster_gate.rs:158; dev/framedump.rs:588 | a, b, d | 2-4 each | many | dev triggers only | not analysed per arm | L in total (low value) |

### Session, Plex, network, platform, telemetry, base

Paths are relative to `rust-modules/`. 22 lifecycles.

| # | lifecycle | where | kind | states | writer sites | user-visible | defect traced | size |
|---|---|---|---|---|---|---|---|---|
| 1 | `CurlSource` https range transfer | net/src/curlio.rs:866 | a,e | ~8 | ~45 assignments in 11 methods | yes: https playback, seek, HLS segment reopen, error attribution | yes: stale `failure`/`heal_pending` after `reopen_until` (fixed, see Traced defects) | L |
| 2 | `HttpStream` plaintext HTTP/1.1 body decoder | net/src/stream.rs:31 | a,e | ~9 | 15 flag assignments in 6 fns, plus `fd`/`interrupted` atomics from 2 threads | yes: plaintext LAN playback and every `plex::http` call | yes: close at a chunk boundary is EOF in one reader, error in the other | L |
| 3 | Server registry `Slot` + `CURRENT`/`FLOOR`/`COUNT`/gens | plex/src/plex/servers.rs:372, 420-457 | c | ~6 per slot × 6 probe codes | ~45 atomic stores in 14 fns | yes: which server/token every request uses | none proven | L |
| 4 | Plaintext consent/grant table | plex/src/plex/grant.rs:97-128 | c,d | ~7 per machine | 12 writer fns over 12 statics | yes: "Unencrypted connections" consent, token over http | none proven | M-L |
| 5 | Telemetry flush scheduler `FLUSHING`/`AGAIN`/`RETRY_SCHEDULED` | telemetry/src/telemetry/mod.rs:254-263, 273-321 | c,d | 4 (+retry bit) | 9 stores/swaps, 3 threads | yes: "Sending report…", withdrawal purge | yes: narrow lost-wakeup race (the wakeup is no longer lost, see Traced defects; the admission window is unchanged) | S |
| 6 | Playback trace recorder `State`/`Status` | platform/src/playback_trace.rs:39, 126-150 | b,a | 7 enum × capture/registered/gen/`ACTIVE` | status assigned at ~9 sites in 7 fns, plus gst callback thread | yes: Settings diagnostics row | yes: Start failure never stored (fixed, see Traced defects) | S-M |
| 7 | Session read cache + local revocation (`Cached`, 3 generations, `PENDING_REVOCATION_REMOVALS`, `PENDING_RETIREMENTS`, `LAST_WRITE_AUTHORITY`, on-disk markers) | plex/src/plex/session.rs:3133, 3157, 3490, 3515, 3679, 5569, 5669, 5859 | b,c,e | 4 cache arms × revoked-marker phases | `replace_cache` is the one cache gate (callers at 401, 3241, 3497-3507, 3683); pending sets written in 6 fns | yes: sign-out must not resurrect credentials | none proven | L |
| 8 | Async persistence `Coordinator::State` + `CACHE` + `LOCKED_STATE` + `ORDINARY_RECEIPT` | plex/src/plex/session/async_persistence.rs:568, 19-20, 973-976 | a,e | ~7 | 5 admission fns + worker `finish_disk` + guard `Drop` | yes: "saved login" proof, persistence warning | none proven | M |
| 9 | TLS trust mode per host (`keypin::State`: `latched`/`roots_latched`/`blocked`/`engaged`) and its two ladder drivers | net/src/net.rs:3313 (state), 2225-2610 (control-plane driver); net/src/curlio.rs:1246-1295, 1608-1662 (media driver) | c,e | 3 modes × 3 blocked | 12 transition fns; rung dispatch written twice | yes: wrong-clock/old-roots servers, "clock" read-out | none proven | M |
| 10 | Storage helper two-phase auth commit + last-error registers (`pending_cleanup`, `keymanager_error`, `LAST_ERROR_STAGE`, `failure::LAST`) | platform/src/storage_service/backend.rs:77, 12, 201-235, 350-610, 742-857; platform/src/storage_service/failure.rs:103 | e,c | ~5 (clean/staged/promoted/confirmed/cleanup-owed) | phases derived from persisted `pending_auth`; registers written at many `stage_error` sites | indirectly: sign-in persistence | none proven | L |
| 11 | Storage client `StartGate` (helper activation) | platform/src/storage/client.rs:312, 346, 350-460 | a,d | 4 | 3 methods + 2 direct field writes in a closure (423, 438) | indirectly: boot/sign-in wait | none proven | S |
| 12 | Telemetry consent fan-out (`transition::commit`/`forget` driving `consent::CURRENT`, native `ACTIVE`, session `MACHINE`, diag `DEFERRED`, delivery `TENURE`, spool) | telemetry/src/telemetry/transition.rs:11, 47; consent.rs:408-412; native.rs:795; telemetry/src/diag/mod.rs:239 | c | ~4 consent phases replicated in 6 latches | 2 fns fan out to 7 subsystems | yes: privacy choices | none proven | M |
| 13 | Report delivery table `DeliveryState` + one-off `FALLBACK` slot | telemetry/src/telemetry/delivery.rs:26, 49-50; oneoff.rs:22 | b | 5 | callers assign via `watch`/`settle`/`settle_if_current`/`fail_unsettled`/`forget` from spool.rs:163,170,333, mod.rs:278,388-407, oneoff.rs:78,98,108 | yes: Report ID status | none proven (`watch` deliberately overwrites a settled state, oneoff.rs:98) | S |
| 14 | `AudioPreferencesCache` single-flight with failure backoff | plex/src/plex/account.rs:294-371 | a,d | 5 | 5 methods | yes: audio/subtitle defaults at play start | none proven | S |
| 15 | Watchdog `Detector`, `Freezes`, `Signals`, `KillLatch`, `WarningState` | base/src/task/watchdog.rs:97, 180, 590; runtime_check.rs:57, 76 | a | 3-4 each | each has one `observe`/`poll` fn; `Signals` written by attach/presented/Drop/set_foreground | dev overlay, freeze reports | none proven | S |
| 16 | `FrameHealth` visit accumulator | base/src/diag/heartbeat/health.rs:79 | a | 4 (cold/warm-idle/chain/suspended) | 6 methods | usage event `route.left` | none proven; latent: `finish()` then `finish_at_transition()` in one iteration underflows `visit.frames` (139-141); the only caller, src/app/run.rs:3312, picks one | S |
| 17 | Go-home `HOME_PENDING`/`HOME_MINIMIZE` | platform/src/webos.rs:404-438 | c | 3 | 5 stores, worker + frame thread | yes: BACK at root | none proven | S |
| 18 | Discovery probe race (`PendingProbe.state` atom + `BatchResult`/`WinWindow`) | session/src/auth.rs:2029-2051, 2100-2215; ports.rs:202 | c,e | 3 per probe; ~5 per batch | `settle_probe_message` is the single settle fn; atom CAS from prober threads | yes: which address wins | none proven | M |
| 19 | QR mint/poll and account-retry worker loops (`MintedCode`, `PollTail`, `PollEnd`, `Stall`, `AccountRetryPolicy`) | session/src/auth.rs:1169-1320, 1418-1530 | d,e | function-local | locals inside worker fns; results leave as `LoginProgress` to the registered owner | yes: sign-in | none proven | M |
| 20 | WebSocket transport driver phase (`opened`, `closing` locals beside codec `State`) | net/src/ws/transport.rs:169-262; codec net/src/ws/connection.rs:36 | e | 3 driver × 4 codec | 2 locals; codec state assigned at 84, 118, 123, 223 | notification feed | none proven | S |
| 21 | Storage worker `SHARED` executor + `RETAINED` retry queue | base/src/storage_worker.rs:59, 94-140 | c,d | 3 + backoff | 4 fns | indirectly | none proven | S |
| 22 | Small latches: `serverinfo::Entry.inflight` (plex/src/plex/serverinfo.rs:64), plex.tv reachability memos (account.rs:945-996), storage diagnostics `WORKER`/`DISABLED` (platform/src/storage/diagnostics.rs:103-104), window `CYCLE_OPEN`/`CYCLE_SCOPES` (telemetry/src/telemetry/window.rs:114-115) | — | c,d | 2-3 each | 2-4 each | marginal | none proven | S each |
## What the readers did not reach

A lifecycle in one of these places is not in the tables because nobody read it, not because it is
not there.

- **Screens**: `library/{menu,rail,parts,lookahead,header,collections_page,status,bookmark}.rs`,
  `watchlist/{menu,options}.rs`, `item_menu.rs` and `item_menu/hold.rs`, the panels
  (`tracks_panel`, `alt_sources`, `versions_panel`, `about_panel`, `person_bio`, `legal`),
  `player/{overlay,ass_subtitles}.rs`, the `detail/` sub-files other than `mod.rs` and `trailer.rs`,
  `home/mod.rs`'s `Grid`, `HeroDisc` and `HeroTint`, and the mount arms of `registry.rs`: fields
  and writers by grep only.
- **UI framework**: most of `gfx/src/gfx.rs` (the blur chain, the capture stream, region paint, the
  video-plane flags), `gfx/src/text.rs`'s prewarm and background state, `ui/src/widgets.rs`,
  `table.rs`, `card_row.rs`, `poster_grid.rs`, `master_detail.rs`, `decision_alert.rs`,
  `popover_glass.rs`, `popover_morph.rs`; in `appkit/`, `more_menu.rs`, `info_panel.rs`,
  `player_hud/{state,render}.rs` and most of `track_menu.rs`.
- **Data**: `stores/paging.rs::Ledger`, `stores/deck.rs::Lane` and `stores/notify_map.rs::NotifyMap`
  look like pagination and owed-work machines and were read at definition level only; the Person
  backoffs, the Search slide and the metadata season and episode-page pumps likewise.
- **Playback core**: the bodies of `player/engine.rs` and `player/pump.rs`, all of `adaptive.rs`
  (`Coordinator`, `OriginalCoordinator`, `TrialRunner`), `abr/{original,window,mode}.rs`, the resume
  ladder in `route_start.rs`, `route/plan.rs` and `uri_preflight.rs`, `player/ass.rs::Engine`.
- **Backends**: `play-feed/src/ff_subs.rs`, `play-feed/src/physical/`, the HLS demux path and the AVIO
  callbacks in `ff.rs` (`AvioState` may hold a reconnect machine), `src/crashtrace.c` beyond its
  latches. No host test executes the C seam: hostsim runs a Rust re-implementation.
- **Application**: `app/bridge.rs` was read by field writers, not method by method; the individual
  dev-scenario arms, `capture.rs`'s thread interleavings and the recorder's replay cursors were not
  traced.
- **Infrastructure**: the curl init latches in `net/src/net.rs`, `session/src/auth.rs` past the
  discovery race, `plex/src/plex/session/persistence.rs` and `prefs/`, the storage service's
  `keymanager`, `bus` and `activation`, and telemetry's `sender`, `queue`, `spool`, `posthog` and
  `sentry` modules: types and statics surveyed only.
