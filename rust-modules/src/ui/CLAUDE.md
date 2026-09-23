# ui/ — the shared UI system (read before touching any screen)

This is a **real product built for production quality.** "Not worth it for a throwaway" is never a
reason to skip a proper component, leave a bespoke `draw_*` in place, or half-finish a primitive.
If a shared piece is missing (a real text-flow view, a chip, a list),
**build it** — a reusable primitive that pays off across screens is exactly the work worth doing.

This directory is a **design system**, not a pile of per-screen draw code. Home, detail, and the
player HUD are all *compositions of the same tokens + components*. When you add or change UI,
**reach for the shared pieces and improve them in place** — do not paste a new `draw_*` function
with hard-coded colors and hand-tuned text offsets. That is exactly the drift this system exists to
kill. Full design + migration status: `docs/ui-system-migration.md`.

## The four rules

1. **Never write a raw color literal.** Every color comes from `theme.rs` as a named token
   (`theme::TEXT_PRIMARY`, `theme::CONTROL_IDLE_FILL`, `theme::scrim(a)`, …). Need a shade that
   doesn't exist yet? **Add a token to `theme.rs`** (with a doc line saying what it's for) and use
   that — don't inline `[0.9, 0.9, 0.9, 1.0]`. If your shade is within a hair of an existing token,
   use the existing one; the point is one value per role, not a value per call site.
   `theme.rs` is **two layers**, so "add a token" has two halves: a new token is a **role** (a job:
   what is this colour *for*) and it resolves to a **primitive** (a stop on the palette, private to
   that module) — reach for an existing stop first, and only add one when the palette genuinely has
   no such shade, as an exact 8-bit code via `rgb8`. Two roles landing on one stop is expected and
   stays two roles (`ACCENT` and `TEXT_PRIMARY` are both Cool 0): retuning the focus fill must not
   restyle every title on screen. Overlays are `with_a(WHITE, a)` on the measured alpha ramp — a new
   overlay is a weight, never a new hue. The layering mirrors the `PlxNative Design System` project
   (`tokens/primitives.css` + `tokens/colors.css`), so a palette decision is one edit in each.

2. **Never write a raw text size — every text size in the UI is a `theme::size` token.** The named
   rungs are `size::HERO` 72 / `size::DISPLAY` 48 / `size::TITLE` 40 / `size::HEADLINE` 32 /
   `size::BODY` 28 / `size::LABEL` 26 / `size::CAPTION` 24 / `size::MICRO` 22 /
   `size::DIAGNOSTIC` 20 — the *size* axis of the design system.
   `CAPTION` (24) is the **couch legibility floor for ordinary product content**; `MICRO` exists
   solely for one-line de-emphasized labels — an episode's air date, a rating row's provider
   caption, a track's file path in the tracks panel — never for content. `DIAGNOSTIC` belongs only to the
   deliberately opened `app::diagnostics` engineering instrument: its fixed schema and complete values
   matter more than couch-copy scale, and it must never migrate into chrome or prose. The hero
   SYNOPSIS is the case that proves
   the rule and it is no longer here: a blurb is the longest run of prose on its page, so it is
   reading copy, and both heroes now draw it at `LABEL` 26 through `ui::hero_synopsis`. (The hero
   META line this parenthetical used to name is `BODY` 28 and always was.) Pass a rung to `Painter::text`/`Label`/`TextView`/`text::elide` instead of a bare
   `24`/`28`/… — a size is a role, not a magic number; pick the nearest rung, and if no rung fits a
   genuinely new role, **add a documented rung to `theme.rs`**, don't inline a literal. Exactly two
   carve-outs live outside the scale (the player-HUD display title `HUD_TITLE_SZ` and the subtitle
   caption); both are **named + commented at their call site**, never bare literals — don't add a
   third; new roles go on the scale. `anim.rs` is a dev-diagnostic overlay, not chrome.

3. **Never hand-place text with a magic y.** `y - sz*0.58` guesses are banned — they mis-center the
   moment a string has a descender (g j y p). Text is positioned by its **cap band** (layout ≠
   paint): use `ui::label::Label` (single run) or `ui::text_view::TextView` (multi-line, pixel-
   wrapped), or if you must call `Painter::text` directly, derive the y from `text::text_vcenter_y` /
   `text::text_cap_band`. See `label.rs`'s module docs for the rule.

