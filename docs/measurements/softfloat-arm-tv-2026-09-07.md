# Host↔ARM soft-float differential table — MATCH (TV session 3, 2026-09-07)

Spec §4.2 states two assumptions rather than assuming them, and this is the measurement for the
second: **are the armv7 soft-float routines correctly rounded for the five operations
(`+ − × ÷ √`) that carry logical state?** `ui/motion.rs::differential_table` runs 4,096 operands
through the same code on both targets and compares one hash.

    softfloat: n=4096 hash=0x65a8e905a259246d host=0x65a8e905a259246d MATCH

- **Set:** the dev LG 49SM9000PLA, webOS 4.5, Cortex-A9, armv7-a soft-float.
- **Build:** `FLAVOR=debug`, dev configuration (the probe is a `devtriggers` surface).
- **Recipe:** `make softfloat-probe` — arms `plxnative-softfloat` in the install's runtime root,
  launches, reads the `softfloat:` line back, and fetches the word table to
  `tests/fixtures/softfloat/arm.tbl` (gitignored: it is a word-by-word diff aid for a DIVERGENCE,
  not a fixture anything reads).
- **Owed since phase 2**, which landed the host half and the probe target; this is the first
  session after the probe existed.

## What this licenses, and what it does not

It closes the arithmetic half of the cross-target replay question: a recording whose measurements
come from the metrics table has no reason to diverge on ARM *because of the spring integrators'
own arithmetic*, so the phase-5b fixtures need no target scoping.

It does **not** widen the promise of §5.5. A replay fixture stays pinned to the build lineage that
recorded it; same-build determinism is the promise, cross-target is the measured goal, and this
result only removes one suspect from a future divergence. It is also one measurement on one
firmware and one CPU — not a general claim about armv7 soft-float.

**Re-run it after any toolchain or `-C target-cpu` change.** Those are the inputs that would move
this hash, and nothing in `make check` can see them: the host half is pinned in `ui/motion.rs` and
the ARM half only exists when somebody has the television.
