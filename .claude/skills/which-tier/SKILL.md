---
name: which-tier
description: >
  Decide WHICH verification tier a change actually needs — and, the half that gets skipped, which
  tiers are structurally blind to it. Use whenever the question is "how do I verify this", "what
  should I run", "is make check enough", "do I need the television for this", "which suite covers
  this", "did I run the right one", or before calling anything verified. Routes by WHAT THE CHANGE
  TOUCHED — pure logic, a screen, text rasterization, an animation, frame rate, the player
  pipeline, selection/resume/markers, FFI, the release feature set, packaging — to the cheapest
  tier that can actually see it, names the tiers that cannot, and hands off to `ui-sim`,
  `tv-session`, `tv-lock` or a plain `make check`. There are three tiers here with non-obvious
  blind spots, and both recorded mistakes were the same shape: a green result from a tier that was
  never able to see the thing being changed.
---

# which-tier — what actually verifies this change

This skill **decides**. `ui-sim`, `tv-session`, `tv-lock`, `fleet-plan`, `crash-triage` and
`cut-release` **execute** — it ends by handing off to one of them, and it deliberately does not
restate what they already document.

The question it answers is not "which tools exist" but **"what can see this change, and what is
structurally incapable of seeing it"**. Both are needed. A tier that cannot see your change does
not fail — it passes, which is worse, because a pass is what you were going to report.

## The two recorded ways of getting this wrong

**1. Grading a pixel with a rate.** The `--fps` tier asserts the app's once/sec heartbeat. It is
blind to what a pixel *looks* like, by construction. On **2026-08-13** an `--fps` run was started
for an *icon geometry* change and the user stopped it mid-flight — "can't find the reason to run
FPS for an icon change, better take screenshots and watch manually". In that same session the
captures earned it: they caught a **watched-disc mark reading "watched" on partly-watched shows**
(`!unwatched` is true for a container after ONE episode) — which **385 green host tests and 11 of
11 fps scenes had all passed over**. The FPS tier had no opinion about that bug and never could.
(An earlier one, 2026-07-27: a full player suite started for a UI edit, also stopped by hand —
minutes of television for a pipeline the change cannot reach.)

**For UI-only work the check is CAPTURES, LOOKED AT.** A shot nobody opens has verified nothing.

**2. Reading a bare `./tests/run.py` as the suite it used to be.** The default **inverted on
2026-08-22**. A bare run is now the **synthetic tier** — generated clips served off your
Mac through `/tmp/plxnative-playurl`, **no Plex anywhere**. Ask `./tests/run.py --list` for the
count — a number written down here has now rotted twice, the second time inside the branch that
was correcting the first. The 21 library-backed cases are
`./tests/run.py --server`; `--fps` and `--fps-player` imply `--server` (their scenes navigate a
real signed-in Home and would otherwise grade a QR screen). `--pipeline` still parses — it names
the default — and pairing it with `--server`/`--fps` is refused rather than silently resolved.

The inversion is right: the obvious command now runs for everybody, needs no credentials and
touches nobody's watch history. But **the synthetic tier cannot see the Plex half at all**, and it
is not a subset failure, it is a whole bypassed layer:

- it writes its five `route` fields **itself**, so `metadata → plan → apply_plan` is never entered
  and a regression there passes it green;
- it reaches **no resume, no markers, no Up Next, no `/:/timeline` reporter, no track SELECTION,
  and no transcode path** whatsoever;
- `engine`'s `_ =>` arm maps an unrecognised audio codec to `"AC3"` and a non-`hevc` video codec to
  the H264 payload — so a trigger that was **never read** still produces exactly the right payload
  for the AC-3 baseline case. That is the tier's own false-PASS shape, and it is why the matrix
  carries cases expecting `"AC3 PLUS"` and `"AAC"`.

**Never ship on the default alone.** `tests/README.md` has the full tier table.

## The tiers, and what each is blind to