4. **Improve a component before forking one.** If a shared widget almost does what you need, add a
   builder method / style variant to it (e.g. `Button::style(ControlStyle)`), don't copy it. A new
   bespoke widget is only justified when nothing here is close — and then it lands *here*, as a
   reusable `View`, so the next screen gets it for free.

## The architecture (restructure spec v4)

The owner's goal, verbatim from the spec: *"the application's behaviour is described by several
linked, DETERMINISTIC state machines — session, navigation, screen instances, playback — each with
an explicit owner, interacting only through typed events, never by writing another machine's
state."* Phases 0–12 have landed. The full design record is
[`docs/ui-restructure-spec-v4.md`](../../../docs/ui-restructure-spec-v4.md); `mod.rs` cites it by
section number on nearly every `mod` line, so `// RESTRUCTURE (spec §6.2)` is a live reference you
can follow.

**The layer rule (§2.1), gated by `ci/check-deps.sh`.** This is why the screens are not here:

| layer | may name |
|---|---|
| `ui/` — the LIBRARY | `crate::{gfx,text,paths,task}` — **never an application type** |
| `screens/` — the application's screens | `ui/`, `stores/`, `plex/` types, `player/` — never a sibling screen |
| `stores/` | data crates and `ui::machine` only — never `screens/` |
| `app/` | everything |

`ui/` is generic over one application bundle and is compiled and tested against `FixtureHost`
with no Plex type in scope (`fixture.rs`). If you find yourself reaching for `crate::plex` or
`crate::browse` from a file in this directory, the design says the code belongs in `screens/`.

**One owner per state, event and resource (§2.2).** `App` owns a containment tree — never
references: `Session` (auth, profile, the one `ProfileScope`), `Consent`, `Input` (press, key-repeat
edges, pointer visibility, the double-buffered hit map, and the `FocusEngine` that holds THE
current focus — screens keep no copy), `Present` (the gate), `Navigation` (the container tree and
every entry/instance), `Player`, `Stores`, `Adapters`. Ownerships that used to be double are now
single: the press belongs to `Input`, the modal phase to `ModalStack`, the present gate to
`Present`, `ProfileScope` to `Session`.

**Navigation is a model, not a route word (§6.2).** `containers/` is the fold:
TabContainer → NavStack → ModalStack. A screen is an owned INSTANCE with logical state inside and
render resources created lazily. `input_owner()` resolves over the shared ModalStack first, then
the top page's own stack, then the page.

**One frame algorithm (§3.3, `dispatch.rs`)** — ten steps, and `frame/` expands only steps 8–10:
ls2_pump → ingest → adapter results in a deterministic `(completion_frame, adapter_rank,
arrival_index)` order → one `Tick` broadcast containers-before-pages → expired timers → a bounded
drain (`MAX_STEPS_PRE = 192`) → **exactly one nav commit per frame**, with `MAX_STEPS_POST = 64`
reserved so a key that opens a page still mounts it in the same frame → present decision →
prepare under the `frame::Budget` → draw. Nothing is ever dropped from that queue; an overrun is
CARRIED to the next frame and rides the heartbeat as `carried=`.

**Frames must not wait on storage or synchronous LS2 calls.** The app loop, bridge and dispatcher
enter `task::FrameScope`; `task::assert_may_block` guards session I/O, helper transactions,
Keymanager and LS2 calls. Tests panic (catchably). In developer builds (`threadcheck`), an unallowed call logs
`main-thread block: <label> (fatal; aborting)`, then aborts on the frame thread before the call runs.
Release builds retain elapsed-time logging once per label. Use cached reads and the bounded `storage_worker` queue, then observe the landing
on the frame thread and call `idle::invalidate()` only when visible content changed. Boot's synchronous loads run before the frame scope; a new blocking exception
must be explicit and justified through `task::allow_blocking`, never added to a tick.
To see it fire on the TV, hold the TV lock and, after the first present in a `threadcheck` + `devtriggers` build, run `tools/tv-session.sh key hang:1000` for the guard abort, or `key hang-raw:1000` for the watchdog's unlabeled warning path (both capped at 5000 ms).

