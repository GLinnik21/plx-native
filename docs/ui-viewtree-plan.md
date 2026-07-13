# UI View-Tree Migration Plan (retui Steps 5–7)

Branch `player-ux`. This plan finishes the retained-view-tree migration begun in
`docs/ui-system-migration.md`. It is **refined against the current working tree** (home.rs is
already a `SCENE`-rooted tree; detail.rs is still loose statics; app.rs still flat route bools).
It supersedes the older steps 5–7 sketch in `ui-system-migration.md` where they disagree.

The **only correctness signal is `make`** (ARM cross-build; no host runtime). Every step below is
sized so `make` is green at its checkpoint, and every pixel-moving step names an on-device
`tools/capture-screen.sh` check. Steps are ordered so each screen stays independently shippable.

**Recommended order (chosen):** detail first (Step 6 — biggest structural payoff, one module, no
cross-screen coupling), then finish home (Step 5 — already 90% a tree), then app Route (Step 7a),
then the *optional* unified focus-path (Step 7b) behind a hard "stop at 7a" valve.

---

## (A) Target architecture

### A.1 Home — a fully trait-conformant retained tree (mostly already built)

```
Home { snap:Spring, bg_phase:f32, bg:Backdrop, hero:Hero, grid:Grid }   // static mut SCENE
  Backdrop : View          (draw-only; self-managed per-element alphas — carve-out)
  Hero     : View          (draw-only; group-faded under p.alpha(hero_a))
  Grid     : View          (update/layout(frame,env)/draw two-phase; nav/vert/hit_test/wheel inherent)
    Shelf[MAX_HUBS]         (inherent update + draw_cells; NOT trait draw — focused-last splits paint)
      Card[MAX_ITEMS] : View  (owns scale:Spring; update steps it; layout stores frame+movie; draw poster)
```

Home is driven by the frozen free fns `home_init/update/draw/move_focus/pointer_focus/wheel` over
`static mut SCENE: Option<Home>` (home.rs:398). `Home::env(dt)` (home.rs:386) bridges the C-global
`fr/fc/snapTarget` + `snap.pos` into a per-frame `Env`, **clamping fr/fc into live hub bounds**
(home.rs:388-393) so a stray write can't index out of range.

**What Step 5 changes:** `Card` becomes a real leaf `View` (owns + steps its own scale spring, gains
a `layout` that resolves+stores its frame Rect and `PmsMovie` pointer). `Grid`/`Shelf` conform to
the trait signatures (`layout(&mut, frame:Rect, env)`), while **keeping** the two-phase focused-last
paint and the inherent `nav/vert/hit_test/wheel` input methods (they need cross-shelf access and
mutate the fr/fc globals — they can never be trait methods). `Backdrop`/`Hero` already `impl View`.

### A.2 Detail — a `DetailView` struct behind thin C-ABI wrappers

```
DetailView {                                   // static mut VIEW: Option<DetailView>, lazy view()
  selected:c_int, section:c_int, col:c_int,    // was SELECTED / SECTION / COL
  scroll:Spring, card_scale:Spring, ep_hscroll:Spring,   // was SCROLL / CARD_SCALE / EP_HSCROLL
  last_resume_ns:i64,                          // was LAST_RESUME_NS
} : View                                       // update() steps exactly 3 springs; draw() dispatches
```

All 7 file-level `static mut`s (detail.rs:21-29) become fields. The bespoke `draw_*` blocks
(`draw_backdrop`/`draw_hero`/`draw_buttons`/`draw_compact_title`/`draw_tabs`/`draw_episodes`/
`draw_related`/`draw_cast`/`draw_about`) become `&self` **methods** taking a bare `Painter`
(the documented safety valve — they do **not** become cascade-driven sub-Views, because
`draw_backdrop`'s self-managed alphas can't route through cascade alpha and the `section_y` flow is
not a VStack). `section_y/block_h/sections/n_items` read only `metadata::current()` (no focus state)
so they **stay free fns** to minimize churn.