| tier | command | cost | can never answer |
|---|---|---|---|
| **1 — host suite** | `make check` | **3.8 s warm**, no TV | anything that needs a native library, a Linux kernel, or a pixel |
| **1.5 — simulator** | `make sim` + the `ui-sim` skill | seconds, N at once | frame rate, text rasterization, anything about video |
| **2 — the device** | `tv-lock` → `wake-tv` → `tv-session` / `tests/run.py` | one television, serialized | what only a PHOTOGRAPH of the panel shows — see below |

There is a fourth blind spot that is not a tier and catches people who did everything right: **a
device CAPTURE is not the panel.** `gfx.rs` set one `glBlendFunc` for colour and alpha, so every
partial-alpha draw computed `dst.a = a² + dst.a(1−a)` and punched a hole in the app's own wayland
surface — at a=.40 the surface fell to 0.76, and the compositor showed black through it. Measured
on the panel as 43 → 33, exactly `43 × 0.76`. In a screenshot it is near-invisible; on the
television it is a hard bar, because sRGB 43 vs 33 is ~2× in luminance down there. It was reported
as *"on screenshot capture I see a pleasing fade, but on TV it is like a shadow"*, and it was never
the screen it was reported on: every scrim, every page and content fade, and every glyph drawn at
less than full alpha had it, and fades had always come out a shade darker than authored. Nothing
but a photograph could show it (`gfx.rs`, at the `glBlendFuncSeparate` call).

### Tier 1 — `make check`

`cargo +$(RUST_NIGHTLY) test --lib`, preceded by `make lint` (three **named** clippy lints —
`ifs_same_cond`, `same_functions_in_if_condition`, `if_same_then_else` — the shadowed-branch gate),
and followed by **three** host checks that are easy to forget are in here: `python3 ci/flavor.py
--selftest` (the flavour transform, whose central assertion is that the STABLE transform is the
identity), `python3 tests/test_harness.py` (pins `run.py`'s skip partition — the path a full
`manifest.local.json` never enters), and `python3 tools/plxnative-lab selftest` (the Lab
Diagnostics receiver: a real TLS listener on loopback, one accepted upload and six refusals — the
whole of what that tool exposes to the public internet, and the only gate it has, since nothing in
cargo can see a python file).

**It is not sub-second, and the figure that circulates is one of its five parts.** The ~0.3 s
everybody quotes is `cargo test --lib` alone; end to end it is now well over ten seconds warm, most
of the growth being suite size and **~7 s of the lab selftest, which is mostly two DELIBERATE
rate-limit waits** — so a ten-second-plus pause there is the target working, not a hang. (That step
also SSDP-probes the LAN to report whether a UPnP gateway is present; still no television, still no
lock.) Do not write a new number here: measure it if you need one — this file has already carried a
`3.8 s` that four separate additions made wrong.

**Do not quote a test count.** Three have already rotted in the docs, one within a single commit.
Count it yourself if you need the number:

```sh
cd rust-modules && cargo +nightly test --lib -- --list | grep -c ': test'
```

**Run it on nightly.** `make check` uses `cargo +$(RUST_NIGHTLY)`; a bare `cargo test` uses your
default toolchain and the two have disagreed — `task.rs`'s refused-spawn test passed on stable
while panicking inside `std` on nightly, which reads as flakiness and is not. Nightly is what
ships, because `-Z build-std` is what ships.

**Three structural limits.** Know them before trusting green:

1. **It cannot run the native libraries, and it no longer fails by failing to LINK.** `ff.rs` used
   to carry `cfg_attr(not(test))`-gated `#[link]` directives, so a host test that called FFmpeg
   died at link time. Everything is on `dynlib!` now, the crate links unconditionally, and such a
   test instead takes `dlopen`'s `None` branch on Darwin. Same boundary, quieter failure — **a test
   that "passes" having never entered FFmpeg or GL is the shape to watch for.**