The release-enabled `task::watchdog` independently checks loop progress every 100 ms. It reports
once after more than 250 ms without progress and once on recovery, using the innermost static
`BlockingLabel` scope or `unlabeled`. The frame entrance increments one atomic counter; the
watchdog owns the clock, so durations are sampled rather than exact frame timings. Static label
descriptors keep scope publication allocation-free. The host harness disables the observer;
tests opt into private signals and drive the detector with synthetic timestamps. Timing starts
only after the first present completes, excluding initial shader/font warm-up. A freeze after
that point is reported once even if no further frame presents; after the loop resumes, the
watchdog reports the total stall duration and the label captured when the stall was detected.
With `threadcheck`, the first hang also publishes a purple runtime warning.
Controlled recording/replay boots suppress the warning's forced presents and pixels, so real-time
linger cannot change per-frame replay grades. Logging and fatal enforcement remain active. The FRAME thread
paints it, so it appears when the stall ENDS and lingers for about three seconds. It never appears
WHILE the stall is happening: the stuck main thread cannot draw. At >=2000 ms the observer sends SIGABRT once per stall to the main pthread
captured at loop start, so crashtrace records the interrupted main thread, not the observer.
Developer draw and swap phases publish label-only scopes (`frame draw`, `gl present`): these
neither invoke the blocking guard nor grant permission for guarded calls. On hardware GL (the TV,
a desktop GPU) a legitimately slow GPU frame still hits the >=2000 ms fatal threshold by design:
a two-second main-thread stall is a bug regardless of cause; use the `guard=log` escape hatch
below when investigating it. The one exception is a CPU rasterizer: when the boot's `GL_RENDERER`
is a software renderer (Apple Software Renderer, llvmpipe, softpipe, swrast, SwiftShader, WARP —
the CI simulator), time sampled in a GL-work phase — `frame draw`, `gl readback` (the capture
stream's and the simulator screenshot's `glReadPixels`), `gl present` — logs and warns but does
not count toward the kill, because that time is the rasterizer's. Two seconds outside those
phases, `unlabeled` included, stays fatal there, and so does the blocking guard.
`threadcheck: software GL renderer` in the log says the exception is armed.
If the watchdog itself misses more than four poll intervals (>400 ms), it cannot distinguish
process freeze, suspend, debugger stop or observer starvation from a main-thread stall. It
rebases the stall origin, clears any warning, resets the fatal latch and reports nothing for
that sample. A fresh uninterrupted two-second stall can still become fatal after the rebase.
A guard's own `abort()` instead records the abort path; its log label identifies the guarded call.
To investigate without termination, write `log` into `/tmp/plxnative-guard` before launch
(`/tmp/plxnative-guard=log` notation; simulator: instance runtime directory). This DIAG trigger
is latched at loop start and downgrades both fatal paths to log plus warning. It requires
`devtriggers`; it never suppresses the profile picker. Release builds contain neither warning,
fatal policy nor signal call, and keep the existing guard/watchdog logs.

**Damage is not an effect in the queue.** `fx.invalidate(provenance)` calls `Present::note` at
once, because a draw-phase report has to survive into the frame it belongs to. `Provenance` is
recorded, so a replay diff can say *why* a frame presented. Workers get exactly one documented
atomic door, `present::wake_from_worker()`.

**Time and measurement are capabilities, not free functions (§4.1, §4.3).** `Tick{ms, dt_us}`
arrives on the event; `Clock` is `SdlClock` or `VirtualClock`. `Measure` has three
implementations — `TtfMeasure` (device/sim), `TableMeasure` (replay, from the recording's metrics
table) and `FixtureMeasure` (host tests) — and travels on `Cx`/`DrawFrame`. The point of both is
§5: real scenarios record and replay deterministically.

**What has NOT landed** — the guide says so rather than letting `mod.rs` imply otherwise:

- §11 lists `nav.rs`, `trail.rs` and `press.rs` for deletion. Only `trail.rs` is gone; `nav.rs`
  (164 lines) and `press.rs` (619) are still here and still used.
