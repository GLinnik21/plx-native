# Owned-input V13 replay anchor

`tests/fixtures/replay/v13-owned-input-content` is a new synthetic recording, not a rewrite of an older golden. The actual simulator's `--write-synthetic-initial` producer and product recorder captured Filmography → Detail → Back → Back. The official `tools/plxnative-rec check` and `import` accepted the unchanged capture. The imported manifest adds only `anchor: true`; its recorded initial state, hash, fingerprint and segment bytes remain the producer's.

Targets and Resolve each replay all recorded frames (922 since the second 2026-11-17 re-recording below) with raw exit 0, `verdict=SAME`, and all eight difference counters zero. Current V13 recordings must continue to replay; an unsupported current or unknown profile is never an expected refusal.

The four pre-V13 recordings remain byte-for-byte unchanged, including the historical fixture whose directory ends in `-current`. `tests/replay-historical-profiles.json` binds the reviewed schema-3 fingerprints 4239819007634745513 and 13512213413816145485 to their exact manifest and segment SHA-256 values. Only these fully parsed immutable artifacts receive the typed early refusal `incompatible owner-input/presentation state; requires V13 capture`. The diagnostic grants no old-profile replay authority. Unknown, modified, malformed, truncated and extra-field artifacts cannot earn an expected refusal.

The shared CI replay driver executes all six recordings in both modes: four current SAME outcomes (this anchor and `13-by-id-filmography`, two modes each) and eight separately counted historical expected refusals. Generic errors, crashes, late bootstrap output, incomplete grades and nonzero difference counters remain failures.

**Re-recorded 2026-11-17** (route pages share one anatomy: every route title, Filmography's
included, is the 48 Settings title instead of the 72 hero). The recorded behaviour changed without
moving the shape (`state_fp` 16894190109976774341): the committed anchor was observed being refused
in both modes (`replay: REFUSED — replay measurement table miss`, the title measured at a rung the
recording never measured). The flow's keys are unchanged. The anchor was recorded afresh
(`tests/focusfp.sh --rec --only 12`, the old fixture removed, `plxnative-rec import`, `anchor: true`
restored; the manifest differs only in the initial-state hash and the mock's port), and the CI driver
(`tests/replay_fixtures.py`, all five recordings) replayed it SAME in Targets and Resolve on three
consecutive runs, 1,172 frames graded, every difference counter zero. The pinned digests in
`tests/test_harness.py` follow the new bytes.

**Re-recorded again 2026-11-17, with the by-id family** (the runtime half of the accessibility
identifiers, #1432). `InputKind::ById` and its `Id` resolution row moved the recorded shape
(`state_fp` 16894190109976774341 `0xea74_3865_5115_46c5` -> 9159521251572352515
`0x7f1d_280c_909c_8603`): the committed anchor was observed being refused (`replay: REFUSED --
invalid or incompatible recording`), so it was recorded afresh the same way (`tests/focusfp.sh
--rec --only 12`, `plxnative-rec import`, `anchor: true` restored; the manifest differs only in the
fingerprint, the initial-state hash and the mock's port), and the CI driver replayed it SAME in
Targets and Resolve, 922 frames graded, every difference counter zero. The outbound guard's and the
tool's `CURRENT_FP` moved with it, and the pinned digests in `tests/test_harness.py` follow the new
bytes. No frame hash was edited by hand.

`tests/fixtures/replay/13-by-id-filmography` is the one fixture that drives the by-id tokens
(`tests/focusfp.sh` flow 13: Flow 12's Filmography page, then `focus:nav.watchlist`,
`focus:filmography.row.3` and `press:filmography.row.0`, no `@subject`, four BACK keys down to Home,
and `focus:nav.watchlist` again). It is an anchor and replays SAME in both modes, 1,209 frames, on
three consecutive runs. The first `focus:nav.watchlist` is REFUSED on purpose: the Filmography page
owns input and has no tab strip, so `a11y: refused focus nav.watchlist unknown owner=filmography` is
recorded, and it is the one by-id refusal `ALPHABET.json` lists, literally (`refusal` 1 on its `Id`
row). `focus:filmography.row.3` and the press resolve (`refusal` 0; the press is the ordinary OK
Down/Up on the first credit row and opens Detail exactly as Flow 12's `down ok` does), and the last
`focus:nav.watchlist` resolves on Home to the strip's Watchlist tab. The unwinding to Home records the
Person store's `close`, which `ALPHABET.json` lists as the `PersonCmd::Close` encoding.

Pressing a Home element (`press:nav.profile`, `press:nav.movies`, `press:home.hero.info`) aborts the
controlled recording with `replay: REFUSED -- unsupported Home application effect`, so no press is
recorded on Home. The recording is also kept short, one second between the BACKs: the loader's
decoded-recording budget (`rec/strict_json.rs`, `Budget`) refused a 1.5 MB take at load
(`record JSON/budget`, line 13918 of 15444 -- the replay's generic `invalid or incompatible
recording`), because Home frames cost about 100 KB per 100 frames.