2. **It runs on Darwin; the app runs on Linux, and they disagree.** `tools/sockprobe.c` exists for
   exactly this: on the TV's kernel `shutdown(2)` **does** abort a `connect(2)` in progress, while
   on Darwin the same call makes `connect_timeout` report *success* on a socket that never
   connected. A socket assertion passing here is evidence about macOS.
3. **Some tests are serialized on crate globals**, not parallel. `metadata.rs`'s take `lib.rs`'s
   crate-wide `testlock::serial()`; `ui/home.rs`'s take that module's `FOCUS` mutex for its
   `static mut fr`/`fc`. `ui/xfade.rs` is the cautionary case, and its own module doc says why:
   pure value semantics **with one exception that costs them their parallelism** — `tick` reports
   to `ui::idle`'s process-global dirty flag, which `ui::idle`'s own "a settled screen does not
   repaint" assertions read. Without the lock they fail *other modules'* tests intermittently,
   which is the worst shape a flake can take. **Anything you make report to the frame gate inherits
   that obligation**, and reach for `testlock` rather than a fresh local mutex when the global is
   shared across modules.

### Tier 1.5 — the simulator

The same app core on desktop SDL2 + desktop GL, drawing the real interface against your real PMS.
It answers **layout, spacing, focus, navigation, route transitions, every screen, and the whole
Plex data layer**. Several run at once (`SIM_DIR` instance roots), which is why it — not the
television — is where parallel UI work belongs. The `ui-sim` skill is the loop.

It provably cannot answer:

- **Frame rate.** Different GPU, driver and compositor. Every simulator heartbeat carries
  **`sim=1`**, so a pasted log cannot be mistaken for a device number. Never quote `fps=` or
  `loop=` from here as a perf result.
- **Text rasterization truth** — different FreeType, so stem/bar weight and the `theme::size`
  ladder stay device questions.
- **Anything about video.** The 29-symbol Starfish/ACB seam does not exist off-device; Play lands
  on the app's real failure read-out, which is correct behaviour and a convenient way to look at
  that screen.

**Two host-only traps that read as your change being broken.** The recipes are `ui-sim`'s; they are
named here only so you can tell them from a regression in your own edit.

- **`make sim-shot` hangs on a settled screen.** `PLXNATIVE_SHOT_FRAME` (`SIM_FRAME`, default 200)
  counts **presented** frames, and `ui::idle` gates presents — so a screen that settles before
  frame 200 never reaches it. Arm `plxnative-noidle` in the instance root, or drive the `shot` FIFO
  token instead, which calls `ui::idle::invalidate()` for you (`shot.rs::request`).
- **A synthetic `SDL_TEXTINPUT` SIGSEGVs inside SDL** — macOS `libSDL2` is sdl2-compat forwarding
  into SDL3, whose text event carries a `char *text` where SDL2 carries an inline `char[32]`. No
  Rust panic, no log line, the process is just gone. The FIFO's key and `ck:` tokens are safe
  because every field they set is a scalar. `docs/search.md` §3.

### Tier 2 — the device

**Take the lock first** (`tv-lock`), and **wake the set** (`wake-tv`) — asleep, every assertion
fails as "no line found", which reads exactly like a total regression. There is one television, no
OS-level mutex, and two jobs on it produce plausible WRONG data rather than a clean failure.

```sh
./tests/run.py                 # the synthetic tier — the player pipeline, no Plex, no credentials
./tests/run.py --server        # the 21 library-backed cases — selection, the whole Plex chain
./tests/run.py --fps           # the 14 UI fps scenes (implies --server)
./tests/run.py --fps-player    # + the 2 player-tier scenes (info-panel, track-menu)
./tests/run.py --list          # OFFLINE: the synthetic cases, each declaration and raster, and
                               #          which fixtures the pack is missing. THE census — a count
                               #          written into prose here rotted inside one commit.
./tests/run.py --list --server # OFFLINE: the 21 cases + 16 fps scenes, each SKIP and its reason
tools/tv-session.sh up --screen <name>   # boot into a screen, then `key` / `click` / `shot`
tools/capture-screen.sh out.png DISPLAY  # the panel, video plane composited in
```