- §11 says `present.rs` REPLACES `idle.rs`. Both exist — `idle.rs` at 1069 lines against
  `present.rs`'s 233 — so the gate has two homes and `idle.rs` is still the larger one.
- §0's done-criterion 9 (`doc_claim_auditor` clean against §11's prose list) is not met.

Modules with no product caller yet carry `#![allow(dead_code)]` with a one-line reason; that is
the spike, not dead code to delete.

## Product idiom (settled owner verdicts)

The reference clients are a checklist of CAPABILITIES, never a mockup. Plex says *what* a screen
must be able to do; Apple TV is what it should look like; the presentation stays ours. Each rule
below is a verdict the owner has already given — satisfy the capability, and do not re-open the
presentation in a design pass.

1. **Menus are popovers, never full-screen sheets.** A new menu — playback settings, a quality
   ladder, an item context menu, the play-queue list — is a `popover.rs` container over the live
   screen with a `table.rs` `TableView` inside, anchored near its trigger, the way `track_menu.rs`
   and the library sort/filter menus already work. Plex's full-screen modals were rejected by name.

2. **Clickable text marks are ALL CAPS.** `MORE` and every other pressable text mark is
   capitalised, overriding the design system's sentence-case rule for app-written text: the mark is
   a control, not prose, and caps is how the app says *pressable*. Use the shared
   `text_view::MORE_MARK` rather than a screen-local literal — the drift that caused this rule was
   two screens each holding their own copy. A tab pill reading "More" is a pill, not a text mark.

3. **The player HUD gets a state glyph, not a transport row.** No `<<` `||` `>>` `■` buttons. The
   HUD keeps the small transport *state indicator* past the elapsed clock: a pause glyph while
   paused, a seek spinner while the transport owns the busy signal, nothing at all while playing.
   Transport is driven by REMOTE KEYS, which must genuinely work. Any `<<` / `>>` added must be a
   small glyph in that same indicator slot at the clock's cap height — never a control, and never a
   fourth entry in the HUD control row (`player_hud.rs`'s `BTN_N`, currently 3).

4. **Failure read-outs are never red — the app does not scold.** A failed verdict is inked bold at
   `size::TITLE` in `theme::TEXT_SECONDARY` (primary only alongside the 96 px glyph), per the
   design system's `StatusOverlay` contract. Home, Library and sign-in read-outs share ONE
   placement: the verdict hangs from `FULL_ANCHOR_TOP` with the reason and action row stacked
   directly under it. A read-out centred in its own container instead lands ~250 px low and reads
   as a different component.

5. **Read the DS contract before inventing a control state.** Focus, hover and selected treatments
   come from the component's `.d.ts` in the design system, not from a lane's judgement. The Search
   field is the standing example: its focus is carried by INK ALONE — no capsule, rim, rule, plate
   or focus pop — and an accent underline was rejected on sight. A punch-list line like "make focus
   more visible" is satisfied WITHIN the contract (widen the ink step), never with a new primitive.

## The route family says where BACK goes ONCE, as a crumb

Every Settings-family route — Settings, Privacy & data, Legal notices, a legal document, About, the
Home-sources editor, the first-run consent pair — draws a caption line above its title: a
left-pointing chevron (`Icon::ChevronLeft`, the mirror of the row chevron and sharing its `ink_x`
entry) and the name of the place BACK returns to. `RouteLayout::draw_narrative` takes it as
`Option<&str>` so a new route cannot forget to answer. **`None` means "BACK does not go anywhere
inside the app"** — three routes qualify today (the Settings modal, the first first-run consent
question, and the QR sign-in), but take the census with
`git grep -A2 'draw_narrative(' -- 'rust-modules/src/ui/*.rs'` rather than trusting that number.

