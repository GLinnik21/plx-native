# Replay fixtures

A directory here is one RECORDING (restructure spec §5.3): `manifest.json` (the header) and
`rec-NNNN.jsonl` segments, taken on the simulator against `tests/mock_pms.py` by arming
`plxnative-rec`. They are regression assertions pinned to the build lineage that recorded them
(§5.5): `tools/plxnative-rec diff` compares two, `tools/plxnative-rec check` verifies one against
`ALPHABET.json`, and `tests/test_harness.py` checks their schema and closed vocabulary on every
`make check`. **`make check-replay` builds the macOS simulator and executes every committed
recording in both Targets and Resolve modes.** The same driver (`tests/replay_fixtures.py`) gates
the macOS Simulator CI job, with outbound networking denied, and retains each run's logs.
It requires a successful process exit, exactly one clean summary, every difference counter zero,
and frame/grade counts matching the complete committed ledger. Missing manifests or segments,
refused fixtures, timeouts, and incomplete replay all fail; no fixture is quarantined or skipped.
The renderer-backed replay gate is separate from the pure host suite: `make check` tests its
strict result parser, but does not launch the simulator.

Controlled bootstrap accepts **Home**, **Settings**, and the typed synthetic
**12-filmography-detail-return** content domain. `tests/focusfp.sh --rec --only 12` asks the
current simulator for Flow 12's complete typed synthetic initial input before boot, using the
same contract as replay restoration. It remains synthetic: it does not seed an auth file or patch
a recorded identifier.
`tests/controlled_bootstrap.py --sim <built-simulator>` exercises normal Home, recording, fresh
and contrasting-ambient replay with outbound IO denied, malformed-input refusals, and a
supported-effect discriminator. Its private recording must pass `tools/plxnative-rec check`
before `import`/`rerecord`; a live-mock focus smoke alone is not replay acceptance.