`run.py` takes the lock itself, at the point it commits to driving the set, and releases it after
teardown — so wrapping it in `tools/tv-lock.sh with` is belt and braces, useful mainly when the run
is one step of a longer device session you do not want to lose the set between.

**`--list` is the cheap move nobody makes**, and three things about it are not obvious. It contacts
nothing — no television, no PMS, no plex.tv — but it **lists the tier you did not ask for**: a bare
`--list` prints the synthetic matrix, and the library cases with their SKIP reasons appear only
under `--list --server`. `run.py` prints two different tables on purpose, because the two matrices
share no key (a `fixture` and a `declare` on one side, an `item`/`rk` on the other). It also still
needs a TV **address** — `.tv-host`, the overlay's `tv`, or `--tv` — which it never dials; with none
of the three it exits saying so.

**And the guard hook refuses it.** `.claude/hooks/tv-lock-guard.py` classifies by COMMAND WORD, so
`./tests/run.py --list` is blocked exactly like a real run (verified 2026-08-23 by feeding the hook
that command), even though `--list` returns long before `acquire_tv_lock` is reached. That is the
right trade — a guard that parsed argv for intent would be a hole the first time a listing flag
grew a side effect — but it means the free move is not free from inside an agent session. Take a
short lease for it (`tools/tv-lock.sh with --why 'coverage listing' -- ./tests/run.py --list
--server`) or read `tests/manifest.json`. Not `PLX_TV_LOCK_BYPASS=1`: that hatch is for a human who
knows the set is theirs.

## The router — keyed on WHAT CHANGED

| what you changed | run, in order | what those tiers CANNOT see |
|---|---|---|
| **pure logic** — `route.rs`, `plex/`, `metadata.rs`, `browse.rs`, `aq.rs`, `stream.rs` parsing, `ff.rs` helpers | `make check` (+ a new test) → `make sim-shot` if a screen reads it | the native libraries; Linux syscall semantics; whether the value reaches a pixel |
| **UI layout / spacing / colour / a new screen** (`ui/`) | `make check` → `make sim-shot SIM_W=1920 SIM_H=1080` (`ui-sim`) → **one device capture, looked at** | the fps tier is blind to pixels (the 2026-08-13 watched-mark bug); a FITTED sim shot is 960x540 on a 1x display, layout evidence only — which is what `SIM_W`/`SIM_H` exist for |
| **text rasterization, fonts, the `theme::size` ladder** | `tools/font-hint-audit.py` → device capture | **the simulator is disqualified** — different FreeType; `make check` never rasterizes anything |
| **anything ANIMATED, or repainting from a CLOCK** | two host tests (runs / rests) → `--fps` with a real `fps_floor`, plus an `fps_ceiling` if the screen settles | `loop_floor` cannot see a stopped animation at all; the simulator cannot see rate |
| **frame rate / perf** | `./tests/run.py --fps` (implies `--server`), unarmed | never the simulator; never a run with a profiler armed; `drift` is reported, never asserted |
| **player pipeline, demux, Starfish/ACB, the Load payload** | `make check` (pure `ff.rs` logic) → `./tests/run.py` (synthetic) → `--server` if selection is involved | the synthetic tier bypasses `metadata → plan → apply_plan` and false-PASSes an unread trigger via `engine`'s `_ =>` arm |
| **track selection, resume, markers, Up Next, `/:/timeline`** | `./tests/run.py --server` — **only** | the synthetic tier reaches none of these; a bare `./tests/run.py` is not evidence about any of them |
| **FFI / linkage / `dynlib!`** | `tools/fwcompat.py` → `make check` → **`make sim` or `make macapp`** → device | **there is no link error any more**; and the device cannot see an Apple-ABI variadic bug — see below |
| **the release feature configuration** | `cargo +nightly check --lib --no-default-features` → `make RELEASE=1` | `make check`, `make` and `make sim` all build DEV features; a broken `RELEASE=1` is invisible to every one of them |
| **the Makefile, packaging, the `.ipk`** | `make check` (flavour selftest) → `make ipk` (runs `ci/check-package.py`) → a real `make FLAVOR=… install` | the dev loop is `make deploy`, which scp's into an already-registered app dir and **never exercises the package** |