**It replaced `Press [BACK] to return` in the bottom action band across the whole family**, and the
trade is the point: the hint spent a 60px band restating a key the remote already has and could not
say where the key WENT, which on a three-deep push is the only part anybody needs. Freeing the band
is what let first-run consent put its two answers there. The sweep is complete —
`git grep -n 'KeyHint::new'` finds no call under a route any more, and the consent document
reader's was the one that survived the first pass, so that route said where BACK went twice.
`KeyHint` is unchanged and still correct where it lives: the read-only ALERT panels
(`screens::about_panel`, `screens::person_bio` and `screens::tracks_panel`), which hold no control at all, so
there the line really is the whole affordance. **One ROUTE draws one since 2026-09-17 and it is the
opposite object** — `screens::detail::trailer`'s `[^] Full screen`, under the hero's
control row while a trailer is playing behind the page. What the sweep removed was a line naming a
key the viewer already knows (BACK) for a destination it could not name; this one names a key that
does something the page gives NO other sign of, and it is drawn only in the seconds the trailer's
picture is up, fading with it. That is the test to apply to the next one: does the hint teach an
affordance that is otherwise invisible, or restate the remote? It reaches the cap through
`KeyHint::glyph`, the `CapFace::Glyph` face added with it — a cap whose legend is an `Icon` centred
in the same keyline rather than a `MICRO` word, because the chevron IS the key's face.

## Where things live

**`mod.rs` is the map.** Every module is declared there with its purpose and, where it came from
the restructure, its spec section. It cannot go stale the way a hand-written table does: rename a
module and the build breaks.

**The `//!` doc at the top of each file is the detail** — 81 of the 86 files here carry one, and
they are the authority on their own module. Read the file's `//!` before changing it.

This guide deliberately does NOT carry a per-file table. One existed, drifted 37 files out of
date, and duplicated the `//!` docs verbatim down to their typos; `docs/ui-framework.md` already
states the rule — *"do not fix this file into a second map of the directory; one map is the
point."* If you add a module, write its `//!` doc and its `mod.rs` line. That is the documentation.

**Adding a SCREEN is a different question**: screens live in `rust-modules/src/screens/` — see
[`../screens/CLAUDE.md`](../screens/CLAUDE.md).

## Glass, dim and dither — rendering policy you must not re-derive

Each of these was measured on the television and each has a cheaper-looking wrong answer that was
tried first. None of it is reconstructable from the code without repeating the measurement.

- **Live-backdrop glass: call it, don't orchestrate it.** To add a surface, call
  `Glass::DYNAMIC_BACKDROP.backdrop` from its normal `Painter` draw. Do **not** prepare,
  invalidate, check for modals, or skip a source pass in the widget. The dispatcher supplies the
  layer order and `frame/backdrop.rs` (owned by `GlassPlan`) declares geometry, excludes the
  glass's own and higher layers, checks occlusion and compares the ordered draw arguments beneath
  each sampling region.

- **A popover panel does not frost a backdrop blur.** Every panel stands on
  `widgets::panel_ground` — the 15×8 underlay field the modal dim already latched (`ui::underlay`,
  reaching a surface as `DrawFrame::underlay`), windowed to the panel's rect and graded under
  `PANEL_LUMA_MAX`. A source-grep test keeps `Glass` out of every other file.

- **A modal's dim is DRAWN, never applied to the source.** The variant that multiplied the host
  page's RGB going into the backdrop was measured and removed: dimming the source destroys the
  modulation the frost is layered over, so the panel arrives flat however dense the material is.
  Do not re-add it from this note. And the scrim is part of the PAGE — the direct source path
  re-renders the page closure before any popover draws, so a scrim drawn with the panel reaches
  the visible frame but not the snapshot, and the panel's ground then comes out brighter than the
  screen around it. Since phase 10 the pairing is the container's (`Screen::scrim` +
  `ModalStack::draw_scrims`), not each panel's. **The dim's ink is the page's own light, not
  black**: a surface names a ROLE (`theme::underlay::DIM_COMPACT`/`DIM_PANEL`/…), never a literal,
  and a test greps for one.

- **Freezing the host page is a separate, shared policy** — `Popover::caching_host()` +
  `ui::popover::host`. The mechanism is a REFUSAL at the renderer's one shared gate
  (`gfx::culled`), not a skipped call tree: the page's draw still runs, so layout, hit rects and
  texture uploads are untouched and only the fill goes away. An owned surface asks for it through
  its `Style` instead.

