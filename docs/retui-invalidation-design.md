# retui invalidation: the motion capability

*Design note. Written 2026-07-31, one day after `ui/idle.rs` landed. Every non-obvious claim below was read out of the tree at the cited line; a short list of things I could **not** confirm, and three claims from the proposals that are **wrong**, is at the end.*

---

## Decision

Build **`rust-modules/src/ui/motion.rs`**: one module that owns the frame clock and exports the only four things in the crate that may produce a time-varying value — `Spring` (moved from `mod.rs:188`), `Ramp` (a drawn linear deadline), `Phase` (a free-running clock), `Timer` (a silent countdown) — plus `Landing<T>`, the worker→main mailbox whose only reader reports. Each type reports at the point where skipping it is not expressible. Then, once those four exist, **`dt` stops being an `f32`**: the eleven `update(dt: f32)` arms at `app.rs:3309-3380` and `Env.dt` (`mod.rs:217`) take a `Tick` with no accessor, so a hand-rolled `self.foo += dt * 1000.0` is a compile error rather than a convention.

Rejected: a whole-frame **record/replay + digest** as the gate, because it can only be computed after the frame is *built* — it gates the swap and the GL submission but never the tree walk, which `idle.rs:47` says is where the 16 points went ("Those cost ~0.3% of a core between them; the 16% was the draw") — and the pacer that recovers the difference is exactly the settle-window heuristic `idle.rs:25-30` was written to avoid. Rejected: a `Wake` **deadline lattice** for springs and ramps, which is provably isomorphic to today's boolean for both (`gfx::spring_zeta` rings, so `|x| > ε` is not monotone in `t` and there is no crossing to solve for); it is kept only for the five genuinely quiescent countdowns, where it is the difference between "in 8 s" and "invisible".