Every frozen `pub(crate)` fn (below) stays a **thin forwarding wrapper of identical signature** onto
a `DetailView` method — e.g. `pub(crate) fn draw() { view().draw(&env_of(0.0), Painter::root()) }`,
`pub(crate) fn open(idx: c_int) { view().open(idx) }`. `view()` **lazy-inits** with
`get_or_insert_with` (detail has **no** `detail_init` C-ABI, unlike `home_init`; `open`/`open_rk` are
always the first calls) — using `.expect()` like home's `scene()` would panic on first open/draw.

### A.3 App — one `enum Route` (Player carries overlay sub-state)

```rust
enum Overlay { None, Menu, Info, Chapters }
enum Route   { Home, Detail, Player { overlay: Overlay } }
```

Replaces the five entangled `let mut` stack locals `playing/detail_open/menu_open/info_open/
chapters_open` (app.rs:329-333). The semantics are **not** flat: `detail_open` is only meaningful
while `!playing`; the three menu/info/chapters overlays are only meaningful while `playing` — hence
`Player{overlay}` rather than four peer variants. `plex_run` stays immediate-mode: it is the loop
that ticks/draws the View tree, never itself a View.

**Suspend/restore is NOT encoded in Route.** Background (0x103/0x104) sets `Route::Home` but keeps
the separate `bg_was_playing/bg_was_paused/bg_pos` guard driving the single-`Load` foreground restore
(app.rs:360-401) — losing that session = black-plane UAF on foreground.

### A.4 Focus (Step 7b, OPTIONAL) — one FocusPath

Optionally merge home's `fr/fc/snapTarget` and detail's `section/col` into one
`FocusPath { section, col, snap }` reached through the existing `g_fr/g_fc/g_snap/set_fr/set_snap`
bridge (app.rs:116-124). This is purely additive uniformity and is the **only** step that touches
app.rs-shared focus state or the fragile snap edge-trigger. It is the correct thing to sacrifice.

---

## (B) Frozen C-ABI / call-site signatures (MUST NOT CHANGE)

These are `pub(crate)` Rust (not `extern "C"`, except `plex_run`), but their **names/arities/return
types are frozen** — app.rs, route.rs, and the `plxnative-*` dev-triggers call them verbatim. Any struct
migration must keep them as thin wrappers.

**home.rs** (callers via the app.rs `g_*`/`set_*` bridge at app.rs:116-124, and `home_*` drivers):
- `fn row() -> c_int` · `fn col() -> c_int` · `fn snap_target() -> f32`
- `fn set_row(v: c_int)` · `fn set_snap_target(v: f32)`  (no `set_col` — fc is written only via nav)
- `fn movie_at(r: c_int, c: c_int) -> *mut PmsMovie`  (app.rs:643/645 routing, app.rs:896 plxnative-playidx)
- `fn home_init()` · `fn home_update(dt: f32)` · `fn home_draw()`
- `fn home_move_focus(sym: c_uint)` · `fn home_pointer_focus(mx: f32, my: f32)` · `fn home_wheel(dy: c_int)`

**detail.rs** (callers app.rs:491/587/631/632/667/674/773/923-948 + internal):
- `fn open(idx: c_int)` · `fn open_rk(rk: &str)` · `fn open_rk_season(show_rk: &str, season_num: c_int)`
- `fn close()` · `fn move_focus(sym: c_int)` · `fn on_ok() -> bool` · `fn last_resume_ns() -> i64`
- `fn update(dt: f32)` · `fn draw()`
- `fn focus() -> c_int` · `fn is_show() -> bool` · `fn selected_ptr() -> *mut PmsMovie`
  (internal-only today, but `pub(crate)` — keep as forwarding wrappers, signatures frozen)

**app.rs**:
- `#[no_mangle] pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int, pms_token: *const c_char, demo_url: *const c_char) -> c_int` — the C boot shim (main.c:97). Any Route/focus refactor stays **inside** its body.

---

## (C) Hard invariants (must survive every step)

1. **`make` green after every step** — the only correctness signal.
2. **LG-SDL raw byte-offset key decode** state@16 / wcode@20 / sym@24 (app.rs:404-406); mouse
   mx@20/my@24; wheel@20. And the **webOS wcode set**: BACK 461/482, PAUSE 72/415, PLAY 450/19/402,
   Stop 413, nav 412/417, pointer 0x1e4; auto-repeat bit 0x100, pressed low-byte==1. **Untouched.**
