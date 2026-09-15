# Detail-hero trailer UX: faster autoplay, a shrinking logo, and a full-trailer mode

**Status (2026-09-15): implemented.** All host-testable pieces below landed
(`rust-modules/src/{player/preview.rs, screens/detail/{mod.rs,hero.rs}, ui/{hero_logo.rs,
landing_hero.rs, detail_layout.rs}}`), `cargo test --lib` (default + `hostsim`) and
`--no-default-features` all green, three clippy lints clean. What shipped vs. what's still owed:

- **§0 (issue #74 prerequisite): turned out to be a false alarm, corrected in place** — see §0's
  own account and `docs/known-issues.md`. No code change was needed here; the fix was already on
  this branch under a different commit hash than the one that was checked.
- **§2.1 (dwell): shipped as the documented FALLBACK, not the investigated mechanism.**
  `DWELL_S` is `2.0` (from `4.5`). The decoupled fetch-start/reveal investigation was not carried
  out — it needs real device data this pass didn't have time to gather, and the constant's own doc
  comment says so and states what would replace it if that investigation happens later.
- **§2.2 (logo shrink), §2.3 (synopsis stays), §2.4 (full-trailer mode: focus collapse + scrim
  lift), §2.5 (DOWN collapses too): all implemented as specced**, including both eng-review fixes
  (the `reconcile`/`valid` focus-legitimacy gate, and the shared `collapse_full_trailer` helper).
- **§5 host tests: added** — `hero::focusable`/`visible_ctls` exhaustive sweep,
  `LogoRung::lerp`'s endpoint/monotonic/clamp tests, the BACK+DOWN collapse test. **Not added**: a
  `DetailScreen`-level integration test driving `reconcile`/`valid` against a genuinely live
  `player::preview::Machine` with `view().picture == true`. That would mean driving process-wide
  singleton state coupled to on-disk session I/O (`plex::session::peek()`) from a fast host test —
  a worse trade than the pure-function coverage already in place, which exercises the exact logic
  gap the review found (`hero::focusable`) exhaustively and deterministically. Also not added: the
  `preview_tick` full-state-table test and the dwell-trigger test from §5's "test-review
  correction" — same root cause (both need a live `view()`), noted here rather than silently
  dropped.
- **§5 device-only items (fps scene, real dwell pass, `hero_scrim_a` anchor-table row,
  `PROMOTED_FIELD`/`K_SCALE` tuning): not done** — no TV access in this pass. Tracked in
  `TODOS.md`.
- **T9 (budget-exhaustion observability): no code needed** — `Machine::picture()`'s existing
  `preview=` log line already carries `nop=` (which counts `BudgetSpent` refusals), so the data
  this task wanted surfaced was already there.

**Status (original).** Design plan, not yet implemented. Targets the existing background-trailer feature
(`e5739f9e feat: add trailer support and extras handling`, `rust-modules/src/player/preview.rs` +
`rust-modules/src/screens/detail/{mod.rs,hero.rs}`). Read `rust-modules/src/player/CLAUDE.md` and
`rust-modules/src/ui/CLAUDE.md` before implementing — this plan assumes both.

**Goal**, as given:

1. Reduce the dwell time before a trailer starts autoplaying in the background.
2. While it plays in the background, move the movie/show title (clearLogo) to the top-left and
   shrink it, smoothly.
3. Keep the short description/synopsis visible while it plays in the background.
4. Pressing UP while the trailer plays opens it "full": the logo and synopsis fade out, leaving
   only the Play/Resume button. BACK or DOWN returns to the background state.

Three explicit decisions were made with the requester before finalizing this plan (recorded so a
reviewer does not have to re-derive them):

- **Dwell target: ~2.0s perceived latency** (from 4.5s) — **eng review updated this**: the
  mechanism is now an investigation (decouple fetch-start from the visible reveal; see §2.1) with a
  straight `DWELL_S` cut to 2.0s as the documented fallback if that isn't worth the complexity.
- **The action row (Play/Restart/Trailer/Also available/watched toggle) stays fully visible**
  during background autoplay — it does not fade with the rest of the meta.
- **In full-trailer mode, focus collapses onto Play/Resume only** (the other controls leave the
  focus ring, not just the screen), **and the darkening layer over the video is also eased down**
  once only the Play/Resume button is left to protect, so the trailer reads brighter and cleaner.

---

## 0. Prerequisite — RESOLVED 2026-09-15: the fix is already present under a different commit hash

**Correction, made while starting implementation.** §0 originally said `feature-trailers` was
missing issue #74's crash fix, verified via `git merge-base --is-ancestor ac305265 HEAD` answering
"fix MISSING." That check was checking the wrong commit. `ac305265`'s exact content — confirmed
byte-identical via `diff <(git show ac305265:src/starfish.c) <(git show 6f3486d0:src/starfish.c)`
and the same for `rust-modules/src/webos.rs`, both producing no output — was independently
re-applied to `main` as commit **`6f3486d0`** (same author, same message, same timestamp), which
**is** an ancestor of current `feature-trailers`. The live `src/starfish.c` on this branch has the
full `g_load_returned` gate; `rust-modules/src/webos.rs` has the full `devjail`/`rtkmem` probe.
**No merge or cherry-pick is needed — this prerequisite is already satisfied.**

`docs/known-issues.md` (which originally sourced this finding) has been corrected in the same pass,
including a note that its own "empirically confirmed still crashes" device claim now contradicts
the code-level evidence and needs a fresh device session to resolve, not a silent overwrite. See
that file for the full account. **Cherry-picking `ac305265` was attempted and aborted** the moment
this was discovered (`git cherry-pick -n ac305265` produced conflicts across 20+ files, all from
unrelated drift, not from the fix being absent — confirming this was a false alarm rather than a
close call).

This does not change anything else in this plan — §2.1's dwell-frequency argument about
`CYCLE_BUDGET` stands on its own memory-ceiling logic regardless of this crash's status, and the
device-verification steps in §5 should still watch for k5lp/k3lp-shaped crashes as a matter of
course, just without treating one as expected/known going in.

## 1. What exists today (verified anchors)

The background-preview *machine* (`player/preview.rs`) is entirely reused — this plan changes
**timing and screen-side presentation**, not the playback state machine.

- `DWELL_S: f32 = 4.5` (`preview.rs:27`) — dwell required, while the hero has focus, unscrolled,
  before a preview `Load` is requested (`DetailScreen::preview_tick`, `mod.rs:2545-2590`).
- `Machine::view()` (`preview.rs:283-295`) returns a `View { art, prose, field, picture, playing }`.
  Before any frame is presented it is `View::STILL` (`art=1, prose=1, field=1`). Once the first
  frame is presented (`picture()`, `preview.rs:259`), it becomes
  `{ art: 0.0, prose: 0.0, field: PREVIEW_FIELD(1.35), picture: true, playing: true }` — **a hard
  cut in the state, eased on the screen side** (below).
- `DetailScreen` (`screens/detail/mod.rs`) owns four screen-local eased scalars that chase `view`'s
  fields at a fixed ~0.35s exponential rate (`ease()`, `mod.rs:2115-2120`):
  `preview_art` (poster alpha, drives `draw_backdrop`'s art texture, `mod.rs:1706-1739`),
  `preview_prose` (identity line + ratings + synopsis + facts + people, all as **one** alpha,
  `mod.rs:1780`), and `preview_chrome` (title/logo **and** the whole action row, `mod.rs:1779`,
  `1810`). There is no screen-local scalar for `field` yet — `draw_backdrop` reads
  `preview: crate::player::preview::View` (the raw, un-eased value) directly (`mod.rs:1759`).
- **UP** promotes (`mod.rs:1419-1434`): only while focus is on the hero (`Located::Hero`) and
  `view.picture` is true, it sets `preview_promoted = true`. `preview_tick` then targets
  `chrome_target = 0.0` (`mod.rs:2576-2580`), which — because `chrome` also gates the whole action
  row — fades **everything** (logo and every button) to zero. The controls stay in the focus ring
  the whole time (nothing removes them), so OK still activates whatever was focused before the
  fade, invisibly.
- **BACK** un-promotes (`mod.rs:1403-1418`): while `preview_promoted`, BACK clears the flag and
  consumes the key instead of leaving the page. There is currently no DOWN handling for this —
  DOWN falls through to ordinary focus navigation.
- `landing_hero::PREVIEW_FIELD = 1.35` (`landing_hero.rs:12`) is a **multiplier on the hero-scrim
  wedge only** (`hero_scrim(p, visible * preview.field, …)`, `mod.rs:1757-1761`); the bottom-anchored
  base gradient (`detail_layout::base_scrim_a`, drawn just above it, `mod.rs:1740-1756`) is **not**
  keyed to preview state at all — it only tracks scroll-driven hero visibility (`visible`).
- Cost model, from `preview.rs`'s own module doc (`preview.rs:15-18`): `CYCLE_BUDGET: u32 = 14`
  admitted Loads per app process, derived from measured RSS headroom (~958 KiB) divided by
  `sf_load`'s fixed 64 KiB slot — **not a tunable**, a device memory ceiling. Every dwell that
  actually starts a Load spends one, permanently, for the life of the process (`Machine::admit`,
  `preview.rs:197-205`). There is also a process-wide breaker that opens (and stops all further
  previews) after one admitted Load fails before showing a frame (`fail_admitted`, `preview.rs:230
  -241`), and a per-item negative-fact cache (`Fact::NoExtra`/`RefusedDirect`) so a known-bad item
  is never retried in the same process.

## 2. The state model this plan adds

Four states, keyed off the same two bits the code already tracks (`view.picture`,
`self.preview_promoted`) plus the (unchanged) dwell/idle/scrolled logic in `preview_tick`:

```
                    hero unfocused / scrolled off / view.picture=false
              ┌───────────────────────────────────────────────────────────┐
              │                                                           │
              ▼                                                           │
      ┌───────────────┐   dwell timer reaches       ┌──────────────────┐  │
      │  Idle/browsing │   the target (§2.1) ──────▶ │  request_preview │  │
      │  or Dwelling   │                             │  (Load admitted) │  │
      └───────┬────────┘                             └────────┬─────────┘  │
              │  ▲                                             │           │
   hero loses │  │ hero (re)focused,                  view.picture         │
   focus, or  │  │ unscrolled, no                     becomes true         │
   view fails │  │ preview yet                          (first frame)      │
   before a   │  │                                             ▼           │
   frame ─────┘  │                                  ┌─────────────────────┐│
                 └──────────────────────────────────│  Background          ││
                                                      │  autoplay            │
                                                      │  art→0, logo→top-left,
                                                      │  synopsis STAYS,      │
                                                      │  action row STAYS,    │
                                                      │  field=1.35           │
                                                      └──────┬─────┬────────┘│
                                                             │     ▲         │
                                                     UP      │     │  BACK / │
                                                (focus=Hero) │     │  DOWN   │
                                                             ▼     │         │
                                                      ┌─────────────────────┐│
                                                      │  Full trailer        ││
                                                      │  logo+synopsis→0,    ││
                                                      │  ONLY Play/Resume    ││
                                                      │  visible+focusable,  ││
                                                      │  field→PROMOTED_FIELD││
                                                      └──────────┬──────────┘│
                                                                 │           │
                                                     EOS / failure / item    │
                                                     changes underneath ─────┘
                                                     (view.picture → false;
                                                      preview_promoted is
                                                      unconditionally cleared)
```

**Recommended code-comment site:** this diagram (or a trimmed version of it) belongs beside
`DetailScreen::preview_tick` in `mod.rs` — it is the one function that reads both `view.picture`
and `self.preview_promoted` to drive every target in the table below, and per your stated
preference for inline diagrams on non-obvious state transitions, this is exactly that case.

Table form of the same four states, with every visual channel spelled out:

| State | Trigger | Poster art | Logo | Synopsis | Other meta (identity/ratings/facts/people) | Action row | Scrim/`field` |
|---|---|---|---|---|---|---|---|
| **Idle / browsing** | default, or hero not focused, or scrolled off | 1.0 (full still) | Hero rung, hero position | visible | visible | visible | 1.0 |
| **Dwelling** | hero focused, unscrolled, no preview yet, dwell timer running | 1.0 | Hero rung, hero position | visible | visible | visible | 1.0 |
| **Background autoplay** | `view.picture == true`, `!preview_promoted` | **0.0** (art fully yields to video) | **Compact rung, top-left**, animated in | **stays visible** (new) | **fades to 0** (unchanged) | **stays visible** (decision: unchanged from today) | 1.35 (unchanged — protects the now-larger amount of text: logo + synopsis) |
| **Full trailer** | `view.picture == true`, `preview_promoted == true` | 0.0 | fades to 0 (from wherever the compact rung left it) | **fades to 0** (new — previously already 0, now explicit) | 0 (unchanged) | **only Play/Resume visible and focusable**, everything else leaves the row | **eased down** to a low residual (new — see §2.4) |

Transitions:

- Idle/Dwelling → Background autoplay: unchanged mechanism, just a shorter dwell (§2.1).
- Background autoplay → Full trailer: **UP**, unchanged trigger condition (focus on hero,
  `view.picture`). No change needed here beyond what already exists.
- Full trailer → Background autoplay: **BACK (unchanged) or DOWN (new, §2.5)**.
- Background autoplay / Full trailer → Idle: `view.picture` goes false (EOS, failure, or the hero
  loses focus / scrolls off), exactly as today — `preview_promoted` is already cleared whenever
  `!view.picture` (`mod.rs:2587-2589`), so a session ending while promoted cannot strand the app in
  a state where nothing is focusable.

### 2.1 Faster autoplay — investigate decoupling fetch-start from reveal before committing to a raw dwell cut

**Outside-voice cross-model tension, resolved: investigate before implementing.** The original
framing here was "one constant change" (`DWELL_S` 4.5 → 2.0), and that framing is exactly what let
its real cost — multiplying `CYCLE_BUDGET` consumption in lockstep with dwell frequency — pass as
a footnote rather than the central question. The felt latency the user is actually asking to
shrink is `dwell + Load time` (network fetch + demux + first frame), and a raw dwell cut is only
one of the two levers on that sum.

**The investigation, before writing any code:**

1. Can `player::preview::request_start` (`Machine::start`, `preview.rs:143-170`) be called at a
   SHORTER "fetch-commit" threshold than the visual "reveal" — i.e., start the Load once the viewer
   has plausibly committed to an item (a short debounce, well under today's 4.5s), while the screen
   keeps showing the full Idle presentation (poster, full logo, full meta) for a separate, slightly
   longer minimum reveal delay, so the visible transition to Background autoplay never happens
   before roughly today's target feel (~2.0s) even if the frame is ready sooner? If Load time is a
   meaningful fraction of total latency, `max(fetch-commit, reveal-delay, Load time)` beats
   `dwell + Load time` serially, without multiplying the Load-*trigger* rate the way a blanket
   dwell cut does.
2. **The real cost this trades for**, which the outside voice raised and did not fully weigh
   either: a shorter fetch-commit threshold means a Load can start on an item the viewer glances
   past and moves on from almost immediately — via the EXISTING cancellation path
   (`preview_tick`'s `occupies() && (!hero || scrolled_off) && !preview_promoted` guard,
   `mod.rs:2554`, which already calls `ContentReq::PreviewStop`/`Machine::abandon` when focus
   leaves), that Load still counted against `CYCLE_BUDGET` the moment it was **admitted**
   (`Machine::admit`, `preview.rs:197-205` — budget is spent on admission, not on reaching a
   picture). A short fetch-commit threshold could spend cycles on browsed-past items FASTER than
   today's single 4.5s dwell does, not slower. The investigation needs to measure or reason through
   whether a realistic fetch-commit threshold (e.g. 0.8-1.2s) meaningfully reduces false-starts
   relative to today's single-timer 4.5s, or whether it's a wash.
3. **Fallback if the investigation finds it isn't worth the complexity or the timeline**: the
   simple single-timer cut, `DWELL_S: f32 = 2.0` (from 4.5), is still the documented default this
   plan falls back to — same code change, same budget-consumption argument, same mitigations
   (per-item negative-fact cache avoids a second Load on a revisited item; the process-wide breaker
   stops further Loads after one admitted Load fails outright). Nothing else in this plan (§2.2-2.5)
   depends on which mechanism wins — they all key off `view.picture`/`preview_promoted`, not off
   how or when the Load was triggered.

**Budget observability (decision: silent degradation, logged for real data — not a UI signal
yet).** Whichever mechanism ships, add the two budget-adjacent events already emitted on the
`preview=` line (`nop=`, which counts every refusal including `BudgetSpent`) to wherever a device
session can see them without a special capture — this is the input a future decision about a
user-visible "previews unavailable this session" signal would need, and there is none of that data
today. Do not design that UI now; the arithmetic (a faster trigger could exhaust 14 cycles in
roughly half the cumulative dwell time a session needs today, i.e. inside an ordinary five-minute
browsing session rather than only a very long one) says this is worth watching, not that it is
already a confirmed problem worth a UI surface.

### 2.2 Logo: shrink to top-left while the trailer plays

This needs new geometry — `detail_layout::COMPACT_TITLE_BOT` and `LogoRung::Compact` already exist,
but for a **different** treatment (the scroll-driven, horizontally **centered** pinned title at
`mod.rs:1987-2016`, which answers "how far down have I scrolled", not "is a trailer playing"). Reusing
its rung *sizing* (`COMPACT_AREA`/`COMPACT_H_MIN`/`COMPACT_H_MAX`, `theme.rs:249-263`) is right; reusing
its *position* is not — this is a new anchor, top-left, independent of scroll.

**New geometry** (`detail_layout.rs`):

```rust
/// Top-left anchor for the logo while a trailer plays in the background — independent of scroll.
/// Sits inside the safe-area margins, above where the hero text column would start.
pub(crate) const PREVIEW_LOGO_X: f32 = MARGIN_X;              // 96
pub(crate) const PREVIEW_LOGO_Y: f32 = MARGIN_Y;              // 54 — top safe-area edge
pub(crate) const PREVIEW_LOGO_MAX_W: f32 = 480.0;             // keeps a wide wordmark from
                                                               // reaching toward center screen
```

**New animated scalar**, following the file's existing pattern exactly (`preview_art`/`preview_prose`
/`preview_chrome`, `mod.rs:101-105`): a screen-local `preview_logo: f32` in `[0, 1]`
(0 = full hero position/size, 1 = fully collapsed to the top-left compact spot), whose *target* is
`1.0` while `view.picture && !preview_promoted`, else `0.0`.

Unlike the alpha scalars, this one drives a **geometric transform** (position + scale), so it
should NOT reuse the linear `ease()` helper — `ui/CLAUDE.md`'s idle-gate rule is explicit that this
app has exactly two motion integrators and anything else invisible to `note_spring` is a standing
hazard (`ui/CLAUDE.md`, the `idle.rs` row). Use a critically-damped `crate::ui::Spring` instead.
**Scope decision (eng review): reuse `ui::consts::K_SCALE` (320.0, `consts.rs:445`) rather than
minting a new `K_PREVIEW_LOGO` constant.** `K_SCALE` is already this app's shared rate for "a UI
element's own frame changing size/position in place" (the control focus-pop spring,
`ui/CLAUDE.md`'s `widgets.rs` row), which is a closer analogue to a logo relocating+shrinking than
either of the ~300 menu-unfurl rates. Minting a bespoke constant before a device pass has set it is
a speculative number this plan cannot justify yet; if the device pass in §5 finds `K_SCALE` reads
wrong for this transform specifically, promote it to its own named constant **then**, with the
measured reason attached — not before.

**Draw-time interpolation** (`draw_hero`, `mod.rs:1770-1791`, replacing the current fixed
`Rect::new(MARGIN_X, TITLE_BOTTOM - band, HERO_TEXT_W, band)`):

```rust
let hero_band = crate::ui::hero_logo::band_h(LogoRung::Hero);
let hero_rect = Rect::new(MARGIN_X, TITLE_BOTTOM - hero_band, HERO_TEXT_W, hero_band);
let compact_band = crate::ui::hero_logo::band_h(LogoRung::Compact);
let compact_rect = Rect::new(
    detail_layout::PREVIEW_LOGO_X,
    detail_layout::PREVIEW_LOGO_Y,
    detail_layout::PREVIEW_LOGO_MAX_W,
    compact_band,
);
let t = self.preview_logo; // 0..1, spring-driven
let rect = hero_rect.lerp(compact_rect, t); // x, y, w, h each lerp independently
let rung = if t > 0.5 { LogoRung::Compact } else { LogoRung::Hero }; // see below
```

Two things to get right, both because `HeroLogo`'s own module doc says sizing is **constant-area,
not a height clamp** (`hero_logo.rs:1-24`) — so this is not a naive `Rect` tween:

- **`HeroLogo::fit` must be called with the INTERPOLATED column width**, not a step function
  between the two rungs' bounds — the area/floor/ceiling solve already takes `col_w` as its last,
  overriding clamp (`hero_logo.rs`'s three-clamp order), so feeding it the lerped rect's width at
  every frame produces a continuous size change for free. Do not snap `LogoRung` at `t > 0.5`;
  instead give `HeroLogo::fit` a **continuously interpolated** `(area, h_min, h_max)` triple between
  `LogoRung::Hero`'s and `LogoRung::Compact`'s bounds (`hero_logo.rs:41-55`) — this needs a small
  addition to `hero_logo.rs`, a `LogoRung::lerp_bounds(a, b, t)` free function or an added
  `LogoRung::Custom(area, h_min, h_max)` variant, rather than a discontinuous jump partway through
  the spring's travel. **This is new surface on `hero_logo.rs` and belongs there**, per `ui/CLAUDE.md`
  rule 4 ("improve a component before forking one") — do not reimplement the area-solve in
  `screens/detail`.
- **Layout ≠ paint still applies**: the band a caller reserves in its flow is the rung's floor
  (`band_h`), and a taller-than-floor logo spills upward as pure paint (`hero_logo.rs:57-63`). The
  hero's own below-hero flow (`ScrollColumn`, `ui/CLAUDE.md`'s `detail.rs` row) does not need to
  know about this transform at all, because nothing in the below-hero flow is anchored to the
  logo's position — confirm this with a host test (§5) rather than by inspection, since the
  original scroll-driven compact title's clearance was itself the subject of a prior regression
  test (`home.rs`'s `the_home_hero_logo_never_reaches_the_top_bar`).

**Sequencing decision (outside voice, confirmed): land this addition to `hero_logo.rs` as its own
isolated, separately-verified step before building §2.3/§2.4 on top of it.** `hero_logo.rs` is a
SHARED component — verified by grep, not assumed: `screens/home/mod.rs`, `screens/home/tests.rs`,
`ui/widgets.rs`, `ui/mod.rs`, `ui/theme.rs`, and `app/adapters/poster.rs` all reference
`hero_logo`/`HeroLogo`/`LogoRung` beside `screens/detail/mod.rs`. Home's hero draws its own
clearLogo through the same `LogoRung::Hero` rung this plan's interpolation touches. Land the
`lerp_bounds`/`Custom`-variant addition alone, with a host test proving `LogoRung::Hero`'s and
`LogoRung::Compact`'s EXISTING call sites (both heroes, the pinned compact title) produce
byte-identical output before/after (the `t=0`/`t=1` bit-for-bit test above already covers this in
spirit; make it explicit that it runs against Home's actual call site, not just a synthetic one),
plus one `tools/capture-screen.sh`/simulator screenshot of Home's hero logo unchanged. Only once
that is verified does the rest of §2 build on it — if it turns out to regress Home, the bisect
should point at one small, isolated commit, not at "the trailer feature."

**Alpha**: the logo keeps drawing at `chrome` alpha (currently 1.0 through background autoplay,
unaffected by this plan's decision to leave the action row alone) until full-trailer mode fades it
with everything else (§2.4).

### 2.3 Synopsis stays; the rest of the meta still clears

Today, `prose` is **one** alpha covering the identity line, ratings, synopsis, and facts/people
(`mod.rs:1780, 1797-1809`). Splitting the synopsis out is the one code change this item needs:

```rust
// mod.rs — DetailScreen fields
preview_prose: f32,      // identity line, ratings, facts, people — unchanged target/behavior
preview_synopsis: f32,   // NEW — synopsis only

// preview_tick — targets
ease(&mut self.preview_prose, view.prose, dt)          // unchanged: 0 once picture, 1 otherwise
ease(&mut self.preview_synopsis, synopsis_target, dt)  // NEW
// synopsis_target = 0.0 while preview_promoted && view.picture, else 1.0
// i.e. synopsis stays through background autoplay and only fades in full-trailer mode

// draw_hero
let synopsis_alpha = p.alpha(self.preview_chrome * self.preview_synopsis);
synopsis_view.draw(synopsis_alpha, …);   // was `prose`
```

**This is a legibility regression risk, and `DESIGN.md` already names the exact reason to take it
seriously**: "Still artwork has a knowable worst case, which is what the PMS `blur` hash is for.
Video does not." (`DESIGN.md:84-85`). Today, no prose is ever drawn over a *playing* trailer — only
the title survives, and titles get the full clearLogo/fallback+shadow treatment plus the boosted
`PREVIEW_FIELD` wedge. A 3-line `LABEL` (26px) synopsis block is materially harder to keep legible
over an arbitrary, unknown, moving frame than a big display-weight title is. Two concrete
follow-ups this plan is not allowed to skip:

1. **`widgets::hero_scrim_a`'s anchor table is a graded contract** (`ui/CLAUDE.md`'s `widgets.rs`
   row, `DESIGN.md:68-71`) and currently has no row for "synopsis text over a bound video plane at
   `field=1.35`". **Scope decision (eng review): this row is a fast-follow verification task, not a
   blocker for this PR** — it needs a real device and real trailer content to mean anything (a host
   test can assert the arithmetic is internally consistent, but "is `field=1.35` actually enough
   contrast against real footage" is not a question a desk review or a host test can answer; see
   §5's Fast-follow list). Land the code with the existing `PREVIEW_FIELD` value, capture the
   device pass immediately after, and if it fails at 1.35 the field boost for this state gets its
   own (probably higher) constant rather than silently tuning `1.35` upward for every consumer of
   that constant (it also protects home's own preview treatment, sharing `landing_hero.rs`).
2. **Cap it, don't reflow it.** `hero_synopsis`/`hero_syn_h`'s 3-line cap (`ui/CLAUDE.md`'s
   `mod.rs` row) already exists and should be kept exactly as-is here — do not grow the synopsis
   block to "use the space the meta line vacated." A longer, denser text block over unpredictable
   video content is a worse bet at every one of the three lines, not a better one at the third.

### 2.4 Full-trailer mode: only Play/Resume, focus collapses, scrim lifts

Three changes to `preview_tick`/`draw_buttons`/`draw_hero`, all gated on the existing
`preview_promoted && view.picture` predicate — no new trigger condition.

**(a) Logo and synopsis fade with the rest of chrome** (mostly already true — logo already faded
via `chrome_target = 0.0`; synopsis now needs the same target the identity/ratings/facts/people
group already gets, which §2.3's `synopsis_target` expression already states).

**(b) The action row keeps only Play/Resume, and focus does not merely hide the rest — it leaves
the ring.** This needs two SEPARATE pieces, not one function trying to do both jobs (eng review
finding — see below for why the original one-function framing was wrong):

**(b.1) What gets DRAWN and ENUMERATED (paint + group extent).**
`hero::hero_ctls`/`hero_btn_rect_at` (`hero.rs:152-174, 333-351`) already build the row from a
`HeroSet` each frame; **do not add a "promoted" flag to `HeroSet` and special-case it inside
`hero_ctls`** — that struct's whole point is describing what the ITEM offers (restart point,
trailer availability, alt source, watch state), not transient UI mode. Instead, add a pure helper
`hero::visible_ctls(set, full_trailer: bool) -> ([HeroCtl; 5], usize)` that returns just
`[HeroCtl::Play]` when `full_trailer` and `hero_ctls(set)` otherwise, and call it from
`draw_buttons` (`mod.rs:1906-1985`, replacing its `hero::hero_ctls(set)` call at `mod.rs:1938`) and
from `Focusable::groups` (`mod.rs:771-803`, which currently computes `hero_n`/`hero_last`/the
group's `len` from the unfiltered `hero::hero_ctls(set)` at lines 773-783 — verified by reading the
function directly).

**(b.2) What is FOCUSABLE (`reconcile`/`valid`) — a separate, verified fix, not covered by (b.1).**
`DetailScreen::reconcile` (`mod.rs:1084-1126`) and `DetailScreen::valid` (`mod.rs:1215-1230`) are
the ONLY two places the engine decides whether a wanted/current focus target is legitimate, and
**both currently check the control against `self.hero_set()` (the item's real facts) with zero
awareness of `preview_promoted`** — quoted directly: `reconcile`'s Hero branch
(`mod.rs:1103-1107`) is `if let Some(Located::Hero(ctl)) = self.locate(want.elem) { let set =
self.hero_set(); if hero::index_of(set, ctl).is_some() { return want; } ... }`, and `valid`'s Hero
arm (`mod.rs:1218`) is `Located::Hero(c) => hero::index_of(self.hero_set(), c).is_some()`. Neither
one shrinks when full-trailer mode does, so if focus was on Restart/Trailer/Alt/the watch toggle
the instant UP promoted, both functions keep saying that focus is fine — the engine's `want` stays
on the now-invisible control. That is "keep it focusable but invisible," the option explicitly
**rejected** in the design review, not "collapse focus onto Play/Resume only." (`place`,
`mod.rs:980`, needs no separate change — it already opens with
`self.locate(*key).filter(|l| self.valid(*l))?`, so fixing `valid()` makes it stop
placing/hit-testing a hidden control for free.)

The fix: add the same guard to both of the two checks above —
`!(self.preview_promoted && crate::player::preview::view().picture) || ctl == HeroCtl::Play`
— gating the existing `hero::index_of(set, ctl).is_some()` test in each. `reconcile`'s watch-toggle
special case (`mod.rs:1108-1114`) must fall under the same guard (a watch toggle that was focused
before promoting must ALSO fall through to the terminal `HeroCtl::Play` fallback at
`mod.rs:1122-1125`, not redirect to whichever face currently exists — that redirect exists for a
different problem, the toggle changing FACE, not for full-trailer mode hiding it entirely). Once
both are gated, un-promoting (BACK/DOWN) needs nothing extra: the very next `reconcile` call, run
with `preview_promoted` now false, re-evaluates the previously-wanted elem against the item's real
`HeroSet` exactly as it does today — if that control still exists, focus returns to it; if not
(e.g. the resume point it depended on was consumed while promoted), the existing terminal fallback
already lands on Play, which is the right answer either way.

**Rationale, recorded (outside voice flagged that this decision's cost wasn't written down
anywhere).** Collapsing to Play/Resume only means Restart/Also-available/mark-watched, and a
resume-point restart, are unreachable without first backing out of full-trailer mode — a real
product cost, traded deliberately for full-trailer mode reading as an actual immersive trailer
view rather than a video playing behind a still-cluttered action row. This was an explicit choice
made in the design review (not a default this plan invented), and it stands as scoped; a resume
point itself is never at risk of being lost or altered by entering/leaving full-trailer mode — it
lives on the server/PMS side, unaffected by which hero controls are drawn.

**(c) The scrim over the video eases down.** New screen-local scalar `preview_field: f32`
(alongside `preview_art`/`preview_prose`/`preview_synopsis`/`preview_chrome`), targeting
`view.field` normally (1.0 idle, 1.35 once `picture`) but a new, lower constant once promoted:

```rust
// landing_hero.rs
/// How much of the preview wedge survives once only the Play/Resume pill needs protecting —
/// full-trailer mode. The pill already carries its own legibility treatment
/// (`ControlGround::Unkeyed`, `ui/CLAUDE.md`'s widgets.rs row) independent of any scrim, so this
/// is not load-bearing for the one thing left on screen; it exists so the transition from
/// "background" to "full" reads as the ambient darkening lifting, not as a hard cut.
pub(crate) const PROMOTED_FIELD: f32 = 0.4;
```

`draw_backdrop` switches its one remaining raw read of `preview.field` (`mod.rs:1759`) to
`self.preview_field`. Tune `0.4` on a device pass against real trailer content — start conservative
(closer to `1.35` than to `0.0`) and lower it only after confirming the Play/Resume pill's label
stays legible at whatever brightness real trailers hit, since — unlike the synopsis case above —
there is no existing anchor-table row this can be checked against at all; it needs a new one.

### 2.5 DOWN also collapses full-trailer mode

**Code-quality decision (eng review): factor the collapse itself out instead of duplicating the
BACK arm's body.** The existing BACK arm (`mod.rs:1411-1414`) and a naively-added DOWN arm would be
four identical lines twice — extract:

```rust
/// Un-promotes full-trailer mode if it was active. Returns whether it fired, so a caller can
/// decide whether to also consume the key.
fn collapse_full_trailer<H: ContentLike>(&mut self, fx: &mut Effects<'_, H>) -> bool {
    if !self.preview_promoted {
        return false;
    }
    self.preview_promoted = false;
    fx.invalidate(Provenance::Input);
    true
}
```

Both the existing BACK arm and the new DOWN arm call it and consume the key only on `true`:

```rust
// BACK arm (mod.rs:1403-1418), replacing its inline preview_promoted check
if matches!(input.kind, InputKind::Key { key: Key::Back, edge: Edge::Down, .. }) {
    if self.collapse_full_trailer(fx) {
        return Handled::Yes;
    }
    self.content(fx, ContentReq::Back);
    return Handled::Yes;
}
// New DOWN arm, same call site, guarded the same way (consuming the key so ordinary
// DOWN-into-episodes/seasons navigation cannot also fire on the same press)
if matches!(input.kind, InputKind::Key { key: Key::Down, edge: Edge::Down, .. })
    && self.collapse_full_trailer(fx)
{
    return Handled::Yes;
}
// fall through to ordinary DOWN handling — unchanged
```

Order matters: this arm must run **before** whatever resolves ordinary DOWN into the focus engine's
`neighbour`/`EdgeRule` walk (`ui/CLAUDE.md`'s `focus.rs` row) for the same reason the BACK arm
already does — a `Screen::key` implementation's own early-return arms take priority over the
generic engine resolution the container falls back to.

## 3. Interaction & focus edge cases

- **No trailer at all**: `HeroSet::trailer` stays `false` (`hero.rs:159-162`) and `view.picture`
  never becomes true, so none of this plan's new states are reachable — unchanged.
- **Trailer ends (EOS) while in full-trailer mode**: `Machine::eos`→`stopped` clears `view.picture`
  (`preview.rs:271-281`), which already clears `preview_promoted` unconditionally
  (`mod.rs:2587-2589`) — the screen falls straight back to Idle with the poster, full logo, full
  meta, and full action row restored (all four eased scalars retarget to `1.0`/`0.0` as appropriate
  and `visible_ctls` stops filtering). No new logic needed; add a host test pinning it (§5), since
  it is exactly the kind of transition that reads fine in the promoted state and wrong on exit.
- **Item changes underneath a live preview** (navigating away from the hero while promoted, a
  season/episode swap, a server data refresh landing): `preview_tick`'s existing
  `crate::player::preview::occupies() && (!hero || scrolled_off) && !self.preview_promoted` guard
  (`mod.rs:2554`) already stops the preview when focus leaves the hero, **but it explicitly does
  not** while `preview_promoted` — that was already true before this plan and stays true; leaving
  the hero row is not possible while full-trailer mode owns focus (nothing outside `HeroCtl::Play`
  is in the ring), so this guard's `!self.preview_promoted` clause is dead in the new focus-collapse
  world in one direction (focus literally cannot leave) but still live for BACK/DOWN un-promoting
  and then continuing to navigate away in the same session.
- **Scrolling during full-trailer mode**: not reachable — focus is pinned to the hero row's one
  surviving control, and nothing scrolls the page without moving focus into the below-hero content
  first.
- **Very short or looping trailers**: unrelated to this plan; `preview.rs`'s EOS handling is
  unchanged.

## 4. Motion & idle-gate obligations

Every animator in this app must report its own motion to `ui::idle` or it silently freezes on a
settled screen (`ui/CLAUDE.md`'s `idle.rs` row — this is not a style note, it is how the whole app
decides whether to keep presenting frames at all). The existing `ease()` calls in `preview_tick`
already do this (`fx.note(PresentEvent::Motion)`, gated on any of the `|`-combined `ease()` calls
returning `true`, `mod.rs:2581-2586`); the new `preview_synopsis` and `preview_field` scalars must
be folded into that same `|` chain, and the new `preview_logo` **spring** must report through
`crate::ui::idle::note_spring` exactly like every other spring in the app (`Xfade::tick` and
`Spinner::draw`'s history in `ui/CLAUDE.md`'s idle row is the cautionary tale for what happens when
a new animator is not wired into this — it ships silently frozen and nothing catches it in a host
test). Add a settle test for `preview_logo` immediately, in the shape of
`hero.rs`'s existing `the_trailer_unfurl_spring_reports_while_opening_and_is_quiet_at_rest`
(`hero.rs:1639-1685`) — that test already exists for the trailer disc's own unfurl spring and is
the literal template to copy.

## 5. Verification

Per `AGENTS.md`'s testing rule, this ships in the order: **reproduce the current behavior in a
host test → confirm the new behavior fails against unmodified code → implement → confirm it
passes → keep the test.** Route by `which-tier`.

**Host (`make check`, no TV, run first and always)**

**Test-review correction:** `screens/detail/tests.rs` (2215 lines, 48 existing `#[test]`s, verified
by grep) has **zero** tests touching `preview_dwell`, `preview_promoted`, or any eased preview
scalar beyond the two fixture-default field initializations — the trailer-preview feature
(`e5739f9e`) shipped with no `DetailScreen`-level test coverage of its own state machine at all
(`preview.rs`'s own `Machine` is well-tested; the screen wrapping it is not). This plan's tests
below are therefore not "add one more case to an existing suite" in several places — they are the
first coverage this state machine has ever had, which raises their priority rather than lowering it.

- **[NEW, test-review finding]** A `DetailScreen`-level dwell-trigger test: seed focus on Hero,
  unscrolled, item has a playable trailer, step `preview_tick` for `dt` sums just under `DWELL_S`
  (2.0s) and assert no `request_preview` fired; step past it and assert one did. This is the one
  path §2.1's dwell-value change actually walks through, and it had no test before this plan either.
- `hero_ctls` vs. a new `hero::visible_ctls`: the existing exhaustive sweep tests
  (`the_row_offers_exactly_one_watched_toggle`, `the_row_s_controls_never_overlap_at_any_set_size`,
  §5 of `hero.rs`'s test module) already parametrize over every `HeroSet`; add `full_trailer: bool`
  as one more swept axis and assert `visible_ctls(set, true)` is always exactly `[Play]`.
- A `preview_tick`-level test asserting the full state table in §2: for each of the four states,
  the four (now five, with `preview_synopsis`) eased targets equal the table's row, and — the
  regression this plan exists to prevent — that promoting and then EOS-ing lands back at exactly
  the Idle row, not some intermediate mix.
- **[CRITICAL, test-review finding, protects Issue 2's fix]** A `reconcile`/`valid`/`place` regression
  test: seed focus on `HeroCtl::Restart` (item has a resume point), promote (`preview_promoted =
  true`, `view.picture` true); assert `reconcile(want=Restart)` now returns `Play`'s `FocusKey`,
  `valid(Located::Hero(Restart))` is `false`, and `place(Restart's elem)` returns `None`. Then
  un-promote and assert focus returns to `Restart` (still in the item's `HeroSet`) — and a second
  case where the resume point was consumed while promoted, asserting the fallback lands on `Play`
  instead of a now-nonexistent `Restart`. Without this test, the fix from Issue 2 has nothing
  standing between it and a silent regression the next time this code is touched.
- **[NEW, test-review finding — no pre-existing test to mirror]** `collapse_full_trailer` and its two
  callers: a case where `preview_promoted` is true and BACK fires it (`Handled::Yes`,
  `preview_promoted == false`); the same for DOWN; and a case for each where `preview_promoted` is
  already false, asserting BACK still falls through to `ContentReq::Back` and DOWN still falls
  through to ordinary navigation, unchanged. (The plan originally described this as "mirroring
  whichever existing test covers the BACK arm" — there isn't one; verified by grep.)
- `HeroLogo`'s new interpolated-bounds path: a host test at `t = 0.0`, `0.5`, `1.0` confirming
  monotonic width/height and that `t=0`/`t=1` reproduce today's `LogoRung::Hero`/`Compact` output
  bit-for-bit (a regression here is invisible on-screen at the extremes and only shows mid-travel).
- `preview_logo`'s spring settle/report test, per §4.

**Device (blocking — there is no host *runtime* for text rasterization, GLES composition, or real
trailer video, `ui/CLAUDE.md`'s "when you're done" section; this repo treats the device as the real
gate for anything that moves pixels)**

- An `fps:`-style scene (`ui/CLAUDE.md`'s FPS-gate section) exercising the logo's move+shrink
  transform end to end — reuse `plxnative-navosc`'s pattern of a headless, repeating trigger rather
  than inventing a new one, since this app's `Spring`-driven pops are already graded this way
  (`K_DISC_UNFURL`'s own disc-unfurl scene is the nearest existing analogue). Needs both a
  `fps_floor` (proves the spring still animates) if the app ever adds a still-settled variant, and
  a `worst_ceiling_ms`/`coldopen_ceiling_ms` check since this transform runs on the item that just
  mounted the hero, right where `coldopen` measurements already live.
- A real-device pass at **2.0s dwell** across an ordinary browsing session (arrow through a shelf
  at a normal human pace) to sanity-check the false-trigger and budget-consumption risk named in
  §2.1 empirically, not just from the source-arithmetic argument in `preview.rs`'s own doc.
- A basic `tools/capture-screen.sh` still of each of the four states, on a real trailer, confirming
  nothing is grossly broken (wrong position, wrong alpha, focus landing somewhere invisible) —
  this is the ordinary "does it look right" pass every UI change gets, independent of the deeper
  legibility grading below.

**Fast-follow (device-only, explicitly NOT blocking this PR — Step 0 scope decision)**

- Add `hero_scrim_a`'s new synopsis-over-video anchor-table row (§2.3) and grade it, plus tune
  `PROMOTED_FIELD` (§2.4c), against real trailer content on a real device. Both need a TV and real
  footage to mean anything — a host test can only assert the arithmetic is internally consistent,
  not that it reads as legible against unknown video — so gating the code on this session happening
  first buys nothing but delay. Land the code with the stated default values, capture this
  immediately after on the next device session, and file a fast-follow PR for anything it finds
  wrong.

## 6. Files touched

| File | Change |
|---|---|
| `rust-modules/src/player/preview.rs` | `DWELL_S` 4.5 → 2.0 |
| `rust-modules/src/ui/landing_hero.rs` | new `PROMOTED_FIELD` constant |
| `rust-modules/src/ui/detail_layout.rs` | new `PREVIEW_LOGO_X/Y/MAX_W` constants |
| `rust-modules/src/ui/hero_logo.rs` | new interpolated-bounds path for a continuous Hero↔Compact transform (§2.2) |
| `rust-modules/src/ui/consts.rs` | new `K_PREVIEW_LOGO` spring rate |
| `rust-modules/src/screens/detail/hero.rs` | new `hero::visible_ctls` (full-trailer control filter), its host tests |
| `rust-modules/src/screens/detail/mod.rs` | new `preview_logo` (Spring), `preview_synopsis`, `preview_field` fields; `preview_tick` target logic for all three plus the split `prose`/`synopsis` targets; `draw_hero`/`draw_backdrop`/`draw_buttons` reading them; new DOWN key arm; focus-collapse wiring through `reconcile`; all associated host tests |
| `rust-modules/src/ui/widgets.rs` (`hero_scrim_a`'s test module, wherever the anchor table lives — see `ui/CLAUDE.md`'s `widgets.rs` row) | new anchor-table row for synopsis-over-bound-video legibility (§2.3) |

## 7. Explicitly out of scope

- Anything about **which** trailer plays, trailer selection/ranking, or the extras data model —
  unchanged, `metadata::Extra`/`Detail::trailer()` as today.
- Home's own hero preview (`ui/CLAUDE.md`'s `screens/home/mod.rs` row's `Backdrop`) is a
  **different** preview surface sharing only `landing_hero.rs`'s pure geometry; this plan does not
  touch it, and `PROMOTED_FIELD`/the logo-shrink transform have no Home analogue since Home's hero
  has no "full trailer" mode to promote into.
- Sound/mute controls — unchanged; the module doc's existing note stands ("Sound stays on... the
  Settings toggle is the only sound control, and that is a platform limit," `preview.rs:12-13`).
- Raising `CYCLE_BUDGET` — explicitly not part of this plan (§2.1); a future change to it needs its
  own RSS-headroom re-measurement, not a paragraph here.
- A hard build/CI gate enforcing issue #74's fix is present — considered (outside voice raised it),
  declined in favor of the documented prerequisite in §0 (eng review decision).
- A user-visible signal for `CYCLE_BUDGET` exhaustion — considered (outside voice raised it),
  declined until real device-session data (from the logging added in §2.1) shows it's a common
  enough experience to warrant UI, not just a theoretical one.
- The decoupled fetch-start/reveal investigation in §2.1 finding that the simple dwell cut is
  actually fine — that's the fallback path already described, not a separate scope item.

---

## GSTACK REVIEW REPORT

### Design-dimension ratings (0-10, on the ORIGINAL request vs. this finished plan)

| Dimension | Original request | This plan | What closed the gap |
|---|---|---|---|
| Interaction completeness | 3/10 — 4 bullets, no state model, no exit-key symmetry (BACK only, no DOWN), no focus behavior for controls that disappear | 9/10 | Full 4-state table (§2), DOWN added for symmetry (§2.5), focus-collapse mechanism named down to which existing primitive (`reconcile`) it must route through (§2.4b) |
| Legibility / accessibility | 2/10 — "keep the synopsis" with no acknowledgment that video has no legibility guarantee, per this project's own DESIGN.md | 8/10 | Named the exact contract (`hero_scrim_a`'s anchor table) that must gain a row before this ships, and split synopsis from the rest of the meta as its own alpha channel rather than silently making the existing `prose` cutoff optional |
| Motion design | 4/10 — "smooth animation" with no rate, no integrator named, no idle-gate wiring | 8/10 | Named a critically-damped `Spring` (not the linear `ease()` already in the file) with its own `K_*` constant, and an explicit `ui::idle::note_spring` wiring requirement with the exact prior incidents (`Xfade`, `Spinner`) this class of bug has already caused in this codebase |
| System-level risk awareness | 1/10 — dwell reduction treated as a pure timing tweak | 7/10 | Named the `CYCLE_BUDGET` 14-Loads-per-process ceiling as a real consequence of a faster dwell, quantified why it is not a tunable, and scoped a real-device empirical check into the verification plan instead of a source-only argument |
| Visual hierarchy across states | 2/10 — logo and synopsis specified, action row and scrim never mentioned | 8/10 (button-row and scrim behavior settled via decision; residual 2 points are the `PROMOTED_FIELD`/`K_PREVIEW_LOGO` numeric tuning, explicitly deferred to a device pass rather than guessed) | The three-question decision pass surfaced two undecided states (button row during background autoplay; the darkening layer in full mode) that the original 4 bullets did not address at all |
| Reuse over invention | 5/10 — request implies bespoke logo positioning | 9/10 | Traced every new piece to the nearest existing primitive it must extend rather than fork: `LogoRung`/`HeroLogo::fit`'s area-solve (extended, not reimplemented), `hero_ctls`'s existing enumeration pattern (filtered, not special-cased), `DetailScreen::reconcile`'s existing "focused control vanished" handling (reused, not re-invented), the trailer-disc unfurl spring's own settle test (copied as the template for the new one) |
| Test-plan completeness | 0/10 — none specified | 9/10 | Full host-test list per new piece of state plus explicit device-only items with the reason each is device-only (§5), following this repo's own "reproduce → fail → fix → pass, keep the artifact" rule from `AGENTS.md` |

### Adaptation note (read before treating this report as the standard `/plan-design-review` shape)

This skill's default workflow assumes a **visual/marketing-style** design surface and drives a
live mockup-generation + comparison-board + outside-model-critique loop before rating. This task is
almost entirely an **interaction and motion spec against an existing, densely-documented native
TV codebase** with no analogous web/marketing surface to mock up meaningfully (the gstack designer
renders HTML/PNG; this app's real constraints — a fixed 1920x1080 canvas, SDL2_ttf rasterization, a
hardware video plane GL cannot read, a spring-based idle-gate — do not exist in that medium, and a
generated PNG of "a shrunk logo in the corner" would not surface any of the actual risks this report
found). Given that mismatch, this review substituted **direct verification against this
project's own architecture documents and source** (`AGENTS.md`, `DESIGN.md`, `ui/CLAUDE.md`,
`player/preview.rs`, `screens/detail/{mod.rs,hero.rs}`, `hero_logo.rs`) for the mockup/comparison-
board/outside-model-critique steps, and used AskUserQuestion only for the three decisions that were
genuinely unresolved by the request and not answerable from the code (dwell target; whether the
action row fades during background autoplay; whether full-trailer mode collapses focus and lifts
the scrim). No design-completeness score above was assigned by guessing — each row cites the
specific document or source location it was checked against.

### Runs

| Check | Status |
|---|---|
| Scope gate (user-named target: the pasted feature bullets, per this skill's own B-exception) | Applied |
| Codebase reconnaissance (`preview.rs`, `screens/detail/{mod.rs,hero.rs}`, `hero_logo.rs`, `landing_hero.rs`, `DESIGN.md`, `ui/CLAUDE.md`) | Done |
| 3-question decision pass (dwell target / button-row behavior / full-mode focus+scrim) | Done — answers folded into §2 |
| Design-dimension rating + gap-closing rewrite | Done (table above) |
| Visual mockup generation / comparison board | Skipped — see Adaptation note |
| Outside-voice (Codex) design critique | Skipped — no visual surface to critique; the equivalent check performed here was tracing every new primitive against this repo's own stated component-reuse rule (`ui/CLAUDE.md` rule 4) |

### VERDICT

Plan is implementation-ready pending two on-device numeric tunings explicitly deferred rather than
guessed: `K_PREVIEW_LOGO` (the logo's move/shrink spring rate) and `landing_hero::PROMOTED_FIELD`
(how much darkening survives in full-trailer mode). Both are called out at their definition sites
in §2.2/§2.4(c) with the device pass that must set them.

**UNRESOLVED DECISIONS:**
- Exact numeric value of `K_PREVIEW_LOGO` (recommended starting point 220.0, per §2.2) — needs a
  device `fps:`-scene pass, not a desk decision.
- Exact numeric value of `landing_hero::PROMOTED_FIELD` (recommended starting point 0.4, per
  §2.4c) — needs a device capture against real trailer content, not a desk decision.
- Whether `hero_scrim_a`'s anchor table can hold a synopsis-weight (`LABEL` 26) text block at
  `field=1.35` at all, or whether background-autoplay needs its own, higher field constant separate
  from `PREVIEW_FIELD` (§2.3) — needs the anchor-table test to actually be written and run before
  this is known either way.
