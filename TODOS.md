# TODOS

## Player / trailer preview

### Grade `hero_scrim_a`'s legibility contract against synopsis text over live trailer video

**What:** Add a row to `widgets::hero_scrim_a`'s anchor table for "synopsis-weight (`LABEL` 26px)
text over a bound video plane at `field=1.35`" and grade it against a real device capture, real
trailer content — then tune `landing_hero::PROMOTED_FIELD` (the full-trailer-mode scrim residual)
the same way.

**Why:** `docs/trailer-ux-plan.md` §2.3 keeps the detail hero's synopsis visible during background
trailer autoplay for the first time — previously no prose was ever drawn over playing video, only
the title. Per this project's own DESIGN.md, "still artwork has a knowable worst case... video does
not," so this is a real legibility question with no existing answer. It's deferred out of the
trailer-UX PR (a scope decision, not an oversight) because it needs a TV and real footage to mean
anything — a host test can only check the arithmetic is internally consistent, not that it's
actually legible.

**Context:** Full detail, the exact anchor-table row to add, and the `PROMOTED_FIELD` tuning
process are already written out in `docs/trailer-ux-plan.md` §2.3 and §2.4(c). Land this as its own
device pass immediately after the trailer-UX PR ships, using `tools/capture-screen.sh` on real
trailer content per §5's "Fast-follow" list.

**Effort:** S
**Priority:** P1
**Depends on:** The trailer-UX plan (`docs/trailer-ux-plan.md`) shipping first.

### Re-measure RSS headroom to see if `CYCLE_BUDGET` (14 Loads/process) can go higher

**What:** Redo the device memory-headroom measurement `player/preview.rs`'s own module doc bases
`CYCLE_BUDGET` on (currently ~958 KiB headroom / `sf_load`'s fixed 64 KiB slot ≈ 14), on current
builds and devices, to see whether the ceiling can safely move.

**Why:** A faster autoplay trigger (whichever mechanism the trailer-UX plan's §2.1 investigation
lands on) makes this fixed budget the binding constraint on how long a browsing session gets
previews before they silently stop — at roughly double the trigger rate, a session could exhaust 14
cycles in about half the cumulative dwell time it takes today. The ceiling itself hasn't been
revisited since it was first measured.

**Context:** This is real device memory profiling, not a quick constant bump — inflating the number
without re-measuring would be exactly the kind of unverified platform claim this project's AGENTS.md
warns against. See `docs/trailer-ux-plan.md` §2.1 for the current arithmetic and why this is called
out as a follow-up rather than folded into that plan.

**Effort:** M
**Priority:** P2
**Depends on:** None — independent of the trailer-UX PR, but informed by its dwell-mechanism
outcome.

### Device verification pass for the trailer UX feature (fps scene, real dwell pass, basic capture)

**What:** Everything in `docs/trailer-ux-plan.md` §5's "Device (blocking)" list that a host test
cannot answer: an `fps:`-style scene exercising the logo's move+shrink transform (floor + worst/
coldopen ceiling checks), a real-device browsing pass at the shipped 2.0s dwell to sanity-check
false-trigger/budget-consumption behavior empirically, and a basic `tools/capture-screen.sh` still
of each of the four states (idle, dwelling, background autoplay, full trailer) confirming nothing
is grossly broken (wrong position, wrong alpha, focus landing somewhere invisible).

**Why:** The code landed with full host-test coverage (`make check` green) but zero device
verification — there is no host runtime for GLES composition, text rasterization, or real trailer
video, so none of this can be confirmed from a desk. This is the ordinary "does it actually look
and feel right" pass every UI change needs before it's trusted, independent of the deeper
legibility grading in the anchor-table TODO above.

**Context:** `docs/trailer-ux-plan.md` §5 has the exact scene design (reuse `plxnative-navosc`'s
headless-trigger pattern, per `K_DISC_UNFURL`'s own disc-unfurl scene as the nearest analogue).

**Effort:** M
**Priority:** P1
**Depends on:** The trailer-UX plan shipping first (done, 2026-09-15).

### Investigate decoupling trailer fetch-start from the visible reveal

**What:** Determine whether `player::preview::Machine::start` can be triggered on a SHORTER
"fetch-commit" threshold than today's single dwell timer, while the screen holds its full Idle
presentation for a separate, longer minimum reveal delay — so felt latency
(currently `dwell + Load time`, serial) becomes closer to `max(fetch-commit, reveal, Load time)`.

**Why:** This was the outside-voice-recommended alternative to a blanket `DWELL_S` cut (which is
what shipped instead, as the documented fallback — see `player/preview.rs`'s `DWELL_S` doc
comment). It was not implemented because it needs real device data to answer the open question it
raises on paper: does a shorter fetch-commit threshold meaningfully reduce false starts, or does it
just spend the same 14-Load `CYCLE_BUDGET` faster on items a viewer glances past and moves on from
(via the existing `PreviewStop`/`abandon` cancellation path, which only fires AFTER a Load is
already admitted and budget-spent)?

**Context:** Full mechanism sketch, the exact risk, and the fallback-replacement contract (nothing
else in the trailer UI depends on which mechanism sets `view.picture`) are in
`docs/trailer-ux-plan.md` §2.1 and the `DWELL_S` doc comment in `player/preview.rs`.

**Effort:** L (real investigation + a device pass to validate whichever answer it finds)
**Priority:** P3
**Depends on:** None, but do the RSS re-measurement TODO above first if this raises Load frequency
further — the two compound.
