# Owned-input V13 replay anchor

`tests/fixtures/replay/v13-owned-input-content` is a new synthetic recording, not a rewrite of an older golden. The actual simulator's `--write-synthetic-initial` producer and product recorder captured Filmography → Detail → Back → Back. The official `tools/plxnative-rec check` and `import` accepted the unchanged capture. The imported manifest adds only `anchor: true`; its recorded initial state, hash, fingerprint and segment bytes remain the producer's.

Targets and Resolve each replay all recorded frames (1,172 since the 2026-11-17 re-recording below) with raw exit 0, `verdict=SAME`, and all eight difference counters zero. Current V13 recordings must continue to replay; an unsupported current or unknown profile is never an expected refusal.

The four pre-V13 recordings remain byte-for-byte unchanged, including the historical fixture whose directory ends in `-current`. `tests/replay-historical-profiles.json` binds the reviewed schema-3 fingerprints 4239819007634745513 and 13512213413816145485 to their exact manifest and segment SHA-256 values. Only these fully parsed immutable artifacts receive the typed early refusal `incompatible owner-input/presentation state; requires V13 capture`. The diagnostic grants no old-profile replay authority. Unknown, modified, malformed, truncated and extra-field artifacts cannot earn an expected refusal.

The shared CI replay driver executes all five recordings in both modes: two current SAME outcomes and eight separately counted historical expected refusals. Generic errors, crashes, late bootstrap output, incomplete grades and nonzero difference counters remain failures.

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