The hash includes `AppFrameV4{route,overlay,focus,tree,session,consent,initial}`: the press machine, the route/overlay words and the
focus fingerprint from phase 2, plus — since phase 5b — `tree`, which is `Dispatcher::state_hash`
(every live container instance's `LogicalState`, the tree's shape and surface phases, saved entry
arguments and return memory even after eviction, the engine's focus and the queue depth), plus
the Session owner's cached logical digest and the typed initial-input digest. Recording and
replay use the same frame-tail composition. Private initialization and complete effect payloads
can contain credentials; only their digests enter shareable probes. Synthetic construction,
not alphabet membership alone, establishes fixture provenance.

Controlled replay supplies Home/Browse results through the production dispatcher and supplies
Flow 12 Detail/Person results at their original store consumers. It binds the recorded Client
explicitly and denies resource execution and data transport. Account, playback, and every other
unlisted domain remain unsupported and fail closed before IO. Product replay has two explicit
modes: Targets substitutes each recorded Focus/Hit resolution before dependent effects, while
Resolve runs the current engine/map and grades every resolution pointwise before continuing.

Person's provider requests use the captured authority throughout controlled recording and replay.
Ambient session-cache recovery cannot retire those requests or introduce unrecorded retries.

The TV's startup WILL/DID foreground notifications are recorded and replayed through the
navigation owner. Recorded notifications cannot restore native playback, change network grants,
or authorize drawing into a physically backgrounded window. During replay the real compositor
still owns window activation, while live keys and FIFO commands cannot join the recorded input
stream. A real background/quit event interrupts replay; recorded background lifecycle remains
unsupported.

Page-capture GPU readiness is also a recorded input, sampled before dispatch. Replay supplies
that observation to the ordinary motion and presentation gates, then computes and grades the
final present decision. Faster GPU completion cannot introduce extra presents or advance a held
transition early. Missing, duplicate, late or unconsumed readiness fails closed; the independent
physical window gate still blocks drawing while backgrounded. Recordings predating this input
are refused at the product shape boundary and must be recorded afresh.

The same pre-dispatch record carries **text readiness** (`CaptureReadinessV2`, the capture row's
`text` field): whether recorded text is still warming. A presented surface stays held at appear 0
while it is (`text::surface_text_pending`, bounded by `SURFACE_TEXT_HOLD_MAX_MS`), and the queue
drains under a wall-clock budget, so a slower CPU held the panel shut for more frames and turned
it `Open` later. Before it was recorded, Flow 12's Filmography modal opened on frame 78 instead of
the recorded 76 on GitHub's macOS runners and on a Mac's efficiency cores (`taskpolicy -c
background`), with every named difference counter at zero and only `diverged=2`. Replay now
supplies the recorded answer; the live queue still drains, which changes only what is drawn.

The admission contract also records each synchronous worker-spawn answer with its full request
identity and frame ordering. A refused attempt stays refused during replay, including its normal
retry/backoff; it is not turned into an admitted worker or an asynchronous failure. Natural
recording reads/mints inputs without saving or migrating credentials until validated capture
and recorder attachment. Writer write/rotation/final-flush failures fail application success.
The reader budgets decoded allocations as well as source bytes; the 64 MiB source cap alone is
not a memory bound. Confirmed local erasure retires this App's writer before sweeping its owned
recording/init/control namespace, never the arbitrary target named by `plxnative-recplay`.

Historically, the phase-11 driver still fetched live but constrained the frame a result was
observed on (`rust-modules/machine/src/landgate.rs`, spec §3.3 step 3). Every landing
SITE — Home's hubs and each legacy pump's mailbox take — consumes its mailbox through a schedule
of `(frame, arrivals)` pairs per store, taken from the recording's `land` records: an arrival
that is early WAITS for its frame, the frame a landing is due polls for a bounded moment, and one
that is late, unrecorded (`extra`) or never produced (`missing`) is reported and rides the
verdict as `land_diffs`.

The **anchors** include `1-boot-home-chip-grid`, `6-settings-family` (Privacy toggle and Legal
document navigation), and `12-filmography-detail-return` (the owned Filmography surface, a
library-matched credit opened in Detail, and both BACK steps). Phase 7 rerecorded the existing anchors
after an observed loader refusal and added the content-return anchor. **Phase 11 rerecorded all
three onto schema 2**, which carries the landing schedule above. **The product Resolve milestone
rerecorded them onto schema 3**, adding a typed, bit-exact Width/Cap/Line measurement table, one
final `fo` after all drains in each product frame, and ordered `rs` Focus/Hit observations.
A key missing from that table refuses the replay, with one exception: the text-prewarm walks
(`rec::speculative`), which lay out a page no frame is drawing yet so its glyphs are resident.
Their answers are recorded like any other. A key an older recording lacks inside such a walk
answers 0.0, and state, presents, effects and Focus/Hit are still graded on every frame. The
held-page walk measures Flow 12's focused cast caption, which that anchor never drew, because the
Person page is pushed before Detail's replacement capture.

**Current migration:** Home, Settings, and Flow 12 Filmography anchors are accepted
controlled-replay coverage. Flow 12 has typed initial state, exact synchronous admissions,
recorded content results, and denied resource execution. This bounded support does not imply
all-domain acceptance. Editing manifest fingerprints or state hashes is never a replacement for
rerecording. The D1 results below describe that earlier build, not current all-domain acceptance.

**Historical Phase 12 (D1) rerecorded all three again**, and it is the cleanest example of what `rerecord` is
for: `enum Route` was folded into `AppArg`, so `ARG_SHAPE` lost its nested `Legacy:Route{…}` and
gained the seven page names flat — `state_fp` moved from `0x2ee80fef41949b4b` to
`0x489bbd488180e355` — while `LogicalState::write` still emits exactly the bytes it did before, so
NO recorded frame hash changed. The committed artifacts could not be loaded at all
(`replay: REFUSED — state shape 0x2ee80fef41949b4b recorded, 0x489bbd488180e355 here`, all three),
which is the machine-checkable condition `rerecord` verifies for itself; nothing was accepted,
because nothing could be compared. All three anchors were subsequently re-recorded against the current state shape
`10372147871357755035` (`0x8ff14588f61a5e9b`). The plaintext-consent initial
fields moved the declared census to `ControlledHomeInitV5` / `SessionInitV4`; all three
anchors were freshly recorded after the old inputs were observed being refused. The library-type
and compact-row change in #258 subsequently moved the screen census; all three anchors were
recorded again on that combined shape and replayed in both modes with every difference counter
at zero. The `CaptureReadinessV1` input subsequently moved the product wire shape; all three
anchors were freshly recorded again, and both modes graded every frame with every difference
counter at zero. `CaptureReadinessV2` (text readiness, above) moved it again, and all three
anchors were recorded afresh after the old ones were observed being refused. The controlled
effect encoder now covers preview
start/stop/transport/seek, item-menu requests, and every content-panel payload with exhaustive
matches. Replay regenerates those typed requests and compares their complete JSON payloads;
it does not decode recorded effects into executable requests. Full playback remains outside
this controlled domain. Flow 12 also waits for the initial Home root to mount before opening
Detail, so a cold renderer taking longer than the 500 ms scenario delay cannot discard the
Detail request. Fixture 12's quarantine is removed. Clean replay summaries require
`input_diffs=0 effect_diffs=0 focus_diffs=0 hit_diffs=0` as well as zero state, presentation,
result, and landing diffs.

Flow 12's own history is worth keeping, because it is what the schedule was built for. Its phase-7
recording was taken while `plxnative-detail` loaded the page with a BLOCKING fetch on the SDL
thread; phase 11 made that boot arm asynchronous, so the recording no longer described the build
and the replay diverged on frames 31 and 32 — both recorded `0xd9d6d1334cf4d698`, both replayed
`0xbb2179d70158cf9b` — before re-converging. A recording taken under the async arm could not be
committed in its place: the landing then arrived ~5 ms after boot, the frame it landed on differed
between the recording and every replay, and 927 of 928 frames diverged (three runs of three). The
anchor could not be rebaselined and a stable re-recording needed the gate first. Controlled Flow
12 capture records the typed offline policy and failed provider replies without a WAN call;
replay supplies those replies and admissions with the mock off and resource execution denied. The
trigger remains part of the synthetic initial contract, not a dependency on a timely external 401.

Take the census from this directory's listing. An anchor refuses `--rebaseline` (below); when a
change instead bumps the recorded state SHAPE (`schema` or `state_fp` — 5b's `tree:u64` term did
exactly this), the old fixture cannot even be LOADED, so `tools/plxnative-rec rerecord <dir> <name>`
is the verb: it verifies the shape actually moved and replaces the fixture, anchor flag preserved.

**Nothing here may carry a household byte.** The mock server's names are `s[0-9a-f]{8}`; every
other string a recording holds is a protocol constant named in `ALPHABET.json`. A recording taken
against a REAL server lives in `plxnative-recordings/` (gitignored, refused by the outbound guard) and
never comes here. A fixture whose `manifest.json` carries `"anchor": true` refuses `--rebaseline`
(a behaviour-change replacement, evidenced by a divergence record); `rerecord`, above, is the
shape-bump escape an anchor does not refuse.

The playback-settings change (#217) adds the two Settings page arguments and their logical
state to the screen census. The prior anchors were observed being refused, then all three were
freshly recorded against shape `13142456728797643905` and passed Targets and Resolve with zero
difference counters. Account/preference I/O remains outside the controlled replay domain: typed
preference effects are rejected by the bridge before capture, worker admission, or persistence.

Localization adds the captured language preference to the controlled initial input
(`ControlledHomeInitV6` / `SessionInitV5`); controlled System resolves to en-US with a 24-hour
clock whatever the host's `LANG`. The #217 anchors were observed being refused on that shape, then
all three were freshly recorded against shape `14389175148146224086` and passed Targets and
Resolve with zero difference counters. The recorded text differs where the change meant it to:
Settings gains the Language destination, the Filmography count is measured as it wraps, and
Detail's release date is the locale's short numeric date (`3/14/2011`, no longer `14 Mar 2011`).

The collection page (#205) adds `AppArg`'s Collection argument, its page memory and the
Collection screen's logical state to the screen census. The prior anchors were observed being
refused (`replay: REFUSED — invalid or incompatible recording`, all three, both modes), then all
three were re-recorded with `tools/plxnative-rec rerecord` against shape `7683106202277674285`
and passed Targets and Resolve with zero difference counters.

The Library's Collections type (#205) moves both halves of the census: `PmsMovie` gains
`child_count` (the hubs record and initial state), and the Library's TYPE transaction and layout
change shape (`LibraryType{code:u32}`, `LibraryLayout{…empty:bool…}`). The prior anchors were
observed being refused (`replay: REFUSED — invalid or incompatible recording`, all three, both
modes), then all three were re-recorded with `tools/plxnative-rec rerecord` against shape
`5354288741423994209` and passed Targets and Resolve with zero difference counters.

The linked collection shelves on Home and the Library (#205) give a Library section-hub shelf an
optional heading group, which moves the screen census. The prior anchors were observed being
refused (`replay: REFUSED — invalid or incompatible recording`, all three, both modes), then all
three were re-recorded with `tools/plxnative-rec rerecord` against shape `2955617958233795258`
and passed Targets and Resolve with zero difference counters.

The detail page's collection shelf (#205) adds an eighth Detail section slot
(`SectionId::Collections`), its remembered column and the `CollectionMember` identity to the page
memory, which moves the screen census again. The anchors recorded against `2955617958233795258`
were observed being refused (`replay: REFUSED — invalid or incompatible recording`, all three,
both modes), then all three were re-recorded with `tools/plxnative-rec rerecord` against shape
`12050413345652660670` and passed Targets and Resolve with zero difference counters.

Merging the collections work into the localization branch combines both census moves: the
captured language preference with the collection page, the Library's Collections type, linked
shelf headings and the detail collection shelf. The anchors recorded against
`12050413345652660670` were observed being refused (`replay: REFUSED — invalid or incompatible
recording`, all three, both modes), then all three were re-recorded with `tools/plxnative-rec
rerecord` against shape `12097267434420408384` and passed Targets and Resolve with zero difference
counters. The closed alphabet gains the Settings root's shortened crash-report sub-line
("Your crash report identifier and how to have reports deleted."), which the fit-to-slot
translation pass introduced and the Settings recording now measures.

The linked heading's member count (#205) gives a Home shelf and a hub row the hub's `totalSize`,
carried by both the hubs record (`HubsResultV1`'s shelves) and the hubs initial state
(`HubsInitialV1`'s hub rows), so the recorded Home result is no longer byte-identical. Before the
two shape descriptors named the new field, the anchors loaded and diverged on that result
(`result_diffs=1 effect_diffs=1`, all three, both modes). Once they did, the anchors recorded
against `12097267434420408384` were observed being refused (`replay: REFUSED — invalid or
incompatible recording`, all three, both modes), then all three were re-recorded with
`tools/plxnative-rec rerecord` against shape `10638452537178981056` and passed Targets and Resolve
with zero difference counters.

The UI fixes batch changed recorded behaviour without moving the shape, so neither verb applied:
an input `FocusMoved` now rides the immediate lane ahead of the frame's Tick (all three anchors:
the same effects reordered, and Home's state from its first key press on), and the About card
now measures its MORE mark at caption size to fade the synopsis under it (Flow 12 refused with
`replay measurement table miss`). Each divergence was attributed by replaying with that change
alone switched off (SAME, every counter zero). The owner approved re-anchoring on 2026-09-29: all
three were recorded afresh on shape `10638452537178981056`, imported with `tools/plxnative-rec
import`, marked anchor again, and passed Targets and Resolve with zero difference counters. The
alphabet gains the mock's audio-enhancement codecs (`aac`, `hevc`) and the client-localized hub
titles (`Recently Added Movies`, `Recently Added TV`).

The navigation-frame-drops change (PR #312) also moved recorded behaviour without moving the
shape: `6-settings-family` and `12-filmography-detail-return` diverged at frames 77 and 76
respectively (all counters otherwise zero), while `1-boot-home-chip-grid` stayed SAME. Both
anchors present a held modal/panel, and the divergence was attributed by replaying with each of
the commit's two behaviour changes switched off individually. Gating focus stops to the visible
walk alone (`DrawFrame::records_stops`) left the divergence unchanged; disabling it instead made
`12-filmography-detail-return` diverge on a second frame, ruling it out. Reverting the held
surface's text-recording path — `PopoverMotion::tick` no longer extending its hold while
`plx_gfx::text::prewarm_pending()` is true, and the surface loop drawing every surface with
`Painter::root()` again — reproduced SAME with every counter at zero on both anchors. That is the
cause: the new hold extension changes how many frames a held Settings/Filmography surface stays at
appear 0 before ramping, which moves `PopoverMotion`'s own state on exactly the frame the anchor
recorded a modal opening. The owner approved re-anchoring on 2026-09-30 ("rerecord"): both were
recorded afresh on the unchanged shape `10638452537178981056` (schema 3) — old fixture removed,
`tools/plxnative-rec import`, `anchor: true` restored by hand in each manifest.json, since an
anchor whose shape has not moved refuses both `rerecord` (shape unchanged) and `rebaseline`
(anchors always refuse it) — and passed Targets and Resolve with zero difference counters. The
closed alphabet did not move.

The Settings root reorder (Libraries, Playback, System, Unencrypted connections, Privacy, About)
moved recorded behaviour without moving the shape: the signed-out root is now Playback, Language,
Privacy, Legal, About, so `6-settings-family`'s scripted keys (`tests/focusfp.sh` flow 6) start
with two DOWNs to reach Privacy, and the new "About" section header entered the measurement
table (`ABOUT` joined the closed alphabet's literals). The anchor was recorded afresh on the
unchanged shape (`tools/plxnative-rec import`, `anchor: true` restored by hand). Two of the four
fresh recordings diverged on replay by one to three frames at a timing-dependent frame; the one
committed replayed SAME in Targets and Resolve on three consecutive runs.

The Settings-form keyed focus (`docs/settings-form.md` PR 2) changed the recorded STATE shape of
the Settings root on purpose: focus elements, the surface's `remembered` seats and `RootState.sel`
are now each row's `RowKey` (Playback 1, Language 4, Privacy 7, Legal 8, About 9) instead of its
table index, so the old `6-settings-family` recording was REFUSED at its first focus resolution
("impossible focus resolution"). The (schema, state_fp) shape itself did not move. The anchor was
recorded afresh (`tools/plxnative-rec import`, `anchor: true` restored by hand); the first
recording replayed SAME in Targets and Resolve on three consecutive runs. The other two anchors
stayed SAME untouched.

The Settings form migration's third step (one navigation path; the picker is a stack page) moved
the screen census (`SCREEN_SHAPES_PIN`): the anchors recorded against `10638452537178981056` were
observed being refused (`replay: REFUSED — invalid or incompatible recording`, all three, both
modes), then all three were re-recorded with `tools/plxnative-rec rerecord` against shape
`9770156787996859790` and passed Targets and Resolve with zero difference counters. The first
`12-filmography-detail-return` recording diverged on two frames in Targets mode on one of three
consecutive replays (the timing-sensitive held-surface frames noted above), so a second recording
was taken and committed; it replayed SAME in both modes on four consecutive runs.

The player sub-menus' second step (PR 2, rebuilt on #339's `TrackRow` forms: the Subtitles tab's page
stack) moved the screen census (`SCREEN_SHAPES_PIN`) on purpose: the player overlay's state shape now
carries the track menu's page path and each stacked page's return id. The anchors recorded against
`299197959872315368` were observed being refused (`replay: REFUSED — invalid or incompatible
recording`, all three), then all three were re-recorded with `tools/plxnative-rec rerecord` against
shape `13647275923436478101` (`0xbd64dd25602c1e95`) and replayed SAME in both modes
(`tests/replay_fixtures.py`, three consecutive runs, and `tests/focusfp.sh --replay`/`--targets`/
`--resolve`) with zero difference counters. None of the three drives the player, so the recorded
frames differ only in the shape they carry.

The player sub-menus' fifth step (PR 5: More's Quality row becomes a page on the shared `PageStack`)
moved the screen census (`SCREEN_SHAPES_PIN`, `10952571532616655710` -> `8098562808894689397`) on
purpose: the player overlay's replay canon now carries More's page path, and the shape string gained
`|More:{depth,pages,key}`. The anchors recorded against shape `13647275923436478101` were observed
being refused (`replay: REFUSED — invalid or incompatible recording`, all three, both modes), then
all three were re-recorded with `tools/plxnative-rec rerecord` against shape
`10119451143357529556` and replayed SAME in both modes (`tests/replay_fixtures.py`, three
consecutive runs) with zero difference counters. None of the three drives the player.
