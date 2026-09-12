# UI restructure Phase 7: verification record

Work resumed from `053a335a` on 2026-09-08. The saved Phase 7 screen files were not declared by
`screens/mod.rs`; the legacy Detail, Person and Filmography modules still supplied the running UI.
This record distinguishes the starting behavior from the migration's eventual verification.

## Starting build

The contract-only commit `2d9c9f52` adds content-navigation effect types and a screen return-memory
query receiving the engine's current focus. It does not mount the new content screens.

- `make check`: passed, including the default and hostsim Rust suites and the harness gates.
  Default: 2,421 passed, 3 ignored. Hostsim: 2,442 passed, 3 ignored.
- Shipping feature check (`CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path
  rust-modules/Cargo.toml --lib --no-default-features`): passed.
- `make sim`: passed.
- Existing replay anchor 1: `frames=579 graded=5 diverged=0 present_diffs=0 verdict=SAME`.
- Existing replay anchor 6: `frames=1270 graded=11 diverged=0 present_diffs=0 verdict=SAME`.

The saved session's suggestion that the Login fingerprint change necessarily required rerecording
these anchors did not reproduce on this build. Rerecording must follow an observed shape change,
not that earlier suggestion.

## A false pass in the Related-hold flow

`tests/focusfp.sh --only 8` originally reported PASS after producing just these meaningful states:

```text
focus route=home snapt=0 snapp=0 hf=0 row=0 col=0 sid=- rk=- press=0
focus route=home snapt=0 snapp=0 hf=0 row=0 col=0 sid=0 rk=200622 press=0
focus route=detail sec=5 col=0 eptext=0 season=- saved=0,0,0,0,0,0 card=0 alt=0 show=0 sid=0 rk=1001 ep=- epwatched=- press=0
hb route=detail
```

All data above came from `tests/mock_pms.py`, seed 1. No ItemMenu opened: `detailsec=3` is three
DOWN presses, not section ID 3. The synthetic movie's order is Hero → Cast → Related → About.
The old success condition only required an alive process and at least two fingerprints.

`tests/focusfp_check.py` rejects that artifact with exit 1 and
`holding Related never opened ItemMenu over Detail`. Changing the input to `detailsec=2` passes
the stronger check: Related card → ItemMenu over Detail → BACK to the same item, section and
column. Four harness tests pin rejection of missing navigation, the wrong host and lost return
identity. Flows 2 and 5 now also require their actual navigation/return sequences.

## Visual comparison baseline

Four 1920×1080 screenshots were captured and inspected from the starting simulator, using the
synthetic PMS and its flat-color artwork: movie hero, show's episode strip, Person, Filmography.
The movie retains its artwork/logo, metadata badges, rating, synopsis, playback facts, action row
and right-side people column. The show preserves separate still and text rows; Filmography uses
the existing two-column route layout, department strip and credit table.

The local artifacts are under `/tmp/plx-phase7-baseline.FxgO9Z/`; that temporary path is evidence
for this working session, not a committed fixture or a reproducible artifact location. The mock
server and four simulator processes were stopped after capture. No TV was used, and no simulator
rate is a device performance measurement.

## Migration verification

The mounted implementation at `269a5db9` passes the ARM cross-build (`make`), the shipping-feature
check, and `make sim`. The default Rust suite reports 2,454 passed, three failed and three ignored;
the failures were in replacement Detail tests, so this is not a green migration gate.

Four final-layout captures were inspected against the baseline. The comparison caught and drove
fixes for the movie's overlapping action/shelf bands, the show's episode-strip offset and missing
rating chips, and Filmography being frozen partway through opening in the host's cached backdrop.
Owned modal surfaces now draw live above that backdrop. These are simulator layout observations,
not LG rasterization or device performance results.

The content smoke run at that commit passes flows 2, 8 and 12, but **fails flow 5**: a short OK on a
Person card opens ItemMenu instead of Detail. The input path loses a same-frame key release before
the queued press-arm effect takes effect. Dedicated card/control tap regressions were added at
`e1de9ca0`; their failure and the subsequent fix must be checked before accepting navigation.
The flow-5 validator also now requires the same Person card identity and focus key after BACK,
not only the expected route sequence.