- **Every slow FIELD takes ONE dither whenever it is drawn, and it is not a per-shader decision.**
  An 8-bit framebuffer quantises: Settings over Home measured a 700-row column spanning luma 55.7
  to 59.1 in FOUR levels, treads of 158, 157 and 146 rows. `shaders/dither.glsl` is the answer and
  `gfx::glsl_dithered!` is how a program gets it — for the four whose ramp is a blur or a
  whole-screen wash, and deliberately NOT the per-rect `fs_src`/`fs_shadow`, which carried it for
  two days and cost the hero paging scene 57→50 fps for a branch that answered "no" on every draw.
  `gfx::dither_for_field` is an AREA test only: a field dithers whenever it is drawn, moving or
  not, and every page wash dithers on every frame.

## Card marks — three marks, three meanings, and no card may blur them

- An **amber ▶** means **this press starts the video**. It is drawn where that is true and nowhere
  else: a Continue Watching deck, and the detail page's episode filmstrip. Watch state does not
  withdraw it — `widgets::still_glyph` is the whole rule, graded by an exhaustive table, because
  the clause that failed first was the absent one: the glyph used to be decided by `PosterMark`
  FIRST, so an in-progress tile drew nothing and a Continue Watching deck — almost entirely
  in-progress tiles — announced nothing and then played.
- An **amber resume BAR** means **how far in you already are**. It is a fact about the item, not a
  promise about the press, so it is drawn wherever there is progress to report, including on a
  shelf that navigates. Hiding it there would hide something true; the triangle beside it was
  always the mark that read as "this will play".
- A **`✓`** means **finished** — likewise a state, not a promise.

**A card with no play indicator NAVIGATES**, and an episode navigates to its own detail page.
Until 2026-09-05 `want_play` was `from_deck || kind == 3` on both Home and the Library, so an
episode played immediately from ANY shelf and every landscape tile drew the triangle whatever its
press did. Behaviour and mark now read the same flag (`Shelf::is_continue` /
`pms::hub_is_continue`), so they cannot disagree. A poster wears nothing at all when it has never
been started — most of a server is unstarted, so a clean shelf is the common case and a mark is
information.

## Gotchas that bite

- **`Label`/`Button` hold a non-owning `*const c_char`.** Keep the `CString` alive for the whole
  draw frame (bind it to a `let` in the same scope) or you'll draw freed memory. (`TextView` is the
  exception — it borrows a `&str` and builds its own `CString`s internally, so it's memory-safe.)
- **The focus ring/glow is shader-baked** (`gfx.rs` `FS_SRC`/`FS_IMG`, folded into the card
  composite pass): callers drive it only through a `focus: f32` scalar and the geometry consts
  (`theme::CARD_RING_RAD`, `consts::GLOW_PAD`). Its color cannot be tokenized — don't try, and
  don't re-add it as a literal.
- **`detail.rs` below-hero layout is a computed flow, not magic constants.** The below-hero sections
  are the children of a shared `ScrollColumn` (`impl Column for DetailView`): the container's
  `child_top(i)` stacks the *present* blocks' `block_h()` heights (via `Column::height`) from
  `CONTENT_TOP` with one of THREE gaps, and which one is `section_gap`'s whole job: `SECTION_GAP`
  between ordinary sections, `TAB_EP_GAP` where the season tabs hug their episodes, and
  `consts::UNDER_LABEL_AIR` after a SHELF section (Related, Cast, Extras), which already carries its
  own label band — stacking a region gap on top of that band is the double-count that made every gap
  below the hero read as a hole. To resize/space
  a section, change its `block_h` (content-derived — e.g. Related tracks `REL_H`=`CARD_H`) or the gap —
  never reintroduce a hard-coded per-section Y. `ScrollColumn::draw` culls off-screen sections with
  `on_axis` and pre-translates each child painter to its origin, so each `draw_*` draws from local
  y=0; the same `child_top` feeds `scroll_target` (via `lift_target`), so draws and scroll can't drift.