3. **Focused-last grid z-order** (home.rs:230-312): all shelves' non-focused cells first (Shelf
   skips the focused cell when `sp>0.5`, :232), THEN the single focused card+ring+title LAST in
   `Grid::draw` (:296-312) — spans the whole grid, so a Shelf can **never** own a self-contained
   `View::draw`. `Painter` has no z-index/clip.
4. **Backdrop per-element alphas** (home.rs:83-111) + **detail draw_backdrop self-managed alphas**
   (detail.rs:298-339) stay immediate-mode; `Painter::ambient` deliberately ignores cascade alpha
   (mod.rs:158). Do **not** fold these into `p.alpha()`.
5. **section_y flow** (detail.rs:135) is the ONLY below-hero Y source; never reintroduce hard-coded
   per-section Y. Section id contract 0 hero / 1 tabs / 2 episodes / 3 related / 4 cast / 5 about is
   load-bearing across `sections()/n_items()/block_h()/section_y()/on_ok()/scroll_target()`.
6. **home_update(dt) runs UNCONDITIONALLY every frame** (app.rs:1151) — home springs settle behind
   detail/player. Do NOT gate it on Route.
7. **Draw mutual-exclusion** playing > detail > home with the `playing` early-`continue` (app.rs:1194).
8. **Snap edge-trigger** (app.rs:588-597, threshold `g_snap()==0.5`: DOWN@<0.5 enters grid
   `set_snap(1.0)+set_fr(0)`; UP@fr==0 exits `set_snap(0.0)`) — split ownership (target in app.rs,
   spring chased in `Home::snap`). Must survive byte-for-byte.
9. **Byte-identical Spring motion** — `Spring::step` delegates to `gfx::spring` (mod.rs:79-80).
   Constants moved 1:1: home K_SCALE=320 / K_SCROLL=170 / K_SNAP=200; detail scroll K_SCROLL,
   card_scale @300, ep_hscroll @240. Grid focus target **1.055** with ring scalar `(s-1)/0.055`
   (home.rs:220,307) — NOT the strip's `CARD_FOCUS_SCALE=1.07` (widgets.rs:72).
10. **Backdrop only-focused-row scroll**: only the focused row's `scroll_x` animates (home.rs:224);
    ALL MAX_ITEMS scale springs step every frame regardless of hub_len.
11. **fr/fc clamped into live hub bounds** in `Home::env` (home.rs:388-393) before any indexing.
12. **`vert()` visual-column alignment** across differently-scrolled rows (home.rs:331-340).
13. **guard() / catch_unwind** wraps every `home_*` body (home.rs:407) — keep new View calls inside.
14. **Detail spring phase**: `card_scale.jump(1.0)` on every LEFT/RIGHT + UP/DOWN change
    (detail.rs:215,228); entering tabs lands on `cur_season` (:230-235); ep_hscroll pins the focused
    card to the 2nd slot (:243-250). `on_ok` sets `last_resume_ns=0` at entry (:682) then applies the
    Plex skip-<10s/>95% rule (:670-677); app.rs reads `last_resume_ns()` ONLY after `on_ok()==true`.
15. **Detail reset order** in `open` (detail.rs:178-193): `track_menu::reset()` → set
    selected/section=0/col=0 → `scroll.jump(0.0)` → `load_detail` (blocking). `open_rk`/`open_rk_season`
    reset selected/section=0/col=0 + `scroll.jump(0.0)` then load (detail.rs:725-745). `close`:
    `metadata::clear()` → selected=-1.
16. **Background suspend keeps the session** (`suspend_bufferfeed` + bg_was_playing/bg_was_paused/
    bg_pos), 0x106 restores via `resume_at` + `start_bufferfeed` (guards a double-start UAF).
17. **All ~12 `plxnative-*` headless dev-triggers** (app.rs:886-1053) poke route state directly — every
    set-site migrates atomically with the enum or headless capture/regression silently breaks.