### The rows that carry a trap

**UI layout.** The simulator is where you ITERATE and the television is where you FINISH, and that
is not a violation of *cheapest first* below — it is the one row where the cheap tier is genuinely
capable of the question you are asking (does this lay out) and structurally incapable of the last
one (glyph rasterization, the panel's own blend, the non-opaque UI plane). So: N sim shots, then
ONE device capture, looked at. `ui-sim` §*What does NOT count* is the boundary, and how to boot a
given screen is its business, not this file's.

**Anything animated.** It owes **two** tests, because the failure modes are opposite and each is
invisible to the other's gate. Host: it reports while running **and** goes quiet at rest —
`ui/idle.rs` and `ui/xfade.rs` are the pattern, including a settled-tree case that steps 439
springs and asserts *nothing* is requested. Device: an `fps_floor` scene proving it still animates
under the present gate, and an `fps_ceiling` proving its screen actually stops. The ceiling is not
politeness — an over-reporting animator gives back the whole ~38-points-of-a-core idle saving while
every `floor` in the suite still passes.

**Anything that animates from a CLOCK rather than a spring** — a millisecond ramp, a phase, a
countdown — must call `ui::idle::invalidate()` itself. `note_spring` cannot see it, and both
`Xfade::tick` (every route dip) and `Spinner::draw` (every loading read-out) **shipped FROZEN**
before they were made to report. No fps scene caught either, because those graded `loop=`. The same
applies to a new async landing that repaints: without an `invalidate()` it arrives invisibly until
the next keypress.

**Frame rate.** Never quote `fps=` from a run with `/tmp/plxnative-profile` or
`/tmp/plxnative-hwcnt` armed: `frame.ui` brackets every frame with two `glFinish`es and drops a
60 fps leg to **45**. Measured 2026-08-19, in the run that also **refuted** the "the panel
thermally throttles" story — a control leg held 60/60/60 across six runs on a set up 2 h 15 m under
continuous load. The 50 fps readings in the archived notes belong to the instrument. Take pacing in
a separate, unarmed run.

**FFI.** `tools/fwcompat.py` grades the built ELF's `DT_NEEDED` and undefined symbols against 14
real LG firmware inventories, offline, in under a second — run it after any linkage or FFI change.
But it grades **whether the app STARTS and nothing else**, and the inventories are symbol lists, so
it cannot answer anything about strings, struct layouts or code (that is `decompile-tv-lib`, and
`bind-tv-lib-abi` is the workflow that requires it). The counter-intuitive half: **the host tiers
catch an FFI bug class the device cannot.** `dynlib!` bound the variadic `curl_easy_setopt` as a
non-variadic wrapper, which is right about the types and wrong about the convention — Apple's ARM64
ABI passes variadic arguments on the stack while named ones go in registers. ARM32 and x86-64 pass
both ways identically, so it compiled, passed `make check`, ran on the television, and SIGSEGV'd
inside libcurl's `strlen` on a Mac at the first plex.tv call (found 2026-08-16 building
`make macapp`; `docs/macos-app.md` §2). No amount of device testing could have found it.

