# Codebase-Wide Simplification Audit — Final Report

*Native webOS Plex client (`plex-native-poc`). 96 candidate findings triaged: 84 confirmed against the tree, 12 refuted. Each confirmed finding passed an adversarial fact-check and a value-judgment (is acting on it a net win for *this* codebase).*

---

## 1. Executive summary

**Counts.** 84 findings confirmed factually accurate; of those, **34 are worth acting on** (collapsing to ~32 distinct actions once the three duplicate "filler `Env` literal" reports are merged) and **50 are technically-true-but-not-worth-the-churn**. 12 findings were **refuted** (half-wrong evidence or a fix that would break something). Exactly **one** confirmed finding is a genuine functional bug: `tests/run.py --build` still invokes the removed `zig` toolchain and cannot succeed.

**Health verdict: strong.** This is a well-factored codebase whose "cruft" is overwhelmingly (a) *documentation lag* from two large completed migrations (zig→webOS NDK; C→Rust) and (b) *deliberate forward scaffolding* under intentional `#![allow(dead_code)]`. There is no rot in the fragile paths — the FFI/Starfish/ACB-bind-order/demux/playback code is clean, and the design system (`ui/`) already enforces tokens/components. The refactor-magnitude findings (the `plex_run` god-function, `route.rs`-as-second-Plex-client, the 17 session `static mut`s) are all *documented, deliberately-deferred* work, not oversights.

**The three sentences that matter.** (1) Fix the one real bug — `tests/run.py:533`'s `cargo zigbuild` — because it silently breaks the harness's headline `--build` command on the only verification surface you have. (2) The single highest-leverage cheap batch is **documentation truth-up**: `CLAUDE.md` says the FFmpeg demuxer is opt-in when it is the *default*, `plex/mod.rs` and `docs/plex-native-plan.md` call the live typed data layer "dead scaffold," and several build docs still say `zig` — these mislead future sessions on the exact facts your instruction files exist to be authoritative about. (3) Everything else worth doing is small, low-risk hygiene in the safe (non-playback) layers; leave the deferred structural rewrites until their own migration lands.

---

## 2. Top recommendations

Ordered by value-per-effort. Findings the value-lens marked *not worthwhile* are in §4, not here.

### Quick wins (small effort)

**A. Fix the FFmpeg-demuxer documentation inversion** — `CLAUDE.md:103` (also `:128`), `rust-modules/src/ff.rs:21`
Why: `CLAUDE.md:103` calls `ff.rs` "an opt-in alternate demuxer (bisect via `/tmp/plxnative-demux=ff`) … not yet the default." The code says the opposite: `USE_FF` defaults to `true` (`ff.rs:23`), `boot()` (437–446) logs "demuxer = libavformat" by default and only falls back to `mkv.rs` when the trigger equals `"mkv"`. `ff::use_ff()` gates the live demux path (`threads.rs:65`, `pump.rs:117/126/176`), so the doc names the wrong demuxer *and* the wrong bisect trigger — pointing a debugger at the wrong file. The in-function comment at `ff.rs:437-439` already states the correct behavior. Change: rewrite `:103` to "libavformat is the DEFAULT demuxer; `/tmp/plxnative-demux=mkv` falls back to `mkv.rs` (still the live fallback, not removed)"; fix the stale `:128` "opt-in FFmpeg demuxer" and the `ff.rs:21` doc-comment in the same pass. Risk: none (docs).

**B. Truth-up the typed Plex layer's "unused" claims** — `rust-modules/src/plex/mod.rs:9-11` (+ redundant `account.rs:14`, `session.rs:6`); `docs/plex-native-plan.md:15,559`
Why: Both assert the typed `plex/` layer is "dead scaffold / Currently unused (`plex::init` is never called)." It is the *live backbone* of the data layer: `home_hubs` (`pms.rs:240`), `metadata/children/related` (`metadata.rs`), `scrobble/unscrobble` (`detail.rs:1126-1128`), `image_transcode_path` (`posters.rs:126`), all of `account.rs` (via `auth.rs`) and `session.rs`. `plex::install()` sets the same singleton and is called at `app.rs:247/305` and `auth.rs:281`. The module-wide `#![allow(dead_code)]` (mod.rs:11) cascades, so `account.rs:14`/`session.rs:6` re-declarations are redundant. Change: rewrite the doc to "read layer live via `plex::install`/`client()`; only the playback/decision path (`route.rs`, transcoder) still bypasses it," drop the two redundant inner attrs. Risk: none (docs + attr removal; no `-D warnings` in the Makefile).