18. **No per-frame perf regression** on weak ARM: `Painter`/`Env` are `Copy`; no per-cell boxed
    Views, no per-frame `Vec` on the hot path; detail `update()` steps exactly 3 springs; scrolling
    rows cull off-screen cells by index (no clip rect).
19. **Design-system rules** (ui/CLAUDE.md): no raw color literals (theme tokens only), no magic-y
    text (cap-band via Label/TextView), improve shared widgets rather than fork. Any new sub-View obeys them.
20. **CString keep-alive**: Button/CircleButton/TabPill/Label hold non-owning `*const c_char` — bind
    the `CString` to a `let` for the whole draw frame.

---

## (D) Ordered implementation plan

Each step: concrete edits → `make` checkpoint (+ capture where visual) → invariants touched.

### Step 6 — Detail onto `DetailView` (do this FIRST: one module, contained blast radius)

**6.1 — Scaffold `DetailView` + lazy `view()` (pure addition, dead code).**
In detail.rs add above the statics: `struct DetailView { selected:c_int, section:c_int, col:c_int,
scroll:Spring, card_scale:Spring, ep_hscroll:Spring, last_resume_ns:i64 }` with `fn new()` reproducing
the static initializers EXACTLY (`selected:-1, section:0, col:0, scroll:Spring::at(0.0),
card_scale:Spring::at(1.0), ep_hscroll:Spring::at(0.0), last_resume_ns:0`). Add
`static mut VIEW: Option<DetailView> = None;` and
`fn view() -> &'static mut DetailView { unsafe { (*addr_of_mut!(VIEW)).get_or_insert_with(DetailView::new) } }`
(mirrors home's SCENE/scene() at home.rs:398-401 but LAZY — detail has no init C-ABI). Wire nothing
yet (file has `#![allow(dead_code)]`, detail.rs:7).
- **Files:** rust-modules/src/ui/detail.rs
- **Checkpoint:** `make` green — `VIEW` unreferenced, zero behavior change.
- **Invariants:** none exercised (pure addition); sets up #15 constructor.

**6.2 — Atomic state migration: statics → fields, logic → methods, frozen fns → thin wrappers.**
THE big churn, ONE make-green step (no host test — eyeball via capture). (1) Move the helpers +
frozen fns onto `impl DetailView` as methods, replacing every `addr_of!(SELECTED/SECTION/COL)` read
with `self.selected/self.section/self.col` and every `(*addr_of_mut!(SCROLL/CARD_SCALE/EP_HSCROLL))`
with `self.scroll/self.card_scale/self.ep_hscroll`, `LAST_RESUME_NS` → `self.last_resume_ns`, across:
`selected()` (:69), `focus()` (:78), `scroll_target()` (:154), `selected_ptr()` (:164), `is_show()`
(:172 — metadata-only, may stay free), `move_focus()` (:202), `ep_hscroll_target()` (:243),
`update()` (:252), `env_of()`→`self.env()` (:268), `draw()` (:272), `set_resume()` (:670),
`last_resume_ns()` (:666), `on_ok()` (:681), `open`/`close`/`open_rk`/`open_rk_season`
(:178/197/725/738). (2) Convert the bespoke `draw_*` helpers (detail.rs:298-944) to `&self` methods
still taking a **bare Painter** (safety valve — methods, NOT trait sub-Views). (3) Keep
`section_y/block_h/sections/n_items` as free fns (no state) to minimize diff. (4) Replace each frozen
free fn with a thin forwarder of IDENTICAL signature: `pub(crate) fn open(idx:c_int){ view().open(idx) }`,
… `on_ok()->bool{ view().on_ok() }`, `update(dt){ view().update(dt) }`,
`draw(){ view().draw(&env_of(0.0), Painter::root()) }`, `focus()`/`is_show()`/`selected_ptr()`, all 12
`pub(crate)`, same names/arities. (5) DELETE the 7 statics (detail.rs:21-29). Preserve reset order
(#15), the three-spring phase (#14), and `on_ok` side effects (`route::play_movie`/`play_episode`/
`set_now_playing`; last_resume_ns default 0).
- **Files:** rust-modules/src/ui/detail.rs
- **Checkpoint:** `make` green; on TV run `capture-screen.sh` + the `plxnative-detail`/`plxnative-detailsec`/
  `plxnative-detailcol`/`plxnative-detailplay` triggers (app.rs:923-948) — focus/scroll/play unchanged, the
  wrapper signatures still drive them. Then grep `SELECTED|SECTION|COL|SCROLL|CARD_SCALE|EP_HSCROLL|
  LAST_RESUME_NS` to **zero** to prove no stale reader survived.
- **Invariants:** #5, #14, #15, #18, #19 (section_y still the Y source; springs unchanged; reset
  order; 3-spring update; tokens/cap-band).