Replay anchor 1 is now explicitly refused by the loader: recorded state shape
`0x844664d233990e72`, current shape `0xb406de68526a5ede`. Unlike the starting build, this is observed
evidence requiring a shape-aware rerecord, not permission to rebaseline behavior.

The tap tests failed on that input path, then passed with `16d283b5`. The twelve focused input
tests pass, including genuine hold behavior. Rebuilt simulator flows 5, 8 and 12 also pass;
flow 5 checks both card identity and engine focus after return. Both existing anchors now report
the shape refusal explicitly, rather than waiting two minutes for a replay that never started.

Further review found requirements the happy-path run could not prove:

- Detail's repeated element keys encode positions, so reordering a list changes the item behind
  an unchanged key. The new `identity_tests` reproduce that and slot reuse after item removal.
- A retained Detail body can return while another item occupies the metadata store. Its saved
  focus then falls back to Hero before the matching data lands; saved-season hydration is also
  missing. The delayed-return and lifecycle-order tests reproduce these failures.
- The real focus engine can leave the episode strip horizontally and land on Hero. The stronger
  geometry test reproduces LEFT reaching Hero instead of clamping to the first episode. Hit-map
  tests now exercise visible intersections, clipped/offscreen cards and gutters in all three strips.
- Filmography's joined rows had become non-holdable controls. A long OK then activated them on
  release. The new hold-protocol test failed before `9d8cfbb2` and passes with rows restored to
  cards that decline the hold action.
- Filmography retained its own credit snapshot but did not hash that snapshot's activation targets
  or pending preview deadline. Both new hash regressions failed before `1ca1a8ee`; all sixteen
  focused Filmography tests pass after that fix. This changes its state shape again.
- Evicted entries' saved arguments and return state also need canonical hash coverage; a live
  body hash cannot cover state after its body has been removed.

## Final host/simulator checkpoint

The findings above were resolved with regression tests observed failing before their fixes.
Detail now interns full item identities; retained and evicted returns hydrate request-time memory,
preserve focus while the addressed source is loading, and reveal the restored row. Person handles
per-source pending media rather than treating its page-level `landed` flag as universal readiness.
The dispatcher rejects stale interactive delivery before a covered screen can cause side effects.
Cold entries' arguments and return payloads are included in canonical state.

The parallel suite also caught an unsafe test fixture: `bare()` began reading the singleton
metadata store without a held test lock. Detail and Filmography fixture constructors now require
borrowed serial guards; Person's global-reading tests were audited and already held the lock.
The hero pointer test now sweeps actual 2/3/4-control sets, asymmetric unfurl phases, and scroll.

The unused `ui/detail.rs`, `ui/person.rs`, and `ui/filmography.rs` files were retired at `82534370`.
They are recoverable in Git. No runtime consumer remained; the replacement tests stay compiled.

Checks:

- `make check` passed before and after retirement, including lint and all host harness gates.
  Default Rust suite: 2,489 passed, 3 ignored; hostsim: 2,510 passed, 3 ignored.
- Three additional consecutive default-suite runs passed after the fixture-race fix and hero sweep.
- Shipping-feature check (`CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path
  rust-modules/Cargo.toml --lib --no-default-features`) passed. ARM `make` and `make sim` passed.
- Content navigation flows 2, 5, 8 and 12 passed. Flow 6 now actually visits Privacy and Legal;
  its heartbeat-stage assertion rejects the previous script, which skipped Privacy.
- All committed recordings passed the synthetic-content check. Final state shape, independently
  derived and confirmed by the runtime test: `0x002cb89ee6a93668`.
- Anchor 1 replay: `frames=575 graded=5 diverged=0 present_diffs=0 verdict=SAME`.
- Anchor 6 replay: `frames=1218 graded=9 diverged=0 present_diffs=0 verdict=SAME`.
- New anchor 12 replay: `frames=926 graded=4 diverged=0 present_diffs=0 verdict=SAME`.
- Final 1920×1080 simulator captures of movie, episode strip, Person and Filmography were opened
  and inspected against the baseline. Artifacts: `/tmp/plx-phase7-verify.T1jf77/retired-visual/`.