**C. Delete the orphaned focus-ring API + fix its stale gotcha** — `ui/mod.rs:177-183`, `theme.rs:175`, `ui/CLAUDE.md:65-66`, `label.rs:12-13`
Why: `Painter::ring` has zero callers (`grep '\.ring('` empty); the glow-ring was folded into `card()`'s composite pass (comments at `card_row.rs:207-210`, `theme.rs:179-181` say "replacing the old glow ring," past tense). Its only-consumer const `CARD_RING_PAD_STRIP` is also 0-use. Worse, `ui/CLAUDE.md`'s *Gotchas* section — the file contributors are told to read first — still presents `Painter::ring` as the live focus-ring API. Change: delete the fn + const; revise (don't delete) the CLAUDE.md bullet to drop the `Painter::ring` mention while keeping the still-live `CARD_RING_RAD`/`FS_SRC` baked-color contract; drop the `label.rs` doc mention. Risk: low — pure UI-draw; `make` fails loudly if either symbol were live.

**D. Add `Env::inert()` and dedup the filler-`Env` literals** — new ctor in `ui/mod.rs` (next to `Env`); replace at `player_hud.rs:17-18` (promote `hud_env`), `detail.rs:859`, `login.rs:64`, `info_panel.rs:176`
Why: The verbatim `Env { dt:0.0, screen:Rect::FULL, fr:0, fc:0, sp:0.0, hero_a:0.0 }` throwaway (leaf widgets ignore it) is hand-built at 4 genuine sites (plus `player_hud` already factored it locally). `Env` is bridged from the old C globals and has visibly accreted fields, so one filler ctor removes the "6 coordinated edits when a field is added" drift. Change: `pub const fn Env::inert()`; leave `profiles.rs:143` (`fc: s.fc`) and `detail.rs:551 env_of` (live `focus()/sp:1.0`) alone — they are **not** stubs. Prefer a named `inert()`/`widget()` over `impl Default` so a screen that *should* compute a real env can't silently grab a zero one. Risk: low (UI). *(This consolidates three separate findings that all describe the same fix.)*

**E. Extract `link_program(vs, fs) -> Option<u32>` for the 6 shader-program create sites** — `gfx.rs:224,256,269,506`; `text.rs:174,191`
Why: `glCreateProgram → attach VS → attach FS → glBindAttribLocation(0,"a_pos") → glLinkProgram → check LINK_STATUS` is repeated 6× (`gfx_compile` is already shared). Change: fold the shared prefix into a helper that returns `None` on link failure; **each caller keeps its bespoke failure policy** — `PROG` `exit(1)`, `APROG` currently skips the check (routing it through the helper *adds* the missing validation), `IPROG/TPROG` early-return, `SPROG/TPROGF` degrade to 0. Do not collapse to "6 identical sites" — that would unify the differing failure semantics. Risk: low (boot-time GL init, behavior-preserving).

**F. Name the HUD-timeout constants** — `app.rs:998` and ~31 sites
Why: `+ 4500` (linger) appears 18×, the `8000` modal-nav extension 6×, the `60000` headless value 7×, all bare literals interleaved through the loop, right below where every *other* tuning value (`SCRUB_*`, `TAP_COMMIT_MS`) is already a named const. Change: `HUD_LINGER_MS=4500`, `HUD_MENU_MS=8000`, `HUD_HEADLESS_MS=60000` next to the `SCRUB_*` block — three distinct semantics, three consts, not one. Risk: none (constant substitution). Note: the `8000` case layers over a per-frame `set_hud(now + 4500)` baseline.

**G. Small dedup batch (design-system + data layer, all low-risk)**
- `text.rs:284-291` → call `gfx::upload_rgba(0, w, h, px)` (already the img.rs pattern); update its doc comment. *(Keep the `glDeleteTextures` eviction path as-is.)*
- `posters.rs:86`, `text.rs:216`, `pms.rs:59`, `ui/home.rs:497` → route NUL-scan reads through a new borrowing `cbuf::as_bytes(&[u8]) -> &[u8]`; switch `text::set_entry_key` (220-224) to a byte variant of `cbuf::set_bytes` (it takes `&[u8]`, not `&str` — add `set_bytes_raw`, don't force UTF-8). This is exactly the drift `cbuf.rs` was created to prevent.
- `route.rs:524/550/562/711` → add symmetric `set_stream_codecs(vc, ac)` (mirrors existing `set_stream_acodec`). Only the two constant sites collapse to one-liners; sites 524/711 keep their surrounding `STREAM_FPS`/`TBASE`/`TSESSION` writes.
- `detail.rs:421,842` → one `tabs_layout()` iterator feeding both `tab_focus_geom` and `draw_tabs`; the `:419` comment ("mirroring draw_tabs' x advance so they can't drift") is a keep-in-sync warning that a shared source resolves.
- `table.rs:349` → replace `[0.0,0.0,0.0,0.6]` with `theme::scrim_black(0.6)` (value-identical; enforces the design system's own rule #1).
- `chapters_panel.rs:134` → `!on_axis(x - scroll, CH_W, SCR_W, 0.0)` (the one strip that hand-rolls the documented single cull primitive).
- `tests/run.py:327` → extract `_reached_target(lines, target_s)`; the two seek ops share a verbatim tail including the `-6` tolerance and error string.

**H. Dead-code + stale-comment cleanup batch (all confirmed, all near-zero risk)**
- Delete write-only/unreachable state: `player/shared.rs:113` `segment_pos` (+ `threads.rs:179`, `engine.rs:560`); `player/pump.rs:16` always-false `Stage::Idle` clause (+ variant at `shared.rs:232`); `plex/client.rs:135` `init` + `:150` `is_installed` (drop from `mod.rs:30` re-export; `install` fully supersedes `init` *and* handles the token-swap `init` silently omits); `plex/client.rs:218-221` `StreamUrl::to_url`; `mkv.rs:41-44` `debug`/`naus_a`/`nkey`/`laced_seen` (+ increments `499/587/518`); `ui/consts.rs:6-7` `ROWS`/`COLS` (dead grid dims + a `pub` shadow of `app.rs`'s local `COLS`); `src/app.h:20,23,24` `RESUME_REWIND_NS`/`SCR_W`/`SCR_H` (no C consumer; Rust owns its own copies); `ui/profiles.rs:477` `step_focus`'s always-`true` `_skip` param (+ call sites 460/462).
- Fix stale comments/misdirection: `mkv.rs:13-14` "MUST match `mkv_ctx` in src/mkv.h" (file deleted; soften the "field offsets unchanged" note too); `src/svg.c:6` "compiled by `zig cc`" → NDK gcc; `src/starfish.h:5` "(C today; Rust once the engine is ported)" → the callbacks are already Rust (`player/mod.rs:281/334`); `Makefile:105` `NDK_HOST := $(shell uname -m | sed 's/arm64/arm64/;s/x86_64/x86_64/')` → the sed is an identity no-op, use `$(shell uname -m)`; `CLAUDE.md:216` drop the non-existent `/tmp/plxnative-mode` and `/tmp/plxnative-variant` knobs (only `-ptype` is read; `18006b9` removed the others), singularize "knob"; `docs/ui-system-migration.md:10` "zig cc ARM cross-build" → "NDK ARM cross-build" (leave the historical `rust-first-plan.md` alone).

**I. Retire the disproven soft-subs plan** — `docs/soft-subs-plan.md`
Why: Its foundation (a "Verified endpoint" `subtitles=auto` → progressive `text/vtt`) was refuted on-device; `plex/CLAUDE.md:33-36` records burn-only and warns "Don't add a soft-subtitle path that only works on paper," and `route.rs:280` only ever sends `subtitles=burn`. Its "landed, tested parser (`rust-modules/src/webvtt.rs`)" was deleted (77c5af6). Change: archive with a prominent SUPERSEDED banner pointing to `plex/CLAUDE.md` (matching how `buffer-feed-plan.md` is kept as flagged history), or delete. Risk: none (no code references it).

### Structural (medium/large effort)

**S1. Fix the broken `--build` path — restore the harness's headline command** — `tests/run.py:526-542` (esp. `:533`)
Why: `do_build()` runs `cargo +nightly zigbuild … --target arm-unknown-linux-gnueabi.2.24` with `-C target-cpu=cortex-a53`. The build migrated off zig to the webOS NDK (`e07430c`); the Makefile uses `cargo +nightly build`, plain triple, and the **load-bearing** `cortex-a9` (a53 codegen SIGILLs — the README even inverts the rationale). `./tests/run.py --build` (documented at `:21`) therefore cannot succeed — zig/cargo-zigbuild are gone. This is the only *functional* defect in the whole audit, on the only verification surface (no host test suite). Change: delete the hand-rolled cargo block and shell out through the existing `make()` helper — `make(["all"])` then `make(["deploy", …])` — which also removes the flag duplication that caused the drift. Effort medium, impact high, risk low (host-side Python only). Also fix the same stale facts in `rust-modules/README.md:38-42,24-29` (zigbuild/glibc-2.24/cortex-a53/`-lunwind`/`src/playback.c`).

**S2. Migrate `PmsMovie` off its vestigial C-ABI shape** — `rust-modules/src/pms.rs:13-33`; `lib.rs:1-4`
Why: The C side references only `plex_run` + the two starfish callbacks (grep of `src/*.c`), yet the browse catalog is still a C-struct — fixed `[u8;N]` buffers, `c_int` fields, a `static mut [PmsMovie;256]` read via raw-pointer accessors — forcing `cfield()/set_c()` copy dances and `unsafe` derefs across `route.rs`/`detail.rs`/`home.rs`/`app.rs`. `metadata.rs` already proves owned `String`/`Vec` works on the same paths (its header names `pms.rs` as the last C-buffer holdout). The no-alloc rationale is not load-bearing here (the catalog is built once per fetch, and reads already allocate via `cbuf::get`). `lib.rs:1-4`'s claim that these modules "expose the same C ABI its `src/*.h` declares" is now false (those headers/callers are gone). Change: `Vec<Movie>` with owned fields + index-range hubs/hero (a field-type swap, not a rearchitecture), drop the raw accessors, fix the `lib.rs` doc. Effort large, impact medium; do it incrementally with on-device verification. *(The `lib.rs` doc fix is a cheap standalone win regardless.)*

---

## 3. Themes

**The dominant signal is documentation lag from two finished migrations, not code rot.** Roughly a third of confirmed findings — and *every* stale-comment finding worth acting on — trace to (a) the zig→webOS-NDK toolchain move and (b) the C→Rust port. Comments still name `zig cc`, `cortex-a53`, `glibc 2.24`, `-lunwind`, `src/playback.c`, and `src/mkv.h`; instruction files (`CLAUDE.md`, `plex/mod.rs`, `docs/plex-native-plan.md`) still describe the FFmpeg demuxer as opt-in and the typed data layer as unused. In a solo-dev, on-device-only project where `CLAUDE.md` and module docs *are* the load-bearing cross-session memory, these are the highest-value fixes despite being one-liners: an authoritative file that inverts reality on "which demuxer runs" or "is the data layer live" costs real debugging time and misdirects future sessions.

**Most "dead code" is deliberate, self-documented scaffolding — and correctly so.** The `plex/` typed client (`hubs.rs`, `library.rs`, `client.rs` speculative ops), the `ff.rs` BSF/constant clusters, `press::is_long/was_long`, `ui::Size`, `Badge::Cc`, and the `admin`/`restricted` DTO fields are all under intentional module-level `#![allow(dead_code)]`, tied to written plans (`docs/plex-api-migration.md`, `docs/ffmpeg-demuxer-plan.md`, `docs/ui-viewtree-plan.md`). The value-lens correctly declined ~all of these: they emit no warnings, cost nothing, and match a documented convention of building complete, spec-derived, forward-looking surfaces. Pruning them would fight the author and get re-added at migration time. The genuine deletions that survived triage are *un-scaffolded* port artifacts — write-only counters, an always-false enum arm, a superseded ring API, redundant `init`.

**The design system is healthy; the duplication findings mostly hit stable plumbing, not screens.** `ui/` already routes through tokens, `Popover`, `TableView`, `CardRow`, `card_row::title_lift`, and `on_axis` — so the surviving UI dedups (season-tab loop, chapters cull, one raw color literal, `Env` filler) are small last-mile misses, and the *larger* proposed UI merges (PopoverTable, popover-singleton macro, episode/chapters→CardRow, `draw_rect` family) were rightly declined as over-abstraction of correctly-composed code. The real duplication weight sits in the GL FFI layer (frozen ABI, low drift risk) and the demuxers (`mkv.rs`/`ff.rs`, whose duplication resolves for free when `ff.rs` proves out and `mkv.rs` is deleted per plan).

**The big structural findings are all documented-deferred, and deferral is the right call.** `plex_run` (~1640 lines), `route.rs` as a parallel Plex client, the 17 session `static mut`s, and the hand-rolled decision-body parse are real — but each is explicitly recorded as intentional backlog (`docs/ui-viewtree-plan.md` "stop at 7a," `docs/plex-api-migration.md` "playback deferred — typed transcoder/timeline stale vs route.rs"). They live on the fragile, on-device-only-verifiable playback path where migrating prematurely risks regressing HDR10/4K-HEVC/MDE/session correlation for an internal-cleanliness win. Leave them until their own parity-first migration lands.

---

## 4. Deliberately NOT recommended

*Read this before re-opening any of these. "Confirmed" = the facts are right; the action still isn't worth it for this codebase.*

**Refuted (evidence half-wrong or fix would break something):**
- `plex/library.rs` `section_items_paged/metadata_many/all_leaves` "no kept rationale" — **false**: `docs/plex-api-migration.md:445-447` names all three as intentional kept capacity.
- `account.rs` `Resource::local_connection` "reimplemented inline at two sites" — **false**: only one site does connection selection, and its logic *differs* (refuses remote); the proposed reuse would change behavior.
- `chapters_panel` "re-implements episode picker; back with CardRow" — **false**: episodes use `TextView` not `text::elide`, label every card (CardRow labels only the focused tile), and already share the real leaf (`draw_card`).
- `route.rs` "X-Plex identity three divergent values the server sees" — **false**: the PMS data transport emits *no* identity headers; the `client.rs` fields are never transmitted.
- `app.rs` "D-pad-mode idiom duplicated verbatim 3×" — **false**: exact block appears 2×; the third and the two cursor-hide sites are different idioms.
- `app.rs` "`g_`-prefixed forwarders, 4+14 cluster" — **false**: only `g_fr`/`g_snap` carry the stale prefix; the rest are clean Rust names.
- `tests/README.md` "install zig does nothing" — **false**: `run.py:533` still *needs* zig for `--build` (the README is accurate; the bug is in `run.py`, → S1).
- `vs_*.vert` "name divergence doubles the location statics" — **false**: uniform locations are per-program, independent of names; the cited `glsl!` concat mechanism doesn't exist.
- `docs/rust-first-plan.md` "`zig` appears only in this doc" — **false and self-contradictory**: 9 files incl. `CLAUDE.md` and the live `run.py`.
- `docs/plex-api-migration.md` / `plex-api-design.md` "stale/drifted" — **false**: the status banner was added *in* the migration commit; `params.rs`/`timeline.rs` are planned-deferred design, not rot.
- `docs/kodi-parity-42-plan.md` "steps landed incl. ac3PlusInfo/aacInfo" — **false**: that's a code *comment*; Step 7 is unimplemented, Steps 2/4/7 open.

**Confirmed but not worth the churn/risk (50 findings — representative reasons):**
- `ff.rs` BSF cluster, 6 libav constants; `plex/` hubs/library speculative ops; `Client::post`; `plex/client.rs` speculative transport surface — deliberate WIP scaffolding under intentional `#![allow(dead_code)]`; delete-list would be re-added at migration.
- `ui::Size`, `Section::accessory`, `Row::dim`/`Badge::Cc`, `TextView::trailing`, `VAlign::Baseline`/`HAlign::Right`, four `theme` tokens, `press::is_long/was_long`, `auth::cancel`, `admin`/`restricted` DTO fields — complete/symmetric design-system or domain API surface the codebase's "build finished reusable primitives" mandate says to keep; removing `space::XS/LG` would punch holes in a documented spacing ladder.
- `gpdebug.c`, `stub/*` extra symbols, `manifest.json library_gaps`, `run.py ALL_TRIGGERS`, `known_gap`/XFAIL — documented-kept tooling/reference; zero build/runtime cost.
- `system.rs G_WL_DISPLAY`, `text.rs` static-mut caches → thread_local, `route.rs` 17 session statics, `pms.rs urlenc_str` home, `route.rs CFG` — house-idiom (`static mut` everywhere by design) or deferred-migration state; cosmetic gain vs churn on fragile/hot paths.
- `PopoverTable`, popover-singleton macro, `draw_related/cast`, `draw_rect` family, episode/chapters→CardRow, `gfx`/`text` GLES FFI dedup, `mkv`/`ff` demuxer dedup, `mkv` vint loop, `u_screen` set-once, `feed_audio_lane` tail, `frame trailer`, `run_case/run_fps_scene` — over-abstraction of correctly-composed code, or dedup between a module and its scheduled replacement; several proposed fixes were also technically unsound (dropped `pad`, wrong return shape, non-existent `Env::default`).
- `plex_run` god-function, `route.rs` second Plex client, `route.rs` decision reparse, dev-trigger reorg, three-modal-overlay dedup, `hud_until` param — real, but documented-deferred and on the on-device-only-verified playback/input path.
- `docs/ui-framework.md`, `plex-openapi.json`, `plex-api.md`, `ui-viewtree-plan.md`, `engine-port-design.md`, `buffer-feed-plan.md` — deliberately-kept reference/history (per the project's own convention of flagging superseded docs in `CLAUDE.md`); several proposed deletes would orphan cross-refs or lose hard-won on-device gotchas (e.g. `plex-api.md`'s chunked-transfer + live-transcode-2nd-connection notes exist nowhere else).
- `HomeUser/HomeUserRef/UserTile` triple, `admin` plumbing — legitimate DTO/persistence/view-model layering; merging couples the UI to the on-disk serde schema.

---

## 5. Known items (earlier shader-focused review — folded in, not re-verified)

1. **`sdBox` SDF duplicated 3× verbatim** across `shaders/fs_src.frag:29`, `fs_shadow.frag:15`, `fs_img.frag:26`. Optional consolidation via a `glsl!` prelude concat — **gated on the shader-compile line-number debugging cost** and the fact that the most recent commit (`3412116`) deliberately moved GLSL into self-contained, standalone-compilable files. The precision-chain contract already has a single documented source with cross-references. Low priority; a cross-reference comment on each copy is the cheap floor.
2. **`glsl!` `include_str!` path resolves caller-relative** — works only because both callers sit in `src/`. Needs a one-line doc note on the macro.
3. **`glsl!` lives in `gfx.rs`; `text.rs` reaches sideways for it** — a neutral `shaders` module would read better. Neutral/cosmetic.
4. **`vs_src.vert` / `vs_text.vert` near-identical**, differ only in `u_rect`/`u_trect` naming — same 3-line rect→flipped-Y-NDC transform; not collapsible without the (nonexistent) concat mechanism, and bodies otherwise differ.

*These overlap the confirmed `gfx`/`text` FFI-dedup findings in §4, which were also declined for the same frozen-ABI / low-drift / on-device-verification-cost reasons.*

---

## 6. Coverage

**Audited (high confidence):** the full Rust core (`app.rs`, `player/`, `route.rs`, `stream.rs`/`mkv.rs`/`aq.rs`/`ff.rs`, `net.rs`, the `plex/` data layer, `auth.rs`/`session.rs`), the entire `ui/` design system and screens, the graphics/text FFI (`gfx.rs`/`text.rs`/`img.rs`/`posters.rs`), the C side (`main.c`/`starfish.c`/`svg.c`/`gpdebug.c`, `app.h`/`starfish.h`), the GLSL shaders, the `Makefile`, the stub `.so` sources, the `tests/` harness (`run.py` + `manifest.json`), and all 15 `docs/*.md`. Every confirmed finding was checked against the working tree (grep counts, line anchors, caller sets, git history) and passed an adversarial refutation attempt; 12 candidates were dropped when their evidence didn't survive.

**Not covered / lower confidence:** No runtime or on-device behavioral verification was performed — there is no host test suite, so any change here must be validated by the standard deploy-and-read-`/tmp/plxnative-events.log` loop on the TV. Correctness of the fragile paths (ACB bind order, Starfish sret/`__asm__` seam, PTS-rebase/seek, A/V-sync feed lanes) was reasoned about statically, not exercised; the recommendations deliberately steer clear of changing that behavior. Dead-code claims rest on static reachability (grep + FFI-export + dev-trigger analysis), which is conclusive for a static lib but assumes no reflection/dynamic dispatch (none exists in this codebase).

**Overall confidence: high** for the factual findings and their risk assessments; the value-judgments are calibrated to this repo's stated production-quality ethos and its documented deferral plans.