**6.3 — `DetailView` conforms to `View`; update()/draw() route through the trait.**
Add `impl View for DetailView`: `update(&mut, env)` = the exact body of detail.rs:252-266
(scroll → `scroll_target()` @K_SCROLL, card_scale → `CARD_FOCUS_SCALE` @300, ep_hscroll →
`ep_hscroll_target()` @240, + `anim::probe`) — EXACTLY 3 springs (perf); `draw(&self, env, p)` = the
dispatcher detail.rs:272-296 (draw_backdrop → hero/compact-title crossfade `hero_a=clamp(1-scroll/400)`
→ if `is_show()` tabs+episodes → related → cast → about). `env_of` stays
`Env{dt, screen:FULL, fr:focus(), fc:0, sp:1, hero_a:1}`. Wrappers now call `view().update(&env)` /
`view().draw(&env, Painter::root())`. `draw_backdrop` stays an immediate-mode `&self` method (#4).
- **Files:** rust-modules/src/ui/detail.rs
- **Checkpoint:** `make` green; capture — hero fade-on-scroll + compact-title crossfade + all rows
  identical. Detail is now a full retained View struct with every frozen signature intact.
- **Invariants:** #4, #5, #9, #18.

> **Detail safety valve:** stop after 6.2/6.3 with the `draw_*` blocks as `DetailView` methods.
> Promoting the below-hero blocks to `(&Env, Painter)` trait sub-Views (splitting layout from draw)
> is explicitly out of scope — it risks reflow/perf with no test, and `draw_backdrop` can never be a
> cascade child anyway.

### Step 5 — Finish home's View tree (already 90% there)

**5.1 — `Card` becomes a leaf `View` owning its scale spring (the ProgressBar exemplar).**
Give `Card` (home.rs:194) fields `frame:Rect` and `movie:*mut PmsMovie`, then `impl View for Card`:
`update(&mut, env)` MOVES the per-cell scale target OUT of `Shelf::update` (home.rs:219-223):
`let t = if self.row==env.fr as usize && self.col==env.fc as usize {1.055} else {1.0};
self.scale.step(t, K_SCALE, env.dt);` (keep **1.055/K_SCALE**, NOT strip 1.07 — the ring scalar
`(s-1)/0.055` at home.rs:307 is tied to 1.055). `layout(&mut, frame:Rect, env)` stores `self.frame`
and resolves `self.movie = movie_at(row,col)` — realizing the trait's `frame:Rect` param (mod.rs:103).
`Shelf::update` (home.rs:216) keeps its `for c in 0..MAX_ITEMS { self.cards[c].update(env) }` loop so
ALL springs still step every frame (#10), THEN computes `scroll_x` for the focused row only
(:224-228). Do **not** add a self-contained `Card::draw` used by the grid pass — the focused-last
z-order forbids it (see 5.2).
- **Files:** rust-modules/src/ui/home.rs
- **Checkpoint:** `make` green; capture optional (no pixel change — motion byte-identical, K_SCALE +
  target 1.055 moved 1:1).
- **Invariants:** #9, #10, #18.

**5.2 — `Grid`/`Shelf` conform to the trait signatures, keeping the two-pass focused-last draw.**
Change `Grid::layout(&mut, env)` (home.rs:268) to the trait's `layout(&mut, frame:Rect, env)` —
`frame` is `env.screen` (Rect::FULL), so the `PEEK_Y→GRID_TOP_Y = 828→150` lerp (:269) and
`scroll_y` stay inside unchanged. Add `impl View for Grid { update (:259-266); layout(frame,env);
draw (:274-313) }`. **KEEP the two passes verbatim** in `Grid::draw`: non-focused cells via
`Shelf::draw_cells` (with the `sp>0.5` skip, :232) for ALL shelves, THEN the single focused
card+ring+title LAST (:296-312). `Shelf` keeps inherent `update` + `draw_cells` (`draw_cells ≠
View::draw` by design — #3). Leave `nav/vert/hit_test/wheel` (:315-371) as inherent input methods
(they write fr/fc and read sibling `scroll_x` for visual-column alignment — #12 — so cannot be trait
methods). `home_draw` calls `h.grid.layout(env.screen, &env); h.grid.draw(&env, p);`. All bodies stay
inside `guard()` (#13).
- **Files:** rust-modules/src/ui/home.rs
- **Checkpoint:** `make` green; capture — focus ring (GLOW_PAD, CARD_RING_RAD*s, focus=(s-1)/0.055)
  and scale-pop pixel-identical; verify the focused card paints over every neighbor shelf and the
  hub-title lift (:283-292) is intact.
- **Invariants:** #3, #9, #12, #13, #18.

**5.3 — `Home` conforms to `View`; `home_*` stay thin guarded wrappers.**
Add `impl View for Home` composing the draw block (`bg.draw(env,p); if env.hero_a>0.01 {
hero.draw(env, p.alpha(env.hero_a)); } grid.layout(env.screen,env); grid.draw(env,p)`). Preserve the
`home_update` ORDER (home.rs:419-422): step `snap` (K_SNAP=200) BEFORE building `env` for
`grid.update`. Keep `frame_clear(CLEAR_RGB)` + `guard()` in the `home_draw`/`home_update` **wrappers**
(`View::draw` can't clear the GL buffer). `Backdrop`/`Hero` already `impl View` and keep their
explicit per-element alphas (#4). `fr/fc` clamp in `Home::env` (:388-393) preserved (#11). Accessors
`row/col/snap_target/set_row/set_snap_target` (home.rs:24-38) untouched → app.rs `g_fr/g_fc/g_snap/
set_fr/set_snap` (app.rs:116-124) compile unchanged. No new C-ABI.
- **Files:** rust-modules/src/ui/home.rs
- **Checkpoint:** `make` green; capture the hero→grid snap sweep — `hero_a=clamp(1-sp/0.55)` group
  fade, `env.sp` continuum (scroll*sp, `sp>0.5` gate, title alpha `with_a(_,sp)`) unchanged.
- **Invariants:** #4, #9, #11, #13, #18.

> **Home safety valve:** `nav/hit_test/wheel` + `Backdrop` + the focused-last pass stay documented
> immediate-mode carve-outs. Do not force them into the depth-first trait shape.

### Step 7 — app.rs Route consolidation

**7a.1 — Introduce `enum Route`/`Overlay` as a shadow, kept in sync (no reads flipped).**
Add the two enums + `let mut route = Route::Home;` alongside the 5 bools (app.rs:329-333). At EVERY
existing set-site of `playing/detail_open/menu_open/info_open/chapters_open` — key handlers,
lifecycle (0x103/0x106, :360-401), and the ~12 `plxnative-*` dev-triggers (:886-1053) — ALSO write the
matching `route` (`playing=true`→`Player{None}`; `detail_open=true`→`Detail`; `menu_open=true`→
`Player{Menu}`; back-to-home→`Home`; etc.). Reads still use the bools. This enumerates every set-site
before the flip and proves the mapping compiles.
- **Files:** rust-modules/src/app.rs
- **Checkpoint:** `make` green — pure shadow, zero behavior change.
- **Invariants:** #17 (forces every set-site to be found).

**7a.2 (THE landing) — flip reads to Route, delete the 5 bools.**
Replace reads (~40 sites): `playing` → `matches!(route, Route::Player{..})` (draw dispatch :1167,
early-continue :1194, scrub gating :413/:705-765/:1082-1118, lifecycle :362-395); `detail_open` →
`matches!(route, Route::Detail)`; `menu_open`/`info_open`/`chapters_open` →
`matches!(route, Route::Player{overlay:Overlay::Menu|Info|Chapters})`. Set-sites: start_bufferfeed
successes → `Route::Player{overlay:Overlay::None}`; Stop/BACK → `Route::Home`; detail opens →
`Route::Detail`; overlay opens → `Route::Player{overlay:..}`; overlay closes →
`Route::Player{overlay:Overlay::None}`. **Lifecycle:** 0x103 sets `route=Home` **and keeps**
`suspend_bufferfeed` + bg_was_playing/bg_was_paused/bg_pos (#16); 0x106 restores
`route=Player{None}` via `resume_at`+`start_bufferfeed` — the separate `bg_was_playing` flag carries
suspended-ness, do NOT encode it in Route. `home_update(dt)` STAYS unconditional (#6). Migrate ALL
~12 `plxnative-*` set-sites in lockstep (#17). Preserve the snap edge-trigger (#8), LG key decode (#2),
`plex_run` C-ABI. Keep `g_fr/g_fc/g_snap/set_fr/set_snap` + home `fr/fc` + detail `section/col` as
SEPARATE focus owners (no unification yet).
- **Files:** rust-modules/src/app.rs
- **Checkpoint:** `make` green; on-TV capture through the full route graph home→detail→play→
  menu/info/chapters→back; run `plxnative-detail*`/`plxnative-menu`/`plxnative-info`/`plxnative-chapters`/`plxnative-play`/
  `plxnative-autoplay`/`plxnative-autoseek`/`plxnative-autopause`/`plxnative-grid` — each reaches the same route + capture;
  verify background→foreground resume single-`Load`s (bg_was_playing path, no double-start UAF).
- **Invariants:** #2, #6, #7, #8, #16, #17, #18.

**7b (OPTIONAL — behind the stop valve) — unified FocusPath.**
ONLY if 7a is rock-solid. Introduce `struct FocusPath { section:c_int, col:c_int, snap:f32 }` as the
single focus source; re-point the `g_fr/g_fc/g_snap/set_fr/set_snap` shims (app.rs:116-124) at it, and
make home `fr/fc/snapTarget` + `DetailView.section/col` views onto it. HARD constraints: the frozen
signatures (`home::row/col/snap_target/set_row/set_snap_target`, `detail::move_focus/focus`) and the
snap edge-trigger (#8, split target/spring ownership at threshold 0.5) survive byte-for-byte; respect
the `g_fc`-has-no-setter asymmetry (fc written only via move_focus/pointer_focus/wheel);
`plxnative-grid` (which pokes `set_fr/set_snap` directly) still resolves through the shims.
- **Files:** rust-modules/src/app.rs, rust-modules/src/ui/home.rs, rust-modules/src/ui/detail.rs, (new focus module)
- **Checkpoint:** `make` green + capture + rerun ALL `plxnative-*` flows AND manually exercise the
  hero↔grid snap boundary. **On ANY regression of the snap edge-trigger or a headless trigger,
  ABORT 7b and revert to the 7a state.**
- **Invariants:** #2, #8, #17.

---

## (E) STOP condition

**Stop after 7a.** At that boundary home (5.1-5.3) and detail (6.1-6.3) are full retained View
structs on tokens + shared components, every frozen C-ABI signature intact, and app has one exclusive
`enum Route` — ~90% of the architectural-uniformity win with none of the shared-focus-state risk.
7b is the only step that mutates app.rs-shared focus state and the only one that can break the
fragile snap edge-trigger (app.rs:588-597) or the headless harness; it is purely additive uniformity
and is the correct thing to sacrifice.

## (F) Top risks & mitigations

- **6.2 is one atomic ~15-site static→field rewrite with NO host test.** A missed `addr_of!` site
  leaves a stale reader diverging silently. → grep the 7 static names to zero after the step;
  eyeball via capture + the `plxnative-detail*` triggers.
- **Lazy `view()` init (detail) ≠ home's eager `scene()`.** Any path drawing detail before
  `open`/`open_rk` must degrade to the default `DetailView` (selected=-1). app.rs always opens before
  routing to Detail (app.rs:491/674) — verify.
- **Focused-last z-order** (#3): a naive `impl View for Grid` with depth-first draw would paint the
  ring under neighbor cards. → keep Grid's two-phase draw; Shelf never owns a self-contained draw.
- **1.055/0.055 vs strip 1.07** (#9): mixing shifts the ring glow. → move constants verbatim,
  capture-compare.
- **Route lifecycle** (#16): `playing=false` on background while keeping the session — mapping to
  Route must NOT lose bg_was_playing/suspend-restore. → keep bg_was_playing SEPARATE, not in Route.
- **~12 `plxnative-*` dev-triggers** (#17): a missed set-site silently breaks headless capture, not the
  build. → migrate them in the SAME commit as the bool deletion; rerun the full trigger matrix at 7a.
- **Backdrop carve-outs** (#4): folding self-managed alphas into `p.alpha()` visibly breaks the hero
  fade. → keep both immediate-mode even inside their View structs.
- **Perf** (#18): no per-frame allocation on the weak-ARM 60fps hot path — View methods and
  `matches!()` stay alloc-free (Painter/Env are Copy); no per-cell boxed Views, no per-frame Vec.

---

## (G) Step 8 — share the scroll/cull/hero infra (DONE; supersedes the "stop at 7a" valve)

The old §E stopped at 7a and the 6.3 valve refused to promote detail's below-hero blocks, citing
"reflow/perf **with no test**." That blocker went stale once the draw profiler (`ui::profile`,
glFinish-bracketed per-phase GPU ms, `/tmp/plxnative-profile`) + the once/sec `FPS=` log landed — reflow and
fill-rate are now directly measurable on-device. So the migration was extended to unify the two
screens' *shared* machinery (both are: backdrop → top hero that fades as content scrolls up → hand-rolled
off-screen culling, since `Painter` has no clip/scissor):

- **8.1** — three retui-core primitives in `mod.rs`: `on_axis(start,extent,span,lead)` (the ONE cull
  test), `hero_alpha(progress,fade_end)` (the ONE hero-fade curve), and `ScrollColumn`+`Column` (the
  scroll-into-content container).
- **8.2** — detail `sections() -> Vec<c_int>` becomes `([c_int;6], usize)` (deletes the one hot-path
  heap alloc, prerequisite for the container calling `len()/height()` per frame).
- **8.3** — BOTH screens adopt `on_axis` (all 6 hand-rolled band/index culls) + `hero_alpha` (home's
  `Env.hero_a`, detail's hero + compact-title crossfade). Home's two culls flip `<=/>=`→`</>`, a
  zero-visible-pixel edge micro-divergence only. Home keeps its two-pass focused-last grid + Backdrop
  alphas verbatim.
- **8.4** — detail's below-hero flow becomes a `ScrollColumn` (`impl Column for DetailView`):
  `child_top` replaces `section_y` as the sole Y source (deleted), `ScrollColumn::draw` owns the scroll +
  `on_axis` cull, each `draw_*` draws local-coord (`section_y(N)` → `0.0`) under a pre-translated child
  painter. **Deviation from the design's full 8.4:** the sections stay free `draw_*` fns and the springs
  (`card_scale`/`ep_hscroll`/`related`) stay on `DetailView` — NOT promoted to per-section Views owning
  their springs. This keeps invariant #14 untouched and avoids the reflow/spring-relocation risk for
  marginal architectural gain; detail still becomes a real container-composed screen sharing the infra.
- **8.5** — docs (this section + `ui/CLAUDE.md`).

**Home is NOT forced into `ScrollColumn`** — its fixed-pitch grid is not a variable-height document flow,
so it shares only the two leaf primitives (`on_axis`/`hero_alpha`) and keeps its cross-row focused-last
draw. `ScrollColumn::draw_with_overlay` exists as the supported hook if home is ever migrated.

Parity was proven by algebra + an adversarial review (child_top(i) == old section_y(s[i+1]) on movie
and show; cull/scroll/borrow/#14 all sound). On-device profiler/FPS/capture verification is the final gate.

**New stop condition:** stop after 8.5. Off-screen culling and the hero-fade are ONE core mechanism each,
detail is a container-composed screen, home is unchanged bar the two primitives. **7b (unified FocusPath)
remains the deferred/optional step** — the profiler does not de-risk the manual snap-boundary sweep, so it
stays the correct thing to sacrifice.