**The release configuration.** `make check` builds default features; `RELEASE=1` drops both
`devtools` and `devtriggers`, and that configuration can be broken with the entire host suite
green. Verified 2026-08-21: inserting a new `#[cfg(feature = "devtriggers")]`-gated fn *between* an
existing gated pair's attribute and its `fn` orphaned the attribute onto the new function's doc
comment, and `density_max_sweep`'s two arms collided — `error[E0428]`, only under
`--no-default-features`, with the whole host suite green throughout. A PostToolUse hook,
**`.claude/hooks/release-config-check.py`**, now runs exactly that `cargo check` after a Rust edit
— 0.55 s warm, because cargo keys unit fingerprints by feature set and the two configurations
coexist in one tree — so the usual case is caught for you. That tree is **`$TMPDIR`**, not the
crate's own `target/`: this checkout can live on a network mount, and cargo cannot take its
incremental-session lock there (`os error 45`), which failed EVERY edit with a "RELEASE-CONFIG
BREAK" that was really a filesystem answer. Set `CARGO_TARGET_DIR` to override. Confirm it is wired in `.claude/settings.json` before relying on it, run
the command by hand before shipping regardless, and prefer `crate::dev::latched_flag!` over
hand-rolling a `#[cfg]`/`#[cfg(not)]` pair.

**Packaging.** Two ipk bugs shipped undetected until 2026-08-02 — a missing
`usr/palm/packages/<id>/packageinfo.json`, and GNU `ar`'s `/`-suffixed member names, which
`appinstalld` rejects with `error_code -5`. Neither is visible from the other's side, and neither is
reachable from `make deploy` at all. `make ipk` now runs `ci/check-package.py` on the machine that
built the package; a real release goes through the **`cut-release`** skill.

## Reading the result

- **`fps=` and `loop=` are two different rates and swapping them is how a frozen animation ships.**
  `loop=` counts loop iterations — liveness only; a settled screen reports ~62 while swapping
  nothing. `fps=` counts frames actually presented. `loop=62 fps=0` is a healthy settled screen;
  `loop=0` is an app in trouble; `fps=0` alone is not a fault. They were **renamed 2026-08-01 and
  the old names REUSED** — a pre-rename log's `FPS=` is today's `loop=` — so an old log says the
  opposite of what it appears to.
- **A pass count is meaningless without the skip count beside it.** Since 2026-08-22 an `item` key
  the overlay cannot resolve is a **SKIP, not a death**: the matrix is a superset of what any one
  library holds, so `16 passed` can mean sixteen of the shapes that installation happens to own. A
  skipped case is coverage you did not get.
- **The `install:` boot line names which of two identically-named binaries wrote the log.** Read it
  before grading anything: `pidof` cannot tell the installs apart and `pkg/plxnative` is a path
  every flavour and configuration writes.
- **A `sim=1` heartbeat is not a device measurement.** Say which tier produced each claim. "Looks
  right in the simulator, not yet device-verified" is a useful, honest status; "verified" without a
  television is not.

## Handoff

| verdict | skill / command that executes it |
|---|---|
| host suite is enough | `make check` — no skill needed |
| needs to be SEEN, no TV required | **`ui-sim`** (`make sim`, N at once) |
| needs the television | **`tv-lock`** → **`wake-tv`** → **`tv-session`** |
| needs a suite on the television | `tv-lock` → `./tests/run.py`, plus `--server` / `--fps` / `--fps-player` as the router says |
| several agents, and one of them needs the set | **`fleet-plan`** — at most ONE lane gets the television; the rest go to the simulator |
| new FFI into a TV library | **`bind-tv-lib-abi`** (evidence via **`decompile-tv-lib`**) |
| it died on the set | **`crash-triage`** |
| it is going out to users | **`cut-release`** |

## Cheapest first, and why it is not just thrift

Run `make check` before waking a television. It costs under a second, it needs no NDK, no lock and
no set, and it is the **only signal you get without taking the mutex** — which matters because the
television is not merely slow, it is *shared*, and every minute you hold it is a minute another
lane is queued behind you or, worse, colliding with you and producing data that looks fine. The
ordering is therefore: host suite → simulator → device, and you move down a tier only when the one
above is structurally incapable of answering, not when it is inconvenient. The inverse is the
mistake this skill exists to prevent — reaching for the device because it feels authoritative, and
grading an icon with a frame rate once you are there.
