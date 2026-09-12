# Replay fixtures

A directory here is one RECORDING (restructure spec §5.3): `manifest.json` (the header) and
`rec-NNNN.jsonl` segments, taken on the simulator against `tests/mock_pms.py` by arming
`plxnative-rec`. They are regression assertions pinned to the build lineage that recorded them
(§5.5): `tools/plxnative-rec diff` compares two, `tools/plxnative-rec check` verifies one against
`ALPHABET.json`, and `tests/test_harness.py` verifies every committed one on every `make check`.

The first controlled-bootstrap domain is **Home**. `tests/focusfp.sh --rec --only 1` now asks the
current simulator for the complete typed synthetic initial input, before boot, using the same
contract as replay restoration. It does not seed an auth file or patch a recorded identifier.
`tests/controlled_bootstrap.py --sim <built-simulator>` exercises normal Home, recording, fresh
and contrasting-ambient replay with outbound IO denied, malformed-input refusals, and a
supported-effect discriminator. Its private recording must pass `tools/plxnative-rec check`
before `import`/`rerecord`; a live-mock focus smoke alone is not replay acceptance.

The hash includes `AppFrameV3{route,overlay,focus,tree,session,initial}`: the press machine, the route/overlay words and the
focus fingerprint from phase 2, plus — since phase 5b — `tree`, which is `Dispatcher::state_hash`
(every live container instance's `LogicalState`, the tree's shape and surface phases, saved entry
arguments and return memory even after eviction, the engine's focus and the queue depth), plus
the Session owner's cached logical digest and the typed initial-input digest. Recording and
replay use the same frame-tail composition. Private initialization and complete effect payloads
can contain credentials; only their digests enter shareable probes. Synthetic construction,
not alphabet membership alone, establishes fixture provenance.

Controlled Home supplies its recorded Home/Browse results through the production dispatcher,
binds the recorded Client explicitly, and denies replay data IO. Other domains and both-mode /
cross-target completion remain open and unsupported families fail closed before IO.

The admission contract also records each synchronous worker-spawn answer with its full request
identity and frame ordering. A refused attempt stays refused during replay, including its normal
retry/backoff; it is not turned into an admitted worker or an asynchronous failure. Natural
recording reads/mints inputs without saving or migrating credentials until validated capture
and recorder attachment. Writer write/rotation/final-flush failures fail application success.
The reader budgets decoded allocations as well as source bytes; the 64 MiB source cap alone is
not a memory bound. Confirmed local erasure retires this App's writer before sweeping its owned
recording/init/control namespace, never the arbitrary target named by `plxnative-recplay`.

Historically, the phase-11 driver still fetched live but constrained the frame a result was
observed on (`rust-modules/src/ui/landgate.rs`, spec §3.3 step 3). Every landing
SITE — Home's hubs and each legacy pump's mailbox take — consumes its mailbox through a schedule
of `(frame, arrivals)` pairs per store, taken from the recording's `land` records: an arrival
that is early WAITS for its frame, the frame a landing is due polls for a bounded moment, and one
that is late, unrecorded (`extra`) or never produced (`missing`) is reported and rides the
verdict as `land_diffs`.

The **anchors** include `1-boot-home-chip-grid`, `6-settings-family` (Privacy toggle and Legal
document navigation), and `12-filmography-detail-return` (the owned Filmography surface, a
library-matched credit opened in Detail, and both BACK steps). Phase 7 rerecorded the existing anchors
after an observed loader refusal and added the content-return anchor. **Phase 11 rerecorded all
three onto schema 2**, which carries the landing schedule above.

**Current migration:** Home requires the controlled typed-init shape. Settings and Filmography
anchors remain historical assertions, not accepted executable coverage of the Home-only driver.
They are retained, not demoted or silently skipped. They need their own capture/restore/IO
boundaries before a later genuine rerecord can count. Editing manifest fingerprints or state
hashes is never a replacement for rerecording. The D1 results below describe that earlier build,
not current all-domain acceptance.

**Phase 12 (D1) rerecorded all three again**, and it is the cleanest example of what `rerecord` is
for: `enum Route` was folded into `AppArg`, so `ARG_SHAPE` lost its nested `Legacy:Route{…}` and
gained the seven page names flat — `state_fp` moved from `0x2ee80fef41949b4b` to
`0x489bbd488180e355` — while `LogicalState::write` still emits exactly the bytes it did before, so
NO recorded frame hash changed. The committed artifacts could not be loaded at all
(`replay: REFUSED — state shape 0x2ee80fef41949b4b recorded, 0x489bbd488180e355 here`, all three),
which is the machine-checkable condition `rerecord` verifies for itself; nothing was accepted,
because nothing could be compared. The replacements replay `verdict=SAME` with `diverged=0
present_diffs=0 result_diffs=0 land_diffs=0`.

Flow 12's own history is worth keeping, because it is what the schedule was built for. Its phase-7
recording was taken while `plxnative-detail` loaded the page with a BLOCKING fetch on the SDL
thread; phase 11 made that boot arm asynchronous, so the recording no longer described the build
and the replay diverged on frames 31 and 32 — both recorded `0xd9d6d1334cf4d698`, both replayed
`0xbb2179d70158cf9b` — before re-converging. A recording taken under the async arm could not be
committed in its place: the landing then arrived ~5 ms after boot, the frame it landed on differed
between the recording and every replay, and 927 of 928 frames diverged (three runs of three). The
anchor could not be rebaselined and a stable re-recording needed the gate first. It also records
with `/tmp/plxnative-nowan` armed, since its person-page arms dial `discover.provider.plex.tv` for
real and the fixture's stability would otherwise depend on that 401 arriving promptly over the
actual internet.

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
