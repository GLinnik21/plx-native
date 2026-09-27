# Detail-hero trailer UX: faster autoplay, a shrinking logo, and a full-trailer mode

**SUPERSEDED IN PART (2026-09-17): full-trailer mode now draws trailer CONTROLS, and the action
row is gone.** Everything below about dwell, the logo shrink, the synopsis and the collapse keys
still stands. What no longer holds, wherever this document says it (§2.4, the state diagram and
table in §1, §8.3, §8.4, the goal's bullet 4 and the third decision bullet):

- Full-trailer mode does NOT leave "only the Play/Resume pill" on screen. It takes the WHOLE page
  off — the action row with it — and draws a trailer transport in its place: the `Trailer` kicker
  over a title (`trailer::transport_title` — the FILM/SHOW title by default, so a trailer whose own
  PMS title is the boilerplate "Trailer" doesn't repeat the kicker word right under it; the extra's
  own title only when it says something the kicker doesn't), the playbar with elapsed/remaining,
  and the play/pause state read-out, built from `ui::player_hud`'s own
  `draw_scrim`/`draw_title`/`draw_playbar`. There is still no quality, subtitle, audio or Info
  control and no track menu — that part of "there is no HUD" was never about the transport, it was
  about the panels that write session state a preview has none of.
- `hero::focusable`'s Play-only narrowing survives as the mode's FOCUS ANCHOR, not as something
  drawn. §8.4's "Play/Resume stays visible and focused" is now "Play stays focusABLE".
- OK and PLAYPAUSE pause and resume the preview session (`player::preview::transport`, performed by
  the loop through `ContentReq::PreviewTransport`). LEFT/RIGHT do NOT seek: every non-in-place seek
  path falls back to a fresh Starfish `Load` that `preview::Machine` never admitted, and a failed
  reload would arm the process-wide breaker. They reveal the controls instead, which auto-hide on
  the player HUD's own 4.5 s linger.
- The wedge's `landing_hero::PROMOTED_FIELD` (0.4) is **deleted**. With no page ink left to protect
  in full-trailer mode, `preview_field` targets 0.0 like the base scrim, and the only scrim in that
  state is the transport's own at the bottom. §2.4(c)'s device-tuning item and its `TODOS.md` entry
  go with it.
- Background autoplay gained a hint — an up-chevron key cap reading "`[^] Full screen`" (glyph
  first, no `Press`/`for` filler — shortened 2026-09-17) under the action row
  (`screens::detail::trailer::hint_shown`), fading in with the picture and out on promotion or
  collapse.

The current record is `rust-modules/src/screens/detail/trailer.rs`, `player/preview.rs`'s module
doc and `hero.rs`'s `focusable`/`visible_ctls`.

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
   only the Play/Resume button. BACK or DOWN returns to the background state. *(2026-09-17: the
   Play/Resume button leaves too, and a trailer transport takes its place — see the banner above.)*

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
  *(2026-09-17: the pill is no longer drawn either, so the darkening goes to zero rather than to a
  residual — see the banner above.)*

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
                                                      │  ALL chrome→0,       ││
                                                      │  trailer transport   ││
                                                      │  up; Play = the      ││
                                                      │  focus anchor only,  ││
                                                      │  field→0 (2026-09-17)││
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

| State | Trigger | Poster art | Logo | Synopsis | Other meta (identity/ratings/facts/people) | Action row | Scrim/`field` (wedge) | Bottom scrim (§8.3) |
|---|---|---|---|---|---|---|---|---|
| **Idle / browsing** | default, or hero not focused, or scrolled off | 1.0 (full still) | Hero rung, hero position | visible | visible | visible | 1.0 | 1.0 (`base_scrim_a`'s own scroll-driven value) |
| **Dwelling** | hero focused, unscrolled, no preview yet, dwell timer running | 1.0 | Hero rung, hero position | visible | visible | visible | 1.0 | 1.0 |
| **Background autoplay** | `view.picture == true`, `!preview_promoted` | **0.0** (art fully yields to video) | **Compact rung, top-left**, animated in | **stays visible** (new) | **fades to 0** (unchanged) | **stays visible** (decision: unchanged from today) | 1.35 (unchanged — protects the now-larger amount of text: logo + synopsis) | 1.0 (unchanged — still protecting the action row) |
| **Full trailer** | `view.picture == true`, `preview_promoted == true` | 0.0 | fades to 0 (from wherever the compact rung left it) | **fades to 0** (new — previously already 0, now explicit) | 0 (unchanged) | **nothing drawn** (2026-09-17: the row fades out with the rest of the chrome; Play stays the focus ANCHOR, and the trailer transport is drawn over the top) | **eased to 0.0** (2026-09-17: was a `PROMOTED_FIELD` residual) | **eased to 0.0** (§8.3, follow-up pass — was unwired at first ship, see §8) |

Transitions:

- Idle/Dwelling → Background autoplay: unchanged mechanism, just a shorter dwell (§2.1). **Since
  §8.2 (follow-up pass): also requires `preview_started_for != Some(preview_cache_rk())`** — this
  item's trailer must not already be the one that finished naturally this visit, or the dwell gate
  stays closed instead of re-triggering. See §8.2 for the full mechanism and the exact tick-ordering
  bug an eng review found and fixed in it.
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

> **Superseded 2026-09-17** (banner at the top of this file): the action row is not drawn in this
> mode at all, the wedge eases to 0.0 rather than to `PROMOTED_FIELD`, and the trailer's own
> transport is what the viewer sees. (a) and (b)'s focus mechanism still stands as the mode's
> anchor; (c)'s residual value does not.

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

## GSTACK REVIEW REPORT — 2026-09-15 (hero UX pass)

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

---

## 8. Follow-up polish pass (2026-09-15): section order, play-once, scrim fade

**Status: planned, not yet implemented.** Four small, independent fixes to the shipped trailer/
extras feature above — three requested, one verified rather than assumed while touching the same
code. Nothing in §1-§7 changes; this section only reorders, adds suppression state, and wires one
previously-unwired scrim. Three explicit decisions were made with the requester before finalizing
this section (recorded so a reviewer does not have to re-derive them):

- **Play-once suppression is scoped per visit**, not per app-session — a new field lives on
  `DetailScreen`, keyed to the current item, and resets whenever you leave and re-enter that item's
  detail page (or swap season/episode). The alternative (reusing `preview.rs`'s existing per-item
  negative-fact cache, which lives for the app process's lifetime) was rejected because it would
  mean a trailer that already played once literally never autoplays again for that item until the
  app restarts — a UX cost bigger than the simplicity it buys.
- **The bottom scrim (`base_scrim_a`) fades all the way to fully transparent** in full-trailer mode,
  not to a residual floor — matching "fades out" literally, and consistent with the existing
  rationale for the wedge's own `PROMOTED_FIELD`: once only the Play/Resume pill is left on screen
  (which already carries its own legibility treatment independent of any scrim, `landing_hero.rs:14
  -21`), nothing at the bottom needs protecting.
- **Play/Resume staying visible and focused in full-trailer mode needed no new code** — verified
  directly against source (below, §8.4), not assumed from the "implemented" status line at the top
  of this doc. It already works; the gap found was a missing regression test, not missing behavior.

### 8.1 Move Extras below Cast & Crew

**Current order** (`sections()`, `screens/detail/mod.rs:627-655`): the function builds the visual
stack by walking `Season → Episode → Extras → Cast → Related → About` (the `Extras` check at
`mod.rs:639-642` runs, and is inserted, **before** the `Cast` check at `mod.rs:643-646`). `SectionId`
itself (`section.rs:12-22`) is an identity enum indexed for layout-cache lookups, not the draw
order — the doc comment there is explicit that "visual order is a separate list," which is exactly
this function.

**Change**: swap the two blocks so `Cast` is appended before `Extras`. New order:
`Hero → Season → Episode → Cast → Extras → Related → About`.

```rust
// sections() — swap these two blocks
if d.credits_len() > 0                 { Cast }
if !d.extras.is_empty()                { Extras }
```

**Two things to re-verify, not just assume, once the swap lands:**

- `section.rs:44-47`'s `is_hide_anchor` set (`Cast`, `Related`, `About` — the sections the compact
  title's appear/disappear threshold anchors to) is defined by `SectionId` membership, not by
  position relative to `Extras`, so moving `Extras` should not change which sections are anchors or
  when the compact title appears. Confirm this with the existing pinning test (below) rather than
  by inspection, since `section.rs:44-47`'s own comment ("Extras is not one: inserting it must not
  move the compact title") was written for the OLD position (between Episode and Cast) and needs
  its wording updated to describe the new one (between Cast and Related) even if the underlying
  invariant still holds.
- `extras.rs`'s Extras shelf itself is unaffected — it still draws every `metadata::Extra` the item
  has via `Extra::caption()` (trailer, behind-the-scenes, featurette, scene, deleted scene,
  interview, extra). This is purely a layout-order change; no data-selection logic moves.

**Test update** — `screens/detail/tests.rs:1868-1940`,
`extras_sit_after_episodes_and_do_not_move_the_compact_title`: rename to reflect the new position
(e.g. `extras_sit_after_cast_and_crew_and_do_not_move_the_compact_title`) and update both fixed
arrays:

```rust
// show case: was &[0, 1, 2, 6, 4, 3, 5] (Hero, Season, Episode, Extras, Cast, Related, About)
assert_eq!(&sections[..n], &[0, 1, 2, 4, 6, 3, 5], "extras sits after Cast and before Related");
// movie case: was &[0, 6, 4, 3, 5] (Hero, Extras, Cast, Related, About)
assert_eq!(&sections[..n], &[0, 4, 6, 3, 5]);
```

### 8.2 Trailer plays only once per visit

**Root cause, confirmed against source, not assumed from the symptom.** `Machine::eos()`
(`preview.rs:284-288`) only moves `Phase::Playing → Phase::Stopping`; the actual teardown
(`after_pump`, `preview.rs:454-460`) resets the machine fully to `Phase::Idle`
(`note_stopped()`, `preview.rs:290-294`) with no replay-suppression bit anywhere — the module's own
doc comment (`preview.rs:5-10`) states this is deliberate for the *re-dwell* case ("a second dwell
on an item that has a trailer starts from the beginning again"). `DetailScreen::preview_tick`'s
`can_dwell` gate (`mod.rs:2613-2675`) has no memory of a prior play either. So after a natural EOS,
`occupies()` returns `false`, `view.playing` is `false`, no negative `Fact` was recorded (those
guard *known-bad* trailers, not *already-finished-successfully* ones), and — if the viewer is still
sitting on the unscrolled, non-promoted hero — `can_dwell` becomes `true` again almost immediately,
the 2.0s dwell timer re-accumulates, and a fresh `request_preview` replays the same trailer from the
start. This is the exact "plays more than once" symptom, and it is a genuine gap: no test in
`screens/detail/tests.rs` or `preview.rs`'s own test module pins EOS→re-dwell behavior at all.

**Fix — a visit-scoped, item-keyed "already played" flag, set only on natural completion.**
**Key type, decided (eng review, issue 1A): reuse `preview_cache_rk()`, not a new concept.**
`DetailScreen::preview_cache_rk()` (`mod.rs:2691-2693`) already resolves "the correct key for
whatever this hero is currently previewing" — the trailer extra's own rk when one exists, else
`self.rk` — and its own doc comment (`mod.rs:2685-2690`) already states `preview_tick`'s dwell gate
must check the negative-fact `blocked()` cache against exactly this key, not `self.rk`
unconditionally. Using anything else for play-once suppression would risk it disagreeing with the
existing blocked-cache key on exactly the cases (an on-deck episode with its own trailer) that key
exists to get right.

```rust
// DetailScreen fields, alongside preview_dwell/preview_promoted (mod.rs:99-118)
preview_played_for: Option<String>,   // the preview_cache_rk() this hero already autoplayed a
                                       // trailer to completion for, this visit — single slot, not
                                       // a history (eng review, issue 1B: bouncing between two
                                       // episodes and back within one visit can replay each once
                                       // more per return; accepted as reasonable, not a bug)
preview_started_for: Option<String>,  // the preview_cache_rk() captured at the MOMENT
                                       // request_preview() starts a Load — NOT re-resolved later,
                                       // so an item swap underneath a live full-trailer session
                                       // can never misattribute "played" to the wrong item
                                       // (outside-voice finding, folded into the fix below)
preview_had_picture: bool,            // last tick's view.picture, to detect the true→false edge
```

**Two bugs an eng review's outside-voice pass found in the first draft of this fix, both closed
below — read this before implementing, not just the final snippet.** The first draft's natural-
completion check read `self.preview_promoted` near the TOP of `preview_tick`, but
`preview_promoted` is only cleared at the very LAST line of the function (`if !view.picture {
self.preview_promoted = false; }`, `mod.rs:2672`) — so on the exact tick a trailer finishes WHILE
in full-trailer mode, the top-of-function read still sees last tick's `true`, the guard fails, and
the trailer that was most likely actually watched (immersive full-trailer completion) never gets
suppressed. The second bug: re-resolving `preview_cache_rk()` fresh at the moment EOS is detected
means an item swap underneath a live full-trailer session (season/episode change while promoted)
could attribute "played" to whichever item happens to be current BY THEN, not the one that actually
finished. Both are closed by (a) capturing the key at Load-start time instead of at EOS-detection
time, and (b) a corrected condition that accounts for `preview_promoted`'s real clear-timing.

**Shared boolean, factored out (eng review, issue 2A — DRY): `hero_active`.** The three-term
condition "hero focused AND not scrolled off" appears, in one form or another, at the existing
abandon-trigger (`mod.rs:2622`), the existing `can_dwell` gate's first two conjuncts
(`mod.rs:2627-2628`), and now this fix's own natural-completion check. Extract it once:

```rust
// preview_tick, right after computing hero/scrolled_off (mod.rs:2619-2621)
let hero_active = hero && !scrolled_off;
```

The existing abandon trigger becomes `occupies() && !hero_active && !self.preview_promoted`
(algebraically identical to today's `occupies() && (!hero || scrolled_off) && !preview_promoted` —
De Morgan on the negated pair), and `can_dwell`'s first two conjuncts become `hero_active &&
!self.preview_promoted && ...`.

**Set `preview_started_for` where the Load actually starts** (`request_preview`, `mod.rs:2695`):

```rust
fn request_preview<H: ContentLike>(&mut self, fx: &mut Effects<'_, H>) {   // &mut self now
    self.preview_started_for = Some(self.preview_cache_rk());
    // ...unchanged body below (resolves extra, sends ContentReq::PreviewStart)
}
```

**The corrected natural-completion check — fires whenever `view.picture` is lost WITHOUT the
screen itself having just caused it**, which happens in exactly two shapes: sitting on an active,
unpromoted hero when EOS lands (the original target case), or being in full-trailer mode when EOS
lands (promoted mode structurally excludes the abandon path — the abandon trigger itself requires
`!preview_promoted` — so ANY picture-loss while promoted can only be natural EOS or an item swap
underneath, and `preview_started_for`'s captured-at-start key makes even that swap case attribute
correctly):

```rust
// preview_tick, using preview_promoted's value from the END of last tick — i.e. BEFORE this
// tick's own clear at mod.rs:2672 — which is exactly what "was full-trailer mode active" means
if self.preview_had_picture && !view.picture && (self.preview_promoted || hero_active) {
    if let Some(started_for) = self.preview_started_for.take() {
        self.preview_played_for = Some(started_for);   // natural completion — suppress re-dwell
    }
}
self.preview_had_picture = view.picture;
```

The gate, added to `can_dwell`:

```rust
let already_played = self.preview_played_for.as_deref() == Some(self.preview_cache_rk().as_str());
let can_dwell = hero_active
    && !self.preview_promoted
    && !view.playing
    && !crate::player::preview::occupies()
    && !already_played   // NEW
    && !blocked;
```

Whenever the current item changes underneath (season/episode swap, a different detail page
mounted), `preview_cache_rk()` no longer matches the stored `preview_played_for`, so `already_played`
evaluates `false` and the new item's trailer can autoplay — no separate reset call needed for THAT
case, same pattern the per-item negative-fact cache in `preview.rs` already relies on.

**Reset on leaving the page (eng review, outside-voice finding #2 — confirmed against source).**
`ScreenEvent::WillLeave(Leave::ForGood) | ScreenEvent::Unmount` (`mod.rs:1513-1524`) already
explicitly zeroes `preview_dwell`/`preview_promoted` on leaving the page — the established
convention for every per-visit preview field, and itself evidence `DetailScreen` instances are
pooled/reused across visits rather than freshly constructed (why else reset fields on leave?). The
two new fields must join it or the "resets whenever you leave and re-enter" scoping this section
opened with (issue 1B's decision) silently doesn't hold:

```rust
// mod.rs:1513-1524, add alongside the existing preview_dwell/preview_promoted resets
self.preview_played_for = None;
self.preview_started_for = None;
```

**Explicitly unaffected**: manual trailer playback from the Extras shelf or the hero's `Trailer`
disc control (`hero::disc_verb`, `hero.rs:292-301`) is a different playback path entirely (the main
`player` module, not `player::preview`'s background machine) and shares no suppression state with
this fix — pressing the Trailer control still plays the trailer on demand, every time, as today.

**Considered and deferred (eng review, outside-voice finding #3 — overcomplexity):** moving this
detection into `player::preview::Machine` itself as a one-shot `just_finished` flag, set at
the `Machine::eos` transition before `Machine::stopped` wipes the session's key (the `note_eos()`/`note_stopped()` of this record's time), would eliminate this fix's ordering
fragility at the source rather than patching around it, and would benefit any future consumer of
trailer-completion signal, not just this screen. Not chosen for this PR — it touches a shared
module other code paths depend on, a larger blast radius than a screen-local fix for what is
fundamentally a screen-local suppression feature — but recorded in `docs/TODOS.md` for if this ever
needs a second consumer.

### 8.3 Bottom scrim animates with full-trailer mode

**Current state, confirmed against source.** `draw_backdrop` (`mod.rs:1762-1824`) draws two
overlapping-but-distinct dark layers, both gated on `visible = hero_alpha(self.scroll.pos,
HERO_FADE)`:

- `detail_layout::base_scrim_a(SCR_H, visible)` (`detail_layout.rs:44-47`) — the bottom-anchored
  base gradient (the "bottom dark opacity"). Its only inputs are screen height and scroll-driven
  `visible`; it has **no** `preview`/`preview_promoted`/`preview_field` parameter at all, so
  entering or exiting full-trailer mode (which doesn't move `self.scroll.pos`) leaves it completely
  unaffected today — it is a hard function of scroll position, not eased against any preview-state
  target.
- `widgets::hero_scrim` (the corner wedge) — already multiplied by `self.preview_field`
  (`mod.rs:1815`), so IT already eases correctly between the idle/background values and
  `PROMOTED_FIELD = 0.4` in full-trailer mode. This part of the original plan's §2.4(c) is verified
  shipped as documented. The wedge is not part of this fix; only the base gradient is.
  *(2026-09-17: the wedge now eases to 0.0 in full-trailer mode and `PROMOTED_FIELD` is deleted —
  there is no page ink left up there to protect.)*

**Fix**: a new eased scalar, alongside `preview_field`, targeting full transparency in full-trailer
mode:

```rust
// DetailScreen fields (mod.rs:99-118)
preview_base_scrim: f32,   // 1.0 = normal (base_scrim_a's own value), 0.0 = fully faded out

// preview_tick, folded into the same target-computation block as preview_field (mod.rs:2644-2661)
let base_scrim_target = if full_trailer { 0.0 } else { 1.0 };
// fold into the existing `|`-combined ease() chain (mod.rs:2581-2586) so idle-gate motion
// reporting sees it — see §4's obligation, unchanged by this addition
| ease(&mut self.preview_base_scrim, base_scrim_target, dt)
```

```rust
// draw_backdrop (mod.rs:1807-1810) — multiply the existing alpha by the new scalar
theme::scrim(crate::ui::detail_layout::base_scrim_a(SCR_H, visible) * self.preview_base_scrim),
```

Initial value `1.0`, so idle and background-autoplay states render byte-identical to today until
full-trailer mode actually engages — this is purely additive to the existing draw call, not a
behavior change outside the new state transition. Uses the file's existing linear `ease()` pattern
(0.35s time constant, same as `preview_field`/`preview_chrome`/`preview_synopsis`) rather than a
`Spring`, consistent with `ui/CLAUDE.md`'s rule that alpha fades use `ease()` and only geometric
transforms (position/scale) use `Spring` — this is an opacity multiplier, not a transform.

### 8.4 Play/Resume stays visible and focused in full-trailer mode — already correct, missing only its regression test

> **Superseded 2026-09-17** (banner at the top of this file): Play stays FOCUSABLE — it anchors the
> engine while the mode is up — but nothing in the row is drawn any more. The regression test this
> section added still grades exactly the right property under its new name.

**Verified directly against source, not assumed from this doc's "implemented" status line** (the
same diligence the scrim item above needed, since that one turned out to be only half-implemented
despite the doc's claim). `hero::focusable(ctl, full_trailer)` (`hero.rs:181-183`) is
`!full_trailer || ctl == HeroCtl::Play`, and `hero::hero_pill_label` (`hero.rs:311-318`) already
makes `HeroCtl::Play` show "Resume" whenever the item has a resume point and "Play" otherwise — so
`HeroCtl::Play` **is** the "Play/continue watching" control the request names, not a separate thing.
Every call site that decides what's drawn, enumerated, or focusable routes through this one
predicate consistently: `groups` (`mod.rs:797`), `draw_buttons` (`mod.rs:1984`), `reconcile`
(`mod.rs:1100-1139`, which explicitly redirects any non-`Play` hero focus to
`hero::HeroCtl::Play.elem()` as the **first** check in the function, before the
`return_pending`/`restore_intent` short-circuits can hand back a stale target), and `valid`
(`mod.rs:1252-1259`). No code change is needed for this requirement.

**What's actually missing**: the original plan's §5 called this exact path "CRITICAL" and specified
a regression test — seed focus on `HeroCtl::Restart` (a resume-point item), promote to full-trailer,
assert `reconcile`/`valid`/`place` all redirect/reject to `Play`, then un-promote and assert focus
returns to `Restart`. Grepping `screens/detail/tests.rs` confirms only the BACK/DOWN
collapse-trigger test exists (`back_and_down_both_collapse_full_trailer_mode_and_are_a_no_op_otherwise`,
`tests.rs:1176`); the `reconcile`/`valid` redirect test was never added. Since this pass is already
touching `preview_tick` and `draw_backdrop` in the same file, add the missing test here rather than
leaving a correct-but-unguarded code path — per the project's own testing culture, a correct
behavior with no regression test is one accidental refactor away from breaking silently.

### 8.5 Files touched

| File | Change |
|---|---|
| `rust-modules/src/screens/detail/mod.rs` | swap Cast/Extras order in `sections()` (§8.1); shared `hero_active` local + new `preview_played_for`/`preview_started_for`/`preview_had_picture` fields + gate/set logic in `preview_tick`, `request_preview` capturing `preview_started_for`, both new fields added to the `WillLeave`/`Unmount` reset block (§8.2); new `preview_base_scrim` field + ease target + fold into idle-gate `\|` chain (§8.3); `draw_backdrop` multiplies `base_scrim_a` by `preview_base_scrim` (§8.3) |
| `rust-modules/src/screens/detail/section.rs` | update `is_hide_anchor`'s stale comment describing Extras' old position (§8.1) |
| `rust-modules/src/screens/detail/tests.rs` | rename + update the section-order pinning test, plus a new `compact_title_hide_pos` pixel-identity regression test (§8.1, eng review issue 3A); new play-once tests: natural-EOS suppression (both the sitting-still case AND the full-trailer-mode-completion case the eng review's outside-voice pass found missing), abandon-then-return still replays, item-change resets, leave-and-return resets via the `WillLeave`/`Unmount` path (§8.2); new scrim-fade test: `preview_base_scrim` eases to 0/1 correctly and reports idle-gate motion (§8.3); new `reconcile`/`valid`/`place` Restart→Play regression test (§8.4) |

### 8.6 Verification

Per `AGENTS.md`'s testing rule: reproduce the current (buggy or unwired) behavior in a host test
first, confirm it fails against unmodified code, implement, confirm it passes, keep the test.

**Host (`make check`, run first and always)**

- `sections()` reorder: the renamed pinning test (§8.1) — write it against the swapped order,
  confirm it fails on unmodified code (since the arrays literally differ), then land the swap.
  **New (eng review, issue 3A):** a second test asserting `compact_title_hide_pos`'s resolved
  `section_top()` pixel value is byte-identical before and after the swap, for both the movie and
  show fixtures — closes the exact hazard class the `detail-sections-array-position-traps` prior
  learning flagged (verified already retired at the `LayoutCache`/`Spot` level by commit
  `2a227a94`, but with no regression test pinning it until now).
- Play-once (§8.2), five cases — **two more than the original draft, both added after an eng
  review's outside-voice pass found the first draft's fix silently missed them:**
  1. A `DetailScreen`-level test simulating a full dwell→admit→picture-true tick sequence, then a
     natural `view.picture` true→false transition while still hero-focused/unscrolled/unpromoted —
     assert `preview_played_for` is now `Some(preview_cache_rk())` and that stepping the dwell timer
     past `DWELL_S` again does **not** call `request_preview`.
  2. **[NEW — closes the ordering bug]** The same sequence, but the natural completion happens
     WHILE `preview_promoted` is `true` (full-trailer mode) — assert `preview_played_for` is set
     correctly even though `preview_promoted` hasn't cleared yet on this tick. Written to fail
     against the uncorrected condition (`!self.preview_promoted` read before the function's own
     clear) before passing against the corrected one (`self.preview_promoted || hero_active`), per
     this repo's reproduce-fail-fix-pass rule.
  3. The same sequence but with the transition happening while `scrolled_off` (simulating the
     existing abandon path) — assert `preview_played_for` stays `None` and a subsequent re-dwell
     **does** fire `request_preview`, pinning that the pre-existing "restart from beginning on
     re-focus" behavior is unchanged.
  4. After natural completion, swap to a different item (new `preview_cache_rk()`) and assert the
     new item's dwell gate is unaffected by the previous item's suppression.
  5. **[NEW — closes the attribution bug]** An item swap while `preview_promoted` is `true` and a
     Load is in flight — assert that when EOS lands, `preview_played_for` is set from
     `preview_started_for` (the item that actually started playing), not from a fresh
     `preview_cache_rk()` read (which would now resolve to the swapped-in item).
  6. **[NEW — closes outside-voice finding #2]** `WillLeave(Leave::ForGood)`/`Unmount` with
     `preview_played_for`/`preview_started_for` set — assert both reset to `None`, so re-entering
     the same item's detail page autoplays its trailer again, matching this section's own "resets
     whenever you leave and re-enter" decision.
- Scrim fade (§8.3): a `preview_tick`-level test asserting `preview_base_scrim`'s target is `0.0`
  exactly when `full_trailer` and `1.0` otherwise (extend the existing four-state table test from
  §2's original plan with this fifth channel), plus a settle/report test confirming the new `ease()`
  term is folded into the idle-gate `\|` chain (mirrors the existing obligation in §4 of this doc for
  every other eased/spring scalar in this screen).
- Full-trailer focus (§8.4): the missing `reconcile`/`valid`/`place` regression test described above,
  written against the CURRENT (already-correct) code — this one is expected to pass immediately, not
  fail-then-pass, since it is closing a coverage gap rather than fixing a behavior bug; note this
  explicitly in the test's own doc comment so a future reader doesn't mistake it for dead weight.

**Device (blocking for anything that moves pixels — no host runtime for GLES composition or real
trailer video)**

- A `tools/capture-screen.sh` still (or live `tv-session` observation) of the detail page confirming
  Extras now renders below Cast & Crew, on both a movie and a show fixture (movies have no
  Season/Episode strips ahead of Cast, per §8.1's two fixed arrays).
- A real-device session sitting still on an item's hero through one full trailer playthrough,
  confirming it does not restart on its own — this is the one thing a host test cannot fully prove,
  since the host test simulates the `view.picture` transition rather than observing a real EOS from
  the bundled FFmpeg pipeline. **Do this twice**: once in background autoplay, once entering
  full-trailer mode (UP) partway through and letting it finish there — the second case is exactly
  the one an eng review's outside-voice pass found the first draft silently failed to suppress.
- A live UP/BACK cycle on an item WITH a resume point, confirming visually: the bottom scrim visibly
  fades to nothing on UP and fades back in on BACK/DOWN, and the visible/focused pill reads "Resume"
  (not "Play") both before promoting and after returning, with no visible flash of `Restart`/
  `Trailer`/the watch toggle mid-transition.

### 8.7 Explicitly out of scope

- The wedge (`hero_scrim`/`PROMOTED_FIELD` = 0.4) — already shipped and unchanged by this pass; only
  the separate base gradient (`base_scrim_a`) is newly wired.
- Manual/on-demand trailer playback (the Extras shelf, the hero's `Trailer` disc control) — a
  different playback path, unaffected by the play-once suppression (§8.2).
- Extras' own internal ordering, or the Cast & Crew shelf's own cast/crew ordering — unchanged;
  this pass only moves the two shelves relative to each other.
- Home's own hero preview — a different surface sharing only `landing_hero.rs`'s pure geometry, per
  §7 of the original plan; not touched here either.
- **[Eng review addition]** Moving natural-completion detection into `player::preview::Machine`
  (the outside-voice's alternative fix for §8.2's ordering bug) — considered, not chosen for this
  PR (larger blast radius, no second consumer today to justify it); recorded in `docs/TODOS.md`
  instead.
- **[Eng review addition]** Tracking play-once suppression as a history/set rather than a single
  slot (issue 1B) — the single-slot design accepted as sufficient; bouncing between two episodes
  and back within one visit can replay each once more per return, judged reasonable rather than a
  gap worth a second data structure.

### 8.8 Eng review addenda: what already existed and was reused

Beyond `preview_cache_rk()` (§8.2's key type, already built for exactly this purpose) and the
`ease()`/idle-gate patterns §8.3 already reused, the eng review pass found two more pieces of
existing machinery this section's fixes now depend on rather than duplicate:

- **The `WillLeave(Leave::ForGood)`/`Unmount` reset block** (`mod.rs:1513-1524`) already existed
  and already reset `preview_dwell`/`preview_promoted` for exactly the "per-visit" scoping this
  section's play-once feature needs — the fix was adding two fields to an existing convention, not
  inventing a new lifecycle hook.
- **The abandon-trigger's own boolean shape** (`mod.rs:2622`) already encoded "is the viewer still
  actively engaged with this hero" as `(!hero || scrolled_off) && !preview_promoted` — the eng
  review's `hero_active` extraction (issue 2A) is a factoring of logic that already existed in three
  near-duplicate forms, not new logic.

---

## GSTACK REVIEW REPORT — 2026-09-15 (design review, §8 follow-up pass)

### Design-dimension ratings (0-10, on the ORIGINAL 3-bullet request vs. this finished section)

| Dimension | Original request | This plan | What closed the gap |
|---|---|---|---|
| Interaction completeness | 3/10 — "play only once" named no scope (per-visit vs. per-session), and the scrim bullet didn't say how far it should fade | 9/10 | Both ambiguities resolved as explicit decisions (visit-scoped suppression; full fade to transparent) before being written into the plan, with the tradeoff of the rejected alternative stated for each |
| Correctness of the starting claim | 2/10 — the request assumed three independent gaps, but one (Play/Resume staying focused) turned out to already be correctly implemented; treating it as a fourth code change without checking would have been wasted/risky work on a path the project's own doc already claimed was done | 9/10 | Verified `hero::focusable`/`reconcile`/`valid`/`draw_buttons` directly against source before writing anything; found the real gap was a missing regression test, not missing behavior, and scoped the fix accordingly |
| Regression risk awareness | 2/10 — none of the three bullets mentioned the existing pinning test, the intentional "restart on re-focus" behavior, or the CRITICAL test the original plan called for and was never added | 8/10 | Named the exact test that breaks (§8.1's array literals), the exact existing behavior that must NOT change (re-dwell-after-abandon in §8.2), and closed the specific missing-test gap from the prior plan pass (§8.4) instead of leaving it for a future surprise |
| Reuse over invention | 4/10 — request implies three separate bespoke mechanisms | 8/10 | Play-once reuses the existing `ease()`/dwell-tick architecture and the same "item changed underneath" inference the codebase already relies on elsewhere, rather than inventing a new subsystem; the scrim fade reuses the exact `preview_field` pattern already shipped one section away in this same file |
| Visual/state consistency across the four preview states (§2's table) | 3/10 — the three bullets didn't reconcile against the existing four-state model at all | 8/10 | Explicitly traced each fix onto the existing Idle/Dwelling/Background/Full-trailer states from §2, confirming none of the three changes needs a fifth state or a new trigger condition |
| Test-plan completeness | 1/10 — none specified | 8/10 | Host tests enumerated per new piece of state (including the two failure-mode variants for play-once: interrupted vs. natural), plus the specific device-only checks and why each needs a real device |

### Adaptation note

Same adaptation as the original plan's report: this is an interaction/state-machine spec against a
native TV codebase with a fixed 1920x1080 canvas, SDL2_ttf rasterization, and a hardware video plane
GL cannot read — none of which a generated mockup PNG could meaningfully represent or de-risk. This
review substituted direct verification against the project's own source (`hero.rs`, `mod.rs`,
`preview.rs`, `detail_layout.rs`, `section.rs`, `tests.rs`) for the mockup/comparison-board/
outside-model-critique steps, and used AskUserQuestion only for the two decisions genuinely
unresolved by the request (play-once scope; scrim fade target) — the third apparent gap (Play/Resume
focus) resolved to "already correct" on inspection rather than needing a decision at all.

### Runs

| Check | Status |
|---|---|
| Scope gate (user pointed at `docs/trailer-ux-plan.md`, this skill's own explicit-path exception) | Applied |
| Codebase reconnaissance (`sections()`, `player/preview.rs`, `draw_backdrop`/`base_scrim_a`, `hero::focusable`/`reconcile`/`valid`) via a dedicated research pass, independent of this doc's own "implemented" claims | Done |
| 2-question decision pass (play-once scope; scrim fade target) | Done — answers folded into §8.2/§8.3 |
| Mid-review scope addition (Play/Resume visible+focused) verified against source before being added to the plan | Done — §8.4 |
| Design-dimension rating + gap-closing rewrite | Done (table above) |
| Visual mockup generation / comparison board | Skipped — no visual medium fits this app's real constraints, same reasoning as the original plan's report |
| Outside-voice (Codex) design critique | Skipped — no visual surface to critique; the equivalent check performed here was independent source verification of every claim before it entered the plan (including catching that the scrim wasn't actually wired despite the doc, and that the focus behavior already was) |

### VERDICT

Section 8 is implementation-ready. One implementation-time detail is intentionally left unpinned
rather than guessed: the exact type/field used as `ItemKey` in §8.2 (verify against whatever already
identifies the current item for `hero_set()`/the preview-extra picker) — this is a naming lookup at
implementation time, not a design decision, and does not block starting the work.

NO UNRESOLVED DECISIONS

---

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | — | not run (bug fix + small UX polish, not a scope/strategy question) |
| Outside Review | Claude subagent fallback (Codex not installed) | Independent 2nd opinion | 1 | unavailable (native fallback completed; not cross-model) | 3 findings — 2 confirmed real (EOS-detection ordering bug, missing leave-reset), 1 architectural alternative considered and deferred to TODOS.md |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | clean | 5 issues found and resolved (2 architecture, 2 code-quality, 1 test), plus 2 outside-voice bugs fixed |
| Design Review | `/plan-design-review` | UI/UX gaps | 1 | clean | score 2/10 → 9/10, 2 decisions made (play-once scope, scrim fade target) |
| DX Review | `/plan-devex-review` | Developer experience gaps | 0 | — | not run (internal native-app screen logic, not a DX-facing surface) |

**OUTSIDE COVERAGE:** Codex CLI not installed on this machine (`CODEX_MODE: not_installed`) — fell
back to a Claude subagent with fresh context per the skill's documented fallback path. The fallback
completed successfully and found two real, source-verified bugs (see CROSS-MODEL below), but per
this skill's own rule a same-harness fallback never supplies genuine outside/cross-model coverage,
so `outside_status` is recorded as `unavailable` despite the fallback's findings being real and
actioned. Install `@openai/codex` for genuine cross-model coverage on a future pass.

**CROSS-MODEL:** No cross-model tension to report — the outside pass was a same-harness (Claude)
fallback, not an independent model, so "cross-model" doesn't strictly apply. It is recorded here
anyway because its findings were substantive and changed the plan: (1) the natural-EOS detector's
placement relative to `preview_promoted`'s clear-timing was wrong and was fixed
(`self.preview_promoted || hero_active`, verified against `mod.rs:2672`'s actual clear site); (2)
the two new fields were missing from the existing `WillLeave`/`Unmount` per-visit reset convention
and were added (`mod.rs:1513-1524`). Both were verified against source before being accepted, not
applied on the fallback's word alone.

**VERDICT:** CEO (not applicable) + ENG CLEARED + DESIGN CLEARED — ready to implement. Outside
coverage remains genuinely unavailable (no Codex CLI on this machine); the plan proceeded on native
review plus a same-harness fallback that found and closed two real bugs, which is the best coverage
available in this environment today.

### Diagrams

- `docs/trailer-ux-plan.md` §2's state table now carries a "Bottom scrim" column and the
  Idle→Background transition note both this review's issue 2B and §8.2's rewrite required — kept in
  sync with the code it describes rather than left to drift, per the standing rule that a stale
  diagram actively misleads.
- No new inline ASCII diagram is warranted in `mod.rs` itself: `preview_tick`'s existing shape
  (sequential `if`/`let` blocks, no branching state machine of its own beyond what §2's diagram
  already covers) doesn't gain clarity from one, and the function is already short enough to read
  linearly. The `hero_active` extraction is a two-line local, not a control-flow change worth
  diagramming.

### Failure modes

For each new codepath, one realistic production failure and whether it's covered:

| Codepath | Realistic failure | Test? | Error handling? | User-visible? |
|---|---|---|---|---|
| `sections()` reorder | A future edit adds an 8th section without widening `SPOT_SECTION_SLOTS`/`SLOTS` | New pixel-identity test (issue 3A) catches position drift; array-size mismatch is a compile-time `const` bound, not runtime | N/A (compile-time) | N/A |
| `preview_played_for` gate | `preview_cache_rk()` returns different strings for what a viewer perceives as "the same item" across two ticks (e.g. a metadata refetch changes the trailer's own rk) | Not directly tested — inherits whatever guarantee `preview_cache_rk()` already provides for the pre-existing `blocked()` check | None beyond re-autoplaying (worst case: one extra play, not a crash or hang) | Yes, but benign — at most an extra unwanted autoplay, not a silent failure |
| `preview_started_for` capture | `request_preview` is called twice before EOS lands (e.g. rapid re-trigger) — second call overwrites the first's captured key | Not directly tested; existing `Machine` single-target refusal (per the `single-video-plane-forces-ambient-not-per-tile` pattern) makes a second admit while one is in flight structurally rare | `Machine::admit`'s existing all-or-nothing contract | No — would at most misattribute which item's key gets marked played, same low-severity class as the row above |
| `preview_base_scrim` ease | Idle-gate motion report forgotten, scrim visibly freezes mid-fade on a settled frame | New settle/report test (§8.6) — this is exactly the `Xfade`/`Spinner` failure class `ui/CLAUDE.md` warns about, now guarded | `ui::idle`'s 2s keepalive bounds the staleness even if forgotten | Yes, but bounded (2s max staleness, not indefinite freeze) |

No **critical gap** (untested AND unhandled AND silent) found. The two lower-severity rows above
(`preview_played_for`/`preview_started_for` key-mismatch edge cases) fail closed toward "plays an
extra time" rather than toward a crash, hang, or silent stuck UI — judged acceptable given the
narrow, low-frequency conditions required to trigger them and that the negative-fact-cache pattern
they inherit from has run in production already.

### Worktree parallelization strategy

Sequential implementation, no parallelization opportunity — all six tasks below touch
`screens/detail/mod.rs` (four directly, two via its test file), so splitting them across worktrees
would produce a merge conflict on nearly every edit rather than saving time.

### Implementation Tasks

Synthesized from this review's findings. Each task derives from a specific finding above. Run with
Claude Code or Codex; checkbox as you ship.

- [ ] **T1 (P1, human: ~30min / CC: ~8min)** — screens/detail — Fix play-once suppression: `hero_active` extraction, corrected EOS condition, `preview_started_for` capture, leave-reset
  - Surfaced by: Outside voice — EOS-detection ordering bug (`preview_promoted` clears after the naive check reads it) + missing `WillLeave`/`Unmount` reset
  - Files: `rust-modules/src/screens/detail/mod.rs`
  - Verify: new host tests in T6 below; `make check`
- [ ] **T2 (P1, human: ~10min / CC: ~3min)** — screens/detail — Swap Extras/Cast order in `sections()`, update `is_hide_anchor` comment
  - Surfaced by: Design review §8.1 + eng review issue 3A verification (`LayoutCache`/`Spot` confirmed identity-keyed, safe to reorder)
  - Files: `rust-modules/src/screens/detail/mod.rs`, `rust-modules/src/screens/detail/section.rs`
  - Verify: updated pinning test + new `compact_title_hide_pos` test (T5); device capture per §8.6
- [ ] **T3 (P1, human: ~15min / CC: ~4min)** — screens/detail — Wire `preview_base_scrim`: new eased scalar, fold into idle-gate chain, multiply into `draw_backdrop`
  - Surfaced by: Design review §8.3 — `base_scrim_a` was completely unwired from full-trailer mode
  - Files: `rust-modules/src/screens/detail/mod.rs`
  - Verify: state-table test extension + idle-gate settle test; device UP/BACK cycle capture
- [ ] **T4 (P2, human: ~20min / CC: ~5min)** — screens/detail — Add `reconcile`/`valid`/`place` Restart→Play regression test in full-trailer mode
  - Surfaced by: Original plan §5 called this CRITICAL; never added. Design review §8.4 closed the gap
  - Files: `rust-modules/src/screens/detail/tests.rs`
  - Verify: `make check` — expected to pass immediately (closing coverage, not fixing a bug)
- [ ] **T5 (P2, human: ~15min / CC: ~4min)** — screens/detail — Add `compact_title_hide_pos` pixel-identity regression test across the Extras/Cast reorder
  - Surfaced by: Eng review issue 3A — closes the `detail-sections-array-position-traps` hazard class with a test, not just a source read
  - Files: `rust-modules/src/screens/detail/tests.rs`
  - Verify: `make check`
- [ ] **T6 (P2, human: ~30min / CC: ~8min)** — screens/detail — Add play-once host tests: natural EOS (sitting-still + full-trailer-mode), abandon-preserves-replay, item-change reset, leave-reset
  - Surfaced by: Design review §8.6 + eng review outside-voice cases 2, 5, 6
  - Files: `rust-modules/src/screens/detail/tests.rs`
  - Verify: `make check` — cases 2 and 6 must be written to fail against the uncorrected/unfixed code first, per this repo's reproduce-fail-fix-pass rule

JSONL artifact: `~/.gstack/projects/MrcRjs-plx-native/tasks-eng-review-20260915-144303.jsonl` (6 tasks, for `/autoplan` aggregation).

### Completion summary

- Step 0: Scope Challenge — scope accepted as-is (3 files, 0 new services — well under the complexity-check threshold)
- Architecture Review: 2 issues found (both resolved: reuse `preview_cache_rk()`; single-slot suppression accepted)
- Code Quality Review: 2 issues found (both resolved: `hero_active` DRY extraction; §2's state table + transition note updated)
- Test Review: diagram produced, 1 gap identified and closed (`compact_title_hide_pos` pixel-identity regression test)
- Performance Review: 0 issues found
- NOT in scope: written (§8.7, extended with 2 eng-review additions)
- What already exists: written (§8.8)
- TODOS.md updates: 1 item proposed and accepted (Machine-level `just_finished` flag, P3)
- Failure modes: 0 critical gaps flagged (2 lower-severity edge cases noted, judged acceptable)
- Outside voice: ran (Claude subagent fallback — Codex not installed) — 2 real bugs found and fixed, 1 alternative deferred to TODOS.md
- Parallelization: 1 lane, sequential (all tasks touch `screens/detail/`)
- Lake Score: 2/2 — both decisions comparing a complete option against a shortcut (issue 1B: single-slot vs. history; the outside-voice EOS-detection fix: screen-local patch vs. Machine-level refactor) chose the option scoped correctly for this PR's size, not the more complex one, and the incomplete option (Machine-level flag) was captured as a TODO rather than dropped
- Unresolved decisions: 0

NO UNRESOLVED DECISIONS