- Final Python harness run after fixture updates passed (253 tests).

At the time of this measurement the product recorder still used the live-store replay path and
the explicit pointwise `--resolve` mode was unwired. It has since been implemented:
`tests/focusfp.sh --resolve` now grades product Focus/Hit answers pointwise before recorded
continuation. The phase-7 results themselves still make no adapter-independent playback claim.

Native capture is pending. The read-only TV-lock status probe reported the set unreachable.
No lease was acquired and no wake, deploy, launch, sound or panel command was sent. The owner was
asked whether to leave it asleep or allow a wake that might briefly light the panel; without a
new instruction, the night-mode constraint remains unchanged. Rendering with the backlight off
is accepted; this is a reachability/permission issue, not a requirement to light the panel.

## Native capture — 2026-09-09

The pending state above is superseded: the set was reachable this session, and the four screens
were captured (plus one follow-up, below) under the `tv-lock` skill's protocol — one lease per
capture group, deployed dev build, TV audio muted for the session, panel/backlight state left
untouched. Real household library items stood in for the simulator's synthetic PMS content;
no ratingKey, server address or other identifying value is recorded here, and no image was
committed — all five PNGs were inspected locally and left under `/tmp`.

- **Movie detail** (a real movie's `detail=<rk>` page, service capture at 1920x1080): backdrop
  artwork, title, the Movie/genre/content-rating/resolution/CC badge row, a three-source rating
  row, synopsis, the date/duration/Direct-Play playback-facts row, the Play-plus-secondary action
  row, and the right-side "Directed by / Starring" people column all render exactly as the
  baseline described, with the Cast & Crew shelf beginning to enter at the bottom edge of the
  viewport.
- **A single TV episode's own detail page** (`detail=<episode rk>`) turned out to render with the
  same section shape as a movie — title, season/episode badge, rating row, synopsis, air-date/
  duration/Direct-Play facts, actions, people column — and no episode strip, because a leaf
  episode is not a show container (`Detail::sections` only emits the season-tab and episode-strip
  blocks when `d.is_show`). The **show's own detail page** (`detail=<show rk>`, the show's
  ratingKey resolved off-device from the episode's `grandparentRatingKey`) is the one that opens
  on a show hero (logo art, a "Season 1" tab chip) and, two DOWN presses later, on the described
  "separate still and text rows": a thumbnail-plus-duration/progress-badge strip above a distinct
  title/synopsis/air-date/content-rating text block per episode, with the season's episode count
  and the Cast & Crew shelf visible beneath. This distinction was not obvious from the baseline
  wording alone and is worth carrying forward — "the show's episode strip" names the SHOW's
  detail page, not any one episode's.
- **Person**: the header (photo, role/born-died line, biography with a "MORE" affordance), the
  Filmography entry pill with its credit count, and the Movies/Shows shelves with per-card watched
  checkmarks all render as described.
- **Filmography**: the two-column layout — back-link, "Filmography" title and credit count on the
  left; a department strip (three tabs, e.g. Actor/Appearances/Other with counts) and a scrollable
  credit table (title, role, year) on the right — renders as described, with no partial or frozen
  backdrop from the host page it was opened over.

No defect was found on any of the five captures: no clipping, no overlap, no missing chips, and no
visible LG text-rasterization artifact — all text stayed crisp at 1080p on the physical panel. Two
non-content elements appear in every capture and are session artifacts rather than app defects: the
`devtools`-feature on-screen FPS counter (a bordered box, top-right — blank on a screen that had not
yet presented a fresh frame after boot, showing a real reading once the show-detail scroll caused
one) and the television's own persistent muted-speaker OSD icon, present because this session muted
system audio as the protocol requires.

Work remains on the bridge branch. Main was not changed or pushed; its independent Profiles and
capture changes must be preserved when integrating phases 6/7. Phases 8–12 have not been started
by this resumed run. Temporary worker checkouts were removed after clean-status checks; their
branches remain. The orphan-cache check found nothing to remove, with 17 GiB free after the fleet.