Two pieces of the rejected proposals are adopted as **orthogonal** commits that share no state with the gate: the `Emit` token (a private-field ZST on the eleven `gfx::draw_*` entry points, which turns `ui/CLAUDE.md:54`'s "never call `gfx::*` directly from a screen" into a type error and closes the one live violation at `app.rs:3515`), and the frame digest — **as a keepalive-frame auditor behind a trigger, never as the gate.**

---

## Why

**The defect is real and it is exactly two mechanisms wide.** `note_spring` has exactly two callers, `gfx.rs:497` and `gfx.rs:523`, so springs are covered by construction. Everything else is a hand-maintained list of eight `invalidate` sites (`app.rs:1596`, `:1622`, `:3394`, `:3412`; `posters.rs:414`; `pms.rs:512`; `browse.rs:403`, `:495`) whose doc-comment census at `idle.rs:132-138` was **already false when it shipped** — `idle.rs:134` names "a route change (`app.rs`)", and no such call exists. Ten sites integrate time without a spring (`grep -rnE '(\+=|-=) *dt|dt \* 1000' rust-modules/src`, run: `anim.rs:89`, `home.rs:1164`, `library.rs:468`, `person.rs:588`, `profiles.rs:104`, `login.rs:35`, `xfade.rs:134`, `xfade.rs:151`, `detail.rs:1313`, `detail.rs:1365`). Four async landings never report at all (`metadata::pump_season` at `metadata.rs:1270`; `person::pump` at `person.rs:297`; `pump_detail`'s failure arm, which settles the spinner at `metadata.rs:1140` one line before the `return false` at `:1141` that suppresses `app.rs:3412`; `pms`'s Failed→Loading retry kick at `pms.rs:533`). And `auth.rs` contains no report of any kind, while `login::ensure_qr_tex` decodes *and uploads* the QR inside `draw` (`login.rs:88` → `:49-63`).

**A per-site list cannot be the answer, and neither can a grep.** The list decayed within a day of being written. A Makefile grep beside `Makefile:236-239` is worth having as belt, but it is a gate over a syntactic pattern, and the whole point of the brief is that the fifteenth animation should be impossible to write wrong, not merely likely to be caught. `Tick` is the only proposal in front of me that makes a mistake a **build error** at the seam where the mistake actually happens — and it is cheap here specifically because the update ladder is already eleven uniform signatures and `Env` has only four real builders (`home.rs:1117`, `detail.rs:1378`, `profiles.rs:154`, `player_hud.rs:17`, plus `mod.rs:229`'s `inert`).

**Why not the command list, in one paragraph, since it is the most ambitious option and the answer is not "later".** The decided z-order design in `docs/ui-framework-improvements.md` explicitly refuses whole-frame recording: `:52` ("**Not** 'record everything and sort'"), `:192` ("**`(Content, 0)` is not a bucket — it is the immediate stream**"), `:232` ("Unmigrated code never enters the recorder at all"), with ~95% of every frame going straight through to `gfx::*` and a ~14 KB budget at `:277`. A digest needs the opposite — 100% recording — so the two are not on a converging path, and "the ambitious option is right once the z-order command list lands" is **false here**: when Part 1 lands it will record the deferred buckets only, and a whole-frame digest will be no closer than it is today. The digest's genuine virtue — it is animator-kind-agnostic, so the fifteenth animation is covered before anyone writes it — is preserved as the auditor in step 10, at 0.5 Hz, where it costs nothing and names the animator someone forgot.

---

## The model

`ui/motion.rs` stamps the frame's clock once (`motion::frame_begin(now, dt)` at `app.rs:3076`) into two thread-locals, and holds the only spendable forms of it. Each animator kind reports on the operation that cannot be elided:

| kind | reports on | why not the other side |
|---|---|---|
| `Spring` | **advance** (`step` / `step_zeta`, i.e. today's `note_spring`) | unchanged; already exact |
| `Spring::jump` | **advance, change-guarded** (`pos != v \|\| vel != 0.0`) | `home.rs:1156-1157` calls `snap.jump(0.0)` **every frame** while `n_hubs() == 0`; an unguarded report pins 60 fps on the exact screen the gate exists for |
| `Ramp` | **advance** (`tick`) | `nav::page_alpha()` (`nav.rs:149`) is read at the root of all four gated screens every frame (`home.rs:1229`, and the peers in `library`/`detail`/`person`) and returns 1.0 at rest — a read-reports ramp pins the loop forever |
| `Phase` | **read** | all six accumulators tick unconditionally at the top of `update` (`library.rs:468` and `login.rs:35` are the first line) — an advance-reports clock pins the loop forever |
| `Timer` | **fire**, as an `invalidate()` | a countdown draws nothing; only its expiry changes a pixel |
| `Landing<T>` | **`take() -> Some`** | draining the mailbox *is* reporting it; `None` must be silent or the loop pins |

`Phase` reporting from `draw` is the one encroachment, and it is safe rather than merely tolerable: a draw-phase report can only **latch the gate on for the next frame**, never off, so it can add a present and never remove one. Draw already has heavier side effects than a TLS store — `widgets::resolve_tex` (`widgets.rs:24-29`) reaches `posters::lookup(…, Touch::Draw)` (`posters.rs:247`, `:252`, `:270`), which claims a slot and enqueues a fetch; `login.rs:60` uploads a texture. It does, however, require the ordering fix (below), without which the report is destroyed at `app.rs:3535` on the frame it is raised.

The gate keeps its current shape exactly: **whole-frame, never sub-frame, never per-widget**, still excluded from the player route at `app.rs:3436`.

### The ordering fix (prerequisite, and *latent*, not live)

The decision is at `app.rs:3436`, the draw runs `app.rs:3454-3518`, and `note_present` clears `DIRTY` at `app.rs:3535` → `idle.rs:168`. So any report raised during the draw is swallowed on the frame it is raised. `frame_begin` at `app.rs:3076` does the same to `MOVING` for the next frame.

Two proposals called this a live bug. It is not, today: all eight `invalidate` sites run on the main thread **before** the gate (`app.rs:1596`/`:1622` in the event pump; `:3394`/`:3412`; `posters.rs:414` via `poster_pump` at `app.rs:3413`; `pms.rs:512` and `browse.rs:403`/`:495` inside the route updates), and no worker thread calls it. It becomes a live bug the instant anything reports from the draw — which is step 3, and also the one-line stopgap in *The two live bugs* below. Fix: `should_present` takes-and-clears `DIRTY` (`idle.rs:158`), `note_present` stops clearing it (`idle.rs:168`), and `app.rs:3436` flips to `motion::should_present(now) || player` so the player short-circuit cannot skip the take.

---

## API surface

```rust
// rust-modules/src/ui/motion.rs   — ui/idle.rs, renamed and grown

// ---- the frame clock -------------------------------------------------------
pub(crate) fn frame_begin(now: u32, dt: f32);  // app.rs:3076, once
pub(crate) fn should_present(now: u32) -> bool; // TAKES-AND-CLEARS `DIRTY`
pub(crate) fn note_present(now: u32);           // no longer clears `DIRTY`
pub(crate) fn invalidate();                     // survives; almost nothing calls it by hand
pub(crate) fn take_presents() -> u32;
pub(crate) fn take_animators() -> u32;          // NEW: `anim=` beside `pres=` on the heartbeat

/// The frame's time as a CAPABILITY. No `dt`, and deliberately **no absolute clock either**
/// (see Risks): `Tick` can only be spent through a primitive that reports.
#[derive(Clone, Copy)]
pub(crate) struct Tick { /* private */ }
impl Tick { pub const INERT: Tick; }   // dt = 0 — what a draw-phase Env carries

// ---- 1. physics ------------------------------------------------------------
pub struct Spring { pub pos: f32, pub vel: f32 }   // verbatim from mod.rs:188-191
impl Spring {
    pub const fn at(p: f32) -> Self;
    pub fn step(&mut self, target: f32, k: f32, t: Tick);
    pub fn step_zeta(&mut self, target: f32, k: f32, zeta: f32, t: Tick);
    pub fn jump(&mut self, v: f32);                // reports iff pos or vel actually moved
}

// ---- 2. a DRAWN deadline ---------------------------------------------------
pub struct Ramp { /* t, ms — private: no silent advance */ }
impl Ramp {
    pub const fn done() -> Self;  pub const fn zero() -> Self;   // const: nav.rs:55 needs it
    pub fn to(&mut self, target: f32, ms: f32);
    pub fn tick(&mut self, t: Tick) -> bool;       // true on the frame it ARRIVES; reports while running
    pub fn t(&self) -> f32;                        // linear — the exact commit deadline
    pub fn eased(&self) -> f32;                    // smoothstep — the alpha
    pub fn running(&self) -> bool;
}

// ---- 3. a free clock -------------------------------------------------------
pub struct Phase;                                  // zero-sized
impl Phase { pub fn wrapped(period_ms: u32) -> f32; }   // READING reports

// ---- 4. a SILENT deadline --------------------------------------------------
pub struct Timer { /* absolute tick, private */ }
impl Timer {
    pub const OFF: Timer;
    pub fn arm(&mut self, t: Tick, ms: u32);  pub fn disarm(&mut self);
    pub fn fired(&mut self, t: Tick) -> bool;      // invalidates on the true frame only
    pub fn left_ms(&self, t: Tick) -> u32;         // reports — a value derived from a clock
}

// ---- 5. the discrete half --------------------------------------------------
pub struct Landing<T>(Mutex<Option<T>>);           // no `lock()` accessor
impl<T> Landing<T> {
    pub const fn new() -> Self;
    pub fn put(&self, v: T);                       // worker thread; also reports (an atomic)
    pub fn take(&self) -> Option<T>;               // main thread; reports on Some
}
```

`Spinner::phase(ms: u32)` (`widgets.rs:736`) and `StatusOverlay::phase` (`widgets.rs:930`) **lose their parameter and their field**; `Spinner::draw` (`widgets.rs:745-757`) reads `Phase::wrapped(Self::PERIOD_MS)` (760 ms, `widgets.rs:714`). A screen author writes:

```rust
Spinner::new(cx, 470.0, 26.0).tint(theme::TEXT_PRIMARY).draw(env, p);   // login.rs:146
```

There is no argument left to pass a private clock to.

`Env`, `Painter`, `View` and `Rect` are otherwise untouched. `Painter` gains no field (a `&Cell` would infect it with a lifetime and break `pub const fn root()` at `mod.rs:265`). `View` (`mod.rs:235-239`) is untouched and this design does not rescue it; the twelve-arm route ladder at `app.rs:3309-3388` survives verbatim.

---

## How coverage becomes structural — and where it is only a documented invariant

**Compile errors (a mistake does not build):**

1. **`Spring::step`'s third parameter becomes `Tick`.** All 48 `.step(`/`.step_zeta(` sites — across `library.rs`, `home.rs`, `person.rs`, `popover.rs`, `chapters_panel.rs`, `table.rs`, `detail.rs`, `widgets.rs`, `card_row.rs`, `press.rs` — fail to compile until fixed. The migration is compiler-driven; no checklist.
2. **`update(dt: f32)` → `update(t: Tick)` on all eleven arms, and `Env.dt: f32` → `Env.t: Tick`.** `Tick` exposes no `dt`, so `self.spin_ms += dt * 1000.0` does not compile. This is what closes the *class*: the ten integrators, the five countdowns, **and** `profiles.rs:258`'s bespoke `((error_ms * 5.0) as i32 % 2)` PIN blink, which no type-per-animator scheme reaches on its own. Note `Env.dt` is a live animation input, read at `home.rs:426`, `:429`, `:431`, `:866`, `:876` — all five are `.step()` calls, so all five convert mechanically.
3. **`Spinner::phase` loses its argument** — 14 call sites (`home.rs:1313`, `library.rs:1061`, `detail.rs:1439`/`:1484`, `person.rs:897`, `login.rs:97`/`:128`/`:146`, `profiles.rs:201`/`:222`/`:253`, `player_hud.rs:454`/`:685`, and `StatusOverlay`'s forwarder at `widgets.rs:958`).
4. **`Ramp`/`Timer` have private fields and no public mutator but `tick`/`fired`.** You cannot advance one without reporting. `Xfade::t` (`xfade.rs:64`) becomes a `Ramp`, so both owners inherit the report without either being edited.
5. **`Landing<T>` exposes no `lock()`.** Eight mailboxes convert: `pms.rs:388`, `browse.rs:147`/`:150`/`:151`, `metadata.rs:1080`/`:1174`, `person.rs:221`, `route.rs:851`. Deliberately **not** converted: `capture::MAILBOX` (main→worker; a reporting take pins the loop) and `route::SCROBBLE` (holds a `JoinHandle`). `Landing` is opt-in per site, never a blanket `Mutex<Option<T>>` replacement.
6. **`Emit`** (step 9): a private-field ZST minted only inside `ui/mod.rs`, required by all eleven `gfx::draw_*` (`gfx.rs:337`, `:360`, `:382`, `:402`, `:414`, `:434`, `:457`, `:554`, `:669`, `:676`, `:684`), `frame_clear` (`:150`), `clip_set` (`:131`), `clip_clear` (`:144`) and `text::draw_text`/`draw_text_fade` (`text.rs:451`/`:486`). A screen reaching GL becomes a type error.

**Documented invariant only — say it plainly:**

- Nothing stops an author deriving motion from a **non-clock** state machine that happens to change every frame (`auth`'s phase, a browse retry counter). Step 7 fixes `auth` by construction; the class is not closed by a type.
- Nothing stops a **worker thread** mutating shared state a draw reads. `Landing` covers the eight mailboxes; `auth::CTL` (`auth.rs:99`, mutated in place and read straight from the draw via `with_ctl` at `auth.rs:104`) gets a bespoke split into a read accessor and a reporting `&mut` mutator. That is a documented one-module exception, not a pattern.
- The **belt** is a ~4-line Makefile gate beside the three named clippy lints (`Makefile:236-239`): `grep -rnE '(\+=|-=) *dt|dt *\* *1000' rust-modules/src/ui` with `ui/motion.rs` and `ui/anim.rs` allowlisted (`anim.rs:89` accumulates only inside `probe`, which early-returns when the `/tmp/plxnative-anim` trigger is off — `anim.rs:47`). I ran this grep: it returns exactly the ten real sites and nothing else today, and zero after step 8. **Do not add the `SDL_GetTicks` half both proposals wanted** — see the corrections at the end.
- `KEEPALIVE_MS` (`idle.rs:75`) **stays at 2000, permanently.** Its doc's "set it to 0 once the invalidation set has been proven complete" is retired: the residue above is real, and the soak that would prove otherwise needs a television.

---

## Migration sequence

Each step compiles, ships, and leaves the app strictly better than the one before.

**1. `ui: one module owns the frame clock, and a draw can be heard` ← THE FIRST COMMIT.**
Rename `ui/idle.rs` → `ui/motion.rs` (`mod.rs:22` + the 9 call sites + the `ui/CLAUDE.md:62` row). Move `Spring` out of `mod.rs:188-212` into it, re-exported as `pub use motion::Spring` so all 48 `.step(` sites still compile. Add the `NOW`/`DT` thread-locals and `frame_begin(now, dt)` at `app.rs:3076`. **Apply the ordering fix** (`idle.rs:158`/`:168` + the `||` flip at `app.rs:3436`). Change-guard `Spring::jump` (`mod.rs:208-211`) — mandatory, because of `home.rs:1157`. Widen `MOVING` (`idle.rs:96`) from `Cell<bool>` to `Cell<u32>` and add `anim=<n>` beside `pres=` on the heartbeat (`app.rs:3612`/`:3614`). Delete the doc list at `idle.rs:132-138`.
*Behaviour change:* the 32 `.jump(` teleports now repaint — including `detail.rs:1305`/`:1309`, which run on the `pump_season` landing frame that reports nothing at all today.

**2. `ui: Ramp — a deadline you cannot run silently`** — fixes live bug #1. 2 files.
**3. `ui: the spinner reads the clock`** — fixes live bug #2. 9 files. Requires step 1.
**4. `test: a screen with an animation on it must keep presenting`** — `present_floor` + `fps:login-spinner`, no Rust. Land it here so 2 and 3 have a gate.
**5. `ui: a countdown that fires says so`** — `Timer`. Users: `detail.rs:1365` (`season_settle`, `SEASON_SETTLE` at `:295`), `home.rs:1167-1171` (`hero_flip_cd`) and `home.rs:1183-1192` (`hero_auto`), `pms.rs:452-458` (`retry_due` — fixes the Failed→Loading kick at `pms.rs:533`), `app.rs:1170`/`:2897-2901` (`refresh_hubs_at`, the 800 ms post-playback hub refresh), and `press.rs:68`'s `commit_at` (covered today only by the accident that the spring-back is still ringing at `COMMIT_MS`). `profiles.rs:66`'s `error_ms` goes to `Ramp`, not `Timer` — it is *drawn* at `profiles.rs:258`.
**6. `ui: draining a mailbox is how you report it`** — `Landing<T>`, the eight sites. Deletes five now-redundant explicit calls (`browse.rs:403`/`:495`, `pms.rs:512`, `app.rs:3394`/`:3412`). Fixes `pump_season`, `person::pump` and `pump_detail`'s failure arm.
**7. `auth: every change to the sign-in state is a repaint`** — split `with_ctl` (`auth.rs:104`) into `read`/`edit`, the latter reporting. ~20 call sites, all in `auth.rs`. Fixes the QR appearing up to 2 s late and the whole Creating→Waiting→Discovering→Error machine.
**8. `ui: dt is a capability` ← the commit that makes it structural.** `Tick` through the eleven update arms (`app.rs:3309-3380`), `Env` (`mod.rs:216-232`) and the 48 spring sites; `nav::tick` (`app.rs:3239`), `press::tick` (`app.rs:3082`) and `pms::pump` (`home.rs:1152`) with them. Mechanical, compiler-driven, zero behaviour change. Add the Makefile grep as belt.
**9. `gfx: only the painter may reach GL`** — the `Emit` token. ~15 signatures in `gfx.rs`/`text.rs`, one real call-site fix (`gfx::draw_number` at `app.rs:3515` → `Painter`), and a `Painter::clear` for the six `frame_clear` sites (`home.rs:1219`, `detail.rs:1396`, `library.rs:1031`, `person.rs:737`, `login.rs:71`, `profiles.rs:150`). Orthogonal to everything above; when `docs/ui-framework-improvements.md` Part 1 lands, `Emit` moves into `ui/frame.rs` unchanged.
**10. (optional) `ui: the keepalive frame audits itself`** — the digest, behind a trigger, on the 0.5 Hz keepalive frame only. See Risks for its two prerequisites.

Steps 2, 3, 5, 6, 7 are independent of each other and each fixes named bugs; they can ship in any order or across days. Step 1 gates 3. Step 8 must come **after** 2–7 (it removes the `dt` those steps need in order to still compile).

---

## The two live bugs

Both are frozen animations in the product **today**. Each has a one- or two-line stopgap that can ship before any of the architecture, and each is then subsumed by a type.

### 1. `ui::xfade` / `ui::nav` — every route transition

`Xfade` is `{ phase, t: f32 }` (`xfade.rs:60-65`) ramping at `xfade.rs:134` and `:151`; there is no spring anywhere in the type, so `note_spring` never sees it. Its two owners are `nav::PAGE` (`nav.rs:55`, ticked unconditionally at `app.rs:3239`) and `library::XF` (`library.rs:138`, ticked at `library.rs:474`).

The failure is worse than "the fade doesn't play": the gate freezes the panel on the last **presented** frame, i.e. at alpha ≈ 1.0 showing the *outgoing* page, so a BACK can look like it did nothing until the 2 s keepalive hard-cuts to the destination. BACK is precisely the uncovered case — all seven `press::begin` sites (`app.rs:1776`, `:2066`, `:2076`, `:2093`, `:2110`, `:2458`, `:2626`) are OK/pointer paths, so `nav_back` arms no spring, and the single SDL-event invalidate at `app.rs:1622` buys exactly one frame of a ~13-frame ramp (`OUT_MS` 70 + `IN_MS` 140, `xfade.rs:33`/`:36`). The `navosc` arm at `app.rs:3214-3230` calls `nav_open`/`nav_back` directly with no SDL event at all.

**Interim (2 lines, no new type, ships today):** beside `if crate::ui::nav::tick(dt)` at `app.rs:3239`, and beside `xf().tick(dt, ready)` at `library.rs:474`, report while the fader is running (`Xfade::is_swapping`, `xfade.rs:171`). It is in the *update* phase, so it needs nothing from step 1. **Full fix (step 2):** `Xfade::t` becomes a `Ramp`, which reports from `tick` — reporting from `alpha()` instead would pin the loop forever, because `nav::page_alpha()` (`nav.rs:149`) is read at the root of all four gated screens every frame and returns 1.0 at rest.

*Scoping note the proposals got half-right:* only `library::XF` can park in `Phase::Hold` (`xfade.rs:143-149`) waiting on a server — `nav::tick` passes `ready = true` unconditionally (`nav.rs:140`), and the module doc at `nav.rs:135-138` says why. So "waiting is not moving" is a real conformance-table row, but it applies to one fader, not two, and the `is_swapping` stopgap above is therefore slightly over-reporting on Library — acceptable for a stopgap, and `Ramp` fixes it properly.

### 2. `widgets::Spinner` on Home and Library

`Spinner::draw` (`widgets.rs:745-757`) keys ten dot alphas off `self.phase % PERIOD_MS` (`:747`), fed through the `phase(ms)` builder (`:736`) from six hand-rolled accumulators. A Home waiting on `/hubs` — the exact state the read-out exists for (`home.rs:1313`) — draws a **stopped** spinner. Same on Library's initial load (`library.rs:1061`), Detail (`detail.rs:1439`/`:1484`), Person (`person.rs:897`), Login (`login.rs:97`/`:128`/`:146`) and Profiles (`profiles.rs:201`/`:222`/`:253`).

**Interim (1 line + the 3-line ordering fix):** `motion::invalidate()` inside `Spinner::draw`. It covers all 14 call sites at once and over-presents by exactly the frames a spinner is on screen, which is what you want. **It does not work without step 1's ordering fix** — `Spinner::draw` runs inside the guard at `app.rs:3454`, and `note_present` would clear the flag at `app.rs:3535` on the same frame. That dependency is why step 1 is first. **Full fix (step 3):** delete `Spinner::phase`'s argument; `draw` reads `Phase::wrapped(PERIOD_MS)`. The six accumulators and their fields go with it (`home.rs:1164-1165`+`:1283`, `library.rs:468`+`:106`, `detail.rs:1313`+`:155`, `person.rs:588`+`:152`, `login.rs:35`+`:17`, `profiles.rs:104`+`:77`), including `library.rs:468`'s never-wrapping `PHASE_MS` — an f32-precision defect independent of the gate, which `home.rs:1165` already wraps against for exactly this reason.

---

## Cost budget

Counted in operations against a 16.6 ms frame on a 1.1 GHz A53. Nothing here was measured; the TV is gone.

| added, per frame | ops |
|---|---|
| `frame_begin` stamps `NOW`/`DT` | +2 TLS stores (was 1) |
| `Spring::jump` change guard | 2 compares × jumps executed (typically 0–2) |
| `Ramp::tick` report | 1 compare + ≤1 TLS store × **≤2** ramps (`nav::PAGE`, `library::XF`) |
| `Phase::wrapped` report | 1 TLS load + 1 store × spinners **drawn** (0–3) |
| `Timer::fired` | replaces an existing `-= dt` 1:1; +1 branch |
| `Landing::take` | identical to today's `lock().take()`; +1 branch |
| `Tick` threading | zero — a `Copy` struct of two words in place of one |
| `Emit` | zero — a ZST by reference |

Ceiling ≈ **20 extra memory ops per frame**, against a gate that saves ~38 points of a core, and below the noise floor of the ~0.3% the entire non-draw loop costs (`idle.rs:47`). `note_spring`'s ~439 calls on a settled Home (`idle.rs:28`, unverified figure) keep the identical predicate; only the sink widens from `Cell<bool>` to `Cell<u32>`.

It also **removes** work: one unconditional `f32 += dt * 1000.0` plus `home.rs:1165`'s modulo per frame. (Only one — the update ladder at `app.rs:3309-3380` is route-gated, so at most one screen's accumulator runs. One proposal claimed six.)

**Memory:** +2 thread-locals (8 B), `MOVING` +3 B, two `Ramp`s at 8 B replacing two 4-byte `t` (+8 B); −24 B of `f32` accumulators, −`Spinner::phase`/`StatusOverlay::phase`. **Net ≈ 0.** No heap, no `dyn`, no BSS table, every primitive `const`-constructible (`static mut PAGE: Xfade = Xfade::new()` at `nav.rs:55` still compiles).

**Files, cumulative:** `ui/idle.rs`→`ui/motion.rs`, `ui/mod.rs`, `ui/xfade.rs`, `ui/nav.rs`, `ui/widgets.rs`, `ui/home.rs`, `ui/library.rs`, `ui/detail.rs`, `ui/person.rs`, `ui/login.rs`, `ui/profiles.rs`, `ui/player_hud.rs`, `ui/press.rs`, `ui/CLAUDE.md`, `app.rs`, `gfx.rs`, `text.rs`, `pms.rs`, `browse.rs`, `metadata.rs`, `person.rs`, `route.rs`, `auth.rs`, `posters.rs`, `Makefile`, `tests/run.py`, `tests/manifest.json`. Roughly 800 lines changed, ~250 of them deletions.

---

## Test plan

### Host (`make check`, ~0.3 s — the only signal available without a TV)

**The unit under test is the animator TYPE, never a screen.** That is a hard boundary: `home_update` (`home.rs:1144`) enters `ui::guard` (`mod.rs:93`), whose `Err` arm calls `gfx::clip_clear` → `glDisable` (`home.rs:1598-1599` records this), and `library`/`detail`/`person` updates reach `text::text_width` → `TTF_SizeUTF8` (`text.rs:335`, `:347`), declared in a bare `extern "C"` block with no `#[link]` and no `cfg(test)` stub (`text.rs:50-58`) — unlike `ff.rs`, whose four `#[link]`s are `cfg_attr(not(test))`-gated precisely so its pure logic stays host-testable.

**A conformance table in `motion.rs`'s test module**, holding `crate::testlock::serial()` (`lib.rs:29-45`; `idle.rs:186-197` already documents why a module-local mutex is wrong — the statics are reached through `gfx::spring`, which other modules' tests also drive). Every type is `pub(crate)` and pure, so `motion.rs` drives each directly; that also keeps `xfade.rs`'s ten currently-parallel tests parallel (`xfade.rs:178-182`) instead of dragging them under the crate lock.

One row per kind, **both directions** — the negative half is the load-bearing half, because an over-reporting "fix" silently restores the full ~38-point cost and the only thing that can currently see it is a 22 s device scene:

```
Spring stepped toward a distant target   → present
Spring settled on target                 → NO present
Spring::jump to a NEW value              → present
Spring::jump to the SAME value           → NO present     ← home.rs:1156
Ramp mid-flight (tick)                   → present
Ramp arrived (tick)                      → NO present
Ramp parked in Hold (waiting on data)    → NO present     ← xfade.rs:143-149, library only
Phase::wrapped() read                    → present
Phase never read                         → NO present     ← the whole point
Timer armed and running                  → NO present     ← draws nothing
Timer on its fire frame                  → present
Landing::take() -> Some                  → present
Landing::take() -> None                  → NO present     ← else the loop pins at 60fps
```

Plus, per converted mailbox, a seed → `pump()` → assert test in the shape `metadata.rs:1655`/`:1757` and `browse.rs:629` already use (holding `testlock::serial()`). Those would have caught `pump_season`, `person::pump` and `pump_detail`'s failure arm. Plus, for step 9, the `Emit` belt as a `make lint`-style grep: `gfx::draw_*|gfx::frame_clear|gfx::clip_set|text::draw_text` outside `gfx.rs`/`text.rs`/`ui/mod.rs` must be empty.

### Device (`./tests/run.py --fps`)

Add **`present_floor`** as the mirror of the `present_ceiling` block at `tests/run.py:1140-1157`: same `parse_pres` (`:1065`), same `len(pres) < 5` false-negative guard (which is what stops a build lacking `pres=` passing vacuously), `sorted(pres)` ascending, `sp[1]`, `>=`. **Grade `pres=`, never `FPS=`** — the heartbeat counts loop iterations by design (`idle.rs:173-177`, emitted at `app.rs:3612`/`:3614`), which is exactly why three scenes carry `_idle_gate_note` (`tests/manifest.json:26`, `:78`, `:94`).

Scenes:

- **`fps:login-spinner` (new)** — `/tmp/plxnative-login` (`app.rs:415`; not in the DIAG list at `app.rs:386`, so it suppresses the boot picker), `present_floor: 30`. This is the purest scene the app has: `login.rs`'s whole `Scene` is `{ spin_ms, qr_tex }` (`login.rs:16-19`) and the module contains **zero** springs (verified against the 48-site `.step(` census, which lists no `login.rs`), so every pixel is static except a phase-driven spinner the gate cannot currently see. It also exercises step 7 — the QR must appear promptly, not on a keepalive.
- **`fps:home-detail-nav` (existing, `tests/manifest.json:135`, `run_secs: 36`)** — add `present_floor: 5`. This one and not `home-library-nav`, because the navosc arm at `app.rs:3214-3230` calls `nav_open`/`nav_back` **directly**, with no SDL event and no press on either leg, so the 70+140 ms dip runs on a screen whose only other motion is the destination's mount springs. At a 1400 ms period a working fade is ~9 presents/s; today it should read near-floor, which is itself the device proof of the bug.
- **`fps:home-idle`** keeps `present_ceiling: 3` (`tests/manifest.json:46`) unchanged as the anti-over-reporting gate; give the three `_idle_gate_note` scenes ceilings too, and drop the notes.

**`anim=<n>` on the heartbeat** is the standing auditor: `anim=0 pres=0` on a settled Home is the gate working; sustained `anim>0` on a settled screen is over-reporting — the failure mode no floor can catch. It costs an increment instead of a store.

---

## Risks / open questions

1. **`Tick` deliberately exposes no absolute clock.** One proposal's `Tick::now() -> u32` would have handed back a non-reporting millisecond source strictly more powerful than `dt` (`self.blink = (t.now() % 700) as f32 / 700.0` compiles and declares nothing), defeating its own tier-1 claim. The consequence is real work: `Timer` must hold its own absolute deadline internally and arm from the private clock, and `press::begin(SDL_GetTicks())` (`app.rs:1776` and its six siblings) stays on the **event** path taking a raw tick, because there is no `Tick` in an event handler. `up_next` (`up_next.rs:77`) stays as it is — it is on the excluded player route. Document both as the two seams `Tick` deliberately does not reach.
2. **The player route stays excluded** (`app.rs:3436`; `system.rs::clear_opaque_region` documents the video plane as slaved to our wayland surface). `player_hud.rs:454`/`:685`'s `phase(now)` and `up_next` are unaffected either way, but converting them in step 3 makes them **safe in advance** if that exclusion is ever lifted.
3. **The step-10 auditor has two prerequisites, both verified and both cheap.** GL texture ids lie in two ways: the glyph LRU frees an id and immediately reallocates (`text.rs:295` deletes through its **own** extern at `text.rs:65`, not through `gfx::delete_tex`), and `player_hud` re-specs the same id with new subtitle pixels via `upload_rgba(prev, …)`. So the auditor must fold **string bytes** for text, never the id, and needs a `TEX_EPOCH` bumped in the only two functions that create and destroy every drawable texture in the crate — `gfx::upload_rgba` (`gfx.rs:612`, `glGenTextures` at `:616`, `glTexImage2D` at `:620`) and `gfx::delete_tex` (`gfx.rs:630`, `glDeleteTextures` at `:632`) — with `text.rs:295` routed through the latter. Failure mode of getting this wrong is a *false quiet* on a diagnostic, not a frozen screen, because the auditor never gates anything.
4. **The on-screen FPS number is already stale on an idle screen.** `fps_shown` changes at most once a second (`app.rs:252-260`) and is drawn on every non-player route at `app.rs:3515`; nothing reports it, so today it freezes for up to 2 s. Do **not** "fix" it by reporting — it would defeat the gate on Home. Move it behind `/tmp/plxnative-profile`/`framedrop_on` when convenient; it is a diagnostic, and `docs/ui-framework-improvements.md`'s composition root wants it in the `System` band anyway.
5. **Everything here is a reading claim.** The device has been handed back; no step below has been compiled, deployed or run.

---

## What happens to `ui/idle.rs`

It becomes `ui/motion.rs`. Nothing device-measured about it is thrown away.

| today | after |
|---|---|
| `note_spring` (`idle.rs:117-127`) | **kept verbatim** — `gfx.rs:497`/`:523` still call it; it is the right shape and the reason springs already work |
| `MOTION_EPS` (`:65`), `KEEPALIVE_MS` (`:75`), `IDLE_POLL_MS` (`:84`), `ENABLED`/`set_enabled` (`:106-115`, the `/tmp/plxnative-noidle` A/B at `app.rs:478-480`) | unchanged. `KEEPALIVE_MS` stays 2000 permanently; its "set it to 0" note is retired |
| `frame_begin()` (`:146`) | `frame_begin(now, dt)` — also stamps the clock. One call site, `app.rs:3076` |
| `should_present` (`:154-162`) | takes-and-clears `DIRTY`; `app.rs:3436` flips to `should_present(now) \|\| player` |
| `note_present` (`:167-171`) | drops `DIRTY.store(false)`; keeps `LAST_PRESENT` + `PRESENTS` |
| `MOVING` (`:96`) | `Cell<u32>`, drained as `anim=` |
| `invalidate`'s doc list (`:132-138`) | **deleted**, not corrected. Replaced by: "the five types in this module are the callers. If you are writing this call by hand, you are probably missing a type." |
| the eight tests (`:182-291`) | kept, plus the conformance table |

The `ui/CLAUDE.md:62` row is rewritten in the same commit. The measured numbers in the module doc (`idle.rs:11-19`) stay — they are the justification, and they do not change.

---

## What this design does NOT do

- **No per-region or damage-rectangle tracking.** No damage rects, no scissored repaint, no per-widget "inputs unchanged" test, no retained sub-frame state. `mod.rs:412`'s rule — *"every frame clears + redraws the whole tree, so the way to 'avoid drawing' is to CULL what isn't visible, not to dirty-track what changed"* — sits inside the shared scroll/cull block (`mod.rs:406-416`) and is about what happens **inside** a frame. It is untouched. `on_axis` (`mod.rs:423`) and `ScrollColumn`'s index culling remain the only "skip this" mechanism.
- **The renderer stays immediate-mode.** When the gate says present, the frame below is byte-for-byte the frame it is today, `gfx::frame_clear` and all. A `Phase` read declares "these pixels came from a clock"; it stores nothing about *what* they were and is discarded by the next `frame_begin`. Dirty tracking remembers the previous picture; this remembers only whether time was consulted.
- **No record/replay slab, no digest gate, no pacer, no z-buckets.** Nothing here blocks or pre-empts `docs/ui-framework-improvements.md` Part 1; `Emit` is the one piece that will move into `ui/frame.rs` when it lands, unchanged.
- **No `View::is_animating()`**, no `dyn`, no allocation, no registry object, no enumeration, no polling. Coverage comes from there being nowhere else to get a time-varying value from — not from anything keeping a list.
- **Nothing on the player route.** `app.rs:3436` keeps its exclusion, for the unchanged `system.rs` reason.
- **It does not resolve B12 / the `View` trait**, and it does not touch the twelve-arm route ladder at `app.rs:3309-3388`.

---

## Corrections, and what I could not verify

Facts the proposals shared that I **confirmed**: two `note_spring` callers (`gfx.rs:497`, `:523`); eight `invalidate` sites; the phantom doc entry; `Xfade`'s ms ramps and lack of spring; `Spinner`'s 14 phase sites; `pump_detail`'s store-then-return-false (`metadata.rs:1140-1141`); `pump_season` and `person::pump` never reporting; the `note_present`-after-draw ordering; the `player ||` short-circuit; exactly ten `dt` integrators; exactly 48 `.step(` sites; exactly one `gfx::draw_*` bypass outside `ui/mod.rs` (`app.rs:3515`) and six `frame_clear` callers; every texture born in `gfx.rs:612` and (bar `text.rs:295`) dying in `gfx.rs:630`; `login.rs` containing zero springs; `home.rs:1156-1157`'s per-frame `jump`; `run.py`'s `present_ceiling` with no floor; no `ui/layer.rs`, `ui/frame.rs` or `ui/hit.rs`.

Wrong, and load-bearing:

1. **"`up_next.rs:190-192` reads `SDL_GetTicks()` inside a draw" — false.** `up_next.rs:191` calls `remaining_ms(now)` with a `now` passed *into* `draw`; `remaining_ms` is `up_next.rs:77`. `grep -rn 'SDL_GetTicks(' rust-modules/src/ui` returns **zero**. The four bare-word hits in `ui/` are doc comments (`idle.rs:100`, `xfade.rs:126`, `up_next.rs:30`, `:74`). Consequence: **do not add the `SDL_GetTicks` half of the proposed Makefile gate** — as a bare-word grep it is red on arrival against four comments, and with the paren it guards nothing that exists.
2. **The stale doc line is `idle.rs:134`, not `:135`.** Two of three proposals cite `:135`.
3. **"Six unconditional `+= dt * 1000.0` per frame are removed" — at most one.** The update ladder is route-gated (`app.rs:3309-3380`).
4. **The command-list proposal cites `docs/ui-framework-improvements.md:196-228`/`:217` as support for 100% recording.** That document decides the opposite at `:52`, `:192` and `:232`; the `push` at `:217` is the `Some(k)` arm of a `match` whose `None` arm calls `gfx::*` directly. It also says "twelve `gfx::draw_*`" — there are **eleven** public ones (`draw_digit` is private) — and gives `delete_tex` as `gfx.rs:629`; it is `:630`. And "no screen module changes at all" contradicts its own `frame_clear` hoist, which touches six screens.
5. **The deadline proposal's `Tick::now() -> u32` defeats its own tier-1 claim** (see Risks 1), and "twelve screens, already one uniform `update(dt: f32)`" is eleven — `up_next::tick(ctrl, now)` at `app.rs:3388` is not one of them.
6. **The ordering defect is latent, not live.** Two proposals called it a bug today; it is a prerequisite that becomes a bug the moment anything reports from the draw. Stated precisely above so nobody has to re-derive it.
7. **`nav`'s `Xfade` can never park in `Hold`** — `nav.rs:140` passes `ready = true` unconditionally and `nav.rs:135-138` says why. The "waiting is not moving" point is correct but scoped to `library::XF` alone.

Could **not** verify (read-only task; no `cargo`, no `make`, no device):

- Whether `library::update` / `detail::update` / `person::update` actually fail to link on the dev Mac. `text.rs:50-58`'s bare `extern "C"` with no `#[link]` and no `cfg(test)` stub is consistent with the claim, but dead-code elimination might save it. Treat "screen updates are not host-drivable" as worth five minutes, not as a fact. The design does not depend on it either way.
- `idle.rs:28`'s ~439 springs per settled Home frame, and every CPU number in this document.
- Anything about device behaviour, including whether the present gate is safe on the player route.