- **A few things are deliberately immediate-mode** (documented in `docs/ui-system-migration.md` §D):
  home's `Backdrop` **per-element alphas** (the alphas, not the component — those alphas are springs
  now and it has a real `update`; what must never happen is routing the per-LAYER reveals through
  the cascade `p.alpha()`, which would fade the art and the wash as one object. The cascade still
  reaches the whole `Backdrop` from above, which is how `nav`'s page dip covers it),
  `player_hud`'s `SCR_H`-offset geometry
  (shared with `app.rs` pointer hit-tests), and the subtitle renderer. Leave them; wrapping them in a
  `View` breaks a load-bearing contract.
- **Clipping: prefer culling; use a scissor clip only for a hard-bounded panel.** Two spellings
  today. The legacy one is `Painter::clip(rect)` / `clip_clear()` (backed by `gfx::clip_set`/
  `clip_clear`) — **global GL state**, so you MUST pair set/clear inside the same frame (the panels,
  the document reader, the glass cut in `home`/`library`/`filmography`, the card underline are its
  users). A screen drawing through a `DrawFrame` uses `f.clip(p, rect)` instead: an RAII scope that
  restores the previous scissor when dropped AND narrows the painter's `clip` cascade value, so a
  `stop` registered inside it is intersected with the visible part (spec §7.6). Do NOT reach for either in
  the big scroll flow: `ScrollColumn`/the shelves deliberately **cull** off-frame children by index
  (`on_axis`) instead, which avoids per-frame scissor churn and needs no clean-up. So: bounded list/panel
  → `clip`; long scrolling document → cull. (The old edge-fade-mask trick is gone — a linear fade can't
  cut a tall two-line row evenly, which read as a broken clip; scissor replaced it.)

## When you're done

Three signals, in ascending cost. **`make check`** first — the host unit suite (`cargo test --lib`,
~28 s for 3,639 tests, measured 2026-09-17; do not re-quote that number, `time` it) includes the UI
geometry and focus tests: `screens/home/tests.rs`'s destination round trips,
row stepping staying inside the shelf array, and the pointer hit column matching the drawn card at
every snap phase; and
`card_row.rs`'s heading-clearance behaviour driven frame by frame (the regression that the heading
must hold still while focus walks the slots beneath it). If you touch focus navigation, hit-testing,
or shelf motion, **add a test here** — that math is host-testable and these caught real bugs.
Note the asymmetry, because it tells you where to put a new test: `card_row.rs` drives a **local**
`CardRow`, so its tests are ordinary and parallel; an owned screen's tests hold no focus of their
own (the `FocusEngine` does). Tests that seed the shared registry or the remaining global stores
take `testlock::serial()`; tests using separate production BrowseStore owners isolate their state
and landings. **`xfade.rs` is
the cautionary case**: its tests were ordinary and parallel until `tick` started reporting to
`ui::idle`'s process-global flag, at which point driving a fader began mutating state *another
module's* assertions read. They all take `testlock::serial()` now. Anything you make report to the
frame gate inherits that obligation — and the lock is ENFORCED rather than merely documented since
2026-09-10: it records the holding thread, and the shared stores (plus the app frame trunk, which
pumps all of them) call `testlock::assert_held`, so an unguarded write panics in the test that made
it rather than in whichever bystander it happened to land on.

**If you add anything that ANIMATES, it owes two tests**, because the failure modes are opposite and
each is invisible to the other's gate. Host: it reports while running **and** goes quiet at rest —
`ui/idle.rs`'s and `ui/xfade.rs`'s test modules are the pattern, including a settled-tree case that
steps 439 springs and asserts *nothing* is requested. Device: an `fps_floor` scene proving it
still animates under the gate, and (if its screen settles) an `fps_ceiling` proving it stops. An
over-reporting animator costs the entire ~38-points-of-a-core saving while every `floor` in the
suite still passes, so the ceiling is not optional politeness — it is the only thing watching.

Then **`make`** must stay green (ARM cross-build). Then the device: there is no host *runtime*, so
nothing above draws a single pixel. For anything that moves pixels, capture the panel on the TV
(`tools/capture-screen.sh out.png DISPLAY|GRAPHIC`) and eyeball it — the token collapses and
cap-band re-centering are invisible until deployed.
